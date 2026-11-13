//! Offline replay — deterministic, network-free, key-free.
//!
//! Reads a recorded tape (`data/labels.csv`) and re-runs the same `risk::RiskEngine`
//! that the live loop used, producing:
//!   1. a **fidelity** number: how often the replayed quotes match what the engine
//!      actually placed on the tape (both sizes, 1% tolerance),
//!   2. a **synthetic** P&L under a documented touch-fill model,
//!   3. a self-contained HTML report (inline SVG, no CDN, no network).
//!
//! Nothing here touches Hyperliquid, `.env`, or a private key. The signer is never
//! constructed. Fills are *assumed*, not observed — the report says so on every page.

use crate::config::Config;
use crate::risk::RiskEngine;
use crate::types::{AccountState, BookLevel, L2Book, Position};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::time::Instant;

const DEFAULT_TAPE: &str = "data/labels.csv";

struct Args {
    tape: String,
    out: String,
    json: String,
    fee_bps: Option<f64>,
    skew_power: Option<f64>,
    max_skew_bps: Option<f64>,
    hard_limit: Option<f64>,
    qty_power: Option<f64>,
    base_notional: Option<f64>,
    min_order_size: Option<f64>,
    notional_map: Option<String>,
    ticks: usize,
}

fn parse_args(argv: &[String]) -> Result<Args> {
    let mut a = Args {
        tape: DEFAULT_TAPE.to_string(),
        out: "demo/report.html".to_string(),
        json: "demo/summary.json".to_string(),
        fee_bps: None,
        skew_power: None,
        max_skew_bps: None,
        hard_limit: None,
        qty_power: None,
        base_notional: None,
        min_order_size: None,
        notional_map: None,
        ticks: 400,
    };
    let mut i = 0;
    while i < argv.len() {
        let k = argv[i].as_str();
        let v = argv.get(i + 1).cloned();
        match k {
            "--tape" => a.tape = v.context("--tape needs a path")?,
            "--out" => a.out = v.context("--out needs a path")?,
            "--json" => a.json = v.context("--json needs a path")?,
            "--fee-bps" => a.fee_bps = Some(v.context("--fee-bps needs a number")?.parse()?),
            "--skew-power" => a.skew_power = Some(v.context("--skew-power needs a number")?.parse()?),
            "--max-skew-bps" => {
                a.max_skew_bps = Some(v.context("--max-skew-bps needs a number")?.parse()?)
            }
            "--hard-limit-ratio" => {
                a.hard_limit = Some(v.context("--hard-limit-ratio needs a number")?.parse()?)
            }
            "--qty-power" => a.qty_power = Some(v.context("--qty-power needs a number")?.parse()?),
            "--base-notional" => {
                a.base_notional = Some(v.context("--base-notional needs a number")?.parse()?)
            }
            "--min-order-size" => {
                a.min_order_size = Some(v.context("--min-order-size needs a number")?.parse()?)
            }
            "--notional-map" => a.notional_map = Some(v.context("--notional-map needs COIN=USD,...")?),
            "--points" => a.ticks = v.context("--points needs an int")?.parse()?,
            other => anyhow::bail!("unknown replay flag: {other}"),
        }
        i += 2;
    }
    Ok(a)
}

#[derive(Debug, Clone, Default)]
struct Rec {
    ts: String,
    coin: String,
    state: String,
    pos_sz: f64,
    equity: f64,
    withdrawable: f64,
    gross_spread_bps: f64,
    net_spread_bps: f64,
    skew_bps: f64,
    position_ratio: f64,
    mid_px: f64,
    bid_sz: f64,
    ask_sz: f64,
    placed_buy_sz: f64,
    placed_sell_sz: f64,
}

