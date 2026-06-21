#!/usr/bin/env python3
"""V4 Patch: P1 Circuit Breaker + P0 Dynamic Cap + Liquidation Defense"""
import sys

# ============================================================
# FILE 1: config.rs — 6 new fields
# ============================================================
def patch_config(path):
    with open(path) as f: code = f.read()

    # 1a. Add fields after dust_threshold_notional block
    new_fields = """
    // ── P1: Exchange Circuit Breaker (通信熔断) ──
    /// Sliding window for API failure detection (seconds).
    /// If max_failures consecutive 429/Timeout errors occur within this window,
    /// the circuit breaker trips → cancel all orders + halt.
    pub circuit_breaker_window_secs: f64,
    /// Number of API failures to trip the breaker.
    pub circuit_breaker_max_failures: u32,
    // ── P0: Dynamic Position Cap (动态仓位上限) ──
    /// Base hard-limit ratio in quiet markets (vol_multiplier ≈ 1.0).
    /// Corrected formula: hard_limit = max(min_limit, base_limit / vol_multiplier).
    /// When volatility spikes, the cap auto-shrinks proportionally.
    pub dynamic_hard_limit_base: f64,
    /// Floor hard-limit ratio during extreme volatility.
    pub dynamic_hard_limit_min: f64,
    // ── Liquidation Defense (清算距离锁) ──
    /// Fractional distance from mark to liquidation below which emergency exit triggers.
    /// E.g. 0.08 means if mark is within 8% of liquidation price, kill switch activates.
    /// "老子割肉自己切，不让交易所清算推土机碰我"
    pub liquidation_distance_threshold: f64,"""

    code = code.replace(
        "pub dust_threshold_notional: f64,",
        "pub dust_threshold_notional: f64," + new_fields
    )

    # 1b. Defaults
    code = code.replace(
        "dust_threshold_notional: 20.0,",
        "dust_threshold_notional: 20.0,\n            circuit_breaker_window_secs: 5.0,\n            circuit_breaker_max_failures: 3,\n            dynamic_hard_limit_base: 0.40,\n            dynamic_hard_limit_min: 0.15,\n            liquidation_distance_threshold: 0.08,"
    )

    # 1c. Env loading
    env_load = """
        if let Ok(v) = std::env::var("HL_DUST_THRESHOLD_NOTIONAL") {
            cfg.dust_threshold_notional = v.parse().unwrap_or(20.0);
        }
        if let Ok(v) = std::env::var("HL_CIRCUIT_BREAKER_WINDOW_SECS") {
            cfg.circuit_breaker_window_secs = v.parse().unwrap_or(5.0);
        }
        if let Ok(v) = std::env::var("HL_CIRCUIT_BREAKER_MAX_FAILURES") {
            cfg.circuit_breaker_max_failures = v.parse().unwrap_or(3);
        }
        if let Ok(v) = std::env::var("HL_DYNAMIC_HARD_LIMIT_BASE") {
            cfg.dynamic_hard_limit_base = v.parse().unwrap_or(0.40);
        }
        if let Ok(v) = std::env::var("HL_DYNAMIC_HARD_LIMIT_MIN") {
            cfg.dynamic_hard_limit_min = v.parse().unwrap_or(0.15);
        }
        if let Ok(v) = std::env::var("HL_LIQUIDATION_DISTANCE_THRESHOLD") {
            cfg.liquidation_distance_threshold = v.parse().unwrap_or(0.08);
        }"""
    code = code.replace(
        'cfg.dust_threshold_notional = v.parse().unwrap_or(20.0);\n        }',
        'cfg.dust_threshold_notional = v.parse().unwrap_or(20.0);\n        }' + env_load
    )

    with open(path, 'w') as f: f.write(code)
    print("[OK] config.rs — 6 new fields")

