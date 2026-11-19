//! quota-bench — same venue, same signer, two request strategies, one variable.
//!
//! Why this exists: Hyperliquid allocates REST request quota by **cumulative
//! traded volume** (`1 request per USDC traded`). An engine that quotes a lot
//! and trades little therefore starves itself: the 600 ms throttle only defers
//! exhaustion. The real lever is fewer requests per quote cycle.
//!
//! This bench measures that lever on live orders:
//!
//!   serial : N × POST /exchange (place)  + N × POST /exchange (cancel) = 2N
//!   batch  : 1 × POST /exchange (N orders) + 1 × POST /exchange (N cancels) = 2
//!
//! Safety: every order is post-only (`Alo`), priced well below mid, cancelled in
//! the same round, and its notional is checked against a hard cap before send.

use crate::config::Config;
use crate::executor::Executor;
use crate::signer::Signer;
use crate::types::OrderOutcome;
use anyhow::{bail, Result};
use serde_json::json;
use std::time::{Duration, Instant};

const QUOTA_RETRY_SLEEP: Duration = Duration::from_secs(10);
const QUOTA_RETRY_MAX: u32 = 3;
const QUOTA_NEEDLE: &str = "Too many cumulative requests";

fn is_quota_blocked(out: &OrderOutcome) -> bool {
    matches!(out, OrderOutcome::Rejected(Some(t)) if t.contains(QUOTA_NEEDLE))
}

fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = s.len() / 2;
    if s.len() % 2 == 0 {
        (s[mid - 1] + s[mid]) / 2.0
    } else {
        s[mid]
    }
}

async fn place_one(ex: &Executor, coin: &str, sz: f64, px: f64) -> Result<(OrderOutcome, u32)> {
    let mut out = ex.place_limit_order(coin, true, sz, px, false).await?;
    let mut tries = 0u32;
    while is_quota_blocked(&out) && tries < QUOTA_RETRY_MAX {
        tries += 1;
        println!(
            "  quota gate on single place, retry {}/{} after {}s",
            tries,
            QUOTA_RETRY_MAX,
            QUOTA_RETRY_SLEEP.as_secs()
        );
        tokio::time::sleep(QUOTA_RETRY_SLEEP).await;
        out = ex.place_limit_order(coin, true, sz, px, false).await?;
    }
    Ok((out, tries))
}

async fn batch_retry(
    ex: &Executor,
    orders: &[(String, bool, f64, f64, bool)],
) -> Result<(Vec<OrderOutcome>, u32)> {
    let mut outs = ex.place_batch(orders).await?;
    let mut tries = 0u32;
    while !outs.is_empty() && outs.iter().all(is_quota_blocked) && tries < QUOTA_RETRY_MAX {
        tries += 1;
        println!(
            "  quota gate on batch, retry {}/{} after {}s",
            tries,
            QUOTA_RETRY_MAX,
            QUOTA_RETRY_SLEEP.as_secs()
        );
        tokio::time::sleep(QUOTA_RETRY_SLEEP).await;
        outs = ex.place_batch(orders).await?;
    }
    Ok((outs, tries))
}

/// How long the caller's own backoff contributed to a phase.
fn backoff_secs(tries: u32) -> f64 {
    tries as f64 * QUOTA_RETRY_SLEEP.as_secs_f64()
}

struct RoundRow {
    serial_req: u64,
    batch_req: u64,
    serial_wall: f64,
    batch_wall: f64,
    /// Wall clock with the caller's own quota backoff removed.
    serial_wall_net: f64,
    batch_wall_net: f64,
    /// Backoff that had to be spent this round (reported, not silently dropped).
    serial_backoff: f64,
    batch_backoff: f64,
    place_med: f64,
    cancel_med: f64,
    place_net_med: f64,
    cancel_net_med: f64,
    batch_place_ms: f64,
    batch_cancel_ms: f64,
    batch_place_net_ms: f64,
    batch_cancel_net_ms: f64,
}

