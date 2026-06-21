//! Risk Engine — cubic skewing, asymmetric quantity control, adaptive IOC shedding.
//!
//! Four-layer architecture for Growth Mode narrow-spread (4-6 bps) environments:
//!
//! **Layer 0** — Cubic³ price skew with **Tick Discretization**.
//!   Converts BPS skew → discrete tick steps to prevent micro-skews from being
//!   rounded away on low-price coins (e.g. PUMP tick=$0.000001 ≈ 2-3 bps).
//!   `skew_bps = MAX_SKEW_BPS × (position_ratio)³` → `skew_ticks = floor(raw / tick_size)`.
//!
//! **Layer 1** — Quadratic² asymmetric quantity + **Min Lot Guard**.
//!   `qty_scale = 1 - position_ratio²` and clamps to min_order_size or zero.
//!   Prevents "ghost orders" at 85%+ position: if scaled size < min, zero out.
//!
//! **Layer 2** — **Adaptive IOC shedding** at 90% position limit.
//!   Single IOC is not enough — loops with 500ms cooldown, re-fetches L2,
//!   and continues shedding until position drops below safe re-entry (70%).
//!
//! **Layer 3** — Cycle sleep 3-5s: prevents CloudFront 429 rate limiting.

use crate::config::Config;
use crate::types::{AccountState, L2Book};

/// Result of risk evaluation — pricing and quantity adjustments.
#[derive(Debug, Clone)]
pub struct RiskOutput {
    /// Adjusted bid price (after tick-discretized skew)
    pub bid_px: f64,
    /// Adjusted ask price (after tick-discretized skew)
    pub ask_px: f64,
    /// Adjusted buy order size (min-lot guarded)
    pub buy_sz: f64,
    /// Adjusted sell order size (min-lot guarded)
    pub sell_sz: f64,
    /// Current position ratio [0, 1]
    pub position_ratio: f64,
    /// Whether IOC shedding is required (triggers at shed_trigger, e.g. 90%)
    pub should_shed: bool,
    /// Shed direction: "BUY" = shed short, "SELL" = shed long
    pub shed_side: Option<String>,
    /// Shed size in notional
    pub shed_size: f64,
    /// Gross spread in bps
    pub gross_spread_bps: f64,
    /// Effective spread after skew in bps
    pub net_spread_bps: f64,
    /// Current skew in bps (applied)
    pub skew_bps: f64,
    /// Number of tick steps the skew moved (for logging)
    pub skew_ticks: i64,
}

pub struct RiskEngine {
    cfg: Config,
}

impl RiskEngine {
    /// Initialize the risk engine with pricing/quantity/position parameters.
    pub fn new(cfg: Config) -> Self {
        Self { cfg }
    }

