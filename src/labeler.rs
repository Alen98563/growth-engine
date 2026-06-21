//! Cycle-level label recorder.
//!
//! Every engine cycle writes one row per coin to a CSV file in append mode.
//! This is the ML fuel pipeline: a separate batch processor (Python) joins
//! consecutive cycles and computes counterfactual labels (CFL).
//!
//! Schema: `ts, coin, state, inst_type, pos_sz, pos_notional, withdrawable,
//!   equity, gross_spread_bps, net_spread_bps, skew_bps, position_ratio,
//!   volatility, mid_px, bid_sz, ask_sz, placed_buy_sz, placed_sell_sz`
//!
//! S3 fix: CSV escaping — fields containing comma, double-quote, or newline
//! are wrapped in double-quotes with internal quotes doubled (RFC 4180).

use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

/// One row of label fuel — rich enough for counterfactual labeling.
#[derive(Debug, Clone, Serialize)]
pub struct CycleRecord {
    pub ts: String,               // ISO-8601 UTC
    pub coin: String,
    pub state: String,            // NORMAL / PASSIVE_UNWIND / ...
    pub pos_sz: f64,              // signed net position size
    pub pos_notional: f64,        // |pos_sz × mid_px|
    pub withdrawable: f64,
    pub equity: f64,
    pub gross_spread_bps: f64,
    pub net_spread_bps: f64,
    pub skew_bps: f64,
    pub position_ratio: f64,
    pub volatility: f64,
    pub mid_px: f64,
    pub bid_sz: f64,             // best bid depth (sum of top levels)
    pub ask_sz: f64,             // best ask depth
    pub placed_buy_sz: f64,      // size placed this cycle (buy)
    pub placed_sell_sz: f64,     // size placed this cycle (sell)
}

/// S3: Escape a CSV field per RFC 4180.
///
/// If the field contains a comma, double-quote, or newline, wrap it in
/// double-quotes and double any internal double-quotes.
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Thread-safe CSV label recorder.
///
/// Opens the file on first write, flushes every row for crash safety.
pub struct Labeler {
    path: PathBuf,
    writer: Mutex<Option<BufWriter<File>>>,
}

impl Labeler {
    /// Create a labeler that appends cycle snapshots to the given CSV path.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            writer: Mutex::new(None),
        }
    }

    /// Append one record. Opens file lazily; writes CSV header on first call.
    /// S3: All string fields are CSV-escaped per RFC 4180.
    pub fn record(&mut self, rec: CycleRecord) {
        let mut guard = self.writer.lock().unwrap();
        if guard.is_none() {
            match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
            {
                Ok(f) => {
                    // Check if file is empty BEFORE moving f into BufWriter
                    let is_empty = f.metadata().map(|m| m.len() == 0).unwrap_or(true);
                    let mut w = BufWriter::new(f);
                    if is_empty {
                        let _ = writeln!(
                            w,
                            "ts,coin,state,pos_sz,pos_notional,withdrawable,equity,gross_spread_bps,\
                             net_spread_bps,skew_bps,position_ratio,volatility,mid_px,\
                             bid_sz,ask_sz,placed_buy_sz,placed_sell_sz"
                        );
                    }
                    *guard = Some(w);
                }
                Err(e) => {
                    tracing::error!(?e, path = %self.path.display(), "labeler: cannot open CSV");
                    return;
                }
            }
        }

        if let Some(ref mut w) = *guard {
            // S3: Build CSV line with proper escaping for all string fields
            let line = format!(
                "{},{},{},{:.6},{:.6},{:.6},{:.6},{:.4},{:.4},{:.4},{:.4},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}",
                csv_escape(&rec.ts),
                csv_escape(&rec.coin),
                csv_escape(&rec.state),
                rec.pos_sz,
                rec.pos_notional,
                rec.withdrawable,
                rec.equity,
                rec.gross_spread_bps,
                rec.net_spread_bps,
                rec.skew_bps,
                rec.position_ratio,
                rec.volatility,
                rec.mid_px,
                rec.bid_sz,
                rec.ask_sz,
                rec.placed_buy_sz,
                rec.placed_sell_sz
            );
            let _ = writeln!(w, "{}", line);
            let _ = w.flush(); // crash-safe: flush every row
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_labeler_appends_and_reads_back() {
        let tmp = std::env::temp_dir().join("ge_labeler_test.csv");
        let _ = std::fs::remove_file(&tmp);

        let mut lab = Labeler::new(tmp.clone());
        lab.record(CycleRecord {
            ts: "2026-06-19T10:00:00Z".into(),
            coin: "ACE".into(),
            state: "NORMAL".into(),
            pos_sz: 100.0,
            pos_notional: 8.4,
            withdrawable: 120.0,
            equity: 150.0,
            gross_spread_bps: 5.0,
            net_spread_bps: 2.5,
            skew_bps: 1.0,
            position_ratio: 0.1,
            volatility: 0.002,
            mid_px: 0.084,
            bid_sz: 5000.0,
            ask_sz: 3000.0,
            placed_buy_sz: 50.0,
            placed_sell_sz: 0.0,
        });

        // Read back
        let mut buf = String::new();
        File::open(&tmp).unwrap().read_to_string(&mut buf).unwrap();
        let lines: Vec<&str> = buf.trim().lines().collect();
        assert_eq!(lines.len(), 2, "header + 1 record");
        assert!(lines[0].starts_with("ts,coin"));
        assert!(lines[1].contains("ACE"));
        assert!(lines[1].contains("NORMAL"));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_csv_escape_safe_fields() {
        assert_eq!(csv_escape("NORMAL"), "NORMAL");
        assert_eq!(csv_escape("2026-06-19T10:00:00Z"), "2026-06-19T10:00:00Z");
    }

    #[test]
    fn test_csv_escape_comma() {
        assert_eq!(csv_escape("ACE,ETH"), "\"ACE,ETH\"");
    }

    #[test]
    fn test_csv_escape_quote() {
        assert_eq!(csv_escape("coin \"ACE\""), "\"coin \"\"ACE\"\"\"");
    }
}
