//! sniper — Toxic Flow Sniper (same-process module)
//!
//! J Directive 2026-07-12:
//!   "Sniper → growth-engine/src/sniper/ 子模块, 编译进同一二进制,
//!    共享 Executor 的 AtomicU64 Nonce, Redis 全移除 → /tmp 文件桥"
//!
//! Architecture:
//!   ToxicFlowDetector → MicroSniper → EvacuationPipeline
//!        ↑ shared Executor (nonce)     ↓
//!   /tmp/whale_sense_state.json   /tmp/sniper_state.json
//!
//! P0 design constraints:
//!   - Shared Executor instance for nonce safety
//!   - /tmp file bridge (no Redis)
//!   - 120s data TTL (consistent with whale_state pattern)
//!   - Opt-in launch: starts only when `sniper_enabled=true` in config

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::executor::Executor;
use crate::types::{self, L2Book, MarketShockSignal, SignalBus};
use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

// ── Constants ──────────────────────────────────────────────

/// File bridge (J directive: no Redis)
const WHALE_STATE_PATH: &str = "/tmp/whale_sense_state.json";
const SNIPER_STATE_PATH: &str = "/tmp/sniper_state.json";

/// Data freshness TTL (consistent with pgn_engine_v2 30s cycle + margin)
const WHALE_TTL_S: u64 = 120;
const SNIPER_STATE_TTL_S: u64 = 120;

/// Detection thresholds
const FILL_BURST_WINDOW_MS: u64 = 500;
const FILL_BURST_MIN_COUNT: usize = 5;
const FILL_BURST_SKEW: f64 = 0.8;
const SPOOF_DEPTH_DROP: f64 = 0.5; // 50% depth vanish in SPOOF_WINDOW_MS
const SPOOF_WINDOW_MS: u64 = 100;
const QUOTE_STUFFING_RATE: usize = 50; // >50 L2 updates/sec for >1s
const QUOTE_STUFFING_DURATION_S: u64 = 1;
const OBI_THRESHOLD: f64 = 0.5;
const OBI_DELTA_THRESHOLD: f64 = 0.15; // per 100ms
const PRE_POSITION_TICKS: u32 = 2;
const PRE_POSITION_TTL_MS: u64 = 100;
const OBI_CYCLE_MS: u64 = 50;