    /// Main risk assessment: takes L2 book + account state → RiskOutput.
    ///
    /// This is the heart of the engine. Called every cycle (3-5s).
    pub fn assess(
        &self,
        book: &L2Book,
        account: &AccountState,
        coin: &str,
    ) -> Option<RiskOutput> {
        let best_bid = book.best_bid()?;
        let best_ask = book.best_ask()?;
        let mid = (best_bid + best_ask) / 2.0;

        // Find position for this coin
        let position = account
            .positions
            .iter()
            .find(|p| p.coin == coin)
            .cloned()
            .unwrap_or_default();

        // ── Compute position metrics ──
        // If withdrawable is negative (leverage), use equity as fallback denominator
        // so position_ratio reflects true risk. Otherwise wd≤0 → ratio=0 → engine
        // thinks flat → never triggers unwind/shed on existing positions.
        let hard_limit = if account.withdrawable > 0.0 {
            account.withdrawable * self.cfg.hard_limit_ratio
        } else {
            (account.equity * self.cfg.hard_limit_ratio).max(1.0)
        };
        let position_notional = (position.size.abs() * mid).abs();
        let position_ratio = (position_notional / hard_limit).min(1.0);

        let gross_spread_bps = (best_ask - best_bid) / mid * 10000.0;

        // ── Layer 0: Cubic³ skew → Tick Discretization ──
        let skew_bps = self.cubic_skew(position_ratio);
        // Cap skew at 85% of gross spread to keep net_spread always positive
        let skew_bps = skew_bps.min(gross_spread_bps * 0.85);
        let net_spread_bps = gross_spread_bps - skew_bps;

        // Per-coin tick size
        let tick = self.cfg.tick_for(coin);

        // Apply tick-discretized skew: convert bps → discrete tick steps
        let (bid_px, ask_px, skew_ticks) = if position.size > 0.0 {
            // LONG — push both prices DOWN (discourage more LONG, encourage SELL)
            let (px, ticks) = self.tick_discretized_skew(best_bid, skew_bps, tick);
            (px, best_ask - (ticks as f64 * tick), ticks)
        } else if position.size < 0.0 {
            // SHORT — push both prices UP (discourage more SHORT, encourage BUY)
            let (px, ticks) = self.tick_discretized_skew(best_ask, -skew_bps, tick);
            (best_bid + (ticks as f64 * tick), px, ticks)
        } else {
            (best_bid, best_ask, 0)
        };

        // ── Post-Only safe clamping: never cross the book ──
        // In 1-tick spread markets, any tick skew would cross → keep safe
        let safe_bid = bid_px.min(best_ask - tick * 0.5);
        let safe_ask = ask_px.max(best_bid + tick * 0.5);
        // Only apply clamping if it does not reverse the spread
        let (bid_px, ask_px) = if safe_bid < safe_ask {
            (safe_bid, safe_ask)
        } else {
            (best_bid, best_ask)
        };

        // ── Path C: Widen by base_spread_ticks (retreat from 1-tick knife fight) ──
        let extra = self.cfg.base_spread_ticks as f64 * tick;
        let bid_px = bid_px - extra;
        let ask_px = ask_px + extra;

        // ── Layer 1: Quadratic² asymmetric quantity → Min Lot Guard ──
        let base_sz = self.cfg.base_order_notional / mid;
        let (buy_sz, sell_sz) = self.asymmetric_qty_guarded(base_sz, position.size, position_ratio);
        tracing::debug!(
            coin = %coin,
            base_sz,
            pos_size = position.size,
            pos_ratio = position_ratio,
            buy_sz,
            sell_sz,
            mid,
            hard_limit,
            hard_limit_ratio = self.cfg.hard_limit_ratio,
            "risk assess qty debug"
        );

        // ── Layer 2: IOC shedding check ──
        let (should_shed, shed_side, shed_size) =
            self.shed_check(position_ratio, position.size, mid);

        Some(RiskOutput {
            bid_px,
            ask_px,
            buy_sz,
            sell_sz,
            position_ratio,
            should_shed,
            shed_side,
            shed_size,
            gross_spread_bps,
            net_spread_bps,
            skew_bps,
            skew_ticks,
        })
    }

    /// Dead-zone skew: `MAX_SKEW × ((ratio - dead_zone) / (1 - dead_zone))^power`
    ///
    /// Zero skew in the dead zone (default 20%) — maximizes net spread at safe ratios.
    /// Beyond the dead zone, ramps quadratically/cubically to full max_skew at 100%.
    pub fn cubic_skew(&self, position_ratio: f64) -> f64 {
        let clamped = position_ratio.clamp(0.0, 1.0);
        let dz = self.cfg.skew_safe_zone;
        if clamped <= dz {
            return 0.0;
        }
        // Remap: [dz, 1.0] → [0, 1] then apply power
        let effective = (clamped - dz) / (1.0 - dz);
        self.cfg.max_skew_bps * effective.powf(self.cfg.skew_power)
    }

