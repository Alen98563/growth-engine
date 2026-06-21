#!/usr/bin/env python3
"""Comprehensive V3 patch — Kill Switch + Ping-Pong + Vol-Adaptive + Dust Filter"""
import re, sys

# ============================================================
# FILE 1: config.rs — new fields
# ============================================================
def patch_config(path):
    with open(path) as f: code = f.read()

    # 1a. Add fields after max_consecutive_cooldowns
    new_fields = """
    // ── Kill Switch ──
    /// Minimum withdrawable balance before emergency kill switch triggers (USD)
    pub killswitch_min_wd: f64,
    // ── Anti-Ping-Pong ──
    /// Cooldown after exiting PASSIVE_UNWIND before NORMAL bids can resume (seconds)
    pub post_unwind_cooldown_secs: f64,
    /// Bid spread multiplier during post-unwind cooldown (ask stays normal)
    pub post_unwind_bid_spread_mult: f64,
    // ── Volatility-Adaptive ──
    /// Number of past mid-price samples for volatility estimation (≈cycles)
    pub vol_lookback_cycles: u32,
    /// Volatility multiplier cap (spread limited to base * this regardless of vol)
    pub vol_multiplier_cap: f64,
    /// Volatility threshold in bps: above this baseline, multiplier activates linearly
    pub vol_threshold_bps: f64,
    // ── Dust Filtering ──
    /// Positions with notional below this (USD) excluded from risk ratio/total calc
    pub dust_threshold_notional: f64,"""

    code = code.replace(
        "/// Maximum consecutive cooldowns before forcing emergency IOC shed\n    pub max_consecutive_cooldowns: u32,",
        "/// Maximum consecutive cooldowns before forcing emergency IOC shed\n    pub max_consecutive_cooldowns: u32," + new_fields
    )

    # 1b. Update Default impl
    code = code.replace(
        "max_consecutive_cooldowns: 3,",
        "max_consecutive_cooldowns: 3,\n            killswitch_min_wd: 80.0,\n            post_unwind_cooldown_secs: 60.0,\n            post_unwind_bid_spread_mult: 3.0,\n            vol_lookback_cycles: 6,\n            vol_multiplier_cap: 3.0,\n            vol_threshold_bps: 5.0,\n            dust_threshold_notional: 20.0,"
    )

    # 1c. Add env loading in from_env()
    env_load = """
        if let Ok(v) = std::env::var("HL_KILLSWITCH_MIN_WD") {
            cfg.killswitch_min_wd = v.parse().unwrap_or(80.0);
        }
        if let Ok(v) = std::env::var("HL_POST_UNWIND_COOLDOWN_SECS") {
            cfg.post_unwind_cooldown_secs = v.parse().unwrap_or(60.0);
        }
        if let Ok(v) = std::env::var("HL_POST_UNWIND_BID_SPREAD_MULT") {
            cfg.post_unwind_bid_spread_mult = v.parse().unwrap_or(3.0);
        }
        if let Ok(v) = std::env::var("HL_VOL_LOOKBACK_CYCLES") {
            cfg.vol_lookback_cycles = v.parse().unwrap_or(6);
        }
        if let Ok(v) = std::env::var("HL_VOL_MULTIPLIER_CAP") {
            cfg.vol_multiplier_cap = v.parse().unwrap_or(3.0);
        }
        if let Ok(v) = std::env::var("HL_VOL_THRESHOLD_BPS") {
            cfg.vol_threshold_bps = v.parse().unwrap_or(5.0);
        }
        if let Ok(v) = std::env::var("HL_DUST_THRESHOLD_NOTIONAL") {
            cfg.dust_threshold_notional = v.parse().unwrap_or(20.0);
        }"""
    code = code.replace(
        'if let Ok(v) = std::env::var("HL_MAX_CONSECUTIVE_COOLDOWNS") {\n            cfg.max_consecutive_cooldowns = v.parse().unwrap_or(3);\n        }',
        'if let Ok(v) = std::env::var("HL_MAX_CONSECUTIVE_COOLDOWNS") {\n            cfg.max_consecutive_cooldowns = v.parse().unwrap_or(3);\n        }' + env_load
    )

    with open(path, 'w') as f: f.write(code)
    print("[OK] config.rs patched")

