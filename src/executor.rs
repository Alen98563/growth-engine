//! Order Executor — REST API interface for HL exchange operations.
//!
//! Handles: place limit orders (Post-Only Alo), cancel orders,
//! IOC market orders (for shedding), GTC orders (for last-resort),
//! state fetching, and cancel-all-by-coin.
//!
//! All REST bodies use the SDK-compatible nested format:
//! `{"action": <action_dict>, "nonce": <timestamp_ms>, "signature": {"r","s","v"}}`
//!
//! Asset indices, px/sz wire strings are sourced from hl_sign.py output
//! to guarantee byte-identical match between signed action and REST body.
//!
//! B6 fix: Nonce uses AtomicU64 with CAS loop to guarantee monotonicity
//! even across NTP clock adjustments.

use crate::config::Config;
use crate::signer::{SignedAction, Signer};
use crate::traits::{MarketAdapter, MarketCapabilities, MarketType, SettlementMode};
use crate::types::{AccountState, L2Book, Oid, OrderOutcome, Position};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

/// HTTP client wrapper for Hyperliquid REST API.
pub struct Executor {
    client: Client,
    cfg: Config,
    signer: Signer,
    /// B6: Monotonically increasing nonce (AtomicU64 with CAS)
    last_nonce: AtomicU64,
}

impl Executor {
    /// Build a new Executor with config, signer, and circuit breaker.
    pub fn new(cfg: Config, signer: Signer) -> Self {
        Self {
            client: Client::new(),
            cfg,
            signer,
            last_nonce: AtomicU64::new(0),
        }
    }

    /// B6: Get next nonce — monotonic even across NTP clock adjustments.
    /// Uses CAS loop: next = max(now_ms, last_nonce + 1).
    fn next_nonce(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        loop {
            let prev = self.last_nonce.load(Ordering::SeqCst);
            let next = now.max(prev.saturating_add(1));
            match self.last_nonce.compare_exchange_weak(
                prev,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return next,
                Err(_) => continue, // CAS failed, retry
            }
        }
    }

    // ──── State ────

