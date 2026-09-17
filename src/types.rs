//! Shared types — order representation, book snapshots, signal bus.

use crate::config::Config;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::watch;

/// Timestamp in milliseconds since epoch.
pub type TimestampMs = u64;

/// Asset name (e.g. "PUMP", "FARTCOIN").
pub type Asset = String;

/// Order ID from Hyperliquid.
pub type Oid = u64;

// ──── REST API types ────

/// HL order placement request.
#[derive(Debug, Clone, Serialize)]
pub struct OrderRequest {
    pub coin: Asset,
    #[serde(rename = "isBuy")]
    pub is_buy: bool,
    pub sz: f64,
    #[serde(rename = "limitPx")]
    pub limit_px: f64,
    #[serde(rename = "orderType")]
    pub order_type: OrderType,
    #[serde(rename = "reduceOnly")]
    pub reduce_only: bool,
    /// "Alo" for Post-Only (maker rebate), "Ioc" for immediate-or-cancel
    pub tif: String,
    /// Client order ID for idempotency
    #[serde(rename = "cloid")]
    pub cloid: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum OrderType {
    Limit,
    #[allow(dead_code)]
    Market,
}

/// HL order response.
#[derive(Debug, Clone, Deserialize)]
pub struct OrderResponse {
    pub status: String,
    pub response: ResponseData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResponseData {
    #[serde(rename = "type")]
    pub response_type: String,
    pub data: Option<OrderData>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderData {
    pub statuses: Vec<OrderStatus>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrderStatus {
    pub resting: Option<RestingOrder>,
    pub filled: Option<FilledOrder>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RestingOrder {
    pub oid: Oid,
    #[serde(rename = "cloid")]
    pub cloid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilledOrder {
    pub oid: Oid,
    #[serde(rename = "totalSz")]
    pub total_sz: String,
    #[serde(rename = "avgPx")]
    pub avg_px: String,
}

// ──── Order Outcome ────

/// Outcome of a limit order placement — used by the engine for
/// Asymmetric Tick Shading feedback loop.
///
/// When Post-Only SELL orders are repeatedly rejected with "would match",
/// the engine retreats BUY prices to prevent one-sided LONG accumulation.
#[derive(Debug, Clone)]
pub enum OrderOutcome {
    /// Order resting on the book (Post-Only success), with OID.
    Rested(u64),
    /// Post-Only order crossed the book and filled as taker (success).
    FilledTaker,
    /// Order rejected. Contains optional rejection reason string.
    Rejected(Option<String>),
}

// ──── L2 Order Book ────

/// Single level in the order book.
#[derive(Debug, Clone, Default)]
pub struct BookLevel {
    pub px: f64,
    pub sz: f64,
    pub n: usize,
}

/// Local L2 order book for one asset.
#[derive(Debug, Clone)]
pub struct L2Book {
    pub coin: Asset,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    pub timestamp: TimestampMs,
}

impl L2Book {
    /// Create a new L2 book for the given asset.
    pub fn new(coin: Asset) -> Self {
        Self {
            coin,
            bids: Vec::new(),
            asks: Vec::new(),
            timestamp: 0,
        }
    }

    /// Return the highest bid price, or None if book is empty.
    pub fn best_bid(&self) -> Option<f64> {
        self.bids.first().map(|l| l.px)
    }

    /// Return the lowest ask price, or None if book is empty.
    pub fn best_ask(&self) -> Option<f64> {
        self.asks.first().map(|l| l.px)
    }

    /// Compute mid-price from best bid/ask, or None if either side is empty.
    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) if ask > bid => Some((bid + ask) / 2.0),
            _ => None,
        }
    }

    /// Gross spread in bps.
    pub fn spread_bps(&self) -> Option<f64> {
        let mid = self.mid_price()?;
        let ask = self.best_ask()?;
        let bid = self.best_bid()?;
        Some((ask - bid) / mid * 10000.0)
    }

    /// Total bid-side depth (sum of all bid level sizes).
    pub fn total_bid_depth(&self) -> f64 {
        self.bids.iter().map(|l| l.sz).sum()
    }

    /// Total ask-side depth (sum of all ask level sizes).
    pub fn total_ask_depth(&self) -> f64 {
        self.asks.iter().map(|l| l.sz).sum()
    }
}

// ──── User state ────

#[derive(Debug, Clone, Default)]
pub struct Position {
    pub coin: Asset,
    /// Signed size (positive = LONG, negative = SHORT)
    pub size: f64,
    pub entry_px: f64,
    pub unrealized_pnl: f64,
}

#[derive(Debug, Clone, Default)]
pub struct AccountState {
    pub withdrawable: f64,
    pub equity: f64,
    pub positions: Vec<Position>,
}

/// Fill from user trade history.
#[derive(Debug, Clone)]
pub struct Fill {
    pub coin: Asset,
    pub oid: Oid,
    pub side: String,
    pub sz: f64,
    pub px: f64,
    pub fee: f64,
    pub time: TimestampMs,
}

// ──── Market Shock Signal (P0: toxic flow defense) ────

/// Computed from WebSocket userFills stream by the WS task.
/// Pushed through a `tokio::sync::watch` lossy channel — the engine
/// always reads the *latest* signal, never replays stale history.
#[derive(Debug, Clone, Default)]
pub struct MarketShockSignal {
    /// Number of fills in the rolling 3-second window.
    pub recent_fills_count: usize,
    /// Direction skew: positive = more buys (taker aggressing on ask side),
    /// negative = more sells (taker aggressing on bid side).
    pub direction_skew: f64,
    /// Timestamp of the most recent fill (ms since epoch).
    pub last_fill_time: u64,
    /// OIDs of fills in the current window (for double-fill detection).
    pub recent_oids: Vec<u64>,
}

impl MarketShockSignal {
    /// Is this a toxic burst? (≥3 fills in same direction within the window).
    pub fn is_toxic_burst(&self) -> bool {
        self.recent_fills_count >= 3
    }

    /// Which side is the toxic flow attacking?
    /// - Some("BUY")  → taker is BUYING  → our ASKS are being eaten  → retreat ask
    /// - Some("SELL") → taker is SELLING → our BIDS are being eaten  → retreat bid
    /// - None → mixed flow or below threshold
    pub fn attack_side(&self) -> Option<&'static str> {
        if !self.is_toxic_burst() {
            return None;
        }
        if self.direction_skew > 0.5 {
            Some("BUY")
        } else if self.direction_skew < -0.5 {
            Some("SELL")
        } else {
            None
        }
    }
}

// ──── Signal Bus (IPC between WS task and Engine task) ────

/// Shared state between WebSocket listener and engine loop.
#[derive(Clone)]
pub struct SignalBus {
    pub books: Arc<RwLock<HashMap<Asset, L2Book>>>,
    pub account: Arc<RwLock<AccountState>>,
    pub fills: Arc<RwLock<Vec<Fill>>>,
    /// WS task sends market shock signals through this sender.
    /// `watch::Sender` is clonable — both WS task and bus clone use it.
    pub shock_tx: watch::Sender<MarketShockSignal>,
}

impl SignalBus {
    /// Create a new SignalBus. Returns (bus, shock_rx) — the caller MUST
    /// pass `shock_rx` to the engine task.
    pub fn new(cfg: &Config) -> (Self, watch::Receiver<MarketShockSignal>) {
        let mut books = HashMap::new();
        for coin in &cfg.coins {
            books.insert(coin.clone(), L2Book::new(coin.clone()));
        }
        let (shock_tx, shock_rx) = watch::channel(MarketShockSignal::default());
        (
            Self {
                books: Arc::new(RwLock::new(books)),
                account: Arc::new(RwLock::new(AccountState::default())),
                fills: Arc::new(RwLock::new(Vec::new())),
                shock_tx,
            },
            shock_rx,
        )
    }

    /// Snapshot of current L2 for a coin.
    pub fn book(&self, coin: &str) -> L2Book {
        self.books
            .read()
            .get(coin)
            .cloned()
            .unwrap_or_else(|| L2Book::new(coin.to_string()))
    }

    /// Snapshot of current account state.
    pub fn account_snapshot(&self) -> AccountState {
        self.account.read().clone()
    }

    /// Drain recent fills for processing.
    pub fn drain_fills(&self) -> Vec<Fill> {
        std::mem::take(&mut *self.fills.write())
    }
}