#[derive(Default)]
struct CoinStats {
    ticks: usize,
    gross_sum: f64,
    net_sum: f64,
    skew_sum: f64,
    skew_max: f64,
    state_changes: usize,
    state_counts: BTreeMap<String, usize>,
    shed_events: usize,
    fidelity_matches: usize,
    fidelity_checked: usize,
    fills: usize,
    buy_fills: usize,
    sell_fills: usize,
    volume: f64,
    fees: f64,
    inv: f64,
    cash: f64,
    max_abs_inv_notional: f64,
    series_spread: Vec<(f64, f64, f64)>, // idx, gross, net
    series_ratio: Vec<(f64, f64)>,      // idx, recorded position_ratio
    series_skew: Vec<(f64, f64, f64)>,  // idx, recorded skew, replayed skew
    series_equity: Vec<(f64, f64)>,     // idx, mark-to-market equity
}

/// Friendly wrapper for the replay loop: build the book the engine would have seen.
fn book_from(coin: &str, mid: f64, gross_spread_bps: f64, bid_sz: f64, ask_sz: f64) -> L2Book {
    let half = (gross_spread_bps / 10000.0) * mid / 2.0;
    let mut b = L2Book::new(coin.to_string());
    b.bids = vec![BookLevel { px: mid - half, sz: bid_sz.max(1.0), n: 1 }];
    b.asks = vec![BookLevel { px: mid + half, sz: ask_sz.max(1.0), n: 1 }];
    b.timestamp = 0;
    b
}

fn account_from(coin: &str, size: f64, equity: f64, withdrawable: f64, mid: f64) -> AccountState {
    let mut acc = AccountState::default();
    acc.equity = equity;
    acc.withdrawable = withdrawable;
    acc.positions = vec![Position { coin: coin.to_string(), size, entry_px: mid, unrealized_pnl: 0.0 }];
    acc
}

/// Engine gate + coarse-tick sizing, mirrored from `engine.rs` (V12.1/V12.2).
///
/// The risk module sizes both sides symmetrically; the engine then throttles
/// them: THIN_SPREAD blocks, one-tick fat-margin coins run at
/// `coarse_tick_size_pct` with the unwind side boosted and the adverse side
/// capped. Replaying without this step would mis-attribute an executor rule to
/// the risk model, so it is reproduced here on purpose.
fn gate_adjust(cfg: &Config, rec: &Rec, out: Option<&crate::risk::RiskOutput>) -> (f64, f64, f64, f64) {
    let Some(o) = out else {
        return (0.0, 0.0, 0.0, 0.0);
    };
    let tick = cfg.tick_for(&rec.coin);
    let tick_bps = if rec.mid_px > 0.0 {
        tick / rec.mid_px * 10000.0
    } else {
        0.0
    };
    let gross_ticks = if tick_bps > 0.0 {
        (o.gross_spread_bps / tick_bps).round()
    } else {
        0.0
    };
    let pct = if o.gross_spread_bps < cfg.maker_fee_bps + cfg.min_margin_bps {
        0.0
    } else if o.gross_spread_bps >= cfg.coarse_tick_bps_threshold && gross_ticks <= 1.0 {
        cfg.coarse_tick_size_pct
    } else {
        1.0
    };

    let mut buy = o.buy_sz * pct;
    let mut sell = o.sell_sz * pct;

    let is_coarse = pct > 0.0 && (pct - cfg.coarse_tick_size_pct).abs() < 1e-12;
    if is_coarse {
        let pr = o.position_ratio;
        if pr > cfg.coarse_pos_ratio_aggressive && pr <= cfg.coarse_pos_ratio_hard_kill {
            if rec.pos_sz < 0.0 {
                buy *= cfg.coarse_unwind_size_boost;
                if buy > 0.0 {
                    sell = sell.min(buy * 0.25);
                } else {
                    sell *= 0.10;
                }
            } else if rec.pos_sz > 0.0 {
                sell *= cfg.coarse_unwind_size_boost;
                if sell > 0.0 {
                    buy = buy.min(sell * 0.25);
                } else {
                    buy *= 0.10;
                }
            }
        } else if pr > cfg.coarse_pos_ratio_hard_kill {
            if rec.pos_sz < 0.0 {
                sell = 0.0;
                buy *= cfg.coarse_unwind_size_boost;
            } else if rec.pos_sz > 0.0 {
                buy = 0.0;
                sell *= cfg.coarse_unwind_size_boost;
            }
        }
    }

    (o.bid_px, o.ask_px, buy, sell)
}

