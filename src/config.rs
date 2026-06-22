//! Configuration — Growth Mode specific parameters.

use anyhow::Result;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Top-level configuration, loaded from .env or command-line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// HL mainnet API URL
    pub api_url: String,
    /// HL WebSocket URL
    pub ws_url: String,
    /// EOA wallet address
    pub address: String,
    /// Private key (hex, 0x-prefixed)
    pub private_key: String,
    /// Coins to market-make (PUMP, FARTCOIN)
    pub coins: Vec<String>,
    /// Whether Growth Mode fee rates apply (hyna deployer, feeScale=0.1111)
    pub growth_mode: bool,

    // ── Pricing ──
    /// Cubic skew power (3.0 = cubic, 2.0 = quadratic)
    pub skew_power: f64,
    /// Maximum skew in basis points (reserve 2.6 bps for profitability)
    pub max_skew_bps: f64,
    /// Position ratio below which skew is effectively zero
    pub skew_safe_zone: f64,

    // ── Quantity ──
    /// Quadratic decay power for asymmetric qty control
    pub qty_power: f64,
    /// Base order size in notional USD
    pub base_order_notional: f64,

    // ── Risk ──
    /// Hard position limit as fraction of withdrawable equity
    pub hard_limit_ratio: f64,
    /// Position ratio at which IOC shedding triggers
    pub shed_trigger: f64,
    /// Fraction of position to shed on trigger
    pub shed_fraction: f64,
    /// Position ratio below which shedding stops (safe re-entry zone)
    pub shed_safe_reentry: f64,
    /// Default tick size (used when per-coin not in map)
    pub tick_size: f64,
    /// Per-coin tick size overrides (e.g. FARTCOIN → 0.00001)
    pub tick_sizes: HashMap<String, f64>,
    /// Number of ticks to widen beyond market spread (0 = market spread).
    pub base_spread_ticks: u32,
    /// Minimum order size in native units (not notional)
    pub min_order_size: f64,
    /// Min withdrawable to keep as reserve
    pub min_reserve: f64,
    /// Passive unwind trigger (fraction of hard_limit, e.g. 0.40)
    pub passive_unwind_watermark: f64,
    /// Hysteresis buffer: exit UNWIND at watermark − hysteresis
    pub unwind_hysteresis: f64,
    /// Directional freeze cycles after a position flip (e.g. 50).
    pub freeze_cycles: u32,
    /// Minimum gross spread in ticks before gate blocks
    pub gate_block_ticks: u32,
    /// V12.1: Minimum gross bps to enter COARSE_TICK_HARVEST (1-tick fat-margin coins)
    pub coarse_tick_bps_threshold: f64,
    /// V12.1: Size reduction for COARSE_TICK_HARVEST mode (0.20 = 20% of normal)
    pub coarse_tick_size_pct: f64,
    pub tsunami_ticks: u32,
    /// V12.1: Max same-side fills in COARSE mode before stopping that side
    pub coarse_tick_max_side_fills: u32,
    /// Min safety margin bps above roundtrip cost
    pub min_margin_bps: f64,
    /// Consecutive cycles with favorable conditions before entering Active from Waiting
    pub spread_stable_cycles: u32,
    /// Force full REST position re-sync every N cycles (0 = disabled)
    pub force_resync_interval: u32,
    /// Max tick retreat attempts when Alo rejected with "would match"
    pub unwind_max_retreat_ticks: u32,

    // ── V12.2: Coarse-tick asymmetric sizing ──
    /// Position ratio threshold (abs) where asymmetric sizing begins. Default 0.05 (5%).
    pub coarse_pos_ratio_aggressive: f64,
    /// Position ratio threshold (abs) where adverse side is killed completely. Default 0.12 (12%).
    pub coarse_pos_ratio_hard_kill: f64,
    /// Size boost factor for the favorable (unwind) side when asymmetric sizing activates.
    pub coarse_unwind_size_boost: f64,

    // ── Timing ──
    /// Engine loop cycle min sleep (seconds)
    pub cycle_sleep_min: f64,
    /// Engine loop cycle max sleep (seconds)
    pub cycle_sleep_max: f64,
    /// Jitter range as fraction of sleep
    pub cycle_jitter: f64,
    /// WebSocket reconnect delay
    pub ws_reconnect_delay: Duration,

    // ── Fee schedule ──
    /// Maker fee in bps (Growth Mode: 0.17, Standard: 1.5)
    pub maker_fee_bps: f64,
    /// Taker fee in bps (Growth Mode: 0.50, Standard: 4.5)
    pub taker_fee_bps: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_url: "https://api.hyperliquid.xyz".into(),
            ws_url: "wss://api.hyperliquid.xyz/ws".into(),
            address: String::new(),
            private_key: String::new(),
            coins: vec!["HMSTR".into()],
            growth_mode: true,

            skew_power: 2.5,
            max_skew_bps: 150.0,
            skew_safe_zone: 0.20,
            qty_power: 2.0,
            base_order_notional: 10.0,

            hard_limit_ratio: 0.60,
            shed_trigger: 0.90,
            shed_fraction: 0.50,
            shed_safe_reentry: 0.70,
            tick_size: 0.000001,
            base_spread_ticks: 1,
            tick_sizes: {
                let mut m = HashMap::new();
                m.insert("PUMP".into(), 0.000001);
                m.insert("FARTCOIN".into(), 0.00001);
                m.insert("HMSTR".into(), 0.000001);
                m
            },
            min_order_size: 1.0,
            min_reserve: 5.0,
            passive_unwind_watermark: 0.40,
            unwind_hysteresis: 0.20,
            unwind_max_retreat_ticks: 3,
            freeze_cycles: 50,
            gate_block_ticks: 0,
            tsunami_ticks: 1,
            coarse_tick_bps_threshold: 15.0,
            coarse_tick_size_pct: 0.20,
            coarse_tick_max_side_fills: 3,
            min_margin_bps: 2.0,
            spread_stable_cycles: 3,
            force_resync_interval: 20,

            // V12.2: asymmetric sizing for coarse-tick markets
            coarse_pos_ratio_aggressive: 0.05,
            coarse_pos_ratio_hard_kill: 0.12,
            coarse_unwind_size_boost: 1.2,

            cycle_sleep_min: 3.0,
            cycle_sleep_max: 5.0,
            cycle_jitter: 0.2,
            ws_reconnect_delay: Duration::from_secs(2),

            maker_fee_bps: 0.17,
            taker_fee_bps: 0.50,
        }
    }
}

