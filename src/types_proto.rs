//! Canonical cross-market types — generated from V8 proto schemas.
//!
//! ## Proto sources (N150 /home/jerry/V8/schemas/)
//!
//! | Proto file              | Rust type        | Role                       |
//! |-------------------------|------------------|----------------------------|
//! | unified_order.proto     | `UnifiedOrder`   | Order lifecycle (25 fields)|
//! | unified_order.proto     | `OrderState`     | 11-state FSM               |
//! | alpha_signal.proto      | `AlphaSignal`    | Signal + gating + ML hooks |
//! | market_snapshot.proto   | `MarketSnapshot` | Tick/OB/Bar aggregation    |
//!
//! ## Design contract
//!
//! These types are the **single source of truth** for all market adapters.
//! Exchange-specific types (e.g. HL `OrderRequest`) are derived from these
//! via `From`/`Into` impls in the adapter layer — never the other way around.
//!
//! Adding a new market:
//! 1. Map your exchange fields → `UnifiedOrder`
//! 2. Use `OrderState` transitions (validate! don't guess)
//! 3. If the exchange has a unique field, use the `meta` extension map

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════════════
// Market identity
// ═══════════════════════════════════════════════════════════════════════════════

/// Product type — mirrors proto `InstType` enum.
/// Maps 1-to-1 with `MarketType` in traits.rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InstType {
    Spot = 1,
    Swap = 2, // Perpetual
    Futures = 3,
    Option_ = 4,
    Margin = 5,
}

impl InstType {
    /// Parse instrument type from exchange wire format ("Limit", "StopMarket", etc.).
    #[allow(dead_code)] // retained for adapter implementations
    pub fn from_wire(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "SPOT" => Some(Self::Spot),
            "SWAP" | "PERP" | "PERPETUAL" => Some(Self::Swap),
            "FUTURES" | "FUTURE" => Some(Self::Futures),
            "OPTION" | "OPTIONS" => Some(Self::Option_),
            "MARGIN" => Some(Self::Margin),
            _ => None,
        }
    }
}

impl std::fmt::Display for InstType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spot => write!(f, "SPOT"),
            Self::Swap => write!(f, "SWAP"),
            Self::Futures => write!(f, "FUTURES"),
            Self::Option_ => write!(f, "OPTION"),
            Self::Margin => write!(f, "MARGIN"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Order direction & type
// ═══════════════════════════════════════════════════════════════════════════════

/// Order side — mirrors proto `OrderSide` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderSide {
    Buy = 1,
    Sell = 2,
}

impl OrderSide {
    /// Convert boolean buy/sell flag to Side enum.
    pub fn from_bool(is_buy: bool) -> Self {
        if is_buy {
            Self::Buy
        } else {
            Self::Sell
        }
    }

    /// Returns true if this is a buy side order.
    pub fn is_buy(&self) -> bool {
        matches!(self, Self::Buy)
    }
    /// Returns true if this is a sell side order.
    pub fn is_sell(&self) -> bool {
        matches!(self, Self::Sell)
    }
}

/// Order type — mirrors proto `OrderType` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderType {
    Market = 1,
    Limit = 2,
    PostOnly = 3,
    #[serde(rename = "FOK")]
    Fok = 4, // Fill-or-Kill
    #[serde(rename = "IOC")]
    Ioc = 5, // Immediate-or-Cancel
}

