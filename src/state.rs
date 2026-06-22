//! State Machine — engine lifecycle, per-coin.
//!
//! Each coin has its own independent state track:
//!
//! ```text
//!                ┌─────────┐
//!                │  Idle   │──── Watch account, wait for enough balance
//!                └────┬────┘
//!                     │ withdrawable > min_reserve
//!                ┌────▼────┐
//!         ┌──────│ ColdStart│──────┐ 2 cycles of post-only, no inventory check
//!         │      └─────────┘      │
//!         │ timeouts/done         │ books stale
//!         │                  ┌────▼────┐
//!         │                  │ NORMAL   │─── Bilateral Post-Only, cubic skew
//!         │                  └────┬────┘
//!         │                       │ pos_ratio >= unwind_watermark (40%)
//!         │                  ┌────▼──────────┐
//!         │                  │ PASSIVE_UNWIND │─── Freeze same-side + tick-retreat opposite
//!         │                  └────┬──────────┘
//!         │                       │ pos_ratio < hysteresis (20%)
//!         │                       │ OR pos_ratio >= shed_trigger (90%)
//!         │                  ┌────▼────┐
//!         │                  │ EMERGENCY│─── IOC 50% takedown
//!         │                  └────┬────┘
//!         │                       │ shed done
//!         │                  ┌────▼────┐
//!         │                  │Cooldown  │─── 30s cooldown, no orders
//!         │                  └────┬────┘
//!         └───────────────────────┘
//!
//! ### Hysteresis Buffer
//! Enter UNWIND at watermark (25%), exit at watermark − hysteresis (25% − 20% = 5%).
//! This 20% band prevents state flickering when position oscillates around 25%.
//!
//! ### GTC Last-Resort (B1 fix)
//! When Shedding IOC fails completely for 3 consecutive cycles, a GTC limit order
//! is placed at 5% price discount to break the deadlock. The `zero_shed_rounds`
//! counter is per-coin and resets on any fill.
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

use crate::types_proto::StateMachine;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum State {
    #[default]
    /// Waiting for account balance / initial conditions
    Idle,
    /// Post-Only only, 2 cycles, no inventory gate
    ColdStart,
    /// Bilateral Post-Only with cubic skew (0%–25%)
    #[serde(alias = "NORMAL")]
    Active,
    /// Passive post-only unwind with tick retreat (25%–90%)
    Unwind,
    /// Emergency IOC takedown (90%+)
    Shedding,
    /// 30s pause after shed or error
    Cooldown,
    /// Conditions not met — logged reason, recheck each cycle
    Waiting,
    /// Gate blocked: all orders cancelled, waiting for market spread to widen
    GateBlocked,
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.state_name())
    }
}

impl StateMachine for State {
    fn is_terminal(&self) -> bool {
        // No engine state is terminal — the engine can always recycle.
        false
    }

    fn can_transition_to(&self, next: &Self) -> bool {
        use State::*;
        matches!(
            (self, next),
            (Idle, ColdStart)
                | (ColdStart, Active | Idle)
                | (Active, Unwind | Shedding)
                | (Unwind, Active | Shedding)
                | (Shedding, Cooldown)
                | (Cooldown, Active | Unwind | Shedding)
                | (Waiting, Active)
                | (GateBlocked, Active)
        )
    }

    fn state_name(&self) -> &'static str {
        match self {
            State::Idle => "IDLE",
            State::ColdStart => "COLD_START",
            State::Active => "NORMAL",
            State::Unwind => "PASSIVE_UNWIND",
            State::Shedding => "EMERGENCY_IOC",
            State::Cooldown => "COOLDOWN",
            State::Waiting => "WAITING",
            State::GateBlocked => "GATE_BLOCKED",
        }
    }
}