    /// Tick-discretized skew: converts BPS → whole tick steps.
    fn tick_discretized_skew(&self, base_px: f64, skew_bps: f64, tick: f64) -> (f64, i64) {
        let raw_skew_price = base_px * (skew_bps.abs() / 10000.0);
        let skew_ticks = (raw_skew_price / tick).round() as i64;

        if skew_ticks == 0 {
            return (base_px, 0);
        }

        let adjusted = if skew_bps > 0.0 {
            // Positive skew: push price DOWN (discourage further accumulation)
            base_px - (skew_ticks as f64 * tick)
        } else {
            // Negative skew: push price UP
            base_px + (skew_ticks as f64 * tick)
        };

        // Safety: never cross the market
        let adjusted = adjusted.max(base_px * 0.5).min(base_px * 1.5);

        (adjusted, skew_ticks)
    }

    /// Asymmetric quantity with min-lot guard.
    ///
    /// At high position (85%+), the quadratic decay can produce order sizes
    /// below the exchange's minimum. Instead of sending a rejectable order,
    /// zero out the size entirely — preventing ghost orders.
    fn asymmetric_qty_guarded(
        &self,
        base_qty: f64,
        position_size: f64,
        position_ratio: f64,
    ) -> (f64, f64) {
        let scale = 1.0 - position_ratio.powf(self.cfg.qty_power);
        let scale = scale.max(0.0);

        let (raw_buy, raw_sell) = if position_size > 0.0 {
            (base_qty * scale, base_qty)
        } else if position_size < 0.0 {
            (base_qty, base_qty * scale)
        } else {
            (base_qty, base_qty)
        };

        // Min-lot guard: if scaled below min, zero out
        let buy_sz = if raw_buy > 0.0 && raw_buy < self.cfg.min_order_size {
            0.0
        } else {
            raw_buy
        };
        let sell_sz = if raw_sell > 0.0 && raw_sell < self.cfg.min_order_size {
            0.0
        } else {
            raw_sell
        };

        (buy_sz, sell_sz)
    }

    /// Check if IOC shedding is required.
    ///
    /// Triggers at `shed_trigger` (default 90%). Sheds `shed_fraction` of
    /// current position as IOC. The engine loop re-checks after 500ms and
    /// continues shedding until position drops below `shed_safe_reentry`.
    pub fn shed_check(
        &self,
        position_ratio: f64,
        position_size: f64,
        mid: f64,
    ) -> (bool, Option<String>, f64) {
        if position_ratio < self.cfg.shed_trigger {
            return (false, None, 0.0);
        }

        let shed_notional = position_size.abs() * mid * self.cfg.shed_fraction;
        let side = if position_size > 0.0 {
            "SELL"
        } else {
            "BUY"
        };

        (true, Some(side.to_string()), shed_notional)
    }

    /// Has position dropped below the safe re-entry threshold?
    /// Used by the engine's adaptive shedding loop.
    pub fn shed_safe(&self, position_ratio: f64) -> bool {
        position_ratio < self.cfg.shed_safe_reentry
    }

    /// Whether the current spread is profitable after fees.
    pub fn is_profitable_spread(&self, net_spread_bps: f64) -> bool {
        net_spread_bps > self.cfg.roundtrip_bps()
    }

    /// Should we enter passive unwind?
    /// Triggers when position_ratio >= watermark (default 40%).
    /// Returns (should_unwind, unwind_side) where side = "SELL" for LONG, "BUY" for SHORT.
    pub fn unwind_check(&self, position_ratio: f64, position_size: f64) -> (bool, Option<String>) {
        if position_ratio < self.cfg.passive_unwind_watermark {
            return (false, None);
        }
        let side = if position_size > 0.0 {
            "SELL"
        } else if position_size < 0.0 {
            "BUY"
        } else {
            return (false, None);
        };
        (true, Some(side.to_string()))
    }