# ============================================================
# FILE 2: state.rs — KillSwitch state + PostUnwind tracking
# ============================================================
def patch_state(path):
    with open(path) as f: code = f.read()

    # 2a. Add KillSwitch to enum
    code = code.replace(
        "    Cooldown,\n}",
        "    Cooldown,\n    /// Emergency kill switch — cancel all, liquidate all, halt.\n    KillSwitch,\n}"
    )

    # 2b. Add KillSwitch to Display
    code = code.replace(
        'State::Cooldown => write!(f, "COOLDOWN"),\n        }',
        'State::Cooldown => write!(f, "COOLDOWN"),\n            State::KillSwitch => write!(f, "KILL_SWITCH"),\n        }'
    )

    # 2c. Add post_unwind_until to PerCoinState
    code = code.replace(
        "    /// Counter for Unwind state — tracks how many cycles we've tried to unwind\n    pub unwind_attempts: u32,\n}",
        "    /// Counter for Unwind state — tracks how many cycles we've tried to unwind\n    pub unwind_attempts: u32,\n    /// Post-unwind cooldown timestamp — bids suppressed until this time (anti-ping-pong)\n    pub post_unwind_until: Option<std::time::Instant>,\n}"
    )

    # 2d. Update PerCoinState::new()
    code = code.replace(
        'consecutive_cooldowns: 0,\n            unwind_attempts: 0,\n        }',
        'consecutive_cooldowns: 0,\n            unwind_attempts: 0,\n            post_unwind_until: None,\n        }'
    )

    # 2e. Add post-unwind methods to CoinStateMachine (before "Can we place orders")
    new_methods = """
    /// Check if this coin is still in post-unwind cooldown (anti-ping-pong).
    pub fn is_post_unwind_cooldown(&mut self, coin: &str) -> bool {
        self.get_or_init(coin).post_unwind_until
            .map(|t| std::time::Instant::now() < t)
            .unwrap_or(false)
    }

    /// Start post-unwind cooldown timer (called when exiting PASSIVE_UNWIND → Active).
    pub fn start_post_unwind_cooldown(&mut self, coin: &str, duration_secs: f64) {
        let cs = self.get_or_init(coin);
        cs.post_unwind_until = Some(std::time::Instant::now() + std::time::Duration::from_secs_f64(duration_secs));
        tracing::info!(
            coin = %coin,
            duration_secs = duration_secs,
            "post-unwind cooldown started — bids suppressed, ask-only"
        );
    }

    /// Is this coin in kill switch state?
    pub fn is_kill_switch(&mut self, coin: &str) -> bool {
        matches!(self.get_or_init(coin).state, State::KillSwitch)
    }"""
    code = code.replace(
        "    /// Can we place orders for this coin?\n    pub fn can_place_orders",
        new_methods + "\n\n    /// Can we place orders for this coin?\n    pub fn can_place_orders"
    )

    # 2f. Allow order placement in KillSwitch (IOC liquidation)
    code = code.replace(
        "State::ColdStart | State::Active | State::Shedding | State::Unwind",
        "State::ColdStart | State::Active | State::Shedding | State::Unwind | State::KillSwitch"
    )

    with open(path, 'w') as f: f.write(code)
    print("[OK] state.rs patched")