/// Per-coin state: each coin has independent lifecycle.
#[derive(Debug, Clone)]
pub struct PerCoinState {
    pub state: State,
    /// Number of completed cold-start cycles
    pub cold_start_cycles: u32,
    /// When we entered the current state
    pub entered_at: Instant,
    /// B1: Cross-cycle counter — consecutive zero-shed rounds (IOC all failed)
    /// Reset on any IOC fill. Triggers GTC last-resort at 3.
    pub zero_shed_rounds: u32,
    /// Asymmetric Tick Shading: rolling count of consecutive Post-Only
    /// SELL "would match" rejections in NORMAL state. Drives dynamic
    /// BUY price retreat to prevent one-sided LONG accumulation.
    pub ask_rejections: u32,
    /// V12.1: Running tally of BUY fills since last side reset (sensitive skew in COARSE mode)
    pub buy_fills_tally: u32,
    /// V12.1: Running tally of SELL fills since last side reset
    pub sell_fills_tally: u32,
    /// V12.2: Consecutive same-side fills counter (toxicity momentum detection)
    pub consecutive_same_side_fills: u32,
    /// V12.2: Last fill side for momentum tracking ("buy" or "sell")
    pub last_fill_side: Option<String>,
    pub unwind_cooldown: u32,
    /// Flip hysteresis: sequence # of the cycle that last changed direction sign.
    /// Prevents flip-flopping — locks opposite direction for FLIP_LOCK_CYCLES after a flip.
    pub last_flip_cycle: u64,
    /// Number of cycles remaining in the flip lockout.
    pub freeze_remaining: u32,
    /// Direction being frozen: -1=SHORT (no BUY), 1=LONG (no SELL), 0=none
    pub freeze_direction: i8,
    /// Gate tier mode: "TSUNAMI" | "SNIPER" | "BLOCKED"
    pub gate_mode: String,
    /// Size multiplier from gate tier (1.0 / 0.4 / 0.0)
    pub size_multiplier: f64,
    /// Number of consecutive cycles with favorable conditions (Waiting->Active hysteresis)
    pub favorable_cycles: u32,
    /// Last reason for being in Waiting state
    pub waiting_reason: String,

        }

impl PerCoinState {
    /// Create a new per-coin state machine in Idle state.
    pub fn new() -> Self {
        Self {
            gate_mode: "BLOCKED".to_string(),
            size_multiplier: 0.0,
            state: State::Idle,
            cold_start_cycles: 0,
            entered_at: Instant::now(),
            zero_shed_rounds: 0,
            ask_rejections: 0,
            unwind_cooldown: 0,
            buy_fills_tally: 0,
            sell_fills_tally: 0,
            consecutive_same_side_fills: 0,
            last_fill_side: None,
            last_flip_cycle: 0,
            freeze_remaining: 0,
            freeze_direction: 0,
            favorable_cycles: 0,
            waiting_reason: String::new(),
        }
    }

    /// Create a PerCoinState with a pre-set state — used by bootstrap recovery
    /// to inject existing positions into the correct state machine lane.
    pub fn new_with_state(state: State) -> Self {
        Self {
            gate_mode: "BLOCKED".to_string(),
            size_multiplier: 0.0,
            state,
            cold_start_cycles: 0,
            entered_at: Instant::now(),
            zero_shed_rounds: 0,
            ask_rejections: 0,
            unwind_cooldown: 0,
            buy_fills_tally: 0,
            sell_fills_tally: 0,
            consecutive_same_side_fills: 0,
            last_fill_side: None,
            last_flip_cycle: 0,
            freeze_remaining: 0,
            freeze_direction: 0,
            favorable_cycles: 0,
            waiting_reason: String::new(),
        }
    }

    /// Time elapsed since this coin entered its current state, in seconds.
    fn elapsed_secs(&self) -> f64 {
        self.entered_at.elapsed().as_secs_f64()
    }
}

/// Per-coin state machine. Each coin manages its own lifecycle independently,
/// preventing one coin's state transition from affecting another's.
pub struct CoinStateMachine {
    /// Per-coin state lookup — keyed by coin ticker
    coins: HashMap<String, PerCoinState>,
    /// Max cold-start cycles before transitioning to Active
    cold_start_max: u32,
    /// Cooldown duration shared across coins
    cooldown_duration_secs: f64,
}

impl CoinStateMachine {
    /// Create a new coin state registry (manages all coins).
    pub fn new() -> Self {
        Self {
            coins: HashMap::new(),
            cold_start_max: 2,
            cooldown_duration_secs: 30.0,
        }
    }

    /// Force-inject a coin with a pre-set state — used by bootstrap recovery
    /// to map existing positions into the correct FSM lane.
    pub fn init_coin(&mut self, coin: &str, state: State) {
        self.coins.insert(coin.to_string(), PerCoinState::new_with_state(state));
    }

    /// Get or initialize state for a coin.
    pub(crate) fn get_or_init(&mut self, coin: &str) -> &mut PerCoinState {
        self.coins
            .entry(coin.to_string())
            .or_insert_with(PerCoinState::new)
        }

    /// Current state for a coin (Idle if never seen).
    pub fn state_of(&mut self, coin: &str) -> State {
        self.get_or_init(coin).state
    }

    /// Transition a coin to a new state. Logs only on actual change.
    /// Also resets ask_rejections counter + V12.1 fill tallies — a state change means the
    /// market context has shifted, so shading + skew tracking should start fresh.
    pub fn transition(&mut self, coin: &str, next: State) {
        let cs = self.get_or_init(coin);
        if cs.state != next {
            cs.ask_rejections = 0; // fresh start in new state
            cs.buy_fills_tally = 0;  // V12.1: reset fill tallies
            cs.sell_fills_tally = 0;
            tracing::info!(
                coin = %coin,
                from = %cs.state,
                to = %next,
                "state transition"
            );
            cs.state = next;
            cs.entered_at = Instant::now();
            cs.ask_rejections = 0; // fresh start in new state
        }
    }