    /// Fetch account state from HL info API.
    pub async fn fetch_account_state(&self) -> Result<AccountState> {
        let body = json!({
            "type": "clearinghouseState",
            "user": self.cfg.address
        });

        let resp = self
            .client
            .post(format!("{}/info", self.cfg.api_url))
            .json(&body)
            .send()
            .await?;
        let data: serde_json::Value = resp.json().await?;

        let margin = &data["marginSummary"];
        let equity = margin["accountValue"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        // Use totalRawUsd (actual USDC balance) as available capital.
        // The Hyperliquid perps API does not have a "withdrawable" field;
        // it returns "totalRawUsd" (accountValue net of unrealized PnL
        // and margin allocations).
        let withdrawable = margin["totalRawUsd"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        let positions_raw = data["assetPositions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let positions: Vec<Position> = positions_raw
            .iter()
            .filter_map(|p| {
                let pos = p.get("position")?;
                let coin = pos.get("coin")?.as_str()?.to_string();
                let szi = pos.get("szi")?.as_str()?.parse::<f64>().ok()?;
                let entry_px = pos
                    .get("entryPx")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let upnl = pos
                    .get("unrealizedPnl")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                Some(Position {
                    coin,
                    size: szi,
                    entry_px,
                    unrealized_pnl: upnl,
                })
            })
            .collect();

        Ok(AccountState {
            withdrawable,
            equity,
            positions,
        })
    }

    /// Fetch L2 book snapshot from REST API (fallback when WS is stale).
    pub async fn fetch_l2_snapshot(&self, coin: &str) -> Result<L2Book> {
        let body = json!({
            "type": "l2Book",
            "coin": coin
        });

        let resp = self
            .client
            .post(format!("{}/info", self.cfg.api_url))
            .json(&body)
            .send()
            .await?;
        let data: serde_json::Value = resp.json().await?;

        let mut book = L2Book::new(coin.to_string());
        crate::order_book::update_from_ws(&mut book, &data);
        Ok(book)
    }

    // ──── Order placement ────

    /// Place a single limit order (Post-Only, Alo).
    ///
    /// Uses px_str/sz_str/asset_idx from the signature output so the REST body
    /// is byte-identical to what was signed (prevents float-format mismatch).
    pub async fn place_limit_order(
        &self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        limit_px: f64,
        reduce_only: bool,
    ) -> Result<OrderOutcome> {
        let nonce = self.next_nonce();
        let sig =
            self.signer
                .sign_l1_order(coin, is_buy, reduce_only, limit_px, sz, nonce, "Alo")?;

        let px_wire = sig.px_str.as_deref().unwrap_or("0");
        let sz_wire = sig.sz_str.as_deref().unwrap_or("0");
        let asset_idx = sig.asset_idx;

        let order_wire = json!({
            "a": asset_idx,
            "b": is_buy,
            "p": px_wire,
            "s": sz_wire,
            "r": reduce_only,
            "t": {"limit": {"tif": "Alo"}}
        });

        let action = json!({
            "type": "order",
            "orders": [order_wire],
            "grouping": "na"
        });

        let body = build_exchange_body(&action, nonce, &sig);

        let resp = self
            .client
            .post(format!("{}/exchange", self.cfg.api_url))
            .json(&body)
            .send()
            .await?;
        let data: serde_json::Value = resp.json().await?;

        let statuses = data["response"]["data"]["statuses"].as_array();
        let mut rejection_reason: Option<String> = None;
        if let Some(statuses) = statuses {
            for status in statuses {
                if let Some(resting) = &status.get("resting") {
                    let oid = resting["oid"].as_u64().unwrap_or(0);
                    if oid > 0 {
                        return Ok(OrderOutcome::Rested(oid));
                    }
                }
                if let Some(_filled) = &status.get("filled") {
                    return Ok(OrderOutcome::FilledTaker);
                }
                if let Some(error) = status.get("error").and_then(|v| v.as_str()) {
                    tracing::warn!(coin = %coin, is_buy = is_buy, error = %error, "order rejected");
                    rejection_reason = Some(error.to_string());
                }
            }
        }

        // V12.4.1: HL API may return empty status {} — silent rejection
        if rejection_reason.is_none() {
            tracing::warn!(coin = %coin, is_buy = is_buy, statuses = ?statuses,
                "order silently rejected (empty status)");
        }
        Ok(OrderOutcome::Rejected(rejection_reason))
    }

    /// Place an IOC (immediate-or-cancel) order for position shedding.
    ///
    /// Used at 90% position limit. Crosses the spread to reduce inventory.
    /// Growth Mode taker fee = 0.50 bps makes this cheap.
    pub async fn place_ioc_order(
        &self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        aggressive_px: f64,
    ) -> Result<Option<f64>> {
        let nonce = self.next_nonce();
        let sig = self
            .signer
            .sign_l1_order(coin, is_buy, true, aggressive_px, sz, nonce, "Ioc")?;

        let px_wire = sig.px_str.as_deref().unwrap_or("0");
        let sz_wire = sig.sz_str.as_deref().unwrap_or("0");
        let asset_idx = sig.asset_idx;

        let order_wire = json!({
            "a": asset_idx,
            "b": is_buy,
            "p": px_wire,
            "s": sz_wire,
            "r": true,
            "t": {"limit": {"tif": "Ioc"}}
        });

        let action = json!({
            "type": "order",
            "orders": [order_wire],
            "grouping": "na"
        });

        let body = build_exchange_body(&action, nonce, &sig);

        let resp = self
            .client
            .post(format!("{}/exchange", self.cfg.api_url))
            .json(&body)
            .send()
            .await?;
        let data: serde_json::Value = resp.json().await?;

        let statuses = data["response"]["data"]["statuses"].as_array();
        if let Some(statuses) = statuses {
            for status in statuses {
                if let Some(filled) = &status.get("filled") {
                    let filled_sz = filled["totalSz"]
                        .as_str()
                        .and_then(|s| s.parse::<f64>().ok())
                        .unwrap_or(0.0);
                    return Ok(Some(filled_sz));
                }
            }
        }

        Ok(None)
    }

    /// B1: Place a GTC limit order (non-Post-Only, can cross the book).
    ///
    /// Used exclusively for the GTC last-resort mechanism when 3 consecutive
    /// IOC shedding attempts produce zero fills. GTC orders accept taker fees
    /// (0.50 bps in Growth Mode) as a cost of breaking the deadlock.
    pub async fn place_gtc_order(
        &self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        limit_px: f64,
        reduce_only: bool,
    ) -> Result<OrderOutcome> {
        let nonce = self.next_nonce();
        let sig =
            self.signer
                .sign_l1_order(coin, is_buy, reduce_only, limit_px, sz, nonce, "Gtc")?;

        let px_wire = sig.px_str.as_deref().unwrap_or("0");
        let sz_wire = sig.sz_str.as_deref().unwrap_or("0");
        let asset_idx = sig.asset_idx;

        let order_wire = json!({
            "a": asset_idx,
            "b": is_buy,
            "p": px_wire,
            "s": sz_wire,
            "r": reduce_only,
            "t": {"limit": {"tif": "Gtc"}}
        });

        let action = json!({
            "type": "order",
            "orders": [order_wire],
            "grouping": "na"
        });

        let body = build_exchange_body(&action, nonce, &sig);

        let resp = self
            .client
            .post(format!("{}/exchange", self.cfg.api_url))
            .json(&body)
            .send()
            .await?;
        let data: serde_json::Value = resp.json().await?;

        let statuses = data["response"]["data"]["statuses"].as_array();
        tracing::warn!(coin=%coin, is_buy, px=%limit_px, sz, "GTC RAW: {}", serde_json::to_string(&data).unwrap_or_default());
        tracing::warn!(coin=%coin, is_buy, px=%limit_px, sz, "GTC statuses: {:?}", statuses);
        let mut rejection_reason: Option<String> = None;
        if let Some(statuses) = statuses {
            for status in statuses {
                if let Some(resting) = &status.get("resting") {
                    let oid = resting["oid"].as_u64().unwrap_or(0);
                    if oid > 0 {
                        return Ok(OrderOutcome::Rested(oid));
                    }
                }
                if let Some(_filled) = &status.get("filled") {
                    return Ok(OrderOutcome::FilledTaker);
                }
                if let Some(error) = status.get("error").and_then(|v| v.as_str()) {
                    tracing::warn!(coin = %coin, is_buy = is_buy, error = %error, "GTC order rejected");
                    rejection_reason = Some(error.to_string());
                }
            }
        }

        // V12.4.1: HL API may return empty status {} — silent rejection
        if rejection_reason.is_none() {
            tracing::warn!(coin = %coin, is_buy = is_buy, statuses = ?statuses,
                "order silently rejected (empty status)");
        }
        Ok(OrderOutcome::Rejected(rejection_reason))
    }

    /// Cancel all orders for a coin using cancelByCloid.
    pub async fn cancel_all_for_coin(&self, coin: &str) -> Result<()> {
        let nonce = self.next_nonce();
        let sig = self.signer.sign_l1_cancel_by_cloid(coin, nonce)?;

        let action = json!({
            "type": "cancelByCloid",
            "asset": sig.asset_idx
        });

        let body = build_exchange_body(&action, nonce, &sig);

        let resp = self
            .client
            .post(format!("{}/exchange", self.cfg.api_url))
            .json(&body)
            .send()
            .await?;
        let _data: serde_json::Value = resp.json().await?;

        Ok(())
    }

    /// Place a limit order with adaptive tick retreat on Post-Only rejection.
    ///
    /// When an Alo order is rejected with "would immediately match", instead
    /// of abandoning the order, retreat the price by 1 tick away from the
    /// opposing book side and retry. This keeps the SELL (or BUY) alive at
    /// the book's edge as a pure Maker while the market breathes.
    ///
    /// Max retreat attempts: `unwind_max_retreat_ticks` (default 3).
    /// Returns the OID on success, or None after exhausting retreats.
    #[allow(clippy::too_many_arguments)]
    pub async fn place_limit_order_with_tick_retreat(
        &self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        mut limit_px: f64,
        reduce_only: bool,
        max_retreat: u32,
        tick_size: f64,
    ) -> Result<OrderOutcome> {
        for retreat in 0..=max_retreat {
            let nonce = self.next_nonce();
            let sig =
                self.signer
                    .sign_l1_order(coin, is_buy, reduce_only, limit_px, sz, nonce, "Alo")?;

            let px_wire = sig.px_str.as_deref().unwrap_or("0");
            let sz_wire = sig.sz_str.as_deref().unwrap_or("0");
            let asset_idx = sig.asset_idx;

            let order_wire = json!({
                "a": asset_idx,
                "b": is_buy,
                "p": px_wire,
                "s": sz_wire,
                "r": reduce_only,
                "t": {"limit": {"tif": "Alo"}}
            });

            let action = json!({
                "type": "order",
                "orders": [order_wire],
                "grouping": "na"
            });

            let body = build_exchange_body(&action, nonce, &sig);

            let resp = self
                .client
                .post(format!("{}/exchange", self.cfg.api_url))
                .json(&body)
                .send()
                .await?;
            let data: serde_json::Value = resp.json().await?;

            let statuses = data["response"]["data"]["statuses"].as_array();
            let mut would_match = false;
            let mut rested = None;

            if let Some(statuses) = statuses {
                for status in statuses {
                    if let Some(error) = status.get("error").and_then(|v| v.as_str()) {
                        if error.contains("would have immediately matched") {
                            would_match = true;
                            tracing::debug!(
                                coin = %coin,
                                side = if is_buy { "BUY" } else { "SELL" },
                                px = limit_px,
                                retreat = retreat,
                                "post-only would match, retreating"
                            );
                        } else {
                            tracing::warn!(coin = %coin, is_buy = is_buy, error = %error, "order rejected");
                        }
                    }
                    if let Some(resting) = status.get("resting") {
                        let oid = resting["oid"].as_u64().unwrap_or(0);
                        if oid > 0 {
                            rested = Some(oid);
                        }
                    }
                }
            }

            if let Some(oid) = rested {
                if retreat > 0 {
                    tracing::info!(
                        coin = %coin,
                        side = if is_buy { "BUY" } else { "SELL" },
                        px = limit_px,
                        retreat = retreat,
                        "post-only rested after retreat"
                    );
                }
                return Ok(OrderOutcome::Rested(oid));
            }

            if would_match && retreat < max_retreat {
                // Retreat: move price AWAY from the opposing book
                limit_px = if is_buy {
                    limit_px - tick_size // BUY: retreat lower (away from ask)
                } else {
                    limit_px + tick_size // SELL: retreat higher (away from bid)
                };
            } else {
                // No more retreats or non-"would match" error
                break;
            }
        }

        Ok(OrderOutcome::Rejected(None))
    }

    /// Cancel a specific order by OID.
    pub async fn cancel_order(&self, coin: &str, oid: Oid) -> Result<()> {
        let nonce = self.next_nonce();
        let sig = self.signer.sign_l1_cancel(coin, oid, nonce)?;

        let action = json!({
            "type": "cancel",
            "cancels": [{"a": sig.asset_idx, "o": oid}]
        });

        let body = build_exchange_body(&action, nonce, &sig);

        let resp = self
            .client
            .post(format!("{}/exchange", self.cfg.api_url))
            .json(&body)
            .send()
            .await?;
        let _data: serde_json::Value = resp.json().await?;

        Ok(())
    }
}

// ──── MarketAdapter trait impl ────

#[async_trait]
impl MarketAdapter for Executor {
    fn market_type(&self) -> MarketType {
        MarketType::CryptoPerp
    }

    fn capabilities(&self) -> MarketCapabilities {
        // B2 fix: Growth Mode maker fee = 0.17 bps (matches hyna deployer rates,
        // NOT 0.35 bps which was a stale estimate). Config::roundtrip_bps() also
        // uses 0.17/0.50 — this is now consistent.
        let (maker_bps, taker_bps) = if self.cfg.growth_mode {
            (0.17, 0.50) // Growth Mode (HIP-3): hyna deployer
        } else {
            (1.5, 4.5) // Standard Tier 0 rates
        };
        MarketCapabilities {
            supports_post_only: true,
            supports_margin: true,
            supports_options: false,
            settlement: SettlementMode::Exchange,
            session_24_7: true,
            maker_fee_bps: maker_bps,
            taker_fee_bps: taker_bps,
        }
    }

    async fn fetch_state(&self) -> Result<AccountState> {
        self.fetch_account_state().await
    }

    async fn fetch_l2(&self, coin: &str) -> Result<L2Book> {
        self.fetch_l2_snapshot(coin).await
    }

    async fn place_limit_order(
        &mut self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        limit_px: f64,
        reduce_only: bool,
    ) -> Result<OrderOutcome> {
        Executor::place_limit_order(self, coin, is_buy, sz, limit_px, reduce_only).await
    }

    async fn place_ioc_order(
        &mut self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        aggressive_px: f64,
    ) -> Result<Option<f64>> {
        Executor::place_ioc_order(self, coin, is_buy, sz, aggressive_px).await
    }

    async fn place_with_tick_retreat(
        &mut self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        limit_px: f64,
        reduce_only: bool,
        max_retreat: u32,
        tick_size: f64,
    ) -> Result<OrderOutcome> {
        Executor::place_limit_order_with_tick_retreat(
            self,
            coin,
            is_buy,
            sz,
            limit_px,
            reduce_only,
            max_retreat,
            tick_size,
        )
        .await
    }

    async fn cancel_order(&mut self, coin: &str, oid: u64) -> Result<()> {
        Executor::cancel_order(self, coin, oid).await
    }

    async fn cancel_all_for_coin(&mut self, coin: &str) -> Result<()> {
        Executor::cancel_all_for_coin(self, coin).await
    }

    async fn place_gtc_order(
        &mut self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        limit_px: f64,
        reduce_only: bool,
    ) -> Result<OrderOutcome> {
        Executor::place_gtc_order(self, coin, is_buy, sz, limit_px, reduce_only).await
    }

    fn tick_for(&self, coin: &str) -> f64 {
        self.cfg.tick_for(coin)
    }

    fn min_order_size(&self) -> f64 {
        self.cfg.min_order_size
    }
}

// ──── MarketAdapter: GTC method (B1 fix) ────
// place_gtc_order is declared on MarketAdapter trait (traits.rs).
// It is used exclusively for the GTC last-resort mechanism.

/// Validate exchange response and extract statuses array.
///
/// Hyperliquid returns two response formats:
///
/// **Success:**
/// ```json
/// {"status": "ok", "response": {"type": "order", "data": {"statuses": [...]}}}
/// ```
///
/// **Error:**
/// ```json
/// {"status": "err", "response": "tx failed: error description"}
/// ```
///
/// If the error format is returned, response is a **string** (not an object),
/// so `data["response"]["data"]` silently yields None and the error is swallowed.
/// This function detects both paths and returns the statuses array (or None).
#[allow(dead_code)] // reference implementation retained for documentation
fn extract_statuses(data: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    // Check for top-level error format first
    if data.get("status").and_then(|v| v.as_str()) == Some("err") {
        let err_msg = data
            .get("response")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown exchange error");
        tracing::error!(error = %err_msg, "exchange rejected action");
        return None;
    }

    // Normal success path: nested response.data.statuses
    let response = data.get("response")?;
    if response.is_string() {
        tracing::warn!(
            response = %response.as_str().unwrap_or("?"),
            "exchange returned string response (unexpected after ok status)"
        );
        return None;
    }
    response.get("data")?.get("statuses")?.as_array()
}

/// Build the exchange POST body in SDK-compatible nested format.
///
/// Format: `{"action": <action_dict>, "nonce": <timestamp>,
///            "signature": {"r": "...", "s": "...", "v": 27}}`
///
/// This matches Hyperliquid Python SDK's `_post_action()` exactly.
fn build_exchange_body(
    action: &serde_json::Value,
    nonce: u64,
    sig: &SignedAction,
) -> serde_json::Value {
    json!({
        "action": action,
        "nonce": nonce,
        "signature": {
            "r": sig.r,
            "s": sig.s,
            "v": sig.v
        }
    })
}
