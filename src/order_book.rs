//! Order Book — L2 book maintenance from WebSocket updates.
//!
//! Processes `l2Book` messages from HL WebSocket and maintains
//! a local snapshot of the top-of-book for each monitored coin.

use crate::types::{BookLevel, L2Book, TimestampMs};

/// Update L2 book from a WebSocket `l2Book` message.
///
/// HL sends `l2Book` messages with the full book snapshot on subscription,
/// then delta updates (not implemented here — full snapshot on each update
/// is simpler and acceptable at 3-5s cycle rate).
pub fn update_from_ws(book: &mut L2Book, data: &serde_json::Value) -> bool {
    let coin = data.get("coin").and_then(|v| v.as_str()).unwrap_or("");
    if coin != book.coin {
        return false;
    }

    let levels = match data.get("levels") {
        Some(l) => l,
        None => return false,
    };

    // HL format: levels[0] = bids, levels[1] = asks
    let bids_raw = match levels.get(0) {
        Some(l) => l,
        None => return false,
    };
    let asks_raw = match levels.get(1) {
        Some(l) => l,
        None => return false,
    };

    // Parse levels
    book.bids = parse_levels(bids_raw);
    book.asks = parse_levels(asks_raw);

    // Timestamp
    if let Some(ts) = data.get("time").and_then(|v| v.as_u64()) {
        book.timestamp = ts;
    } else {
        book.timestamp = now_ms();
    }

    true
}

/// Parse raw level array from HL WebSocket.
///
/// Each level is `{"px": "0.0015", "sz": "1234.0", "n": 3}`.
fn parse_levels(raw: &serde_json::Value) -> Vec<BookLevel> {
    let arr = match raw.as_array() {
        Some(a) => a,
        None => return vec![],
    };

    arr.iter()
        .filter_map(|level| {
            let px = level.get("px")?.as_str()?.parse::<f64>().ok()?;
            let sz = level.get("sz")?.as_str()?.parse::<f64>().ok()?;
            let n = level.get("n").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            Some(BookLevel { px, sz, n })
        })
        .collect()
}

/// Current timestamp in milliseconds.
fn now_ms() -> TimestampMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as TimestampMs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_levels() {
        let raw = serde_json::json!([
            {"px": "0.0015", "sz": "1000.0", "n": 3},
            {"px": "0.0016", "sz": "500.0", "n": 2}
        ]);

        let levels = parse_levels(&raw);
        assert_eq!(levels.len(), 2);
        assert!((levels[0].px - 0.0015).abs() < 1e-6);
        assert!((levels[0].sz - 1000.0).abs() < 1e-6);
        assert_eq!(levels[0].n, 3);
    }

    #[test]
    fn test_update_from_ws_basic() {
        let mut book = L2Book::new("PUMP".into());
        let data = serde_json::json!({
            "coin": "PUMP",
            "time": 1700000000000_u64,
            "levels": [
                [{"px": "0.0015", "sz": "1000.0", "n": 1}],
                [{"px": "0.0016", "sz": "500.0", "n": 1}]
            ]
        });

        assert!(update_from_ws(&mut book, &data));
        assert!((book.best_bid().unwrap() - 0.0015).abs() < 1e-6);
        assert!((book.best_ask().unwrap() - 0.0016).abs() < 1e-6);
    }
}