impl OrderType {
    /// Return the Time-In-Force as exchange wire format string (Gtc, Ioc, Alo).
    pub fn tif_str(&self) -> &'static str {
        match self {
            Self::Limit => "Gtc",
            Self::PostOnly => "Alo",
            Self::Ioc => "Ioc",
            Self::Market => "Ioc", // market → immediate
            Self::Fok => "Ioc",    // FOK not natively supported on HL, fallback IOC
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Order State Machine — 11 states with validated transitions
// ═══════════════════════════════════════════════════════════════════════════════

/// Order lifecycle state — mirrors proto `OrderState`.
///
/// Transition table (source: V8 `common/engine.py` `_ALLOWED`):
/// ```text
/// 0: UNSPECIFIED → NEW
/// 1: NEW         → POSTED, REJECTED
/// 2: POSTED      → PARTIAL_FILLED, FILLED, CANCELED, REJECTED, EXPIRED, PENDING_CANCEL
/// 3: PARTIAL_FILLED → PARTIAL_FILLED, FILLED, PARTIAL_CANCELED, CANCELED, PENDING_CANCEL
/// 4: FILLED      → (terminal)
/// 5: PARTIAL_CANCELED → (terminal)
/// 6: CANCELED    → (terminal)
/// 7: REJECTED    → (terminal)
/// 8: EXPIRED     → (terminal)
/// 9: UNKNOWN     → (terminal)
/// 10: PENDING_CANCEL → PARTIAL_CANCELED, CANCELED, PARTIAL_FILLED, FILLED
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum OrderState {
    Unspecified = 0,
    New = 1,
    Posted = 2,
    PartialFilled = 3,
    Filled = 4,
    PartialCanceled = 5,
    Canceled = 6,
    Rejected = 7,
    Expired = 8,
    Unknown = 9,
    PendingCancel = 10,
}

// ─── StateMachine trait ───

/// Unified state-machine trait shared by:
/// - `OrderState` (11 states — order lifecycle)
/// - `State` (6 states — engine lifecycle, from `state.rs`)
/// - `StrategyGeneState` (4 states — strategy evolution, from ORACLE-FORGE)
pub trait StateMachine: Sized {
    /// Is this state terminal (no further transitions allowed)?
    fn is_terminal(&self) -> bool;

    /// Can we legally move from `self` to `next`?
    fn can_transition_to(&self, next: &Self) -> bool;

    /// Validate and return `Ok(next)` or `Err(reason)`.
    fn transition_to(&self, next: Self) -> Result<Self, String> {
        if self.can_transition_to(&next) {
            Ok(next)
        } else {
            Err("invalid transition".to_string())
        }
    }

    fn state_name(&self) -> &'static str;
}

// ─── OrderState → StateMachine impl ───

impl StateMachine for OrderState {
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Filled
                | Self::PartialCanceled
                | Self::Canceled
                | Self::Rejected
                | Self::Expired
                | Self::Unknown
        )
    }

    fn can_transition_to(&self, next: &Self) -> bool {
        use OrderState::*;
        matches!(
            (self, next),
            (Unspecified, New)
                | (New, Posted | Rejected)
                | (
                    Posted,
                    PartialFilled | Filled | Canceled | Rejected | Expired | PendingCancel
                )
                | (
                    PartialFilled,
                    PartialFilled | Filled | PartialCanceled | Canceled | PendingCancel
                )
                | (
                    PendingCancel,
                    PartialCanceled | Canceled | PartialFilled | Filled
                )
        )
    }

    fn state_name(&self) -> &'static str {
        match self {
            Self::Unspecified => "UNSPECIFIED",
            Self::New => "NEW",
            Self::Posted => "POSTED",
            Self::PartialFilled => "PARTIAL_FILLED",
            Self::Filled => "FILLED",
            Self::PartialCanceled => "PARTIAL_CANCELED",
            Self::Canceled => "CANCELED",
            Self::Rejected => "REJECTED",
            Self::Expired => "EXPIRED",
            Self::Unknown => "UNKNOWN",
            Self::PendingCancel => "PENDING_CANCEL",
        }
    }
}

impl std::fmt::Display for OrderState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.state_name())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// StrategyGeneState — 4-state strategy evolution (from ORACLE-FORGE)
// ═══════════════════════════════════════════════════════════════════════════════

/// Strategy lifecycle — mirrors ORACLE-FORGE `strategy_gene.py` GeneState.
///
/// Transition table:
/// ```text
/// SANDBOX  → PROBATION  (fitness > threshold for N consecutive periods)
/// PROBATION → CORE       (promotion criteria: Sharpe > 1.0, hit-rate > 55%)
/// PROBATION → SANDBOX    (fitness dropped below threshold)
/// CORE     → RETIRED     (max_age expired or user kills)
/// CORE     → PROBATION   (degradation: Sharpe < 0.5 or drawdown > -20%)
/// RETIRED  → (terminal)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StrategyGeneState {
    /// Promising but unproven — small size, tight risk gates
    Sandbox,
    /// Live-tested, passed minimum bar — moderate size
    Probation,
    /// Battle-hardened, full allocation — maximum size
    Core,
    /// Decommissioned, keep historical data only
    Retired,
}