    /// Time elapsed since coin entered its current state, in seconds.
    pub fn elapsed_secs(&mut self, coin: &str) -> f64 {
        self.get_or_init(coin).elapsed_secs()
    }

    /// Check if cooldown has elapsed for a specific coin.
    pub fn cooldown_done(&mut self, coin: &str) -> bool {
        let cs = self.get_or_init(coin);
        cs.state == State::Cooldown && cs.elapsed_secs() >= self.cooldown_duration_secs
    }

    /// Transition to Waiting with a human-readable reason.
    /// Transition to GateBlocked: fires cancel_all externally, but tracks state.
    pub fn enter_gate_blocked(&mut self, coin: &str, reason: &str) {
        let cs = self.get_or_init(coin);
        cs.state = State::GateBlocked;
        cs.entered_at = std::time::Instant::now();
        cs.waiting_reason = reason.to_string();
        cs.favorable_cycles = 0;
    }

    /// Check if coin is currently gate-blocked.
    pub fn is_gate_blocked(&self, coin: &str) -> bool {
        self.coins.get(coin).map_or(false, |cs| cs.state == State::GateBlocked)
    }

    /// Transition to Waiting with a human-readable reason.
    pub fn enter_waiting(&mut self, coin: &str, reason: &str) {
        let cs = self.get_or_init(coin);
        cs.state = State::Waiting;
        cs.entered_at = std::time::Instant::now();
        cs.waiting_reason = reason.to_string();
        cs.favorable_cycles = 0;
    }

    /// Tick favorable_cycles in Waiting state. Returns true if > threshold.
    pub fn tick_waiting(&mut self, coin: &str, threshold: u32) -> bool {
        let cs = self.get_or_init(coin);
        if cs.state == State::Waiting {
            cs.favorable_cycles += 1;
            cs.favorable_cycles >= threshold
        } else {
            false
        }
    }

    /// Get the waiting reason.
    pub fn waiting_reason_for(&mut self, coin: &str) -> String {
        self.get_or_init(coin).waiting_reason.clone()
    }

    /// Advance cold-start counter for a coin; returns true if ready for Active.
    pub fn advance_cold_start(&mut self, coin: &str) -> bool {
        let cs = self.get_or_init(coin);
        cs.cold_start_cycles += 1;
        cs.cold_start_cycles >= self.cold_start_max
    }

    /// Can we place orders for this coin?
    pub fn can_place_orders(&mut self, coin: &str) -> bool {
        matches!(
            self.get_or_init(coin).state,
            State::ColdStart | State::Active | State::Shedding | State::Unwind
        )
    }

    /// Is this coin in passive unwind mode?
    pub fn is_unwind(&mut self, coin: &str) -> bool {
        matches!(self.get_or_init(coin).state, State::Unwind)
    }

    /// Remaining anti-ping-pong cooldown cycles for a coin.
    pub fn unwind_cooldown_left(&mut self, coin: &str) -> u32 {
        self.get_or_init(coin).unwind_cooldown
    }

    /// Decrement the post-unwind cooldown counter by one cycle.
    pub fn tick_unwind_cooldown(&mut self, coin: &str) {
        let cs = self.get_or_init(coin);
        if cs.unwind_cooldown > 0 { cs.unwind_cooldown -= 1; }
    }

    /// Set or reset the post-unwind BUY-suppression cooldown to a fixed number of cycles.
    pub fn reset_unwind_cooldown(&mut self, coin: &str, cycles: u32) {
        self.get_or_init(coin).unwind_cooldown = cycles;
    }

    /// Is this coin in emergency shed mode?
    pub fn is_shedding(&mut self, coin: &str) -> bool {
        matches!(self.get_or_init(coin).state, State::Shedding)
    }

    // ── B1: GTC last-resort — cross-cycle zero-shed tracking ──

    /// Increment and return zero_shed_rounds for this coin.
    pub fn increment_zero_shed(&mut self, coin: &str) -> u32 {
        let cs = self.get_or_init(coin);
        cs.zero_shed_rounds += 1;
        cs.zero_shed_rounds
    }

    /// Reset zero_shed counter (any IOC fill resets).
    pub fn reset_zero_shed(&mut self, coin: &str) {
        self.get_or_init(coin).zero_shed_rounds = 0;
    }

    /// Get current zero_shed_rounds for a coin.
    pub fn zero_shed_rounds(&mut self, coin: &str) -> u32 {
        self.get_or_init(coin).zero_shed_rounds
    }

    // ── Asymmetric Tick Shading ──