    /// Is position back in the safe zone?
    ///
    /// Uses hysteresis: exit UNWIND at `watermark − hysteresis`.
    /// Example: watermark=40%, hysteresis=20% → exit below 20%.
    /// This prevents state flickering when position oscillates near 40%.
    pub fn unwind_safe(&self, position_ratio: f64) -> bool {
        let exit_threshold = (self.cfg.passive_unwind_watermark - self.cfg.unwind_hysteresis).max(0.0);
        position_ratio < exit_threshold
    }

    /// Safety check: do we have enough reserve?
    pub fn has_reserve(&self, withdrawable: f64) -> bool {
        withdrawable >= self.cfg.min_reserve
    }

    /// Total portfolio notional vs equity hard limit.
    /// Prevents placing new orders when overall leverage is too high.
    pub fn portfolio_under_limit(&self, total_notional: f64, equity: f64) -> bool {
        let limit = equity * self.cfg.hard_limit_ratio;
        total_notional < limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            skew_power: 3.0,
            max_skew_bps: 150.0,
            skew_safe_zone: 0.75,
            qty_power: 2.0,
            base_order_notional: 1.0,
            hard_limit_ratio: 0.60,
            shed_trigger: 0.90,
            shed_fraction: 0.50,
            shed_safe_reentry: 0.70,
            tick_size: 0.000001,
            min_order_size: 1.0,
            ..Config::default()
        }
    }

    #[test]
    fn test_cubic_skew_at_safe_zone() {
        let engine = RiskEngine::new(test_config());
        let skew = engine.cubic_skew(0.75);
        assert!((skew - 1.477).abs() < 0.01, "got {skew}");
    }

    #[test]
    fn test_cubic_skew_at_50_percent() {
        let engine = RiskEngine::new(test_config());
        let skew = engine.cubic_skew(0.50);
        assert!((skew - 0.4375).abs() < 0.001, "got {skew}");
    }

    #[test]
    fn test_tick_discretization_rounds_down_minor_skew() {
        let engine = RiskEngine::new(test_config());
        let tick = 0.000001;
        // 0.06 bps skew on $0.08 price → 0.0000048 → floor = 0 ticks
        let (px, ticks) = engine.tick_discretized_skew(0.08, 0.06, tick);
        assert_eq!(ticks, 0, "micro skew should be rounded to 0 ticks");
        assert!((px - 0.08).abs() < 1e-9, "price unchanged");
    }

    #[test]
    fn test_tick_discretization_applies_visible_skew() {
        let engine = RiskEngine::new(test_config());
        let tick = 0.000001;
        // 2.0 bps skew on $0.08 → 0.00016 → floor = 160 ticks = $0.00016
        let (px, ticks) = engine.tick_discretized_skew(0.08, 2.0, tick);
        assert!(ticks > 0, "visible skew should create non-zero ticks");
        assert!((px - 0.07984).abs() < 1e-6, "price should drop by ticks");
    }

    #[test]
    fn test_min_lot_guard_zeros_below_threshold() {
        let engine = RiskEngine::new(test_config());
        // base_qty=1.0, position_ratio=0.9, scale=1-0.81=0.19 → 0.19 < min_order_size(1.0)
        let (buy, sell) = engine.asymmetric_qty_guarded(1.0, 100.0, 0.9);
        assert_eq!(buy, 0.0, "below-min order should be zeroed");
        assert!(sell > 0.0, "full side still active");
    }

    #[test]
    fn test_asymmetric_qty_flat() {
        let engine = RiskEngine::new(test_config());
        let (buy, sell) = engine.asymmetric_qty_guarded(10.0, 0.0, 0.0);
        assert!((buy - 10.0).abs() < 0.01);
        assert!((sell - 10.0).abs() < 0.01);
    }

    #[test]
    fn test_asymmetric_qty_long_half() {
        let engine = RiskEngine::new(test_config());
        let (buy, sell) = engine.asymmetric_qty_guarded(10.0, 100.0, 0.5);
        assert!(buy < 10.0, "buy should be reduced");
        assert!((sell - 10.0).abs() < 0.01, "sell should be full");
    }
}
