//! WebSocket Client — HL WebSocket connection manager.
//!
//! Subscribes to L2 book updates and user fills, pushes data
//! into the SignalBus for the engine task to consume.
//!
//! P0: Computes MarketShockSignal from real-time userFills stream
//!     and broadcasts via tokio::sync::watch lossy channel. The
//!     engine reads the latest signal without replaying history.

use crate::config::Config;
use crate::order_book;
use crate::types::{Fill, MarketShockSignal, SignalBus};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Rolling window duration for toxic burst detection (seconds).
const SHOCK_WINDOW_SECS: u64 = 3;

/// Run the WebSocket event loop. Called from a tokio task.
pub async fn run(cfg: Config, bus: SignalBus) -> Result<()> {
    let url = format!("{}/ws", cfg.ws_url.trim_end_matches("/ws"));

    loop {
        tracing::info!(url = %url, "connecting WebSocket");

        match connect_and_subscribe(&url, &cfg, &bus).await {
            Ok(()) => tracing::warn!("WebSocket closed cleanly, reconnecting..."),
            Err(e) => tracing::error!(?e, "WebSocket error, reconnecting..."),
        }

        tokio::time::sleep(cfg.ws_reconnect_delay).await;
    }
}

/// Connect, subscribe, and process messages.
async fn connect_and_subscribe(url: &str, cfg: &Config, bus: &SignalBus) -> Result<()> {
    let (ws_stream, _) = connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Subscribe to L2 books for all monitored coins
    for coin in &cfg.coins {
        let sub = serde_json::json!({
            "method": "subscribe",
            "subscription": {
                "type": "l2Book",
                "coin": coin
            }
        });
        write.send(Message::Text(sub.to_string())).await?;
    }

    // Subscribe to user fills (if we have address)
    if !cfg.address.is_empty() {
        let sub = serde_json::json!({
            "method": "subscribe",
            "subscription": {
                "type": "userFills",
                "user": cfg.address
            }
        });
        write.send(Message::Text(sub.to_string())).await?;
    }

    tracing::info!("subscribed to {} coins", cfg.coins.len());

    // ── P0: Rolling fill window for toxic burst detection ──
    // Tracks fill timestamps and direction in a cheap Vec.
    struct FillTick {
        time: u64, // ms since epoch
        sign: f64, // +1.0 for BUY, -1.0 for SELL
        oid: u64,
    }
    let mut fill_window: Vec<FillTick> = Vec::new();

    // Message processing loop
    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;

                process_message(&text, bus).await;

                // ── P0: Compute and broadcast shock signal on every WS message ──
                // Parse fill data from the raw WS text to update the rolling window.
                // This is independent of the bus.fills buffer (which the engine drains
                // per-cycle for logging).
                if let Some((side, oid, fill_time)) = extract_fill_meta(&text) {
                    let sign = if side.to_uppercase().contains("B") {
                        1.0
                    } else {
                        -1.0
                    };
                    fill_window.push(FillTick {
                        time: fill_time,
                        sign,
                        oid,
                    });

                    // Expire ticks outside the rolling window
                    let cutoff = now_ms.saturating_sub(SHOCK_WINDOW_SECS * 1000);
                    fill_window.retain(|t| t.time >= cutoff);

                    // Compute direction skew
                    let count = fill_window.len();
                    let direction_skew = if count > 0 {
                        fill_window.iter().map(|t| t.sign).sum::<f64>() / count as f64
                    } else {
                        0.0
                    };

                    let signal = MarketShockSignal {
                        recent_fills_count: count,
                        direction_skew,
                        last_fill_time: fill_time,
                        recent_oids: fill_window.iter().map(|t| t.oid).collect(),
                    };

                    // Lossy send: if engine hasn't read the previous signal yet,
                    // overwrite it — latest state is all that matters.
                    let _ = bus.shock_tx.send(signal);
                }
            }
            Ok(Message::Ping(data)) => {
                let _ = write.send(Message::Pong(data)).await;
            }
            Ok(Message::Close(_)) => break,
            Err(e) => {
                tracing::error!(?e, "WebSocket read error");
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Extract fill metadata from a raw WS message without full deserialization.
///
/// Returns (side, oid, time_ms) if the message contains a fill event.
/// The `userFills` channel sends individual fill objects (not arrays).
fn extract_fill_meta(text: &str) -> Option<(String, u64, u64)> {
    let data: serde_json::Value = serde_json::from_str(text).ok()?;

    let channel = data.get("channel")?.as_str()?;
    if channel != "userFills" {
        return None;
    }

    let fill_data = data.get("data")?;
    // Skip snapshot messages — only count streaming fills
    if fill_data
        .get("isSnapshot")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return None;
    }

    let side = fill_data.get("dir")?.as_str()?.to_string();
    let oid = fill_data.get("oid").and_then(|v| v.as_u64()).unwrap_or(0);
    let time = fill_data.get("time").and_then(|v| v.as_u64()).unwrap_or(0);

    Some((side, oid, time))
}

/// Process a single WebSocket message — updates L2 books and fills buffer.
async fn process_message(text: &str, bus: &SignalBus) {
    let data: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };

    let channel = data.get("channel").and_then(|v| v.as_str()).unwrap_or("");

    match channel {
        "l2Book" => {
            let coin = data
                .get("data")
                .and_then(|d| d.get("coin"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if let Some(data_obj) = data.get("data") {
                let mut books = bus.books.write();
                if let Some(book) = books.get_mut(coin) {
                    order_book::update_from_ws(book, data_obj);
                }
            }
        }

        "userFills" => {
            if let Some(fills_data) = data.get("data") {
                // Parse fill info (for engine-side logging via bus.fills)
                if let Some(fill) = parse_fill(fills_data) {
                    bus.fills.write().push(fill);
                }
            }
        }

        "pong" | "subscriptionResponse" => {
            // Acknowledge but don't process
        }

        other => {
            tracing::debug!(channel = %other, "unhandled WS channel");
        }
    }
}

/// Parse a fill from the `userFills` channel data.
fn parse_fill(data: &serde_json::Value) -> Option<Fill> {
    let coin = data.get("coin")?.as_str()?.to_string();
    let oid = data.get("oid").and_then(|v| v.as_u64()).unwrap_or(0);
    let side = data
        .get("dir")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let sz = data
        .get("sz")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())?;
    let px = data
        .get("px")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())?;
    let fee = data
        .get("fee")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let time = data.get("time").and_then(|v| v.as_u64()).unwrap_or(0);

    Some(Fill {
        coin,
        oid,
        side,
        sz,
        px,
        fee,
        time,
    })
}