# ============================================================
# FILE 3: risk.rs — VolTracker + Dust filter + Enhanced assess
# ============================================================
def patch_risk(path):
    with open(path) as f: code = f.read()

    # 3a. Add VolTracker struct after imports, before RiskEngine
    vol_tracker = """
/// Tracks recent mid-prices to compute a volatility multiplier.
/// Used for volatility-adaptive spread: when prices jump fast, spread widens.
pub struct VolTracker {
    history: Vec<f64>,
    max_samples: usize,
}

impl VolTracker {
    pub fn new(max_samples: usize) -> Self {
        Self { history: Vec::new(), max_samples }
    }

    /// Push a new mid-price observation.
    pub fn push(&mut self, mid: f64) {
        if mid <= 0.0 { return; }
        self.history.push(mid);
        if self.history.len() > self.max_samples {
            self.history.remove(0);
        }
    }

    /// Compute volatility multiplier [1.0, cap].
    /// Uses normalized standard deviation: σ / μ × 10000 = vol in bps.
    /// If vol_bps exceeds threshold, multiplier = 1 + (vol - threshold) / threshold.
    /// Returns 1.0 if not enough data (< 3 samples).
    pub fn multiplier(&self, threshold_bps: f64, cap: f64) -> f64 {
        if self.history.len() < 3 { return 1.0; }
        let n = self.history.len() as f64;
        let avg = self.history.iter().sum::<f64>() / n;
        if avg <= 0.0 { return 1.0; }
        let variance = self.history.iter()
            .map(|&v| { let d = v - avg; d * d })
            .sum::<f64>() / n;
        let vol_bps = variance.sqrt() / avg * 10000.0;
        if vol_bps < threshold_bps { 1.0 }
        else { (1.0 + (vol_bps - threshold_bps) / threshold_bps).min(cap) }
    }

    /// Whether we have enough data for a meaningful multiplier.
    pub fn ready(&self) -> bool { self.history.len() >= 3 }
}
"""
    code = code.replace(
        "pub struct RiskEngine {\n    cfg: Config,\n}",
        vol_tracker + "\npub struct RiskEngine {\n    cfg: Config,\n}"
    )

    # 3b. Add dust_threshold_notional to position_ratio calc
    # The position_ratio uses hard_limit (based on withdrawable * hard_limit_ratio).
    # We need position_notional to only include non-dust positions.
    # AND we need to filter dust from the position itself (if this coin is dust, use 0).
    # Currently: position_notional is only for the CURRENT coin's position.
    # The dust filter applies to the total_notional calculation in engine.rs (across all coins).
    # For risk.rs, position_ratio uses only THIS coin's position_notional — if this coin is dust, ratio should be 0.
    
    old_pos_notional = """        // ── Compute position metrics ──
        let hard_limit = account.withdrawable * self.cfg.hard_limit_ratio;
        let position_notional = (position.size.abs() * mid).abs();
        let position_ratio = if hard_limit > 0.0 {
            (position_notional / hard_limit).min(1.0)
        } else {
            0.0
        };"""
    
    new_pos_notional = """        // ── Compute position metrics (with dust filtering) ──
        let hard_limit = account.withdrawable * self.cfg.hard_limit_ratio;
        let raw_notional = (position.size.abs() * mid).abs();
        // Dust filter: positions below threshold don't participate in risk decisions
        let position_notional = if raw_notional < self.cfg.dust_threshold_notional { 0.0 } else { raw_notional };
        let position_ratio = if hard_limit > 0.0 {
            (position_notional / hard_limit).min(1.0)
        } else {
            0.0
        };"""
    
    code = code.replace(old_pos_notional, new_pos_notional)

    # 3c. Add dust_filtered_total_notional method
    dust_method = """
    /// Total portfolio notional excluding dust positions (< dust_threshold).
    /// Used by engine for portfolio hard-limit checks to prevent noise from
    /// tiny positions corrupting the risk assessment.
    pub fn dust_filtered_total_notional(&self, account: &AccountState, mid_prices: &std::collections::HashMap<String, f64>) -> f64 {
        account.positions.iter()
            .map(|p| {
                let mid = mid_prices.get(&p.coin).copied().unwrap_or(p.entry_px);
                let notional = (p.size.abs() * mid).abs();
                if notional < self.cfg.dust_threshold_notional { 0.0 } else { notional }
            })
            .sum()
    }

    /// Portfolio under limit using dust-filtered notional.
    pub fn portfolio_ok_with_dust_filter(&self, account: &AccountState, mid_prices: &std::collections::HashMap<String, f64>) -> bool {
        let total = self.dust_filtered_total_notional(account, mid_prices);
        self.portfolio_under_limit(total, account.equity)
    }"""

    code = code.replace(
        "    /// Total portfolio notional vs equity hard limit.\n    /// Prevents placing new orders when overall leverage is too high.\n    pub fn portfolio_under_limit",
        dust_method + "\n\n    /// Total portfolio notional vs equity hard limit.\n    /// Prevents placing new orders when overall leverage is too high.\n    pub fn portfolio_under_limit"
    )

    with open(path, 'w') as f: f.write(code)
    print("[OK] risk.rs patched")