impl StateMachine for StrategyGeneState {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Retired)
    }

    fn can_transition_to(&self, next: &Self) -> bool {
        use StrategyGeneState::*;
        matches!(
            (self, next),
            (Sandbox, Probation) | (Probation, Core | Sandbox) | (Core, Probation | Retired)
        )
    }

    fn state_name(&self) -> &'static str {
        match self {
            Self::Sandbox => "SANDBOX",
            Self::Probation => "PROBATION",
            Self::Core => "CORE",
            Self::Retired => "RETIRED",
        }
    }
}

impl std::fmt::Display for StrategyGeneState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.state_name())
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// UnifiedOrder — canonical cross-market order type
// ═══════════════════════════════════════════════════════════════════════════════

/// Canonical order representation.
///
/// Corresponds to proto `UnifiedOrder` message (25 fields).
/// Every exchange adapter converts its native order into this type before
/// the Engine, Risk gates, or Signal pipeline touch it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedOrder {
    // ── Market identity ──
    /// Exchange-normalised instrument ID.  e.g. "ETH-USDT-SWAP", "PUMP", "BTC/USD"
    pub inst_id: String,
    /// Product type (swap, spot, futures, …)
    pub inst_type: InstType,

    // ── Order identity ──
    /// Client-generated order ID (idempotency key)
    pub cl_ord_id: String,
    /// Exchange-assigned order ID (empty until exchange responds)
    pub ord_id: String,

    // ── Trade direction ──
    pub side: OrderSide,
    pub order_type: OrderType,

    // ── Size & price (string to avoid f64 precision loss — proto contract) ──
    pub sz: String,
    pub px: String,

    // ── State tracking ──
    pub state: OrderState,
    pub state_msg: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,

    // ── Fill info ──
    pub fill_sz: String,
    pub fill_px: String,
    pub fee: String,

    // ── Risk / trace ──
    pub tag: String,
    pub trace_id: String,

    // ── AI extension (AlphaCast / MCTS) ──
    pub alphacast_conf: f64,
    pub mcts_path_value: f64,

    /// Extension map for exchange-specific fields not covered above.
    pub meta: HashMap<String, String>,
}

/// Conversion: canonical UnifiedOrder → HL-specific OrderRequest wire fields.
impl UnifiedOrder {
    /// Extract HL-compatible order placement fields.
    /// Used by Executor to convert from canonical type to exchange JSON.
    pub fn to_hl_fields(&self) -> HlOrderFields {
        HlOrderFields {
            coin: self.inst_id.clone(),
            is_buy: self.side.is_buy(),
            sz: self.sz_f64(),
            limit_px: self.px_f64(),
            reduce_only: self
                .meta
                .get("reduce_only")
                .map(|v| v == "true")
                .unwrap_or(false),
            tif: self.order_type.tif_str().to_string(),
            cloid: Some(self.cl_ord_id.clone()),
            order_type: match self.order_type {
                OrderType::Limit | OrderType::PostOnly => "Limit",
                _ => "Limit", // HL only supports Limit
            }
            .to_string(),
        }
    }
}

/// HL-specific order wire fields (subset of UnifiedOrder for exchange API).
#[derive(Debug, Clone)]
pub struct HlOrderFields {
    pub coin: String,
    pub is_buy: bool,
    pub sz: f64,
    pub limit_px: f64,
    pub reduce_only: bool,
    pub tif: String,
    pub cloid: Option<String>,
    pub order_type: String,
}

impl UnifiedOrder {
    /// Create a new limit order with minimum required fields.
    pub fn new(
        inst_id: &str,
        inst_type: InstType,
        side: OrderSide,
        order_type: OrderType,
        sz: f64,
        px: f64,
        trace_id: &str,
    ) -> Self {
        let ts = chrono::Utc::now().timestamp_millis();
        Self {
            inst_id: inst_id.to_string(),
            inst_type,
            cl_ord_id: format!("ge_{}", uuid::Uuid::new_v4()),
            ord_id: String::new(),
            side,
            order_type,
            sz: format!("{sz}"),
            px: format!("{px}"),
            state: OrderState::New,
            state_msg: String::new(),
            created_at_ms: ts,
            updated_at_ms: ts,
            fill_sz: "0".to_string(),
            fill_px: "0".to_string(),
            fee: "0".to_string(),
            tag: String::new(),
            trace_id: trace_id.to_string(),
            alphacast_conf: 0.0,
            mcts_path_value: 0.0,
            meta: HashMap::new(),
        }
    }