pub async fn run(args: &[String]) -> Result<()> {
    let mut rounds = 3usize;
    let mut n = 5usize;
    let mut coin = "ETH".to_string();
    let mut notional = 12.0f64;
    let mut max_notional = 15.0f64;
    let mut sz_decimals = 4u32;
    let mut px_off = 0.96f64;
    let mut px_tick = 0.1f64;
    let mut quota_gate = false;
    let mut quota_reserve = 3u64;
    let mut seed_quota: Option<(u64, u64, Option<f64>)> = None;
    let mut out_path: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let value = |idx: usize| -> Result<String> {
            args.get(idx + 1)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("{} needs a value", args[idx]))
        };
        match args[i].as_str() {
            "--rounds" => {
                rounds = value(i)?.parse()?;
                i += 2;
            }
            "--n" => {
                n = value(i)?.parse()?;
                i += 2;
            }
            "--coin" => {
                coin = value(i)?;
                i += 2;
            }
            "--notional" => {
                notional = value(i)?.parse()?;
                i += 2;
            }
            "--max-notional" => {
                max_notional = value(i)?.parse()?;
                i += 2;
            }
            "--sz-decimals" => {
                sz_decimals = value(i)?.parse()?;
                i += 2;
            }
            "--px-off" => {
                px_off = value(i)?.parse()?;
                i += 2;
            }
            "--px-tick" => {
                px_tick = value(i)?.parse()?;
                i += 2;
            }
            // V12.6: exercise the pre-check. Without it the bench keeps hitting
            // the venue so the real rejection behaviour stays observable.
            "--quota-gate" => {
                quota_gate = true;
                i += 1;
            }
            "--quota-reserve" => {
                quota_reserve = value(i)?.parse()?;
                i += 2;
            }
            // Seed the ledger with `sent allowed [volume]` (typically the last
            // numbers the venue printed) so the estimate is live from request 1.
            "--seed-quota" => {
                let raw = value(i)?;
                let v: Vec<&str> = raw.split(':').collect();
                if v.len() < 2 {
                    bail!("--seed-quota wants SENT:ALLOWED[:VOLUME], got {}", raw);
                }
                seed_quota = Some((
                    v[0].parse()?,
                    v[1].parse()?,
                    v.get(2).and_then(|s| s.parse::<f64>().ok()),
                ));
                i += 2;
            }
            "--out" => {
                out_path = Some(value(i)?);
                i += 2;
            }
            other => bail!("unknown argument: {} (try --n 5 --rounds 3 --coin ETH)", other),
        }
    }
    if notional > max_notional {
        bail!("notional ${} exceeds the hard cap ${}", notional, max_notional);
    }
    if !(2..=20).contains(&n) {
        bail!("--n must be within 2..=20 (got {})", n);
    }
    if px_off >= 0.99 {
        bail!("--px-off must be < 0.99 so post-only never crosses");
    }

    let cfg = Config::from_env()?;
    let mut signer = Signer::new(cfg.private_key.clone(), cfg.address.clone());
    signer.load_asset_indices(&[coin.clone()]);
    let ex = Executor::new(cfg.clone(), signer);
    if let Some((sent, allowed, volume)) = seed_quota {
        ex.seed_quota(sent, allowed, volume);
    }
    ex.set_quota_reserve(quota_reserve);
    ex.set_quota_gate(quota_gate);

    let mids = ex.fetch_all_mids().await?;
    let mid: f64 = mids
        .get(&coin)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| anyhow::anyhow!("no mid price for {}", coin))?;

    let scale = 10f64.powi(sz_decimals as i32);
    let sz = ((notional / mid) * scale).floor() / scale;
    // Prices must be a whole number of venue ticks, otherwise the order is rejected
    // with "Price must be divisible by tick size".
    let px_steps = (mid * px_off / px_tick).floor();
    let px = (px_steps * px_tick * 1e8).round() / 1e8;
    let eff_notional = sz * mid;
    if sz <= 0.0 {
        bail!("computed size is zero for {} at mid {}", coin, mid);
    }
    if eff_notional > max_notional {
        bail!(
            "computed notional ${:.2} exceeds the cap ${:.0}",
            eff_notional,
            max_notional
        );
    }

    println!(
        "COIN={} MID={:.4} PX={:.2} SZ={} NOTIONAL=${:.2} cap=${:.0} N={} ROUNDS={}",
        coin, mid, px, sz, eff_notional, max_notional, n, rounds
    );
    println!("start quota={:?}", ex.quota_state());
    println!("start gauge={}", ex.quota_gauge().line());

    let mut rows: Vec<RoundRow> = Vec::new();

    for r in 0..rounds {
        // ── serial: one request per order, one per cancel ──
        let req0 = ex.requests_sent();
        let t0 = Instant::now();
        let mut oids: Vec<u64> = Vec::with_capacity(n);
        let mut place_ms: Vec<f64> = Vec::with_capacity(n);
        let mut place_net_ms: Vec<f64> = Vec::with_capacity(n);
        let mut serial_rejects = 0usize;
        let mut serial_tries = 0u32;
        for _ in 0..n {
            let t = Instant::now();
            let rb = ex.requests_sent();
            let (out, tries) = place_one(&ex, &coin, sz, px).await?;
            serial_tries += tries;
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            // Net = the venue round-trip only; NaN when nothing was actually sent.
            let net = if ex.requests_sent() > rb {
                ex.last_attempt_ms()
            } else {
                f64::NAN
            };
            match out {
                OrderOutcome::Rested(oid) => {
                    oids.push(oid);
                    place_ms.push(ms);
                    place_net_ms.push(net);
                }
                OrderOutcome::FilledTaker => bail!("post-only order filled — bench aborted"),
                OrderOutcome::Rejected(reason) => {
                    serial_rejects += 1;
                    println!("  serial place rejected: {:?}", reason);
                }
            }
        }
        let mut cancel_ms: Vec<f64> = Vec::with_capacity(oids.len());
        let mut cancel_net_ms: Vec<f64> = Vec::with_capacity(oids.len());
        for oid in oids.clone() {
            let t = Instant::now();
            let rb = ex.requests_sent();
            ex.cancel_order(&coin, oid).await?;
            cancel_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            if ex.requests_sent() > rb {
                cancel_net_ms.push(ex.last_attempt_ms());
            }
        }
        let serial_req = ex.requests_sent() - req0;
        let serial_wall = t0.elapsed().as_secs_f64();
        let serial_backoff = backoff_secs(serial_tries);
        let serial_wall_net = (serial_wall - serial_backoff).max(0.0);

        tokio::time::sleep(Duration::from_secs(2)).await;

        // ── batch: one request for N orders, one for N cancels ──
        let req1 = ex.requests_sent();
        let t1 = Instant::now();
        let orders: Vec<(String, bool, f64, f64, bool)> =
            (0..n).map(|_| (coin.clone(), true, sz, px, false)).collect();
        let (outs, batch_tries) = batch_retry(&ex, &orders).await?;
        let batch_place_ms = t1.elapsed().as_secs_f64() * 1000.0;
        let batch_place_net_ms = if ex.requests_sent() > req1 {
            ex.last_attempt_ms()
        } else {
            f64::NAN
        };
        let mut boids: Vec<u64> = Vec::new();
        let mut batch_rejects = 0usize;
        for out in outs {
            match out {
                OrderOutcome::Rested(oid) => boids.push(oid),
                OrderOutcome::FilledTaker => bail!("post-only batch filled — bench aborted"),
                OrderOutcome::Rejected(reason) => {
                    batch_rejects += 1;
                    println!("  batch place rejected: {:?}", reason);
                }
            }
        }
        let t2 = Instant::now();
        let rc = ex.requests_sent();
        let cancels: Vec<(String, u64)> = boids.iter().map(|o| (coin.clone(), *o)).collect();
        let cancelled = ex.cancel_batch(&cancels).await?;
        let batch_cancel_ms = t2.elapsed().as_secs_f64() * 1000.0;
        let batch_cancel_net_ms = if ex.requests_sent() > rc {
            ex.last_attempt_ms()
        } else {
            f64::NAN
        };
        let batch_req = ex.requests_sent() - req1;
        let batch_wall = t1.elapsed().as_secs_f64();
        let batch_backoff = backoff_secs(batch_tries);
        let batch_wall_net = (batch_wall - batch_backoff).max(0.0);

        let leftover = ex.open_orders_count(&coin).await.unwrap_or(9999);

        println!(
            "ROUND {} serial: req={} wall={:.2}s wall_net={:.2}s backoff={:.0}s place_med={:.0}ms place_net={:.0}ms cancel_med={:.0}ms cancel_net={:.0}ms rested={} rejected={}",
            r + 1,
            serial_req,
            serial_wall,
            serial_wall_net,
            serial_backoff,
            median(&place_ms),
            median(&place_net_ms),
            median(&cancel_ms),
            median(&cancel_net_ms),
            oids.len(),
            serial_rejects
        );
        println!(
            "ROUND {} batch : req={} wall={:.2}s wall_net={:.2}s backoff={:.0}s place={:.0}ms place_net={:.0}ms cancel={:.0}ms cancel_net={:.0}ms rested={} cancelled={} rejected={}",
            r + 1,
            batch_req,
            batch_wall,
            batch_wall_net,
            batch_backoff,
            batch_place_ms,
            batch_place_net_ms,
            batch_cancel_ms,
            batch_cancel_net_ms,
            boids.len(),
            cancelled,
            batch_rejects
        );
        println!(
            "ROUND {} leftover_open_orders={} quota={:?}",
            r + 1,
            leftover,
            ex.quota_state()
        );
        println!("ROUND {} gauge={}", r + 1, ex.quota_gauge().line());

        rows.push(RoundRow {
            serial_req,
            batch_req,
            serial_wall,
            batch_wall,
            serial_wall_net,
            batch_wall_net,
            serial_backoff,
            batch_backoff,
            place_med: median(&place_ms),
            cancel_med: median(&cancel_ms),
            place_net_med: median(&place_net_ms),
            cancel_net_med: median(&cancel_net_ms),
            batch_place_ms,
            batch_cancel_ms,
            batch_place_net_ms,
            batch_cancel_net_ms,
        });

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let serial_place_med = median(&rows.iter().map(|r| r.place_med).collect::<Vec<_>>());
    let serial_cancel_med = median(&rows.iter().map(|r| r.cancel_med).collect::<Vec<_>>());
    let batch_place_med = median(&rows.iter().map(|r| r.batch_place_ms).collect::<Vec<_>>());
    let batch_cancel_med = median(&rows.iter().map(|r| r.batch_cancel_ms).collect::<Vec<_>>());
    let serial_wall_med = median(&rows.iter().map(|r| r.serial_wall).collect::<Vec<_>>());
    let batch_wall_med = median(&rows.iter().map(|r| r.batch_wall).collect::<Vec<_>>());
    let serial_wall_net_med = median(&rows.iter().map(|r| r.serial_wall_net).collect::<Vec<_>>());
    let batch_wall_net_med = median(&rows.iter().map(|r| r.batch_wall_net).collect::<Vec<_>>());
    let serial_backoff_med = median(&rows.iter().map(|r| r.serial_backoff).collect::<Vec<_>>());
    let batch_backoff_med = median(&rows.iter().map(|r| r.batch_backoff).collect::<Vec<_>>());
    let serial_place_net_med = median(&rows.iter().map(|r| r.place_net_med).collect::<Vec<_>>());
    let serial_cancel_net_med = median(&rows.iter().map(|r| r.cancel_net_med).collect::<Vec<_>>());
    let batch_place_net_med = median(&rows.iter().map(|r| r.batch_place_net_ms).collect::<Vec<_>>());
    let batch_cancel_net_med = median(&rows.iter().map(|r| r.batch_cancel_net_ms).collect::<Vec<_>>());
    let serial_req_med = median(&rows.iter().map(|r| r.serial_req as f64).collect::<Vec<_>>());
    let batch_req_med = median(&rows.iter().map(|r| r.batch_req as f64).collect::<Vec<_>>());

    println!("\n=== SUMMARY n={} rounds={} ===", n, rounds);
    println!(
        "requests per round (quote+cancel {} orders): serial={:.0} batch={:.0}  ->  {:.1}x fewer",
        n, serial_req_med, batch_req_med,
        if batch_req_med > 0.0 { serial_req_med / batch_req_med } else { 0.0 }
    );
    println!(
        "wall per round: serial={:.2}s batch={:.2}s",
        serial_wall_med, batch_wall_med
    );
    println!(
        "wall_net per round (backoff excluded: serial {:.0}s / batch {:.0}s): serial={:.2}s batch={:.2}s",
        serial_backoff_med, batch_backoff_med, serial_wall_net_med, batch_wall_net_med
    );
    println!(
        "place ack: serial/order={:.0}ms  batch(one ack for {} orders)/order={:.0}ms",
        serial_place_med,
        n,
        batch_place_med / n as f64
    );
    println!(
        "place_ack_net (venue round-trip only): serial/order={:.0}ms  batch/order={:.0}ms",
        serial_place_net_med,
        batch_place_net_med / n as f64
    );
    println!(
        "cancel ack: serial/order={:.0}ms  batch(one ack for {} cancels)/order={:.0}ms",
        serial_cancel_med,
        n,
        batch_cancel_med / n as f64
    );
    println!(
        "cancel_ack_net (venue round-trip only): serial/order={:.0}ms  batch/order={:.0}ms",
        serial_cancel_net_med,
        batch_cancel_net_med / n as f64
    );
    println!("final quota={:?}", ex.quota_state());

    // Never leave anything behind.
    let leftover = ex.open_orders_count(&coin).await?;
    if leftover > 0 {
        println!("cleanup: {} orders still open, sweeping", leftover);
        ex.cancel_all_for_coin(&coin).await?;
        let after = ex.open_orders_count(&coin).await?;
        println!("cleanup: open orders after sweep = {}", after);
    } else {
        println!("cleanup: 0 open orders left");
    }

    let stamp = chrono::Utc::now().to_rfc3339();
    let report = json!({
        "ts": stamp,
        "coin": coin,
        "mid": mid,
        "px": px,
        "sz": sz,
        "notional": eff_notional,
        "n": n,
        "rounds": rounds,
        "serial": {"requests_median": serial_req_med, "wall_median_s": serial_wall_med,
                    "wall_net_median_s": serial_wall_net_med, "backoff_median_s": serial_backoff_med,
                    "place_ack_median_ms": serial_place_med, "cancel_ack_median_ms": serial_cancel_med,
                    "place_ack_net_median_ms": serial_place_net_med, "cancel_ack_net_median_ms": serial_cancel_net_med},
        "batch": {"requests_median": batch_req_med, "wall_median_s": batch_wall_med,
                  "wall_net_median_s": batch_wall_net_med, "backoff_median_s": batch_backoff_med,
                  "place_ack_median_ms": batch_place_med, "cancel_ack_median_ms": batch_cancel_med,
                  "place_ack_net_median_ms": batch_place_net_med, "cancel_ack_net_median_ms": batch_cancel_net_med},
        "derived": {
            "requests_per_round_serial": serial_req_med,
            "requests_per_round_batch": batch_req_med,
            "batch_place_total_ack_ms": batch_place_net_med,
            "batch_place_per_order_ack_ms": batch_place_net_med / n as f64,
            "batch_cancel_total_ack_ms": batch_cancel_net_med,
            "batch_cancel_per_order_ack_ms": batch_cancel_net_med / n as f64,
        },
        "measurement": {
            "timer": "post_exchange_raw: after signing+nonce, before HTTP send, through JSON decode",
            "excludes": ["strategy", "risk_assess", "order_params", "signing", "nonce"],
            "scope": "HTTP round-trip + venue processing + response parse",
            "clock": "std::time::Instant (monotonic)",
            "host": std::env::var("GE_HOST").unwrap_or_else(|_| "unknown".into()),
            "git_commit": std::env::var("GE_GIT_COMMIT").unwrap_or_else(|_| "unknown".into()),
        },
        "quota": {"requests_sent": ex.requests_sent(), "quota_rejects": ex.quota_rejects(),
                   "last_hint": ex.quota_hint(), "headroom": ex.quota_headroom(),
                   "headroom_est": ex.quota_gauge().headroom_est,
                   "used_est": ex.quota_gauge().used_est,
                   "requests_per_usdc": ex.quota_gauge().requests_per_usdc,
                   "usdc_to_unlock": ex.quota_gauge().usdc_to_unlock,
                   "deferred": ex.quota_gauge().deferred,
                   "state": ex.quota_gauge().state},
    });
    println!("RESULT_JSON {}", report);
    if let Some(path) = out_path {
        if let Some(parent) = std::path::Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
        println!("WROTE {}", path);
    }
    Ok(())
}