pub fn run(argv: &[String]) -> Result<()> {
    let args = parse_args(argv)?;    let t0 = Instant::now();

    let mut cfg = Config::default();
    // The recorded run used the crate defaults; flags below let a reader perturb
    // them and watch the fidelity number move.
    cfg.growth_mode = true;
    if let Some(v) = args.hard_limit {
        cfg.hard_limit_ratio = v;
    }
    if let Some(v) = args.skew_power {
        cfg.skew_power = v;
    }
    if let Some(v) = args.max_skew_bps {
        cfg.max_skew_bps = v;
    }
    if let Some(v) = args.qty_power {
        cfg.qty_power = v;
    }
    if let Some(v) = args.base_notional {
        cfg.base_order_notional = v;
    }
    if let Some(v) = args.min_order_size {
        cfg.min_order_size = v;
    }
    if let Some(v) = args.fee_bps {
        cfg.maker_fee_bps = v;
    }
    // Tick sizes as observed on the venue for the five coins in the tape.
    cfg.tick_size = 0.000001;
    cfg.tick_sizes.insert("FARTCOIN".into(), 0.00001);
    cfg.tick_sizes.insert("PUMP".into(), 0.000001);
    cfg.tick_sizes.insert("HMSTR".into(), 0.00001);
    cfg.tick_sizes.insert("MEME".into(), 0.00001);
    cfg.tick_sizes.insert("RESOLV".into(), 0.00001);

    // Quote notional per coin. The tape's own sizes show the $10 base everywhere;
    // the visible small quotes on HMSTR/MEME come from the engine's coarse-tick
    // throttle below, not from a smaller budget.
    let mut notional: BTreeMap<String, f64> = BTreeMap::new();
    for c in ["PUMP", "RESOLV", "FARTCOIN", "HMSTR", "MEME"] {
        notional.insert(c.to_string(), cfg.base_order_notional);
    }
    if let Some(m) = args.notional_map.as_ref() {
        for kv in m.split(',') {
            if let Some((k, v)) = kv.split_once('=') {
                notional.insert(k.trim().to_string(), v.trim().parse()?);
            }
        }
    }
    let mut engines: BTreeMap<String, RiskEngine> = BTreeMap::new();

    // ── Read tape ──
    let f = fs::File::open(&args.tape)
        .with_context(|| format!("cannot open tape: {}", args.tape))?;
    let mut lines = BufReader::new(f).lines();
    let header = lines.next().context("empty tape")??;
    let cols: Vec<&str> = header.split(',').map(|s| s.trim()).collect();
    let col = |name: &str| -> Result<usize> {
        cols.iter()
            .position(|c| *c == name)
            .with_context(|| format!("tape is missing column `{name}`"))
    };
    let (c_ts, c_coin, c_state) = (col("ts")?, col("coin")?, col("state")?);
    let (c_pos, c_eq, c_wd) = (col("pos_sz")?, col("equity")?, col("withdrawable")?);
    let (c_gross, c_net, c_skew, c_ratio) =
        (col("gross_spread_bps")?, col("net_spread_bps")?, col("skew_bps")?, col("position_ratio")?);
    let (c_mid, c_bsz, c_asz) = (col("mid_px")?, col("bid_sz")?, col("ask_sz")?);
    let (c_pb, c_ps) = (col("placed_buy_sz")?, col("placed_sell_sz")?);

    let mut stats: BTreeMap<String, CoinStats> = BTreeMap::new();
    let mut latency_us: Vec<f64> = Vec::new();
    let mut prev: Option<Rec> = None;
    // (coin, bid, ask, buy, sell) produced for the previous row of the tape.
    let mut prev_quote: Option<(String, f64, f64, f64, f64)> = None;
    let mut rows = 0usize;
    let mut first_eq = 0.0f64;

    for line in lines {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split(',').collect();
        if f.len() < cols.len() {
            continue;
        }
        let p = |i: usize| -> f64 { f[i].trim().parse::<f64>().unwrap_or(0.0) };
        let rec = Rec {
            ts: f[c_ts].trim().to_string(),
            coin: f[c_coin].trim().to_string(),
            state: f[c_state].trim().to_string(),
            pos_sz: p(c_pos),
            equity: p(c_eq),
            withdrawable: p(c_wd),
            gross_spread_bps: p(c_gross),
            net_spread_bps: p(c_net),
            skew_bps: p(c_skew),
            position_ratio: p(c_ratio),
            mid_px: p(c_mid),
            bid_sz: p(c_bsz),
            ask_sz: p(c_asz),
            placed_buy_sz: p(c_pb),
            placed_sell_sz: p(c_ps),
        };
        rows += 1;
        if first_eq == 0.0 && rec.equity.abs() > 1e-9 {
            first_eq = rec.equity;
        }

        // ── replay decision for this row ──
        let book = book_from(&rec.coin, rec.mid_px, rec.gross_spread_bps, rec.bid_sz, rec.ask_sz);
        let acc = account_from(
            &rec.coin,
            rec.pos_sz,
            rec.equity,
            rec.withdrawable,
            rec.mid_px,
        );
        let coin = rec.coin.clone();
        let risk = engines.entry(coin.clone()).or_insert_with(|| {
            let mut c2 = cfg.clone();
            if let Some(n) = notional.get(&coin) {
                c2.base_order_notional = *n;
            }
            RiskEngine::new(c2)
        });
        let t = Instant::now();
        let out = risk.assess(&book, &acc, &rec.coin);
        latency_us.push(t.elapsed().as_secs_f64() * 1e6);

        let st = stats.entry(rec.coin.clone()).or_default();
        st.ticks += 1;
        st.gross_sum += rec.gross_spread_bps;
        st.net_sum += rec.net_spread_bps;
        st.skew_sum += rec.skew_bps;
        if rec.skew_bps.abs() > st.skew_max {
            st.skew_max = rec.skew_bps.abs();
        }
        *st.state_counts.entry(rec.state.clone()).or_insert(0) += 1;
        if let Some(pv) = prev.as_ref() {
            if pv.coin == rec.coin && pv.state != rec.state {
                st.state_changes += 1;
            }
        }

        let replay_skew = out.as_ref().map(|o| o.skew_bps).unwrap_or(0.0);
        // Engine-level gate + V12.1/V12.2 coarse-tick sizing, mirrored from engine.rs.
        let (qb, qa, qbuy, qsell) = gate_adjust(&cfg, &rec, out.as_ref());
        if let Some(o) = out.as_ref() {
            if o.should_shed {
                st.shed_events += 1;
            }
            // Fidelity is measured per side, and only on sides the engine actually
            // quoted (recorded size > 0). A zero recorded size is a state gate,
            // not a pricing mismatch.
            let near = |a: f64, b: f64| (a - b).abs() <= 0.01 * a.abs().max(b.abs()).max(1e-9);
            if rec.placed_buy_sz > 0.0 {
                st.fidelity_checked += 1;
                if near(qbuy, rec.placed_buy_sz) {
                    st.fidelity_matches += 1;
                }
            }
            if rec.placed_sell_sz > 0.0 {
                st.fidelity_checked += 1;
                if near(qsell, rec.placed_sell_sz) {
                    st.fidelity_matches += 1;
                }
            }
        }

        // ── synthetic touch-fill against the previous row's quote ──
        if let Some((pcoin, pbid, pask, pbuy, psell)) = prev_quote.clone() {
            if pcoin == rec.coin {
                let fee_rate = cfg.maker_fee_bps / 10000.0;
                let st = stats.get_mut(&rec.coin).expect("coin exists");
                if rec.mid_px <= pbid && pbuy > 0.0 {
                    let notional = pbid * pbuy;
                    st.cash -= notional * (1.0 + fee_rate);
                    st.inv += pbuy;
                    st.fills += 1;
                    st.buy_fills += 1;
                    st.volume += notional;
                    st.fees += notional * fee_rate;
                }
                if rec.mid_px >= pask && psell > 0.0 {
                    let notional = pask * psell;
                    st.cash += notional * (1.0 - fee_rate);
                    st.inv -= psell;
                    st.fills += 1;
                    st.sell_fills += 1;
                    st.volume += notional;
                    st.fees += notional * fee_rate;
                }
                let mark = st.cash + st.inv * rec.mid_px;
                let inv_notional = (st.inv * rec.mid_px).abs();
                if inv_notional > st.max_abs_inv_notional {
                    st.max_abs_inv_notional = inv_notional;
                }
                let idx = st.series_equity.len() as f64;
                st.series_equity.push((idx, mark));
                st.series_ratio.push((idx, rec.position_ratio));
                st.series_spread.push((idx, rec.gross_spread_bps, rec.net_spread_bps));
                st.series_skew.push((idx, rec.skew_bps, replay_skew));
            }
        }

        prev_quote = Some((rec.coin.clone(), qb, qa, qbuy, qsell));
        prev = Some(rec);
    }

    // ── latency percentiles ──
    latency_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| -> f64 {
        if latency_us.is_empty() {
            return 0.0;
        }
        let i = ((latency_us.len() as f64 - 1.0) * p).round() as usize;
        latency_us[i]
    };

    // ── JSON summary ──
    let mut coins_json = serde_json::Map::new();
    for (coin, st) in stats.iter() {
        let fidelity = if st.fidelity_checked > 0 {
            st.fidelity_matches as f64 / st.fidelity_checked as f64 * 100.0
        } else {
            0.0
        };
        coins_json.insert(
            coin.clone(),
            json!({
                "ticks": st.ticks,
                "gross_spread_bps_mean": st.gross_sum / st.ticks.max(1) as f64,
                "net_spread_bps_mean": st.net_sum / st.ticks.max(1) as f64,
                "skew_bps_mean": st.skew_sum / st.ticks.max(1) as f64,
                "skew_bps_abs_max": st.skew_max,
                "state_changes": st.state_changes,
                "state_counts": st.state_counts,
                "shed_events": st.shed_events,
                "fidelity_pct": fidelity,
                "fidelity_checked": st.fidelity_checked,
                "fills": st.fills,
                "buy_fills": st.buy_fills,
                "sell_fills": st.sell_fills,
                "volume_notional": st.volume,
                "fees_paid": st.fees,
                "end_inventory": st.inv,
                "max_abs_inventory_notional": st.max_abs_inv_notional,
                "synthetic_pnl": st.cash + st.inv * 0.0,
            }),
        );
    }
    let summary = json!({
        "tape": args.tape,
        "rows": rows,
        "coins": stats.len(),
        "maker_fee_bps": cfg.maker_fee_bps,
        "skew_power": cfg.skew_power,
        "max_skew_bps": cfg.max_skew_bps,
        "assess_latency_us": { "p50": pct(0.50), "p90": pct(0.90), "p99": pct(0.99), "samples": latency_us.len() },
        "wall_seconds": t0.elapsed().as_secs_f64(),
        "first_equity_abs": first_eq,
        "per_coin": coins_json,
    });

    if let Some(dir) = std::path::Path::new(&args.json).parent() {
        fs::create_dir_all(dir).ok();
    }
    fs::write(&args.json, serde_json::to_string_pretty(&summary)?)
        .with_context(|| format!("cannot write {}", args.json))?;

    // ── HTML report ──
    let html = render_html(&args, &summary, &stats, pct(0.50), pct(0.90), pct(0.99), rows, t0.elapsed().as_secs_f64());
    if let Some(dir) = std::path::Path::new(&args.out).parent() {
        fs::create_dir_all(dir).ok();
    }
    fs::write(&args.out, html).with_context(|| format!("cannot write {}", args.out))?;

    println!("replay: {} rows, {} coins", rows, stats.len());
    println!("  fidelity    {:.1}% of quotes reproduced", {
        let m: usize = stats.values().map(|s| s.fidelity_matches).sum();
        let c: usize = stats.values().map(|s| s.fidelity_checked).sum();
        if c > 0 { m as f64 / c as f64 * 100.0 } else { 0.0 }
    });
    println!("  fills       {}", stats.values().map(|s| s.fills).sum::<usize>());
    println!("  synthetic P&L {:.4}", stats.values().map(|s| s.cash).sum::<f64>());
    println!("  assess p50 {:.2}us  p99 {:.2}us", pct(0.50), pct(0.99));
    println!("  report      {}", args.out);
    println!("  summary     {}", args.json);
    Ok(())
}