impl Config {
    /// Load from environment variables.
    pub fn from_env() -> Result<Self> {
        let _ = dotenvy::dotenv();

        let mut cfg = Self::default();

        if let Ok(v) = std::env::var("HL_API_URL") {
            cfg.api_url = v;
        }
        if let Ok(v) = std::env::var("HL_WS_URL") {
            cfg.ws_url = v;
        }
        cfg.address =
            std::env::var("HL_ADDRESS").unwrap_or_else(|_| "0xEC1FbcaD".into());
        cfg.private_key = std::env::var("HL_PRIVATE_KEY")
            .map_err(|_| anyhow::anyhow!("HL_PRIVATE_KEY must be set"))?;

        if let Ok(v) = std::env::var("HL_COINS") {
            cfg.coins = v.split(',').map(|s| s.trim().to_string()).collect();
        }
        if let Ok(v) = std::env::var("HL_GROWTH_MODE") {
            cfg.growth_mode = v.parse().unwrap_or(true);
        }
        if let Ok(v) = std::env::var("HL_TICK_SIZE") {
            cfg.tick_size = v.parse().unwrap_or(0.000001);
        }
        if let Ok(v) = std::env::var("HL_MIN_ORDER_SIZE") {
            cfg.min_order_size = v.parse().unwrap_or(1.0);
        }
        if let Ok(v) = std::env::var("HL_BASE_ORDER_NOTIONAL") {
            cfg.base_order_notional = v.parse().unwrap_or(10.0);
        }
        if let Ok(v) = std::env::var("HL_PASSIVE_UNWIND_WATERMARK") {
            cfg.passive_unwind_watermark = v.parse().unwrap_or(0.40);
        }
        if let Ok(v) = std::env::var("HL_UNWIND_HYSTERESIS") {
            cfg.unwind_hysteresis = v.parse().unwrap_or(0.20);
        }
        if let Ok(v) = std::env::var("HL_TICK_SIZES") {
            for pair in v.split(',') {
                let parts: Vec<&str> = pair.splitn(2, '=').collect();
                if parts.len() == 2 {
                    let coin = parts[0].trim().to_string();
                    let tick: f64 = parts[1].trim().parse().unwrap_or(0.000001);
                    cfg.tick_sizes.insert(coin, tick);
                }
            }
        }

        if cfg.growth_mode {
            cfg.maker_fee_bps = 0.17;
            cfg.taker_fee_bps = 0.50;
        } else {
            cfg.maker_fee_bps = 1.5;
            cfg.taker_fee_bps = 4.5;
        }

        Ok(cfg)
    }

    /// Round-trip cost in basis points.
    pub fn roundtrip_bps(&self) -> f64 {
        self.maker_fee_bps + self.taker_fee_bps
    }

    /// Get per-coin tick size (falls back to default).
    pub fn tick_for(&self, coin: &str) -> f64 {
        self.tick_sizes.get(coin).copied().unwrap_or(self.tick_size)
    }
}