    /// Parse the order size string to f64 (0.0 on failure).
    pub fn sz_f64(&self) -> f64 {
        self.sz.parse().unwrap_or(0.0)
    }

    /// Parse the order price string to f64 (0.0 on failure).
    pub fn px_f64(&self) -> f64 {
        self.px.parse().unwrap_or(0.0)
    }

    /// Parse the fill size string to f64 (0.0 on failure).
    pub fn fill_sz_f64(&self) -> f64 {
        self.fill_sz.parse().unwrap_or(0.0)
    }

    /// Parse the fee string to f64 (0.0 on failure).
    pub fn fee_f64(&self) -> f64 {
        self.fee.parse().unwrap_or(0.0)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// AlphaSignal — canonical ML signal type (for Step 5 registry)
// ═══════════════════════════════════════════════════════════════════════════════

/// Alpha engine output signal — mirrors proto `AlphaSignal`.
///
/// Every alpha engine (OBI, FundingRate, CrossSection, AlphaCast, …) emits
/// this struct. The decision layer aggregates signals per instrument, applies
/// gating, and converts to `UnifiedOrder`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlphaSignal {
    pub trace_id: String,
    pub inst_id: String,
    pub ts_ms: i64,
    pub pulse_id: i32,

    /// Engine name, e.g. "obi_v2", "funding_rate_arb", "cross_engine"
    pub alpha_name: String,
    /// Raw signal ∈ [-1, 1]; positive = bullish, negative = bearish
    pub raw_signal: f64,
    /// Empirical confidence ∈ [0, 1]
    pub confidence: f64,

    // Gating results
    pub g1_pass: bool, // Liquidity gate
    pub g2_pass: bool, // Regime gate
    pub g3_pass: bool, // Cross-section gate
    pub g4_pass: bool, // MetaLabeler gate

    // ML extension (Phase 4+)
    pub alphacast_score: f64,
    pub alphacast_conf: f64,
    pub alphacast_sigma: f64,
    pub mcts_ev: f64,

    /// 178d feature snapshot (empty until Phase 3)
    pub feature_snapshot: Vec<f32>,

    /// True when all gates pass → tradable
    pub passed: bool,
}

impl AlphaSignal {
    /// Create a new order wire format with required fields.
    pub fn new(alpha_name: &str, inst_id: &str) -> Self {
        Self {
            trace_id: String::new(),
            inst_id: inst_id.to_string(),
            ts_ms: 0,
            pulse_id: 0,
            alpha_name: alpha_name.to_string(),
            raw_signal: 0.0,
            confidence: 0.0,
            g1_pass: false,
            g2_pass: false,
            g3_pass: false,
            g4_pass: false,
            alphacast_score: 0.0,
            alphacast_conf: 0.0,
            alphacast_sigma: 0.0,
            mcts_ev: 0.0,
            feature_snapshot: Vec::new(),
            passed: false,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// MarketSnapshot — cross-market L2 aggregation (for future WS adapters)
// ═══════════════════════════════════════════════════════════════════════════════

/// Aggregated market view — mirrors proto `MarketSnapshot`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub inst_id: String,
    pub ts_ms: i64,
    pub best_bid: f64,
    pub best_ask: f64,
    pub bid_depth_10: f64,
    pub ask_depth_10: f64,
    pub spread_bps: f64,
    pub mid_price: f64,
    pub tick_count_5m: i32,
    pub buy_vol_5m: f64,
    pub sell_vol_5m: f64,
    pub pulse_id: i32,
}

impl MarketSnapshot {
    /// Build a cancel request for all open orders on the given instrument.
    pub fn new(inst_id: &str, book: &crate::types::L2Book) -> Self {
        let mid = book.mid_price().unwrap_or(0.0);
        let spread = book.spread_bps().unwrap_or(0.0);
        let bid10: f64 = book.bids.iter().take(10).map(|l| l.sz).sum();
        let ask10: f64 = book.asks.iter().take(10).map(|l| l.sz).sum();
        Self {
            inst_id: inst_id.to_string(),
            ts_ms: book.timestamp as i64,
            best_bid: book.best_bid().unwrap_or(0.0),
            best_ask: book.best_ask().unwrap_or(0.0),
            bid_depth_10: bid10,
            ask_depth_10: ask10,
            spread_bps: spread,
            mid_price: mid,
            tick_count_5m: 0,
            buy_vol_5m: 0.0,
            sell_vol_5m: 0.0,
            pulse_id: 0,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_state_transitions_valid() {
        // Normal lifecycle
        assert!(OrderState::New.can_transition_to(&OrderState::Posted));
        assert!(OrderState::Posted.can_transition_to(&OrderState::PartialFilled));
        assert!(OrderState::PartialFilled.can_transition_to(&OrderState::Filled));
        assert!(OrderState::Filled.is_terminal());

        // Cancel path
        assert!(OrderState::Posted.can_transition_to(&OrderState::PendingCancel));
        assert!(OrderState::PendingCancel.can_transition_to(&OrderState::Canceled));

        // Reject
        assert!(OrderState::New.can_transition_to(&OrderState::Rejected));
        assert!(OrderState::Rejected.is_terminal());
    }

    #[test]
    fn order_state_transitions_invalid() {
        // Can't go back from terminal
        assert!(!OrderState::Filled.can_transition_to(&OrderState::Posted));
        // Can't skip states
        assert!(!OrderState::New.can_transition_to(&OrderState::Filled));
        // Can't cancel after filled
        assert!(!OrderState::Filled.can_transition_to(&OrderState::Canceled));
    }

    #[test]
    fn unified_order_builder() {
        let o = UnifiedOrder::new(
            "BTC-USDT-SWAP",
            InstType::Swap,
            OrderSide::Buy,
            OrderType::PostOnly,
            0.01,
            65000.0,
            "trace-001",
        );
        assert!(o.cl_ord_id.starts_with("ge_"));
        assert_eq!(o.state, OrderState::New);
        assert_eq!(o.sz_f64(), 0.01);
        assert_eq!(o.px_f64(), 65000.0);
    }

    #[test]
    fn engine_state_transitions() {
        use crate::state::State;
        // Happy path: Idle → ColdStart → NORMAL → UNWIND → NORMAL
        assert!(State::Idle.can_transition_to(&State::ColdStart));
        assert!(State::ColdStart.can_transition_to(&State::Active));
        assert!(State::Active.can_transition_to(&State::Unwind));
        assert!(State::Unwind.can_transition_to(&State::Active));
        // Emergency: NORMAL → EMERGENCY_IOC → COOLDOWN → NORMAL
        assert!(State::Active.can_transition_to(&State::Shedding));
        assert!(State::Shedding.can_transition_to(&State::Cooldown));
        assert!(State::Cooldown.can_transition_to(&State::Active));
        // No engine state is terminal
        assert!(!State::Idle.is_terminal());
        assert!(!State::Cooldown.is_terminal());
        // Invalid: can't skip states
        assert!(!State::Idle.can_transition_to(&State::Active));
        assert!(!State::Idle.can_transition_to(&State::Shedding));
    }

    #[test]
    fn strategy_gene_transitions() {
        // Promotion path
        assert!(StrategyGeneState::Sandbox.can_transition_to(&StrategyGeneState::Probation));
        assert!(StrategyGeneState::Probation.can_transition_to(&StrategyGeneState::Core));
        // Demotion path
        assert!(StrategyGeneState::Core.can_transition_to(&StrategyGeneState::Probation));
        // Retirement
        assert!(StrategyGeneState::Core.can_transition_to(&StrategyGeneState::Retired));
        assert!(StrategyGeneState::Retired.is_terminal());
        // Invalid
        assert!(!StrategyGeneState::Sandbox.can_transition_to(&StrategyGeneState::Core));
        assert!(!StrategyGeneState::Retired.can_transition_to(&StrategyGeneState::Sandbox));
    }
}