// ───────────────────────── HTML ─────────────────────────

fn downsample<T: Clone>(v: &[T], n: usize) -> Vec<T> {
    if v.len() <= n {
        return v.to_vec();
    }
    let step = v.len() as f64 / n as f64;
    (0..n).map(|i| v[(i as f64 * step) as usize].clone()).collect()
}

fn polyline(points: &[(f64, f64)], w: f64, h: f64, color: &str, y_min: f64, y_max: f64) -> String {
    if points.len() < 2 {
        return String::new();
    }
    let x0 = points.first().unwrap().0;
    let x1 = points.last().unwrap().0;
    let span = (x1 - x0).max(1.0);
    let range = (y_max - y_min).max(1e-9);
    let mut d = String::new();
    for (i, (x, y)) in points.iter().enumerate() {
        let px = (x - x0) / span * w;
        let py = h - (y - y_min) / range * h;
        d.push_str(if i == 0 { "M" } else { "L" });
        d.push_str(&format!("{px:.1} {py:.1} "));
    }
    format!(
        "<path d=\"{d}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"1.6\" stroke-linejoin=\"round\"/>"
    )
}

fn chart(series: &[(f64, f64, f64)], labels: (&str, &str), w: f64, h: f64, colors: (&str, &str), n: usize) -> String {
    let s = downsample(series, n);
    let mut ymin = f64::MAX;
    let mut ymax = f64::MIN;
    for (_, a, b) in s.iter() {
        ymin = ymin.min(*a).min(*b);
        ymax = ymax.max(*a).max(*b);
    }
    if !ymin.is_finite() || !ymax.is_finite() {
        return "<p class=\"muted\">no data</p>".into();
    }
    if (ymax - ymin).abs() < 1e-9 {
        ymax = ymin + 1.0;
    }
    let pad = (ymax - ymin) * 0.08;
    let ymin = ymin - pad;
    let ymax = ymax + pad;
    let a: Vec<(f64, f64)> = s.iter().map(|(x, v, _)| (*x, *v)).collect();
    let b: Vec<(f64, f64)> = s.iter().map(|(x, _, v)| (*x, *v)).collect();
    format!(
        "<svg viewBox=\"0 0 {w} {h}\" class=\"chart\" preserveAspectRatio=\"none\">\
         <line x1=\"0\" y1=\"{h}\" x2=\"{w}\" y2=\"{h}\" stroke=\"#e5e7eb\"/>\
         {}{}\
         <text x=\"4\" y=\"12\" class=\"axl\">{:.1} / {:.1}</text></svg>\
         <div class=\"legend\"><span style=\"color:{c1}\">■ {}</span><span style=\"color:{c2}\">■ {}</span></div>",
        polyline(&a, w, h, colors.0, ymin, ymax),
        polyline(&b, w, h, colors.1, ymin, ymax),
        ymax,
        ymin,
        labels.0,
        labels.1,
        c1 = colors.0,
        c2 = colors.1
    )
}