// ── Domain Types ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToxicEventKind {
    FillBurst,
    Spoof,
    QuoteStuffing,
    OIDivergence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EvacuationLevel {
    Monitor = 0,
    Alert = 1,
    Reduce = 2,
    Hedge = 3,
    FullEvac = 4,
    Panic = 5,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToxicFlowEvent {
    pub kind: ToxicEventKind,
    pub coin: String,
    pub attack_side: String, // "BUY" | "SELL"
    pub severity: u8,
    pub action: EvacuationLevel,
    pub whale_state: String,
    pub whale_score: f64,
    pub direction_bias: Option<String>, // parsed from EXECUTE_STANDARD_LONG → "LONG"
    pub ts_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhaleSnapshot {
    pub state: String,
    pub score: f64,
    pub decision: String,
    pub direction_bias: Option<String>,
    pub z_p: f64,
    pub z_oi: f64,
    pub z_cvd: f64,
    pub active_layers: u32,
    pub consecutive_bars: u32,
    pub price: f64,
    pub fetched_at: u64,
}

#[derive(Debug, Clone)]
struct FillRecord {
    coin: String,
    side: String,
    sz: f64,
    px: f64,
    ts_ms: u64,
}

#[derive(Debug, Clone)]
struct L2Snapshot {
    coin: String,
    bid_depth: f64,
    ask_depth: f64,
    mid_px: f64,
    spread_bps: f64,
    ts_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SniperState {
    pub active: bool,
    pub coin: Option<String>,
    pub direction: Option<String>,
    pub phase: String, // Idle|Probing|Sniping|Retreating
    pub severity: u8,
    pub cooldown_until_ms: u64,
    pub updated_at: u64,
}

impl Default for SniperState {
    fn default() -> Self {
        Self {
            active: false,
            coin: None,
            direction: None,
            phase: "Idle".into(),
            severity: 0,
            cooldown_until_ms: 0,
            updated_at: 0,
        }
    }
}

// ── Whale State Bridge (file-based, J directive) ─────────

fn load_whale_state() -> HashMap<String, WhaleSnapshot> {
    let mut out = HashMap::new();
    let raw = match std::fs::read_to_string(WHALE_STATE_PATH) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let root: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return out,
    };
    let coins = match root.get("coins").and_then(|c| c.as_object()) {
        Some(c) => c,
        None => return out,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    for (coin, obj) in coins {
        let state = obj
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("随机游走");
        let score = obj
            .get("whale_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let decision = obj
            .get("decision")
            .and_then(|v| v.as_str())
            .unwrap_or("HOLD");
        let direction_bias = parse_direction(decision);
        let z_p = obj.get("z_p").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let z_oi = obj.get("z_oi").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let z_cvd = obj.get("z_cvd").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let active_layers = obj
            .get("active_layers")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let consecutive_bars = obj
            .get("consecutive_bars")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let price = obj.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0);

        out.insert(
            coin.clone(),
            WhaleSnapshot {
                state: state.to_string(),
                score,
                decision: decision.to_string(),
                direction_bias,
                z_p,
                z_oi,
                z_cvd,
                active_layers,
                consecutive_bars,
                price,
                fetched_at: now,
            },
        );
    }
    out
}

/// Parse direction from whale decision string.
///
/// ```text
/// EXECUTE_STANDARD_LONG → LONG
/// EXECUTE_AGGRESSIVE_LONG → LONG
/// FORCE_QUIT / EXECUTE_SHORT → SHORT
/// HOLD / FORCE_CLOSE → None
/// ```
fn parse_direction(decision: &str) -> Option<String> {
    let d = decision.to_uppercase();
    if d.contains("LONG") {
        Some("LONG".into())
    } else if d.contains("SHORT") || d.contains("FORCE_QUIT") {
        Some("SHORT".into())
    } else {
        None
    }
}

fn write_sniper_state(state: &SniperState) {
    let json = serde_json::to_string(state).unwrap_or_default();
    let _ = std::fs::write(SNIPER_STATE_PATH, &json);
}

// ── ToxicFlowDetector ─────────────────────────────────────

pub struct ToxicFlowDetector {
    fill_history: VecDeque<FillRecord>,
    book_snapshots: VecDeque<L2Snapshot>,
    l2_update_count: usize,
    l2_update_window_start: Instant,
    whale_cache: HashMap<String, WhaleSnapshot>,
    last_whale_load: Instant,
}

impl ToxicFlowDetector {
    pub fn new() -> Self {
        Self {
            fill_history: VecDeque::with_capacity(128),
            book_snapshots: VecDeque::with_capacity(120),
            l2_update_count: 0,
            l2_update_window_start: Instant::now(),
            whale_cache: HashMap::new(),
            last_whale_load: Instant::now(),
        }
    }

    /// Feed a new fill from the WS shock signal bus.
    pub fn feed_fill(&mut self, fill: FillRecord) {
        self.fill_history.push_back(fill);
        while self.fill_history.len() > 128 {
            self.fill_history.pop_front();
        }
    }

    /// Feed an L2 book snapshot (called from OBI cycle).
    pub fn feed_l2(&mut self, snap: L2Snapshot) {
        self.book_snapshots.push_back(snap);
        while self.book_snapshots.len() > 120 {
            self.book_snapshots.pop_front();
        }
    }

    /// Refresh whale state cache from file bridge (called each cycle).
    pub fn refresh_whale(&mut self) {
        if self.last_whale_load.elapsed().as_secs() < 10 {
            return;
        }
        self.whale_cache = load_whale_state();
        self.last_whale_load = Instant::now();
    }

    /// Main detection pipeline: evaluate all 4 detection modes for a coin.
    pub fn detect(&self, coin: &str, now_ms: u64) -> Vec<ToxicFlowEvent> {
        let mut events = Vec::new();
        let whale = self
            .whale_cache
            .get(coin)
            .cloned()
            .unwrap_or(WhaleSnapshot {
                state: "随机游走".into(),
                score: 0.0,
                decision: "HOLD".into(),
                direction_bias: None,
                z_p: 0.0,
                z_oi: 0.0,
                z_cvd: 0.0,
                active_layers: 0,
                consecutive_bars: 0,
                price: 0.0,
                fetched_at: now_ms,
            });

        // ── 1. Fill burst detection ──
        if let Some(burst_event) = self.detect_fill_burst(coin, now_ms, &whale) {
            events.push(burst_event);
        }

        // ── 2. Spoof detection ──
        if let Some(spoof_event) = self.detect_spoof(coin, now_ms, &whale) {
            events.push(spoof_event);
        }

        // ── 3. Quote stuffing ──
        if let Some(qs_event) = self.detect_quote_stuffing(coin, now_ms, &whale) {
            events.push(qs_event);
        }

        // ── 4. OI divergence (uses whale z-score directly, J-directed) ──
        if let Some(oi_event) = self.detect_oi_divergence(coin, now_ms, &whale) {
            events.push(oi_event);
        }

        events
    }

    fn detect_fill_burst(
        &self,
        coin: &str,
        now_ms: u64,
        whale: &WhaleSnapshot,
    ) -> Option<ToxicFlowEvent> {
        let window_start = now_ms.saturating_sub(FILL_BURST_WINDOW_MS);
        let recent: Vec<&FillRecord> = self
            .fill_history
            .iter()
            .filter(|f| f.coin == coin && f.ts_ms >= window_start)
            .collect();

        if recent.len() < FILL_BURST_MIN_COUNT {
            return None;
        }

        let buys = recent.iter().filter(|f| f.side == "BUY").count();
        let skew = buys as f64 / recent.len() as f64;

        if skew > FILL_BURST_SKEW || skew < (1.0 - FILL_BURST_SKEW) {
            let attack_side = if skew > FILL_BURST_SKEW {
                "BUY"
            } else {
                "SELL"
            };
            let severity = calculate_severity(60, whale);
            return Some(ToxicFlowEvent {
                kind: ToxicEventKind::FillBurst,
                coin: coin.to_string(),
                attack_side: attack_side.to_string(),
                severity,
                action: severity_to_evac_level(severity),
                whale_state: whale.state.clone(),
                whale_score: whale.score,
                direction_bias: whale.direction_bias.clone(),
                ts_ms: now_ms,
            });
        }
        None
    }

    fn detect_spoof(
        &self,
        coin: &str,
        now_ms: u64,
        whale: &WhaleSnapshot,
    ) -> Option<ToxicFlowEvent> {
        let recent: Vec<&L2Snapshot> = self
            .book_snapshots
            .iter()
            .filter(|s| s.coin == coin && s.ts_ms >= now_ms.saturating_sub(SPOOF_WINDOW_MS))
            .collect();

        if recent.len() < 2 {
            return None;
        }

        let first = recent.first()?;
        let last = recent.last()?;

        // Check bid-side vanish
        if first.bid_depth > 0.0 {
            let bid_drop = (first.bid_depth - last.bid_depth) / first.bid_depth;
            if bid_drop > SPOOF_DEPTH_DROP {
                let severity = calculate_severity(55, whale);
                return Some(ToxicFlowEvent {
                    kind: ToxicEventKind::Spoof,
                    coin: coin.to_string(),
                    attack_side: "SELL".into(),
                    severity,
                    action: severity_to_evac_level(severity),
                    whale_state: whale.state.clone(),
                    whale_score: whale.score,
                    direction_bias: whale.direction_bias.clone(),
                    ts_ms: now_ms,
                });
            }
        }

        // Check ask-side vanish
        if first.ask_depth > 0.0 {
            let ask_drop = (first.ask_depth - last.ask_depth) / first.ask_depth;
            if ask_drop > SPOOF_DEPTH_DROP {
                let severity = calculate_severity(55, whale);
                return Some(ToxicFlowEvent {
                    kind: ToxicEventKind::Spoof,
                    coin: coin.to_string(),
                    attack_side: "BUY".into(),
                    severity,
                    action: severity_to_evac_level(severity),
                    whale_state: whale.state.clone(),
                    whale_score: whale.score,
                    direction_bias: whale.direction_bias.clone(),
                    ts_ms: now_ms,
                });
            }
        }
        None
    }

    fn detect_quote_stuffing(
        &self,
        _coin: &str,
        now_ms: u64,
        whale: &WhaleSnapshot,
    ) -> Option<ToxicFlowEvent> {
        // Quote stuffing: global L2 update rate (not per-coin)
        if self.l2_update_count < QUOTE_STUFFING_RATE {
            return None;
        }
        if self.l2_update_window_start.elapsed().as_secs() < QUOTE_STUFFING_DURATION_S {
            return None;
        }
        let severity = calculate_severity(50, whale);
        Some(ToxicFlowEvent {
            kind: ToxicEventKind::QuoteStuffing,
            coin: "ALL".into(),
            attack_side: "NONE".into(),
            severity,
            action: EvacuationLevel::Reduce,
            whale_state: whale.state.clone(),
            whale_score: whale.score,
            direction_bias: whale.direction_bias.clone(),
            ts_ms: now_ms,
        })
    }

    /// OI divergence: use whale_sense z-score directly (J-directed).
    /// signal_obi.py alignment: price up + OI down = distribution = bearish tox
    fn detect_oi_divergence(
        &self,
        coin: &str,
        now_ms: u64,
        whale: &WhaleSnapshot,
    ) -> Option<ToxicFlowEvent> {
        // Pattern: z_p > 1.5 (price above mean) AND z_oi < -0.5 (OI declining)
        // → distribution / short toxic
        if whale.z_p > 1.5 && whale.z_oi < -0.5 {
            let severity = calculate_severity(65, whale);
            return Some(ToxicFlowEvent {
                kind: ToxicEventKind::OIDivergence,
                coin: coin.to_string(),
                attack_side: "SELL".into(),
                severity,
                action: severity_to_evac_level(severity),
                whale_state: whale.state.clone(),
                whale_score: whale.score,
                direction_bias: whale.direction_bias.clone(),
                ts_ms: now_ms,
            });
        }
        // Pattern: z_p < -1.5 (price below mean) AND z_oi > 0.5 (OI building)
        // → accumulation / bull trap build = potential long tox
        if whale.z_p < -1.5 && whale.z_oi > 0.5 {
            let severity = calculate_severity(55, whale);
            return Some(ToxicFlowEvent {
                kind: ToxicEventKind::OIDivergence,
                coin: coin.to_string(),
                attack_side: "BUY".into(),
                severity,
                action: severity_to_evac_level(severity),
                whale_state: whale.state.clone(),
                whale_score: whale.score,
                direction_bias: whale.direction_bias.clone(),
                ts_ms: now_ms,
            });
        }
        None
    }

    pub fn tick_l2_rate(&mut self) {
        self.l2_update_count += 1;
        // Reset counter every second
        if self.l2_update_window_start.elapsed().as_secs() >= 2 {
            self.l2_update_count = 0;
            self.l2_update_window_start = Instant::now();
        }
    }
}

// ── Severity helpers ──────────────────────────────────────

/// Calculate severity 0-100 with whale context weighting.
fn calculate_severity(base: u8, whale: &WhaleSnapshot) -> u8 {
    let mut s = base as f64;

    // whale_score ≥ 60 → +20% (whale is present, tox matters more)
    if whale.score >= 60.0 {
        s *= 1.2;
    }
    // active_layers in 主力吸筹 or 高位派发 → +10%
    if whale.active_layers >= 3 {
        s *= 1.1;
    }
    // consecutive_bars ≥ 3 → additional +5%
    if whale.consecutive_bars >= 3 {
        s *= 1.05;
    }
    (s as u8).min(100)
}

fn severity_to_evac_level(severity: u8) -> EvacuationLevel {
    match severity {
        0..=30 => EvacuationLevel::Monitor,
        31..=50 => EvacuationLevel::Alert,
        51..=65 => EvacuationLevel::Reduce,
        66..=80 => EvacuationLevel::Hedge,
        81..=90 => EvacuationLevel::FullEvac,
        _ => EvacuationLevel::Panic,
    }
}

// ── OBIStream ─────────────────────────────────────────────

pub struct OBIStream {
    depth: usize,
    weighted: bool,
}

impl OBIStream {
    pub fn new(depth: usize, weighted: bool) -> Self {
        Self { depth, weighted }
    }

    /// Compute Order Book Imbalance: aligned with London signal_obi.py
    /// OBI = (Σ bid_sz / rank − Σ ask_sz / rank) / total
    pub fn compute_obi(&self, book: &L2Book) -> f64 {
        let levels = book.bids.len().min(self.depth);
        if levels == 0 {
            return 0.0;
        }

        let bid_sum: f64 = book
            .bids
            .iter()
            .take(self.depth)
            .enumerate()
            .map(|(i, l)| {
                if self.weighted {
                    l.sz / (i + 1) as f64
                } else {
                    l.sz
                }
            })
            .sum();
        let ask_sum: f64 = book
            .asks
            .iter()
            .take(self.depth)
            .enumerate()
            .map(|(i, l)| {
                if self.weighted {
                    l.sz / (i + 1) as f64
                } else {
                    l.sz
                }
            })
            .sum();
        let total = bid_sum + ask_sum;
        if total == 0.0 {
            0.0
        } else {
            (bid_sum - ask_sum) / total
        }
    }

    /// Fast L2 snapshot extraction for detector feed
    pub fn snapshot(&self, coin: &str, book: &L2Book, now_ms: u64) -> L2Snapshot {
        let best_bid = book.bids.first().map(|l| l.px).unwrap_or(0.0);
        let best_ask = book.asks.first().map(|l| l.px).unwrap_or(0.0);
        let mid = if best_bid > 0.0 && best_ask > 0.0 {
            (best_bid + best_ask) / 2.0
        } else {
            0.0
        };
        let spread_bps = if mid > 0.0 {
            (best_ask - best_bid) / mid * 10000.0
        } else {
            0.0
        };
        let bid_depth = book.bids.iter().take(self.depth).map(|l| l.sz).sum();
        let ask_depth = book.asks.iter().take(self.depth).map(|l| l.sz).sum();

        L2Snapshot {
            coin: coin.to_string(),
            bid_depth,
            ask_depth,
            mid_px: mid,
            spread_bps,
            ts_ms: now_ms,
        }
    }
}

// ── FrontrunController ────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum FrontrunState {
    Idle,
    PrePositioning,
    Frontrunning,
    Filled,
}

pub struct FrontrunController {
    obi_history: VecDeque<ObiPoint>,
    state: FrontrunState,
}

#[derive(Debug, Clone)]
struct ObiPoint {
    ts_ms: u64,
    obi: f64,
    coin: String,
}

impl FrontrunController {
    pub fn new() -> Self {
        Self {
            obi_history: VecDeque::with_capacity(64),
            state: FrontrunState::Idle,
        }
    }

    /// Called each OBI tick (50ms). Returns true if frontrun should activate.
    pub fn tick_obi(&mut self, coin: &str, obi: f64, now_ms: u64) -> bool {
        self.obi_history.push_back(ObiPoint {
            ts_ms: now_ms,
            obi,
            coin: coin.to_string(),
        });
        while self.obi_history.len() > 64 {
            self.obi_history.pop_front();
        }

        if self.state != FrontrunState::Idle {
            return false;
        }

        // OBI threshold check
        if obi.abs() < OBI_THRESHOLD {
            return false;
        }

        // Delta check: is OBI accelerating?
        if self.obi_history.len() >= 3 {
            let recent: Vec<&ObiPoint> = self.obi_history.iter().rev().take(3).collect();
            let delta = recent[0].obi - recent[2].obi;
            if delta.abs() < OBI_DELTA_THRESHOLD {
                return false;
            }
        }

        self.state = FrontrunState::PrePositioning;
        true
    }

    pub fn consume(&mut self) -> FrontrunState {
        std::mem::replace(&mut self.state, FrontrunState::Idle)
    }
}

// ── MicroSniper ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ProbePhase {
    Idle,
    Probing,
    Sniping,
    Retreating,
}

pub struct MicroSniper {
    phase: ProbePhase,
    current_coin: Option<String>,
    probe_started_at: Option<u64>,
    cooldown_until: u64,
    // probe size fixed at $2 (J: conservative Phase 1)
    probe_sz: f64,
    snipe_sz: f64,
}

impl MicroSniper {
    pub fn new(probe_sz: f64, snipe_sz: f64) -> Self {
        Self {
            phase: ProbePhase::Idle,
            current_coin: None,
            probe_started_at: None,
            cooldown_until: 0,
            probe_sz,
            snipe_sz,
        }
    }

    /// Receive toxic flow event → decide whether to probe
    pub fn on_toxic_flow(&mut self, event: &ToxicFlowEvent, now_ms: u64) -> Option<SniperAction> {
        if self.phase != ProbePhase::Idle && self.phase != ProbePhase::Retreating {
            return None;
        }
        if self.phase == ProbePhase::Retreating && now_ms < self.cooldown_until {
            return None;
        }
        if event.severity < 55 {
            return None; // not severe enough for snipe
        }

        let direction = match event.attack_side.as_str() {
            "SELL" => "BUY", // snipe opposite direction of attack
            "BUY" => "SELL",
            _ => return None,
        };

        self.phase = ProbePhase::Probing;
        self.current_coin = Some(event.coin.clone());
        self.probe_started_at = Some(now_ms);

        Some(SniperAction::Probe {
            coin: event.coin.clone(),
            direction: direction.to_string(),
            sz: self.probe_sz,
            probe_ttl_ms: PRE_POSITION_TTL_MS,
        })
    }

    /// Probe fill confirmed → advance to snipe
    pub fn on_probe_fill(&mut self, _now_ms: u64) -> Option<SniperAction> {
        if self.phase != ProbePhase::Probing {
            return None;
        }
        self.phase = ProbePhase::Sniping;
        let coin = self.current_coin.clone().unwrap_or_default();
        Some(SniperAction::Snipe {
            coin,
            sz: self.snipe_sz,
            max_hold_ms: 2000,
        })
    }

    /// Probe rejected / timeout → retreat
    pub fn on_probe_rejected(&mut self, now_ms: u64) {
        self.phase = ProbePhase::Retreating;
        self.cooldown_until = now_ms + 10_000; // 10s cooldown
        self.current_coin = None;
        self.probe_started_at = None;
    }

    /// Snipe completed → retreat with cooldown
    pub fn on_snipe_complete(&mut self, now_ms: u64) {
        self.phase = ProbePhase::Retreating;
        self.cooldown_until = now_ms + 10_000;
        self.current_coin = None;
        self.probe_started_at = None;
    }

    pub fn phase(&self) -> &ProbePhase {
        &self.phase
    }
}

#[derive(Debug, Clone)]
pub enum SniperAction {
    Probe {
        coin: String,
        direction: String,
        sz: f64,
        probe_ttl_ms: u64,
    },
    Snipe {
        coin: String,
        sz: f64,
        max_hold_ms: u64,
    },
}

// ── EvacuationPipeline ────────────────────────────────────

pub struct EvacuationPipeline {
    /// Per-coin evacuation state
    states: HashMap<String, EvacuationLevel>,
}

impl EvacuationPipeline {
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
    }

    /// Process a toxic flow event → escalate or update evacuation level.
    /// J directive: max severity wins (concurrent signals don't downgrade).
    pub fn ingest(&mut self, event: &ToxicFlowEvent) -> EvacuationResponse {
        let current = self
            .states
            .get(&event.coin)
            .copied()
            .unwrap_or(EvacuationLevel::Monitor);
        let new_level = std::cmp::max(current, event.action);
        self.states.insert(event.coin.clone(), new_level);

        let needs_cancel_all = matches!(
            new_level,
            EvacuationLevel::FullEvac | EvacuationLevel::Panic
        );
        let needs_hedge = matches!(
            new_level,
            EvacuationLevel::Hedge | EvacuationLevel::FullEvac
        );

        EvacuationResponse {
            coin: event.coin.clone(),
            level: new_level,
            cancel_all: needs_cancel_all,
            hedge_direction: if needs_hedge {
                // Hedge opposite to attack direction
                match event.attack_side.as_str() {
                    "BUY" => Some("SELL".into()),
                    "SELL" => Some("BUY".into()),
                    _ => None,
                }
            } else {
                None
            },
            cooldown_ms: match new_level {
                EvacuationLevel::FullEvac => 300_000, // 5min
                EvacuationLevel::Panic => 600_000,    // 10min
                EvacuationLevel::Hedge => 120_000,    // 2min
                _ => 60_000,                          // 1min
            },
        }
    }

    /// Reset evacuation state for a coin (e.g. after cooldown expires).
    pub fn reset(&mut self, coin: &str) {
        self.states.remove(coin);
    }

    pub fn current_state(&self) -> SniperState {
        let mut s = SniperState::default();
        if let Some((coin, level)) = self.states.iter().max_by_key(|(_, l)| *l) {
            s.active = true;
            s.coin = Some(coin.clone());
            s.severity = *level as u8;
            s.phase = match *level {
                EvacuationLevel::FullEvac | EvacuationLevel::Panic => "Retreating".into(),
                EvacuationLevel::Hedge | EvacuationLevel::Reduce => "Retreating".into(),
                EvacuationLevel::Monitor | EvacuationLevel::Alert => "Idle".into(),
            };
        }
        s.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        s
    }
}

