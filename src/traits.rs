//! Market Adapter Traits — abstract exchange/market interface.
//!
//! Every market (Crypto Perps, Prediction, Equity, Forex, Options, Futures)
//! implements this trait so the engine, risk gates, and signal pipeline
//! never touch exchange-specific types or API details.
//!
//! # Design
//!
//! ```text
//! Engine → [dyn MarketAdapter] → CryptoAdapter / PolyAdapter / EquityAdapter …
//!                │
//!                └─ UnifiedOrder, L2Book, AccountState are the ONLY shared types
//! ```
//!
//! Adding a new market = implementing this trait + registering in main.

use crate::types::{AccountState, L2Book, OrderOutcome};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Asset class tag — drives feature plugins, risk gates, and session calendars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MarketType {
    /// Perpetual futures on centralised exchanges (Hyperliquid, Binance, OKX)
    CryptoPerp,
    /// On-chain prediction / event markets (Polymarket CLOB)
    Prediction,
    /// US equities via Alpaca / IB (NYSE, NASDAQ)
    Equity,
    /// Spot forex via MT4/5 (EUR/USD, GBP/JPY …)
    Forex,
    /// Equity / index options (IB API)
    Options,
    /// Date-delivery futures (CME, Binance Delivery)
    Futures,
}

impl std::fmt::Display for MarketType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CryptoPerp => write!(f, "CryptoPerp"),
            Self::Prediction => write!(f, "Prediction"),
            Self::Equity => write!(f, "Equity"),
            Self::Forex => write!(f, "Forex"),
            Self::Options => write!(f, "Options"),
            Self::Futures => write!(f, "Futures"),
        }
    }
}

/// Capability flags — tells upper layers what this adapter supports.
///
/// Simpler than Gemini's proposed Mixin pattern; a plain struct is sufficient
/// until we need per-method capability checks. The engine gates on these before
/// attempting features like Post-Only or PDT.
#[derive(Debug, Clone)]
pub struct MarketCapabilities {
    /// Does the exchange support Post-Only (Alo / Maker-only)?
    pub supports_post_only: bool,
    /// Does the exchange allow margin / leverage?
    pub supports_margin: bool,
    /// Can we trade options (multi-leg combos, Greeks required)?
    pub supports_options: bool,
    /// Settlement model — drives P&L accounting tempo
    pub settlement: SettlementMode,
    /// Is the market open 24/7? (Poly/Crypto = true, NYSE = false)
    pub session_24_7: bool,
    /// Maker fee in bps (positive = pay, negative = rebate)
    pub maker_fee_bps: f64,
    /// Taker fee in bps
    pub taker_fee_bps: f64,
}

/// How positions settle — used by the unified P&L aggregator (L7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementMode {
    /// Instant on-chain settlement (Polymarket, DEX perps)
    Instant,
    /// Exchange-internal margin settlement (CEX perps)
    Exchange,
    /// T+2 DTCC settlement (NYSE equities)
    T2,
    /// Immediate spot settlement + overnight swap (forex, spot crypto)
    Spot,
}

/// Unified market adapter — the only contract between the engine and any exchange.
///
/// ## Add-a-market checklist
/// 1. Implement this trait (~200-400 lines typically)
/// 2. Register in `main.rs`
/// 3. Add `MarketType` variant if new asset class
/// 4. Wire up `MarketCapabilities`
/// Zero changes needed in engine / risk / state / decision layers.
#[async_trait]
#[allow(clippy::empty_line_after_outer_attr, clippy::too_many_arguments)]
pub trait MarketAdapter: Send {
    /// ── Identity ──

    /// Asset class this adapter serves.
    fn market_type(&self) -> MarketType;

    /// Static capability flags (read once at startup).
    fn capabilities(&self) -> MarketCapabilities;

    /// ── State ──

    /// Fetch account state (withdrawable, equity, positions).
    async fn fetch_state(&self) -> Result<AccountState>;

    /// Fetch L2 order book snapshot (REST fallback when WS is stale).
    async fn fetch_l2(&self, coin: &str) -> Result<L2Book>;

    /// ── Orders ──

    /// Place a single limit order.
    ///
    /// Returns `Ok(OrderOutcome::Rested(oid))` on resting fill,
    /// `Ok(OrderOutcome::FilledTaker)` on instant cross,
    /// `Ok(OrderOutcome::Rejected(Some(reason)))` on rejection.
    async fn place_limit_order(
        &mut self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        limit_px: f64,
        reduce_only: bool,
    ) -> Result<OrderOutcome>;

    /// Place an IOC (immediate-or-cancel) order for position shedding.
    ///
    /// Crosses the spread to reduce inventory. Returns filled size or None.
    async fn place_ioc_order(
        &mut self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        aggressive_px: f64,
    ) -> Result<Option<f64>>;

    /// Place a limit order with adaptive tick retreat on Post-Only rejection.
    ///
    /// When an Alo order is rejected with "would immediately match", retreat
    /// the price by 1 tick away from the opposing book side and retry.
    async fn place_with_tick_retreat(
        &mut self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        limit_px: f64,
        reduce_only: bool,
        max_retreat: u32,
        tick_size: f64,
    ) -> Result<OrderOutcome>;

    /// B1: Place a GTC (Good-Til-Cancelled) limit order.
    ///
    /// Used exclusively for the GTC last-resort mechanism: when 3 consecutive
    /// IOC shedding cycles produce zero fills, a GTC order at 5% price discount
    /// breaks the deadlock. Accepts taker fees as a cost of last resort.
    ///
    /// Unlike Post-Only orders, GTC can cross the book and execute as taker.
    async fn place_gtc_order(
        &mut self,
        coin: &str,
        is_buy: bool,
        sz: f64,
        limit_px: f64,
        reduce_only: bool,
    ) -> Result<OrderOutcome>;

    /// Cancel a specific order by OID.
    async fn cancel_order(&mut self, coin: &str, oid: u64) -> Result<()>;

    /// Cancel all open orders for a coin.
    async fn cancel_all_for_coin(&mut self, coin: &str) -> Result<()>;

    /// ── Market parameters ──

    /// Per-coin tick size (minimum price increment).
    fn tick_for(&self, coin: &str) -> f64;

    /// Minimum order size in base units (shares / contracts / native).
    fn min_order_size(&self) -> f64;

    /// Round-trip cost in bps = maker_fee + taker_fee.
    ///
    /// Default from capabilities, but per-coin overrides possible (e.g. Growth Mode).
    fn roundtrip_bps(&self) -> f64 {
        self.capabilities().maker_fee_bps + self.capabilities().taker_fee_bps
    }
}