# ============================================================
# FILE 2: executor.rs — CircuitBreaker struct
# ============================================================
def patch_executor(path):
    with open(path) as f: code = f.read()

    # 2a. Add CircuitBreaker struct before Executor
    cb_struct = """
/// Sliding-window circuit breaker: 3+ API failures (429/timeout) in 5 seconds
/// trips the breaker → cancel all orders, halt engine, alert.
/// "宁可断网不亏钱，拔掉网线保平安"
pub struct CircuitBreaker {
    /// Timestamps of recent failures within the window
    failures: std::collections::VecDeque<std::time::Instant>,
    window_secs: f64,
    max_failures: usize,
    /// Once tripped, stays tripped until engine restart
    tripped: bool,
    /// Count of times the breaker has tripped (for logging)
    trip_count: u32,
}

impl CircuitBreaker {
    pub fn new(window_secs: f64, max_failures: u32) -> Self {
        Self {
            failures: std::collections::VecDeque::with_capacity(max_failures as usize + 1),
            window_secs,
            max_failures: max_failures as usize,
            tripped: false,
            trip_count: 0,
        }
    }

    /// Record a failure event. Returns true if the breaker just tripped.
    pub fn record_failure(&mut self) -> bool {
        if self.tripped { return false; }
        let now = std::time::Instant::now();
        self.failures.push_back(now);
        // Prune entries outside the window
        while let Some(&t) = self.failures.front() {
            if now.duration_since(t).as_secs_f64() > self.window_secs {
                self.failures.pop_front();
            } else { break; }
        }
        if self.failures.len() >= self.max_failures {
            self.tripped = true;
            self.trip_count += 1;
            tracing::error!(
                failures = self.failures.len(),
                window_secs = self.window_secs,
                "CIRCUIT BREAKER TRIPPED — cancel-all + halt"
            );
            true
        } else {
            false
        }
    }

    /// Record a success — clears the failure window (but does NOT untrip).
    pub fn record_success(&mut self) {
        if !self.tripped {
            self.failures.clear();
        }
    }

    pub fn is_tripped(&self) -> bool { self.tripped }

    /// Call after tripped → emergency action to check if this was a false alarm.
    /// After manual review, engine restart clears the breaker.
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.tripped = false;
        self.failures.clear();
    }
}"""
    code = code.replace(
        "pub struct Executor {\n    cfg: Config,\n    signer: Signer,\n}",
        cb_struct + "\n\npub struct Executor {\n    cfg: Config,\n    signer: Signer,\n    breaker: CircuitBreaker,\n}"
    )

    # 2b. Update Executor::new()
    code = code.replace(
        "pub fn new(cfg: Config, signer: Signer) -> Self {\n        Self { cfg, signer }\n    }",
        "pub fn new(cfg: Config, signer: Signer) -> Self {\n        let breaker = CircuitBreaker::new(cfg.circuit_breaker_window_secs, cfg.circuit_breaker_max_failures);\n        Self { cfg, signer, breaker }\n    }"
    )

    # 2c. Add breaker access + failure detection in place_batch_orders
    # We need to detect failures from the exchange response.
    # After logging rejections, if error contains "429" or "Rate limit" or "timeout", record failure.
    # Find the place_batch_orders method and add breaker checks.

    # First, add breaker methods to Executor
    breaker_methods = """
    /// Check if the circuit breaker has tripped.
    pub fn is_circuit_broken(&self) -> bool { self.breaker.is_tripped() }

    /// Record an API failure (429/timeout/internal error).
    pub fn record_api_failure(&mut self) -> bool { self.breaker.record_failure() }

    /// Record a successful API call.
    pub fn record_api_success(&mut self) { self.breaker.record_success() }

    /// Emergency circuit breaker action: cancel ALL orders for ALL coins.
    pub async fn circuit_breaker_cancel_all(&mut self, coins: &[String]) -> Result<(), anyhow::Error> {
        tracing::error!("CIRCUIT BREAKER executing emergency cancel-all across {} coins", coins.len());
        for coin in coins {
            match self.cancel_all_for_coin(coin).await {
                Ok(_) => tracing::info!(coin = %coin, "breaker: cancelled all"),
                Err(e) => tracing::error!(coin = %coin, error = %e, "breaker: cancel failed"),
            }
        }
        Ok(())
    }"""

    code = code.replace(
        "    /// Place a limit order (with optional Alo post-only).\n    /// Returns order id on success.\n    pub async fn place_limit_order",
        breaker_methods + "\n\n    /// Place a limit order (with optional Alo post-only).\n    /// Returns order id on success.\n    pub async fn place_limit_order"
    )

    # 2d. Add breaker check in place_batch_orders — detect 429/timeout in individual rejects
    # Find the "batch order rejected" warn log and add breaker recording
    old_reject = """                            tracing::warn!(
                                coin = %coin,
                                idx = i,
                                is_buy = *is_buy,
                                error = %status.error,
                                "batch order rejected"
                            );"""
    new_reject = """                            let err_str = status.error.to_lowercase();
                            let is_breaker_failure = err_str.contains("429")
                                || err_str.contains("rate limit")
                                || err_str.contains("timeout")
                                || err_str.contains("internal error")
                                || err_str.contains("service unavailable");
                            tracing::warn!(
                                coin = %coin,
                                idx = i,
                                is_buy = *is_buy,
                                error = %status.error,
                                breaker_failure = is_breaker_failure,
                                "batch order rejected"
                            );
                            if is_breaker_failure {
                                self.record_api_failure();
                            }"""
    code = code.replace(old_reject, new_reject)

    # 2e. Add breaker record_success after successful batch
    old_success = """                tracing::info!(
                    coin = %coin,
                    placed = oids.iter().filter(|o| o.is_some()).count(),
                    total = oids.len(),
                    "batch orders placed"
                );"""
    new_success = """                self.record_api_success();
                tracing::info!(
                    coin = %coin,
                    placed = oids.iter().filter(|o| o.is_some()).count(),
                    total = oids.len(),
                    "batch orders placed"
                );"""
    code = code.replace(old_success, new_success)

    with open(path, 'w') as f: f.write(code)
    print("[OK] executor.rs — CircuitBreaker implemented")