#[derive(Debug, Clone)]
pub struct EvacuationResponse {
    pub coin: String,
    pub level: EvacuationLevel,
    pub cancel_all: bool,
    pub hedge_direction: Option<String>,
    pub cooldown_ms: u64,
}

// ── Sniper Engine (top-level orchestrator) ────────────────

pub struct Sniper {
    pub detector: Arc<RwLock<ToxicFlowDetector>>,
    pub obi_stream: OBIStream,
    pub frontrun: Arc<RwLock<FrontrunController>>,
    pub micro_sniper: Arc<RwLock<MicroSniper>>,
    pub evacuation: Arc<RwLock<EvacuationPipeline>>,
    pub bus: SignalBus,
    /// Shared executor (J directive: same AtomicU64 nonce)
    pub executor: Arc<Executor>,
    enabled: bool,
    coins: Vec<String>,
}

impl Sniper {
    pub fn new(bus: SignalBus, executor: Arc<Executor>, coins: Vec<String>) -> Self {
        Self {
            detector: Arc::new(RwLock::new(ToxicFlowDetector::new())),
            obi_stream: OBIStream::new(5, true),
            frontrun: Arc::new(RwLock::new(FrontrunController::new())),
            micro_sniper: Arc::new(RwLock::new(MicroSniper::new(2.0, 10.0))),
            evacuation: Arc::new(RwLock::new(EvacuationPipeline::new())),
            bus,
            executor,
            enabled: !coins.is_empty(),
            coins,
        }
    }

