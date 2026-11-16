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
use parking_lot::Mutex;
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
    /// V12.5: count of POST /exchange requests issued by this process.
    requests_sent: AtomicU64,
    /// V12.5: count of responses that hit the venue's request-quota gate.
    quota_rejects: AtomicU64,
    /// V12.5: last (sent, allowed) pair parsed out of the quota rejection text.
    quota_hint: Mutex<Option<(u64, u64)>>,
    /// V12.6: request-budget ledger — turns the rejection text into a live
    /// estimate so the engine can stop *before* spending a doomed request.
    quota: Mutex<crate::quota::QuotaLedger>,
    /// V12.6: whether the pre-check may refuse to send (1) or not (0).
    /// Off by default so the bench observes the venue's real behaviour.
    quota_gate_on: AtomicU64,
    /// V12.6: requests kept in hand as a safety margin.
    quota_reserve: AtomicU64,
    /// V12.7: duration (microseconds) of the last fetch that actually went out.
    ///
    /// Callers that retry with a backoff can read this to separate the network
    /// round-trip from the time they themselves spent sleeping.
    last_attempt_us: AtomicU64,
}

impl Executor {
    /// Build a new Executor with config, signer, and circuit breaker.
    pub fn new(cfg: Config, signer: Signer) -> Self {
        Self {
            client: Client::new(),
            cfg,
            signer,
            last_nonce: AtomicU64::new(0),
            requests_sent: AtomicU64::new(0),
            quota_rejects: AtomicU64::new(0),
            quota_hint: Mutex::new(None),
            quota: Mutex::new(crate::quota::QuotaLedger::default()),
            quota_gate_on: AtomicU64::new(0),
            quota_reserve: AtomicU64::new(3),
            last_attempt_us: AtomicU64::new(0),
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
                prev, next,
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

        let positions_raw = data["assetPositions"].as_array().cloned().unwrap_or_default();
        let positions: Vec<Position> = positions_raw
            .iter()
            .filter_map(|p| {
                let pos = p.get("position")?;
                let coin = pos.get("coin")?.as_str()?.to_string();
                let szi = pos.get("szi")?.as_str()?.parse::<f64>().ok()?;
                let entry_px = pos.get("entryPx")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let upnl = pos.get("unrealizedPnl")
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


    /// Single funnel for `POST /exchange`: meters the request, decodes the body,
    /// and records a quota rejection so callers never have to remember to.
    async fn post_exchange(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        self.note_request();
        if std::env::var("GE_DUMP_BODY").is_ok() {
            eprintln!("BODY {}", serde_json::to_string(body).unwrap_or_default());
        }
        let t_attempt = std::time::Instant::now();
        let resp = self
            .client
            .post(format!("{}/exchange", self.cfg.api_url))
            .json(body)
            .send()
            .await?;
        let data: serde_json::Value = resp.json().await?;
        self.last_attempt_us
            .store(t_attempt.elapsed().as_micros() as u64, Ordering::Relaxed);
        self.note_if_quota(&data);
        Ok(data)
    }

    /// POST /exchange with the exact action bytes the signer covered.
    ///
    /// The venue hashes the action's msgpack, so key order is part of the
    /// signed payload. Rust's serde_json sorts keys, therefore any action the
    /// signer built must be re-posted verbatim instead of being re-serialised.
    async fn post_exchange_raw(
        &self,
        action_json: &str,
        nonce: u64,
        r: &str,
        s: &str,
        v: u32,
    ) -> Result<serde_json::Value> {
        self.note_request();
        let body = format!(
            "{{\"action\":{},\"nonce\":{},\"signature\":{{\"r\":\"{}\",\"s\":\"{}\",\"v\":{}}}}}",
            action_json, nonce, r, s, v
        );
        if std::env::var("GE_DUMP_BODY").is_ok() {
            eprintln!("BODY {}", body);
        }
        let t_attempt = std::time::Instant::now();
        let resp = self
            .client
            .post(format!("{}/exchange", self.cfg.api_url))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await?;
        let data: serde_json::Value = resp.json().await?;
        // Time the round-trip only: the caller's backoff sleep is not ours to bill.
        self.last_attempt_us
            .store(t_attempt.elapsed().as_micros() as u64, Ordering::Relaxed);
        self.note_if_quota(&data);
        Ok(data)
    }

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
        if let Some(rej) = self.quota_precheck(1) {
            return Ok(rej);
        }
        let nonce = self.next_nonce();
        let sig = self
            .signer
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

        let action_json = sig
            .action_json
            .clone()
            .unwrap_or_else(|| serde_json::to_string(&action).unwrap_or_default());
        let data = self
            .post_exchange_raw(&action_json, nonce, &sig.r, &sig.s, sig.v)
            .await?;


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
                if let Some(filled) = &status.get("filled") {
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
        if self.quota_precheck(1).is_some() {
            return Ok(None);
        }
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

        let action_json = sig
            .action_json
            .clone()
            .unwrap_or_else(|| serde_json::to_string(&action).unwrap_or_default());
        let data = self
            .post_exchange_raw(&action_json, nonce, &sig.r, &sig.s, sig.v)
            .await?;


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
        if let Some(rej) = self.quota_precheck(1) {
            return Ok(rej);
        }
        let nonce = self.next_nonce();
        let sig = self
            .signer
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

        let action_json = sig
            .action_json
            .clone()
            .unwrap_or_else(|| serde_json::to_string(&action).unwrap_or_default());
        let data = self
            .post_exchange_raw(&action_json, nonce, &sig.r, &sig.s, sig.v)
            .await?;


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
                if let Some(filled) = &status.get("filled") {
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

    /// Cancel every open order for a coin in ONE request per chunk.
    ///
    /// The previous implementation posted `cancelByCloid` without a cloid list,
    /// which the venue rejects as a malformed action — so the "sweep" silently
    /// cancelled nothing while still burning a request. This version reads the
    /// live order set first, then sends one multi-cancel action per chunk.
    pub async fn cancel_all_for_coin(&self, coin: &str) -> Result<()> {
        let open = self.fetch_open_orders().await?;
        let mine: Vec<(String, u64)> = open
            .iter()
            .filter(|o| o.get("coin").and_then(|c| c.as_str()) == Some(coin))
            .filter_map(|o| {
                o.get("oid")
                    .and_then(|v| v.as_u64())
                    .map(|oid| (coin.to_string(), oid))
            })
            .collect();
        if mine.is_empty() {
            return Ok(());
        }
        let mut done = 0usize;
        for chunk in mine.chunks(MAX_CANCELS_PER_REQUEST) {
            done += self.cancel_batch(chunk).await.unwrap_or(0);
        }
        tracing::info!(
            coin = %coin,
            open = mine.len(),
            cancelled = done,
            requests_sent = self.requests_sent(),
            "cancel_all_for_coin: batched sweep"
        );
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
            if let Some(rej) = self.quota_precheck(1) {
                return Ok(rej);
            }
            let nonce = self.next_nonce();
            let sig = self
                .signer
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

            let action_json = sig
                .action_json
                .clone()
                .unwrap_or_else(|| serde_json::to_string(&action).unwrap_or_default());
            let data = self
                .post_exchange_raw(&action_json, nonce, &sig.r, &sig.s, sig.v)
                .await?;

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

        let action_json = sig
            .action_json
            .clone()
            .unwrap_or_else(|| serde_json::to_string(&action).unwrap_or_default());
        let data = self
            .post_exchange_raw(&action_json, nonce, &sig.r, &sig.s, sig.v)
            .await?;

        Ok(())
    }

    // ──── V12.5: request-quota metering ────

    /// Number of POST /exchange requests this process has issued.
    pub fn requests_sent(&self) -> u64 {
        self.requests_sent.load(Ordering::Relaxed)
    }

    /// Number of responses that hit the venue's request-quota gate.
    pub fn quota_rejects(&self) -> u64 {
        self.quota_rejects.load(Ordering::Relaxed)
    }

    /// Network round-trip of the most recent `POST /exchange`, in milliseconds.
    ///
    /// Deliberately excludes anything the caller did around the call (notably
    /// the 10 s quota backoff), so latency figures can be reported net of it.
    /// Returns `f64::NAN` when no request has gone out yet.
    pub fn last_attempt_ms(&self) -> f64 {
        let us = self.last_attempt_us.load(Ordering::Relaxed);
        if us == 0 {
            f64::NAN
        } else {
            us as f64 / 1000.0
        }
    }

    /// Last `(sent, allowed)` pair the venue reported, if any.
    pub fn quota_hint(&self) -> Option<(u64, u64)> {
        *self.quota_hint.lock()
    }

    /// Remaining request head-room implied by the last rejection, if known.
    ///
    /// Negative means the account is already over budget and every extra
    /// request is a lottery: the venue lets some through as volume accrues.
    pub fn quota_headroom(&self) -> Option<i64> {
        self.quota_hint()
            .map(|(sent, allowed)| allowed as i64 - sent as i64)
    }

    pub fn quota_state(&self) -> QuotaState {
        QuotaState {
            requests_sent: self.requests_sent(),
            quota_rejects: self.quota_rejects(),
            last_hint: self.quota_hint(),
            headroom: self.quota_headroom(),
        }
    }

    fn note_request(&self) {
        self.requests_sent.fetch_add(1, Ordering::Relaxed);
        self.quota.lock().on_request();
    }

    // ──── V12.6: request-budget ledger ────

    /// Allow (or forbid) the pre-check to refuse sends. Bench keeps it off so
    /// it can measure the venue's true behaviour; the engine turns it on.
    pub fn set_quota_gate(&self, on: bool) {
        self.quota_gate_on
            .store(if on { 1 } else { 0 }, Ordering::Relaxed);
    }

    /// Requests to keep in hand as a safety margin.
    pub fn set_quota_reserve(&self, reserve: u64) {
        self.quota_reserve.store(reserve, Ordering::Relaxed);
    }

    /// Seed the ledger from a known observation, so the estimate is usable
    /// before the first rejection of this process.
    pub fn seed_quota(&self, sent: u64, allowed: u64, volume: Option<f64>) {
        self.quota.lock().seed(sent, allowed, volume);
        *self.quota_hint.lock() = Some((sent, allowed));
    }

    /// Live budget estimate, with a one-line rendering for logs.
    pub fn quota_gauge(&self) -> crate::quota::QuotaGauge {
        let reserve = self.quota_reserve.load(Ordering::Relaxed);
        self.quota.lock().gauge(reserve)
    }

    /// Pre-flight check for a place that needs `need` requests.
    ///
    /// Returns `Some(rejection)` when the budget says "do not spend this
    /// request": the caller must surface that to the engine instead of hitting
    /// the venue. Cancels never go through here — leaving an order behind is
    /// worse than spending one more request.
    fn quota_precheck(&self, need: u64) -> Option<OrderOutcome> {
        if self.quota_gate_on.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let reserve = self.quota_reserve.load(Ordering::Relaxed);
        let gate = self.quota.lock().gate(need, reserve);
        match gate {
            crate::quota::Gate::Allow => None,
            crate::quota::Gate::Wait { after } => {
                self.quota.lock().note_deferred();
                tracing::warn!(
                    gauge = %self.quota_gauge().line(),
                    wait_s = after.as_secs(),
                    "quota precheck: request withheld"
                );
                Some(OrderOutcome::Rejected(Some(
                    "quota precheck: budget exhausted, request withheld".into(),
                )))
            }
            crate::quota::Gate::MustTrade { need } => {
                self.quota.lock().note_deferred();
                let usdc = self.quota.lock().usdc_to_unlock(need);
                tracing::warn!(
                    gauge = %self.quota_gauge().line(),
                    need_requests = need,
                    usdc_traded_needed = ?usdc,
                    "quota precheck: only traded volume can unlock more requests"
                );
                Some(OrderOutcome::Rejected(Some(format!(
                    "quota precheck: need ~${} traded volume to unlock {} requests",
                    usdc.map(|u| format!("{:.0}", u)).unwrap_or_else(|| "?".into()),
                    need
                ))))
            }
        }
    }

    /// Record a quota rejection and remember the venue's own numbers.
    fn note_quota_error(&self, text: &str) {
        let parsed = self.quota.lock().on_reject(text);
        if !parsed {
            tracing::warn!(text = %text, "quota rejection text not parseable, estimate left stale");
        }
        if let Some(pair) = parse_quota_pair(text) {
            *self.quota_hint.lock() = Some(pair);
        }
        self.quota_rejects.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            gauge = %self.quota_gauge().line(),
            "request quota gate hit"
        );
    }

    /// Inspect a decoded response and count it if it was a quota rejection.
    fn note_if_quota(&self, data: &serde_json::Value) {
        if data.get("status").and_then(|v| v.as_str()) != Some("err") {
            return;
        }
        if let Some(msg) = data.get("response").and_then(|v| v.as_str()) {
            if msg.contains("Too many cumulative requests") {
                self.note_quota_error(msg);
            }
        }
    }

    // ──── V12.5: batch requests ────

    /// Place N limit orders in ONE signed action → ONE POST /exchange.
    ///
    /// Each order is an independent `(coin, is_buy, sz, limit_px, reduce_only)`
    /// tuple with `tif=Alo` (post-only). Returns one outcome per input, in input
    /// order, so callers can map OIDs back without guessing.
    pub async fn place_batch(
        &self,
        orders: &[(String, bool, f64, f64, bool)],
    ) -> Result<Vec<OrderOutcome>> {
        if orders.is_empty() {
            return Ok(Vec::new());
        }
        // One batch = one request; withhold it whole rather than half-sending.
        if let Some(rej) = self.quota_precheck(1) {
            return Ok(vec![rej; orders.len()]);
        }

        let payload: Vec<serde_json::Value> = orders
            .iter()
            .map(|(coin, is_buy, sz, px, ro)| {
                json!({
                    "coin": coin,
                    "is_buy": *is_buy,
                    "px": *px,
                    "sz": *sz,
                    "reduce_only": *ro,
                    "tif": "Alo",
                })
            })
            .collect();

        let nonce = self.next_nonce();
        let sig = self
            .signer
            .sign_l1_batch_order(&serde_json::to_string(&payload)?, nonce)?;

        let wires: Vec<serde_json::Value> = sig
            .per_order
            .iter()
            .zip(orders.iter())
            .map(|(w, (_coin, _is_buy, _sz, _px, ro))| {
                json!({
                    "a": w.asset_idx,
                    "b": w.is_buy,
                    "p": w.px_str,
                    "s": w.sz_str,
                    "r": *ro,
                    "t": {"limit": {"tif": "Alo"}},
                })
            })
            .collect();

        let action = json!({"type": "order", "orders": wires, "grouping": "na"});
        // Summit the exact JSON the signer covered (canonical key order).
        let action_json = sig
            .action_json
            .clone()
            .unwrap_or_else(|| serde_json::to_string(&action).unwrap_or_default());

        let data = self
            .post_exchange_raw(&action_json, nonce, &sig.r, &sig.s, sig.v)
            .await?;

        let count = orders.len();
        let mut outcomes: Vec<OrderOutcome> = Vec::with_capacity(count);
        match extract_statuses(&data) {
            Some(statuses) => {
                for status in statuses.iter().take(count) {
                    if let Some(resting) = status.get("resting") {
                        let oid = resting.get("oid").and_then(|v| v.as_u64()).unwrap_or(0);
                        outcomes.push(OrderOutcome::Rested(oid));
                    } else if status.get("filled").is_some() {
                        outcomes.push(OrderOutcome::FilledTaker);
                    } else if let Some(err) = status.get("error").and_then(|v| v.as_str()) {
                        tracing::warn!(error = %err, "batch order rejected (element)");
                        outcomes.push(OrderOutcome::Rejected(Some(err.to_string())));
                    } else {
                        outcomes.push(OrderOutcome::Rejected(None));
                    }
                }
            }
            None => {
                let reason = data
                    .get("response")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                for _ in 0..count {
                    outcomes.push(OrderOutcome::Rejected(reason.clone()));
                }
            }
        }
        Ok(outcomes)
    }

    /// Cancel N orders in ONE signed action → ONE POST /exchange.
    ///
    /// Returns how many cancels the venue acknowledged as `ok`.
    pub async fn cancel_batch(&self, cancels: &[(String, u64)]) -> Result<usize> {
        if cancels.is_empty() {
            return Ok(0);
        }

        let wire: Vec<serde_json::Value> = cancels
            .iter()
            .map(|(coin, oid)| json!({"a": self.signer.asset_index(coin), "o": *oid}))
            .collect();
        let action = json!({"type": "cancel", "cancels": wire});

        let nonce = self.next_nonce();
        let sig = self
            .signer
            .sign_l1_action(&serde_json::to_string(&action)?, nonce)?;
        let action_json = sig
            .action_json
            .clone()
            .unwrap_or_else(|| serde_json::to_string(&action).unwrap_or_default());
        let data = self
            .post_exchange_raw(&action_json, nonce, &sig.r, &sig.s, sig.v)
            .await?;

        let ok = match extract_statuses(&data) {
            Some(statuses) => statuses.iter().filter(|s| is_ok_status(s)).count(),
            None => 0,
        };
        Ok(ok)
    }

    /// Live open orders (info endpoint; not an exchange action).
    pub async fn fetch_open_orders(&self) -> Result<Vec<serde_json::Value>> {
        let body = json!({"type": "openOrders", "user": self.cfg.address});
        let data: serde_json::Value = self
            .client
            .post(format!("{}/info", self.cfg.api_url))
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        Ok(data.as_array().cloned().unwrap_or_default())
    }

    /// Count open orders for one coin.
    pub async fn open_orders_count(&self, coin: &str) -> Result<usize> {
        let open = self.fetch_open_orders().await?;
        Ok(open
            .iter()
            .filter(|o| o.get("coin").and_then(|c| c.as_str()) == Some(coin))
            .count())
    }

    /// Every mid price from the info endpoint (one call, all coins).
    pub async fn fetch_all_mids(&self) -> Result<serde_json::Value> {
        let data: serde_json::Value = self
            .client
            .post(format!("{}/info", self.cfg.api_url))
            .json(&json!({"type": "allMids"}))
            .send()
            .await?
            .json()
            .await?;
        Ok(data)
    }
}

/// V12.5: a snapshot of the account's REST request budget.
#[derive(Debug, Clone)]
pub struct QuotaState {
    pub requests_sent: u64,
    pub quota_rejects: u64,
    /// (cumulative requests the venue counted, cumulative allowed)
    pub last_hint: Option<(u64, u64)>,
    /// allowed − sent, from the last rejection
    pub headroom: Option<i64>,
}

/// Max cancels packed into one `cancel` action (venue accepts more, but a
/// single oversized action is all-or-nothing, so chunk conservatively).
const MAX_CANCELS_PER_REQUEST: usize = 20;

/// Parse `(25200 > 21140)` out of the venue's quota rejection sentence.
fn parse_quota_pair(text: &str) -> Option<(u64, u64)> {
    let anchor = "requests sent (";
    let start = text.find(anchor)?.checked_add(anchor.len())?;
    let rest = text.get(start..)?;
    let end = rest.find(')')?;
    let pair = rest.get(..end)?;
    let mut parts = pair.split('>');
    let sent = parts.next()?.trim().parse::<u64>().ok()?;
    let allowed = parts.next()?.trim().parse::<u64>().ok()?;
    Some((sent, allowed))
}

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

    /// V12.7: real batched placement — one POST /exchange for N orders.
    async fn place_batch(
        &mut self,
        orders: &[(String, bool, f64, f64, bool)],
    ) -> Result<Vec<OrderOutcome>> {
        Executor::place_batch(self, orders).await
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
            self, coin, is_buy, sz, limit_px, reduce_only, max_retreat, tick_size,
        ).await
    }

    async fn cancel_order(&mut self, coin: &str, oid: u64) -> Result<()> {
        Executor::cancel_order(self, coin, oid).await
    }

    async fn cancel_all_for_coin(&mut self, coin: &str) -> Result<()> {
        Executor::cancel_all_for_coin(self, coin).await
    }

    /// V12.8: expose the ledger to the engine's periodic gauge log.
    fn quota_gauge_line(&self) -> Option<String> {
        Some(self.quota_gauge().line())
    }

    fn quota_headroom_est(&self) -> Option<i64> {
        self.quota_gauge().headroom_est
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
/// A cancel status is reported as the bare string `"success"` (or an object
/// with `status: "ok"` for newer shapes). Accept both.
fn is_ok_status(s: &serde_json::Value) -> bool {
    match s {
        serde_json::Value::String(t) => t == "success" || t == "ok",
        _ => s
            .get("status")
            .and_then(|v| v.as_str())
            .map(|v| v == "ok")
            .unwrap_or(false),
    }
}

/// This function detects both paths and returns the statuses array (or None).
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