fn render_html(
    args: &Args,
    summary: &serde_json::Value,
    stats: &BTreeMap<String, CoinStats>,
    p50: f64,
    p90: f64,
    p99: f64,
    rows: usize,
    secs: f64,
) -> String {
    let total_fills: usize = stats.values().map(|s| s.fills).sum();
    let total_vol: f64 = stats.values().map(|s| s.volume).sum();
    let total_fee: f64 = stats.values().map(|s| s.fees).sum();
    let fidelity: f64 = {
        let m: usize = stats.values().map(|s| s.fidelity_matches).sum();
        let c: usize = stats.values().map(|s| s.fidelity_checked).sum();
        if c > 0 {
            m as f64 / c as f64 * 100.0
        } else {
            0.0
        }
    };

    let mut coin_sections = String::new();
    let mut coin_rows = String::new();
    for (coin, st) in stats.iter() {
        let gm = st.gross_sum / st.ticks.max(1) as f64;
        let nm = st.net_sum / st.ticks.max(1) as f64;
        let sm = st.skew_sum / st.ticks.max(1) as f64;
        let fid = if st.fidelity_checked > 0 {
            st.fidelity_matches as f64 / st.fidelity_checked as f64 * 100.0
        } else {
            0.0
        };
        coin_rows.push_str(&format!(
            "<tr><td><b>{coin}</b></td><td>{}</td><td>{gm:.2}</td><td>{nm:.2}</td><td>{sm:.2}</td>\
             <td>{}</td><td>{:.1}%</td><td>{}</td><td>${:.2}</td><td>${:.2}</td><td>${:.2}</td></tr>",
            st.ticks,
            st.state_changes,
            fid,
            st.fills,
            st.volume,
            st.fees,
            st.cash
        ));

        let spread_chart = chart(&st.series_spread, ("gross spread (bps)", "net after skew"), 860.0, 150.0, ("#6366f1", "#10b981"), args.ticks);
        let ratio_chart = {
            let s: Vec<(f64, f64, f64)> = st.series_ratio.iter().map(|(x, v)| (*x, *v, *v)).collect();
            chart(&s, ("position ratio", "(same)"), 860.0, 120.0, ("#f59e0b", "#f59e0b"), args.ticks)
        };
        let skew_chart = chart(&st.series_skew, ("recorded skew", "replayed skew"), 860.0, 120.0, ("#94a3b8", "#ef4444"), args.ticks);
        let equity_chart = {
            let s: Vec<(f64, f64, f64)> = st.series_equity.iter().map(|(x, v)| (*x, *v, *v)).collect();
            chart(&s, ("synthetic P&L (USD)", "(same)"), 860.0, 140.0, ("#0ea5e9", "#0ea5e9"), args.ticks)
        };

        let states: String = st
            .state_counts
            .iter()
            .map(|(k, v)| format!("<span class=\"chip\">{k} <b>{v}</b></span>"))
            .collect();

        coin_sections.push_str(&format!(
            "<section><h2>{coin}</h2>\
             <p class=\"muted\">{ticks} ticks · {changes} state changes · {shed} shed signals · synthetic fills {fills}</p>\
             <h3>Spread</h3>{spread_chart}\
             <h3>Inventory pressure</h3>{ratio_chart}\
             <h3>Skew: recorded vs replayed</h3>{skew_chart}\
             <h3>Synthetic P&L (touch-fill model)</h3>{equity_chart}\
             <h3>State machine occupancy</h3><div>{states}</div></section>",
            ticks = st.ticks,
            changes = st.state_changes,
            shed = st.shed_events,
            fills = st.fills,
        ));
    }

    let config_note = format!(
        "maker_fee={} bps · skew_power={} · max_skew={} bps · tape={}",
        summary["maker_fee_bps"], summary["skew_power"], summary["max_skew_bps"], args.tape
    );

    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Growth Engine — offline replay</title>
<style>
:root{{--ink:#0f172a;--muted:#64748b;--line:#e2e8f0;--bg:#f8fafc}}
*{{box-sizing:border-box}}
body{{margin:0;background:var(--bg);color:var(--ink);font:15px/1.6 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif}}
.wrap{{max-width:960px;margin:0 auto;padding:40px 20px 80px}}
h1{{font-size:30px;line-height:1.25;margin:0 0 8px}}
h2{{font-size:20px;margin:44px 0 6px;padding-top:18px;border-top:1px solid var(--line)}}
h3{{font-size:13px;text-transform:uppercase;letter-spacing:.06em;color:var(--muted);margin:22px 0 6px}}
p{{margin:0 0 12px}}
.muted{{color:var(--muted);font-size:13px}}
.warn{{background:#fff7ed;border:1px solid #fed7aa;border-radius:10px;padding:14px 16px;margin:18px 0 26px}}
.warn b{{color:#c2410c}}
.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:12px;margin:22px 0 8px}}
.card{{background:#fff;border:1px solid var(--line);border-radius:12px;padding:14px}}
.card .k{{font-size:11px;letter-spacing:.06em;text-transform:uppercase;color:var(--muted)}}
.card .v{{font-size:22px;font-weight:650;margin-top:4px}}
table{{width:100%;border-collapse:collapse;background:#fff;border:1px solid var(--line);border-radius:12px;overflow:hidden;font-size:13px}}
th,td{{padding:9px 10px;text-align:right;border-bottom:1px solid var(--line)}}
th{{background:#f1f5f9;font-weight:600;font-size:12px}}
th:first-child,td:first-child{{text-align:left}}
.chart{{width:100%;height:auto;background:#fff;border:1px solid var(--line);border-radius:10px;display:block}}
.axl{{font-size:10px;fill:#94a3b8}}
.legend{{font-size:12px;color:var(--muted);margin:6px 0 0;display:flex;gap:16px}}
.chip{{display:inline-block;background:#fff;border:1px solid var(--line);border-radius:8px;padding:3px 9px;margin:3px 4px 0 0;font-size:12px;color:#334155}}
code{{background:#eef2ff;padding:1px 6px;border-radius:5px;font-size:13px}}
footer{{margin-top:48px;padding-top:16px;border-top:1px solid var(--line);color:var(--muted);font-size:12px}}
</style></head><body><div class="wrap">
<h1>Growth Engine — offline replay</h1>
<p class="muted">A recorded live tape, re-run through the same risk engine. No network, no keys, no exchange.</p>

<div class="warn"><b>Read this first.</b> Every P&amp;L number below is a <b>model</b>, not a result: fills are assumed whenever the next mid touches the quoted price, at the quoted price, minus the maker fee. A real fill needs a counterparty. The one real live run on this tape <b>lost money</b> — $137.65 → $128.88 over 10 hours (−6.4%). This page exists to show that the decision path is reproducible, not to show a profit.</div>

<div class="cards">
<div class="card"><div class="k">tape rows</div><div class="v">{rows}</div></div>
<div class="card"><div class="k">coins</div><div class="v">{coins}</div></div>
<div class="card"><div class="k">quote fidelity</div><div class="v">{fid:.1}%</div></div>
<div class="card"><div class="k">synthetic fills</div><div class="v">{fills}</div></div>
<div class="card"><div class="k">assess p50 / p99</div><div class="v">{p50:.1} / {p99:.1} µs</div></div>
</div>
<p class="muted">wall {secs:.2}s · p90 {p90:.1} µs · fees paid ${fee:.2} · notional traded ${vol:.2} · {config_note}</p>

<h2>Per-coin</h2>
<table><tr><th>coin</th><th>ticks</th><th>gross</th><th>net</th><th>skew</th><th>state chg</th><th>fidelity</th><th>fills</th><th>volume</th><th>fees</th><th>synth P&amp;L</th></tr>
{coin_rows}</table>
{coin_sections}
<footer>Generated by <code>growth-engine replay --tape {tape}</code> · deterministic, offline · the same <code>RiskEngine::assess</code> the live loop calls.</footer>
</div></body></html>"#,
        rows = rows,
        coins = stats.len(),
        fid = fidelity,
        fills = total_fills,
        p50 = p50,
        p90 = p90,
        p99 = p99,
        secs = secs,
        fee = total_fee,
        vol = total_vol,
        config_note = config_note,
        coin_rows = coin_rows,
        coin_sections = coin_sections,
        tape = args.tape
    )
}

/// Silence an unused-import warning without changing behaviour.
#[allow(dead_code)]
fn _write_marker(w: &mut dyn Write) -> std::io::Result<()> {
    w.write_all(b"")
}