    /// Main sniper loop — runs as a tokio task alongside the engine.
    ///
    /// Two sub-cycles:
    ///   - OBI (50ms): compute OBI → feed frontrun detector + L2 snapshots
    ///   - Detection (500ms): scan whale state + fill history → toxic events
    pub async fn run(self: Arc<Self>) -> Result<()> {
        if !self.enabled {
            tracing::info!("sniper: disabled (no coins), skipping");
            return Ok(());
        }

        tracing::info!(coins=?self.coins, "sniper: enabled");

        let mut obi_interval = tokio::time::interval(Duration::from_millis(OBI_CYCLE_MS));
        let mut detect_interval = tokio::time::interval(Duration::from_millis(500));

        loop {
            tokio::select! {
                _ = obi_interval.tick() => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;

                    // Feed L2 snapshots to detector for each coin
                    for coin in &self.coins {
                        let book = self.bus.book(coin);
                        let snap = self.obi_stream.snapshot(coin, &book, now);
                        let obi = self.obi_stream.compute_obi(&book);
                        let mut det = self.detector.write();
                        det.feed_l2(snap);
                        det.tick_l2_rate();

                        // Frontrun check
                        let mut fr = self.frontrun.write();
                        if fr.tick_obi(coin, obi, now) {
                            tracing::info!(%coin, obi, "sniper: frontrun signal triggered");
                        }
                    }
                }

                _ = detect_interval.tick() => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;

                    // Refresh whale cache
                    self.detector.write().refresh_whale();

                    // Detect toxic flow per coin
                    let events: Vec<ToxicFlowEvent> = self.coins.iter()
                        .flat_map(|coin| self.detector.read().detect(coin, now))
                        .collect();

                    for event in &events {
                        tracing::warn!(
                            coin=%event.coin,
                            kind=?event.kind,
                            severity=%event.severity,
                            action=?event.action,
                            "sniper: toxic flow detected"
                        );

                        // Feed evacuation pipeline (max severity wins)
                        let response = self.evacuation.write().ingest(event);
                        if response.cancel_all {
                            tracing::error!(
                                coin=%response.coin,
                                level=?response.level,
                                "sniper: EVACUATION triggered — cancel_all + cooldown {}ms",
                                response.cooldown_ms
                            );
                            // Use shared executor (J directive: same AtomicU64 nonce)
                            if let Err(e) = self.executor.cancel_all_for_coin(&response.coin).await {
                                tracing::error!(%e, "sniper: cancel_all failed");
                            }
                        }

                        // Feed micro sniper
                        if let Some(action) = self.micro_sniper.write().on_toxic_flow(event, now) {
                            tracing::info!(?action, "sniper: action dispatched");
                        }
                    }

                    // Write state to file bridge (pgn_engine_v2 reads this)
                    let state = self.evacuation.read().current_state();
                    write_sniper_state(&state);
                }
            }
        }
    }
}