# ============================================================
# FILE 4: engine.rs — Kill Switch + PostUnwind Cooldown + VolTracker
# ============================================================
def patch_engine(path):
    with open(path) as f: code = f.read()

    # 4a. Add vol_tracker + mid_prices tracking after level_managers init
    old_init = """    // Active order tracking: (coin, oid)
    // Multi-level order tracking: coin → LevelManager
    let mut level_managers: std::collections::HashMap<String, crate::levels::LevelManager> =
        std::collections::HashMap::new();

    tracing::info!("engine started");"""
    
    new_init = """    // Active order tracking: coin → LevelManager (multi-level quoting)
    let mut level_managers: std::collections::HashMap<String, crate::levels::LevelManager> =
        std::collections::HashMap::new();

    // Volatility tracker: rolling mid-price history for vol-adaptive spread
    let mut vol_tracker = crate::risk::VolTracker::new(cfg.vol_lookback_cycles as usize);

    // Per-coin mid prices for dust-filtered portfolio check
    let mut coin_mids: std::collections::HashMap<String, f64> = std::collections::HashMap::new();

    tracing::info!("engine started (v3: killswitch={}, post_unwind_cooldown={}s, vol_cap={}x, dust=${})",
        cfg.killswitch_min_wd, cfg.post_unwind_cooldown_secs, cfg.vol_multiplier_cap, cfg.dust_threshold_notional);"""
    
    code = code.replace(old_init, new_init)

    # 4b. Add Kill Switch check right after state fetch, before per-coin loop
    ks_check = """
        // ── 0. Kill Switch: global stop-loss ──
        if account.withdrawable < cfg.killswitch_min_wd && account.withdrawable > 0.0 {
            tracing::error!(
                wd = account.withdrawable,
                min = cfg.killswitch_min_wd,
                equity = account.equity,
                "KILL_SWITCH TRIGGERED — emergency liquidation"
            );
            // Cancel all orders for all coins
            for coin in &cfg.coins {
                let _ = executor.cancel_all_for_coin(coin).await;
            }
            // IOC liquidate all positions
            let _ = emergency_portfolio_shed(&mut executor, &account, &cfg).await;
            tracing::error!("KILL_SWITCH complete — engine halted");
            break; // Exit main loop
        }"""
    
    code = code.replace(
        "        // ── 2. Process each coin ──\n\n        // First-cycle grace",
        ks_check + "\n\n        // ── 2. Process each coin ──\n\n        // First-cycle grace"
    )

    # 4c. Update mid tracking + vol_tracker after computing book.mid()
    old_mid = """            let mid = book.mid().unwrap_or(0.0);
            let can_place = coin_state.can_place_orders(coin);"""
    
    new_mid = """            let mid = book.mid().unwrap_or(0.0);
            // Track mid for dust-filtered portfolio check and volatility
            if mid > 0.0 {
                coin_mids.insert(coin.clone(), mid);
                vol_tracker.push(mid);
            }
            let can_place = coin_state.can_place_orders(coin);"""
    
    code = code.replace(old_mid, new_mid)

    # 4d. Add post-unwind cooldown start when exiting Unwind → Active
    old_exit_unwind = """                    if risk.unwind_safe(risk_out.position_ratio) {
                        tracing::info!(
                            coin = %coin,
                            position_ratio = risk_out.position_ratio,
                            exit_threshold = %(cfg.passive_unwind_watermark - cfg.unwind_hysteresis),
                            "passive unwind complete → NORMAL"
                        );
                        coin_state.transition(coin, State::Active);"""
    
    new_exit_unwind = """                    if risk.unwind_safe(risk_out.position_ratio) {
                        tracing::info!(
                            coin = %coin,
                            position_ratio = risk_out.position_ratio,
                            exit_threshold = %(cfg.passive_unwind_watermark - cfg.unwind_hysteresis),
                            "passive unwind complete → NORMAL"
                        );
                        coin_state.transition(coin, State::Active);
                        // Start post-unwind cooldown (anti-ping-pong): suppress bids
                        coin_state.start_post_unwind_cooldown(coin, cfg.post_unwind_cooldown_secs);"""
    
    code = code.replace(old_exit_unwind, new_exit_unwind)

    # 4e. Update portfolio limit check to use dust-filtered notional
    old_portfolio = """            // ── Portfolio limit pre-calc (needed by state handler) ──
            let total_notional: f64 = account
                .positions
                .iter()
                .map(|p| (p.size.abs() * p.entry_px).abs())
                .sum();
            let portfolio_ok = risk.portfolio_under_limit(total_notional, account.equity);"""
    
    new_portfolio = """            // ── Portfolio limit pre-calc (dust-filtered, needed by state handler) ──
            let total_notional = risk.dust_filtered_total_notional(&account, &coin_mids);
            let portfolio_ok = risk.portfolio_under_limit(total_notional, account.equity);"""
    
    code = code.replace(old_portfolio, new_portfolio)

    # 4f. Post-unwind cooldown: suppress bid orders with 3x spread in order placement section
    old_orders = """                let is_unwind = coin_state.is_unwind(coin);
                let coin_tick = cfg.tick_for(coin);
                let freeze_buy = is_unwind && pos.size > 0.0 || (!portfolio_ok && !is_first_cycle);
                let freeze_sell = is_unwind && pos.size < 0.0;"""
    
    new_orders = """                let is_unwind = coin_state.is_unwind(coin);
                let post_unwind_cool = coin_state.is_post_unwind_cooldown(coin);
                let coin_tick = cfg.tick_for(coin);
                // Post-unwind cooldown: suppress buy orders (anti-ping-pong, 60s ask-only)
                let freeze_buy = is_unwind && pos.size > 0.0
                    || post_unwind_cool
                    || (!portfolio_ok && !is_first_cycle);
                let freeze_sell = is_unwind && pos.size < 0.0;"""
    
    code = code.replace(old_orders, new_orders)

    # 4g. Post-unwind cooldown already handled by freeze_buy (set to true in step 4f).
    # No additional batch-level changes needed — buy loop is simply skipped.
    # The trace log records "post-unwind cooldown started — bids suppressed"

    # 4i. Add KILL_SWITCH state handling (just skip — we break the loop before here)
    old_state_match = """            // ── 4. Per-coin state transitions ──
            match current_state {"""
    
    new_state_match = """            // ── 4. Per-coin state transitions ──
            if current_state == State::KillSwitch {
                tracing::warn!(coin = %coin, "kill switch active, skipping");
                continue;
            }
            match current_state {"""
    
    code = code.replace(old_state_match, new_state_match)

    with open(path, 'w') as f: f.write(code)
    print("[OK] engine.rs patched")

# ============================================================
# Main
# ============================================================
if __name__ == "__main__":
    base = sys.argv[1] if len(sys.argv) > 1 else "/tmp/growth-engine/src"
    patch_config(f"{base}/config.rs")
    patch_state(f"{base}/state.rs")
    patch_risk(f"{base}/risk.rs")
    patch_engine(f"{base}/engine.rs")
    print("\n=== ALL PATCHES APPLIED ===")
    print("Files modified: config.rs, state.rs, risk.rs, engine.rs")