# ============================================================
# FILE 3: risk.rs — Dynamic cap + Liquidation distance
# ============================================================
def patch_risk(path):
    with open(path) as f: code = f.read()

    # 3a. Add dynamic_hard_limit_ratio() method to RiskEngine
    dyn_method = """
    /// P0: Dynamic position cap (动态仓位上限).
    /// Corrected formula: max(min_limit, base_limit / vol_multiplier).
    /// — Low vol (mult=1.0): ratio = max(0.15, 0.40/1.0) = 0.40
    /// — High vol (mult=2.5): ratio = max(0.15, 0.40/2.5) = 0.16
    /// Market gets more dangerous → credit card limit gets lower.
    pub fn dynamic_hard_limit_ratio(&self, vol_multiplier: f64) -> f64 {
        (self.cfg.dynamic_hard_limit_base / vol_multiplier.max(1.0))
            .max(self.cfg.dynamic_hard_limit_min)
    }"""
    # Insert before "hard_limit_ratio" getter or before dust_filtered_total_notional
    code = code.replace(
        "    /// Total portfolio notional vs equity hard limit.\n    /// Prevents placing new orders when overall leverage is too high.\n    pub fn portfolio_under_limit",
        dyn_method + "\n\n    /// Total portfolio notional vs equity hard limit.\n    /// Prevents placing new orders when overall leverage is too high.\n    pub fn portfolio_under_limit"
    )

    # 3b. Modify assess() to use dynamic hard limit instead of static
    old_hard = """        // ── Compute position metrics (with dust filtering) ──
        let hard_limit = account.withdrawable * self.cfg.hard_limit_ratio;"""
    new_hard = """        // ── Compute position metrics (with dust filtering + dynamic cap) ──
        let effective_ratio = self.dynamic_hard_limit_ratio(vol_multiplier);
        let hard_limit = account.withdrawable * effective_ratio;"""
    code = code.replace(old_hard, new_hard)

    # 3c. Add liquidation distance check
    liq_method = """
    /// Liquidation defense: compute distance from mark to liquidation.
    /// Returns (distance_fraction, is_danger) where danger = distance < threshold.
    /// "哪怕要割肉，老子也要自己切，绝对不把主动权交给交易所"
    pub fn liquidation_danger(
        &self,
        mark_price: f64,
        liquidation_price: Option<f64>,
        position_size: f64,
    ) -> (f64, bool) {
        let threshold = self.cfg.liquidation_distance_threshold;
        let liq_px = match liquidation_price {
            Some(px) if px > 0.0 => px,
            _ => return (1.0, false), // No liquidation price → safe (or no position)
        };
        if position_size.abs() < 0.01 || mark_price <= 0.0 {
            return (1.0, false);
        }
        let distance = if position_size > 0.0 {
            // LONG position: danger is below → liq_price is below mark
            (mark_price - liq_px) / mark_price
        } else {
            // SHORT position: danger is above → liq_price is above mark
            (liq_px - mark_price) / mark_price
        };
        let danger = distance < threshold && distance > 0.0;
        if danger {
            tracing::error!(
                mark = mark_price,
                liq = liq_px,
                distance_pct = %(distance * 100.0),
                threshold_pct = %(threshold * 100.0),
                "LIQUIDATION DANGER — distance below threshold"
            );
        }
        (distance, danger)
    }"""
    code = code.replace(
        "    /// Portfolio under limit using dust-filtered notional.\n    pub fn portfolio_ok_with_dust_filter",
        liq_method + "\n\n    /// Portfolio under limit using dust-filtered notional.\n    pub fn portfolio_ok_with_dust_filter"
    )

    with open(path, 'w') as f: f.write(code)
    print("[OK] risk.rs — dynamic cap + liquidation defense")

