//! Multi-Level Quote Grid - configurable N-level order placement per side.
//!
//! Each level has a sigma multiplier determining its distance from mid-price.
//! Level 0 = nearest to mid, Level N-1 = farthest. Tracks per-level state
//! (Idle -> Active -> Filled/Cancelled) and detects drift for re-quoting.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// A single price level in the multi-level quote grid
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelQuote {
    /// Level index (0 = nearest to mid, N-1 = farthest)
    pub level: u32,
    /// Price for this level
    pub px: f64,
    /// Size in notional USD
    pub sz: f64,
    /// Sigma multiplier used for this level (for logging)
    pub sigma_mult: f64,
}

/// State of a single level order on the exchange
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelState {
    /// No order placed yet
    Idle,
    /// Order is resting on the book
    Active,
    /// Order was filled (executed)
    Filled,
    /// Order was cancelled (too far from mid, or replaced)
    Cancelled,
}

/// Per-level order tracking
#[derive(Debug, Clone)]
pub struct LevelOrder {
    /// Exchange order ID
    pub oid: u64,
    /// Current state
    pub state: LevelState,
    /// Price at which this order was placed
    pub placed_px: f64,
    /// Size at which this order was placed
    pub placed_sz: f64,
    /// Timestamp when placed (ms epoch)
    pub placed_at_ms: u64,
}

/// Manages multi-level order grid for a single coin
#[derive(Debug, Clone)]
pub struct LevelManager {
    /// Currently active levels: level_index → order info
    pub orders: HashMap<u32, LevelOrder>,
}

impl LevelManager {
    /// Create an empty level manager.
    pub fn new() -> Self {
        Self {
            orders: HashMap::new(),
        }
    }

    /// Register a placed order at a specific level
    pub fn register(&mut self, level: u32, oid: u64, px: f64, sz: f64, now_ms: u64) {
        self.orders.insert(
            level,
            LevelOrder {
                oid,
                state: LevelState::Active,
                placed_px: px,
                placed_sz: sz,
                placed_at_ms: now_ms,
            },
        );
    }

    /// Mark a level as filled
    pub fn mark_filled(&mut self, level: u32) {
        if let Some(o) = self.orders.get_mut(&level) {
            o.state = LevelState::Filled;
        }
    }

    /// Mark a level as cancelled
    pub fn mark_cancelled(&mut self, level: u32) {
        if let Some(o) = self.orders.get_mut(&level) {
            o.state = LevelState::Cancelled;
        }
    }

    /// Check if a level needs re-quoting:
    /// true if filled, cancelled, or price drifted too far (>2 sigma away)
    pub fn needs_requote(&self, level: u32, current_target_px: f64, tick: f64, sigma_ticks: f64) -> bool {
        match self.orders.get(&level) {
            None => true,
            Some(o) => match o.state {
                LevelState::Idle | LevelState::Filled | LevelState::Cancelled => true,
                LevelState::Active => {
                    // Requote if price drifted too far from target
                    let drift = (o.placed_px - current_target_px).abs();
                    let threshold = sigma_ticks * 0.5 * tick; // half a sigma tick
                    drift > threshold
                }
            },
        }
    }

    /// Get all active order IDs (for cancellation at shutdown)
    pub fn active_oids(&self) -> Vec<u64> {
        self.orders
            .values()
            .filter(|o| o.state == LevelState::Active)
            .map(|o| o.oid)
            .collect()
    }

    /// Count of currently active orders
    pub fn active_count(&self) -> usize {
        self.orders.values().filter(|o| o.state == LevelState::Active).count()
    }

    /// Total notional of active orders (approximate, from placed_sz)
    pub fn active_notional(&self) -> f64 {
        self.orders
            .values()
            .filter(|o| o.state == LevelState::Active)
            .map(|o| o.placed_sz)
            .sum()
    }
}

impl Default for LevelManager {
    fn default() -> Self {
        Self::new()
    }
}