    /// Increment and return the rolling ask-rejection counter for this coin.
    /// Called when a SELL Post-Only order is rejected with "would match".
    pub fn increment_ask_rejections(&mut self, coin: &str) -> u32 {
        let cs = self.get_or_init(coin);
        cs.ask_rejections += 1;
        cs.ask_rejections
    }

    /// Reset the ask-rejection counter (e.g. after a successful BUY order
    /// or state transition away from NORMAL).
    pub fn reset_ask_rejections(&mut self, coin: &str) {
        self.get_or_init(coin).ask_rejections = 0;
    }

    /// Get the current ask-rejection count for a coin.
    pub fn ask_rejection_count(&mut self, coin: &str) -> u32 {
        self.get_or_init(coin).ask_rejections
    }

    // ── V12.1 Sensitive Skew Control (COARSE mode 1-tick fill tally) ──

    /// Increment buy-side fill tally. Returns new count.
    pub fn tally_buy_fill(&mut self, coin: &str) -> u32 {
        let cs = self.get_or_init(coin);
        cs.buy_fills_tally += 1;
        cs.buy_fills_tally
    }

    /// Increment sell-side fill tally. Returns new count.
    pub fn tally_sell_fill(&mut self, coin: &str) -> u32 {
        let cs = self.get_or_init(coin);
        cs.sell_fills_tally += 1;
        cs.sell_fills_tally
    }

    /// Reset all fill tallies (e.g. on state transition or side rebalancing).
    pub fn reset_fill_tallies(&mut self, coin: &str) {
        let cs = self.get_or_init(coin);
        cs.buy_fills_tally = 0;
        cs.sell_fills_tally = 0;
    }

    /// Check if buy side should be stopped (COARSE mode sensitive skew).
    pub fn buy_side_tapped_out(&self, coin: &str, max_fills: u32) -> bool {
        self.coins.get(coin).map_or(false, |cs| cs.buy_fills_tally >= max_fills)
    }

    /// Check if sell side should be stopped (COARSE mode sensitive skew).
    pub fn sell_side_tapped_out(&self, coin: &str, max_fills: u32) -> bool {
        self.coins.get(coin).map_or(false, |cs| cs.sell_fills_tally >= max_fills)
    }

    // ── V8 Flip Hysteresis: prevent directional whiplash ──

    /// Set flip lock for FLIP_LOCK_CYCLES. Called when position sign flips.
    pub fn set_freeze(&mut self, coin: &str, direction: i8) {
        let cs = self.get_or_init(coin);
        cs.freeze_remaining = 50;
        cs.freeze_direction = direction;
        cs.last_flip_cycle = 0;
    }

    /// Tick down freeze counter. Call once per cycle.
    pub fn tick_freeze(&mut self, coin: &str) {
        let cs = self.get_or_init(coin);
        if cs.freeze_remaining > 0 {
            cs.freeze_remaining -= 1;
        }
    }

    /// Returns true if directional freeze is active.
    pub fn is_frozen(&self, coin: &str) -> bool {
        self.coins.get(coin).map(|c| c.freeze_remaining > 0).unwrap_or(false)
    }

    /// Returns the freeze direction: -1=SHORT_freeze, 1=LONG_freeze, 0=none.
    pub fn freeze_direction_for(&self, coin: &str) -> i8 {
        self.coins.get(coin).map(|c| c.freeze_direction).unwrap_or(0)
    }

    // ── V12.2: Fill toxicity momentum tracking ──

    /// Record a fill side for toxicity momentum detection.
    /// Resets the consecutive counter if side switches.
    /// Returns the new consecutive count for this side.
    pub fn record_fill_side(&mut self, coin: &str, is_buy: bool) -> u32 {
        let cs = self.get_or_init(coin);
        let this_side = if is_buy { "buy" } else { "sell" };
        match &cs.last_fill_side {
            Some(last) if last == this_side => {
                cs.consecutive_same_side_fills += 1;
            }
            _ => {
                cs.consecutive_same_side_fills = 1;
            }
        }
        cs.last_fill_side = Some(this_side.to_string());
        cs.consecutive_same_side_fills
    }

    /// Check if a side is blocked by toxicity momentum (>=3 consecutive same-side fills).
    /// Returns Some("buy") or Some("sell") if blocked, None if neither.
    pub fn toxicity_blocked_side(&self, coin: &str, threshold: u32) -> Option<String> {
        let cs = self.coins.get(coin)?;
        if cs.consecutive_same_side_fills >= threshold {
            cs.last_fill_side.clone()
        } else {
            None
        }
    }

    /// Reset toxicity tracking (on state transition, gate mode change, or manual reset).
    pub fn reset_toxicity(&mut self, coin: &str) {
        let cs = self.get_or_init(coin);
        cs.consecutive_same_side_fills = 0;
        cs.last_fill_side = None;
    }
}