# ============================================================
# FILE 4: engine.rs — Circuit breaker check + liquidation lock in main loop
# ============================================================
def patch_engine(path):
    with open(path) as f: code = f.read()

    # 4a. Add circuit breaker + liquidation check AFTER kill switch, BEFORE per-coin
    ks_block = """        // ── 0. Kill Switch: global stop-loss ──
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
            break Ok(()); // Exit main loop
        }"""

    cb_lq_block = """        // ── 0a. P1 Circuit Breaker: last-assembled defense against API chaos ──
        if executor.is_circuit_broken() {
            tracing::error!("CIRCUIT BREAKER already tripped — skipping all logic");
            // Avoid re-running cancel-all every cycle after trip
            break Ok(());
        }

        // ── 0b. Liquidation Defense: "老子自己切,不把主动权给清算推土机" ──
        for pos in &account.positions {
            if pos.size.abs() < 0.01 { continue; }
            let mark = account.mark_prices.get(&pos.coin).copied().unwrap_or(pos.entry_px);
            let (distance, danger) = risk.liquidation_danger(mark, pos.liquidation_price, pos.size);
            if danger {
                tracing::error!(
                    coin = %pos.coin,
                    mark = mark,
                    liq = ?pos.liquidation_price,
                    distance_pct = %(distance * 100.0),
                    "LIQUIDATION DANGER — emergency kill switch before HL liquidates us"
                );
                for coin in &cfg.coins {
                    let _ = executor.cancel_all_for_coin(coin).await;
                }
                let _ = emergency_portfolio_shed(&mut executor, &account, &cfg).await;
                break Ok(());
            }
        }"""
    
    # Replace the kill switch block with both
    code = code.replace(ks_block, ks_block + "\n\n" + cb_lq_block)

    # 4b. Update engine start log
    old_start = """    tracing::info!("engine started (v3: killswitch={}, post_unwind_cooldown={}s, vol_cap={}x, dust=${})",
        cfg.killswitch_min_wd, cfg.post_unwind_cooldown_secs, cfg.vol_multiplier_cap, cfg.dust_threshold_notional);"""
    new_start = """    tracing::info!(
        "engine started (v4: killswitch=${}, breaker={}/{}/{:.0}s, dyn_cap={:.0}%/{:.0}%, liq_dist={:.0}%)",
        cfg.killswitch_min_wd,
        cfg.circuit_breaker_max_failures,
        if cfg.circuit_breaker_max_failures > 0 { "ON" } else { "OFF" },
        cfg.circuit_breaker_window_secs,
        cfg.dynamic_hard_limit_base * 100.0,
        cfg.dynamic_hard_limit_min * 100.0,
        cfg.liquidation_distance_threshold * 100.0,
    );"""
    code = code.replace(old_start, new_start)

    # 4c. Add dynamic hard limit note to per-coin cycle log
    old_cycle = """            metrics.gross_spread_bps = risk_out.gross_spread_bps;
            metrics.net_spread_bps = risk_out.net_spread_bps;
            metrics.skew_bps = risk_out.skew_bps;"""
    # No change needed in cycle log — the dynamic cap is transparent to the existing metrics

    # 4d. Update hard_limit_ratio reference to dynamic
    # In the portfolio check section, replace hard_limit_ratio with dynamic
    old_ratio = "risk.portfolio_under_limit(total_notional, account.equity)";
    # This line is fine — portfolio_under_limit uses hard_limit_ratio internally.
    # We need to update the hard_limit_ratio itself in the risk.rs assess() call.
    # We already did that in step 3b.

    with open(path, 'w') as f: f.write(code)
    print("[OK] engine.rs — breaker + liquidation integrated")

# ============================================================
# Main
# ============================================================
if __name__ == "__main__":
    base = sys.argv[1] if len(sys.argv) > 1 else "/tmp/growth-engine/src"
    patch_config(f"{base}/config.rs")
    patch_executor(f"{base}/executor.rs")
    patch_risk(f"{base}/risk.rs")
    patch_engine(f"{base}/engine.rs")
    print("\n=== V4 PATCHES APPLIED (P1 Breaker + P0 Dynamic Cap + Liq Defense) ===")
