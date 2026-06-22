//! Core Engine — per-coin state machine + risk loop + label recording.
//! Version: V12.4 (portfolio-reduce) | 17 defense layers
//!
//! The engine runs a continuous loop:
//!
//! ```text
//! cycle:
//!   1. Fetch state (account + L2 books)
//!   2. Read WS shock signal (P0: toxic burst detection)
//!   3. Gate evaluation (CROSSED→THIN_SPREAD→COARSE→TSUNAMI→SNIPER→BLOCKED)
//!   4. Place new orders FIRST (P0: place-before-cancel eliminates 150ms gap)
//!   5. Cancel stale orders SECOND (P0: old orders still protect during place)
//!   6. Check fills (order status API → position tracking)
//!   7. Risk assess (cubic skew + asymmetric qty + shed/unwind check)
//!   8. Portfolio limit check + portfolio-reduce bypass (V12.4)
//!   9. Per-coin state transition (NORMAL→PASSIVE_UNWIND→EMERGENCY_IOC→COOLDOWN→WAITING)
//!      With 20% hysteresis: enter UNWIND at <watermark>, exit at <watermark − hysteresis>
//!  10. Log metrics (per-coin state shown)
//!  11. Record label (CycleRecord → CSV)
//!  12. Sleep (3-5s + jitter to avoid 429)
//!
//! Graceful shutdown: SIGTERM/SIGINT → cancel all open orders → exit.
//!
//! ## Key Architecture Decisions (V12.4)
//!
//! - **Place-before-cancel**: Orders placed first, then stale ones cancelled.
//!   Eliminates the 150ms bare window where the book has no orders.
//! - **Portfolio-reduce bypass**: When aggregate notional exceeds the hard limit
//!   (60% equity default), position-reducing orders are still allowed. LONG positions
//!   can only SELL; SHORT positions can only BUY. Flat coins are blocked.
//!   This fixes the restart deadlock where no coin individually exceeds the 40%
//!   unwind threshold but the aggregate portfolio is over limit.
//! - **Gate runs BEFORE state transitions**: Spread conditions evaluated per-cycle;
//!   state transitions follow. A BLOCKED coin can still transition to UNWIND
//!   (unwind bypasses the gate for survival).
//! - **THIN_SPREAD gate (V12.3)**: Coins where gross_bps < maker_fee + min_margin
//!   are blocked automatically. Prevents fine-tick coins (RESOLV 0.46 bps) from
//!   trading at negative net spread.
//! - **COARSE dual-axis gate (V12.1)**: 1-tick coins with bps >= 15 enter reduced
//!   (30%) COARSE harvest mode with zero shading and asymmetric sizing.
//! - **Crossed-book gate (V12)**: When REST L2 returns best_ask ≤ best_bid,
//!   the market is untradeable for Post-Only makers → BLOCKED.
//! - **Shading cap (V12)**: Coarse-tick coins (gross_bps ≥ 30) have shading offset
//!   capped at 1 tick to prevent suicidal over-isolation.
//! ```

use crate::config::Config;
use crate::labeler::{CycleRecord, Labeler};
use crate::risk::RiskEngine;
use crate::state::{CoinStateMachine, State};
use crate::traits::MarketAdapter;
use crate::types::{MarketShockSignal, OrderOutcome, SignalBus};
use anyhow::Result;
use std::collections::HashMap;
use futures_util::FutureExt;
use rand::Rng;
use tokio::signal;
use tokio::sync::watch;

/// Intra-cycle rate limiting: 600ms delay between API calls to stay under 2 req/s.
const RATE_LIMIT_DELAY_MS: u64 = 600;
async fn rate_limit_delay() {
    tokio::time::sleep(std::time::Duration::from_millis(RATE_LIMIT_DELAY_MS)).await;
}

/// Metrics for one cycle.
#[derive(Debug, Default)]
struct CycleMetrics {
    coin: String,
    state: State,
    gross_spread_bps: f64,
    net_spread_bps: f64,
    skew_bps: f64,
    skew_ticks: i64,
    position_ratio: f64,
    buy_sz: f64,
    sell_sz: f64,
    size_multiplier: f64,
    gate_mode: String,
    placed_buy: bool,
    placed_sell: bool,
    shed_filled: f64,
    withdrawable: f64,
    equity: f64,
    cycle_ms: u64,
    /// P0: whether defensive shading was active this cycle
    toxic_defense_active: bool,
}

/// Maximum extra ticks to retreat when toxic burst detected.
const TOXIC_DEFENSE_TICKS: u32 = 2;

/// Run the main engine loop.
pub async fn run<A: MarketAdapter>(
    cfg: Config,
    bus: SignalBus,
    mut shock_rx: watch::Receiver<MarketShockSignal>,
    mut adapter: A,
    mut labeler: Labeler,
) -> Result<()> {
    let risk = RiskEngine::new(cfg.clone());
        let mut coin_state = CoinStateMachine::new();
    // P0+: Track chain position between cycles to detect WS-missed fills
    let mut last_known_position: HashMap<String, f64> = HashMap::new();
    let mut last_pos_sign: HashMap<String, i32> = HashMap::new();
    let mut rng = rand::thread_rng();

    // B0: Bootstrap — recover existing exchange positions into FSM at cold start
    if let Err(e) = bootstrap_recovery(&mut adapter, &cfg, &bus, &mut coin_state).await {
        tracing::warn!(?e, "bootstrap recovery failed, continuing with IDLE");
    }

    // Active order tracking: (coin, oid)
    let mut active_orders: Vec<(String, u64)> = Vec::new();

    // S1: Graceful shutdown via SIGTERM / SIGINT
    let mut sigterm = Box::pin(signal::ctrl_c().fuse());

    tracing::info!("engine started (graceful shutdown: SIGTERM/SIGINT, toxic defense: ON)");

    loop {
        let cycle_start = std::time::Instant::now();

        // ── S1 shutdown check at start of cycle ──
        if sigterm.as_mut().now_or_never().is_some() {
            tracing::info!("SIGTERM received at cycle start, cancelling all open orders...");
            graceful_shutdown(&mut adapter, &cfg, &active_orders).await;
            return Ok(());
        }

        // ── 0. P0: Read latest WS shock signal (non-blocking) ──
        let shock = shock_rx.borrow_and_update().clone();
        let toxic_active = shock.is_toxic_burst();
        if toxic_active {
            tracing::warn!(
                fills_count = shock.recent_fills_count,
                direction_skew = shock.direction_skew,
                attack_side = ?shock.attack_side(),
                "TOXIC BURST DETECTED — defensive shading active"
            );
        }

        // ── 1. Fetch state ──
        let account = match adapter.fetch_state().await {
            Ok(s) => {
                *bus.account.write() = s.clone();
                s
            }
            Err(e) => {
                tracing::warn!(?e, "failed to fetch account state, using last known");
                bus.account.read().clone()
            }
        };

        // ── B3/S4: Total portfolio hard limit (cross-coin guard, checked BEFORE per-coin loop) ──
        let total_notional: f64 = account
            .positions
            .iter()
            .filter(|p| cfg.coins.contains(&p.coin))
            .map(|p| {
                let mid = bus.book(&p.coin).mid_price().unwrap_or(p.entry_px);
                (p.size.abs() * mid).abs()
            })
            .sum();
        let portfolio_over_limit = !risk.portfolio_under_limit(total_notional, account.equity);
        if portfolio_over_limit {
            tracing::warn!(
                total_notional = total_notional,
                equity = account.equity,
                limit_ratio = %cfg.hard_limit_ratio,
                "PORTFOLIO HARD LIMIT: freezing flat coins"
            );
            for c in &cfg.coins {
                let has_pos = account.positions.iter().any(|p| &p.coin == c && p.size.abs() > 0.001);
                if matches!(coin_state.state_of(c), State::Active) && !has_pos {
                    coin_state.transition(c, State::Cooldown);
                }
            }
        }

        // ── 2. Process each coin ──
        for coin in &cfg.coins.clone() {
            let current_state = coin_state.state_of(coin);

            let mut metrics = CycleMetrics {
                coin: coin.clone(),
                state: current_state,
                toxic_defense_active: toxic_active,
                ..Default::default()
            };

            // ── S2: Get L2 book with staleness guard ──
            let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;

            let book_stale = {
                let books = bus.books.read();
                books.get(coin).map_or(true, |b| {
                    b.best_bid().is_none() || b.timestamp < now_ms.saturating_sub(10_000)
                })
            };

            let book = if book_stale {
                rate_limit_delay().await;
                        
                        match adapter.fetch_l2(coin).await {
                    Ok(b) => {
                        let mut books = bus.books.write();
                        let still_stale = books.get(coin).map_or(true, |existing| {
                            existing.timestamp < now_ms.saturating_sub(10_000)
                        });
                        if still_stale {
                            books.insert(coin.clone(), b.clone());
                        }
                        b
                    }
                    Err(_) => bus.book(coin),
                }
            } else {
                bus.book(coin)
            };

            // ── 3. Risk assess ──
            let risk_out = match risk.assess(&book, &account, coin) {
                Some(r) => r,
                None => {
                    tracing::debug!(coin = %coin, "no book data, skipping");
                    continue;
                }
            };

            // ── P0: Apply defensive shading if toxic burst active ──
            // Retreat the attacked side by TOXIC_DEFENSE_TICKS to avoid being
            // picked off by the informed taker. Only in NORMAL/UNWIND states
            // (not during Shedding which already uses aggressive IOC pricing).
            let (defensive_bid_px, defensive_ask_px) = if toxic_active
                && matches!(current_state, State::Active | State::Unwind)
            {
                let tick = cfg.tick_for(coin);
                let attack = shock.attack_side();
                let mut bid = risk_out.bid_px;
                let mut ask = risk_out.ask_px;

                match attack {
                    Some("BUY") => {
                        // Taker is buying → our asks are being eaten → retreat ask UP
                        let retreated = ask + (TOXIC_DEFENSE_TICKS as f64 * tick);
                        tracing::info!(
                            coin = %coin,
                            base_ask = ask,
                            defended_ask = retreated,
                            retreat_ticks = TOXIC_DEFENSE_TICKS,
                            "toxic defense: ask retreated"
                        );
                        ask = retreated;
                    }
                    Some("SELL") => {
                        // Taker is selling → our bids are being eaten → retreat bid DOWN
                        let retreated = bid - (TOXIC_DEFENSE_TICKS as f64 * tick);
                        tracing::info!(
                            coin = %coin,
                            base_bid = bid,
                            defended_bid = retreated,
                            retreat_ticks = TOXIC_DEFENSE_TICKS,
                            "toxic defense: bid retreated"
                        );
                        bid = retreated;
                    }
                    _ => {
                        // Mixed flow → retreat both sides slightly
                        bid -= TOXIC_DEFENSE_TICKS as f64 * tick;
                        ask += TOXIC_DEFENSE_TICKS as f64 * tick;
                    }
                }

                (bid, ask)
            } else {
                (risk_out.bid_px, risk_out.ask_px)
            };

            metrics.gross_spread_bps = risk_out.gross_spread_bps;
            metrics.net_spread_bps = risk_out.net_spread_bps;
            metrics.skew_bps = risk_out.skew_bps;
            metrics.skew_ticks = risk_out.skew_ticks;
            metrics.position_ratio = risk_out.position_ratio;
            metrics.withdrawable = account.withdrawable;
            metrics.equity = account.equity;

            let pos = account.positions.iter()
                .find(|p| &p.coin == coin).cloned().unwrap_or_default();

            // ── P0+: Detect position changes missed by WS fill events ──
            {
                let chain_size = pos.size;
                if let Some(&last) = last_known_position.get(coin) {
                    let delta = chain_size - last;
                    if delta.abs() > 0.001 {
                        tracing::warn!(
                            coin = %coin,
                            delta,
                            from = last,
                            to = chain_size,
                            "CHAINT POSITION DRIFT: WS fill event missed, {:.4} position change", delta
                        );
                    }
                }
                last_known_position.insert(coin.clone(), chain_size);
            }

            // ── V12 动态阶梯闸门 + Crossed-Book 检测 ──
            let coin_tick = cfg.tick_for(coin);
            // V12: Detect crossed book (best_ask <= best_bid). REST-verified, NOT local desync.
            // In a CLOB, crossed books mean ghost liquidity — real trades happen at the crossing
            // point, which Post-Only orders can never reach. Force GATE_BLOCKED.
            let (gross_ticks, book_crossed) = match (book.best_bid(), book.best_ask()) {
                (Some(bid), Some(ask)) if ask > bid => ((ask - bid) / coin_tick.max(1e-9), false),
                (Some(_bid), Some(_ask)) => (0.0, true),
                _ => (999.0, false),
            };
            let gi = gross_ticks.round() as u32;

            // ── V12.1: coarse tick bps for dual-axis gate ──
            let gross_bps = risk_out.gross_spread_bps;

            let (gate_mode_str, size_multiplier) = if book_crossed {
                tracing::warn!(
                    coin = %coin,
                    "CROSSED_BOOK: best_ask <= best_bid (REST-verified), forcing GATE_BLOCKED"
                );
                ("CROSSED", 0.0)
            } else if gross_bps < cfg.maker_fee_bps + cfg.min_margin_bps {
                // V12.3: Net-spread safety gate — block if gross can't cover
                // maker fee + safety margin even at 100% maker fill rate.
                // Prevents fine-tick coins (e.g. RESOLV 0.46 bps 1-tick) from
                // trading at negative net spread.
                tracing::warn!(
                    coin = %coin,
                    gross_bps = %format!("{:.1}", gross_bps),
                    maker_fee = cfg.maker_fee_bps,
                    min_margin = cfg.min_margin_bps,
                    needed = cfg.maker_fee_bps + cfg.min_margin_bps,
                    "SPREAD_TOO_THIN: gross {:.1} < maker_fee {} + margin {} = {:.1} — blocking",
                    gross_bps, cfg.maker_fee_bps, cfg.min_margin_bps,
                    cfg.maker_fee_bps + cfg.min_margin_bps
                );
                ("THIN_SPREAD", 0.0)
            } else if gross_bps >= cfg.coarse_tick_bps_threshold && gi == 1 {
                // V12.1: Coarse-tick harvest — 1 tick ≥ 15 bps (e.g. HMSTR 53 bps)
                // Zero shading, 20% size, sensitive skew control.
                tracing::info!(
                    coin = %coin,
                    gross_ticks = %format!("{:.1}", gross_ticks),
                    gross_bps = %format!("{:.1}", gross_bps),
                    size_pct = cfg.coarse_tick_size_pct,
                    "COARSE_TICK_HARVEST {:.1}x — 1-tick fat margin", cfg.coarse_tick_size_pct
                );
                ("COARSE", cfg.coarse_tick_size_pct)
            } else if gi >= cfg.tsunami_ticks {
                tracing::debug!(
                    coin = %coin,
                    gross_ticks = %format!("{:.1}", gross_ticks),
                    tsunami_min = cfg.tsunami_ticks,
                    "TSUNAMI_HARVEST 1.0x"
                );
                ("TSUNAMI", 1.0)
            } else if gi >= cfg.gate_block_ticks {
                ("SNIPER", 0.4)
            } else {
                ("BLOCKED", 0.0)
            };

            // Persist multiplier for order sizing
            let old_gate = coin_state.get_or_init(coin).gate_mode.clone();
            coin_state.get_or_init(coin).size_multiplier = size_multiplier;
            coin_state.get_or_init(coin).gate_mode = gate_mode_str.to_string();

            // V12.1: Reset fill tallies on gate mode change (new market regime)
            if old_gate != gate_mode_str {
                coin_state.reset_fill_tallies(coin);
                tracing::debug!(coin=%coin, old=%old_gate, new=gate_mode_str,
                    "gate mode changed → fill tallies reset");
            }

            if size_multiplier == 0.0 {
                if !coin_state.is_gate_blocked(coin) {
                    tracing::warn!(
                        coin = %coin,
                        gross_ticks = %format!("{:.1}", gross_ticks),
                        gate_threshold = cfg.gate_block_ticks,
                        "GATE_BLOCKED: cancelling all, gross {:.1} < {} ticks", gross_ticks, cfg.gate_block_ticks
                    );
                    adapter.cancel_all_for_coin(coin).await.ok();
                    coin_state.enter_gate_blocked(coin,
                        &format!("gross {:.1} < {}", gross_ticks, cfg.gate_block_ticks));
                }
            } else {
                if coin_state.is_gate_blocked(coin) {
                    let cs = coin_state.get_or_init(coin);
                    cs.favorable_cycles += 1;
                    if cs.favorable_cycles >= cfg.spread_stable_cycles {
                        tracing::info!(coin=%coin, gross_ticks=%format!("{:.1}", gross_ticks),
                            mode=gate_mode_str, size_mult=%format!("{:.1}x", size_multiplier),
                            "GATE LIFTED → {}", gate_mode_str);
                        coin_state.transition(coin, State::Active);
                    }
                }
            }

            // ── 4. Per-coin state transitions ──
            match current_state {
                State::Idle => {
                    if risk.has_reserve(account.withdrawable)
                        && risk_out.gross_spread_bps > 0.0
                    {
                        coin_state.transition(coin, State::ColdStart);
                    }
                }

                State::ColdStart => {
                    if coin_state.advance_cold_start(coin) {
                        coin_state.transition(coin, State::Active);
                    }
                }

                State::Active => {
                    if risk_out.should_shed {
                        tracing::warn!(
                            coin = %coin,
                            position_ratio = risk_out.position_ratio,
                            "EMERGENCY_IOC triggered at 90%"
                        );
                        coin_state.transition(coin, State::Shedding);
                    } else if !risk.has_reserve(account.withdrawable) {
                        if pos.size.abs() < 0.001 {
                            tracing::warn!(
                                coin = %coin,
                                withdrawable = account.withdrawable,
                                "reserve depleted with zero position, cooldown"
                            );
                            coin_state.transition(coin, State::Cooldown);
                        }
                    } else {
                        let (should_unwind, _) = risk.unwind_check(
                            risk_out.position_ratio, pos.size
                        );
                        if should_unwind {
                            tracing::info!(
                                coin = %coin,
                                position_ratio = risk_out.position_ratio,
                                watermark = %cfg.passive_unwind_watermark,
                                "entering PASSIVE_UNWIND"
                            );
                            coin_state.transition(coin, State::Unwind);
                        }
                        // Active but conditions deteriorated -> Waiting
                        let rtc = cfg.roundtrip_bps();
                        let msp = rtc + cfg.min_margin_bps;
                        if !should_unwind && risk_out.net_spread_bps <= msp && matches!(coin_state.state_of(coin), State::Active) {
                            let reason = format!("net_spread {:.2} <= min {:.2} bps (deteriorated)", risk_out.net_spread_bps, msp);
                            tracing::warn!(coin=%coin, reason=%reason, "Active: spread collapsed -> Waiting");
                            coin_state.enter_waiting(coin, &reason);
                        }
                        if !should_unwind && portfolio_over_limit && matches!(coin_state.state_of(coin), State::Active) {
                            tracing::warn!(coin=%coin, "Active: portfolio over limit -> Waiting");
                            coin_state.enter_waiting(coin, "portfolio over limit (deteriorated)");
                        }
                    }
                }

                State::Unwind => {
                    if risk.unwind_safe(risk_out.position_ratio) {
                        coin_state.reset_unwind_cooldown(coin, 5);
                        tracing::info!(
                            coin = %coin,
                            position_ratio = risk_out.position_ratio,
                            exit_threshold = %(cfg.passive_unwind_watermark - cfg.unwind_hysteresis),
                            "passive unwind complete → NORMAL (5-cycle cooldown)"
                        );
                        coin_state.transition(coin, State::Active);
                    } else if risk_out.should_shed {
                        tracing::warn!(
                            coin = %coin,
                            position_ratio = risk_out.position_ratio,
                            "PASSIVE_UNWIND failing, escalating to EMERGENCY_IOC"
                        );
                        coin_state.transition(coin, State::Shedding);
                    }
                }

                State::Shedding => {
                    let mut shed_iterations = 0u32;
                    const MAX_SHED_ITERATIONS: u32 = 6;
                    let mut total_shed = 0.0_f64;
                    let mut shed_side = String::new();

                    loop {
                        rate_limit_delay().await;
                        
                        let fresh_book = match adapter.fetch_l2(coin).await {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::warn!(?e, coin = %coin, "EMERGENCY_IOC: L2 fetch failed");
                                break;
                            }
                        };

                        let recheck = match risk.assess(&fresh_book, &account, coin) {
                            Some(r) => r,
                            None => break,
                        };

                        shed_side = recheck.shed_side.clone().unwrap_or_default();

                        if risk.shed_safe(recheck.position_ratio) {
                            tracing::info!(
                                coin = %coin,
                                position_ratio = recheck.position_ratio,
                                "EMERGENCY_IOC: position back to safe zone"
                            );
                            coin_state.reset_zero_shed(coin);
                            break;
                        }

                        if shed_iterations >= MAX_SHED_ITERATIONS {
                            tracing::warn!(
                                coin = %coin,
                                iterations = shed_iterations,
                                position_ratio = recheck.position_ratio,
                                "EMERGENCY_IOC: max iterations reached, forcing cooldown"
                            );
                            break;
                        }

                        shed_iterations += 1;

                        let is_buy = shed_side == "BUY";
                        let aggressive_px = if is_buy {
                            fresh_book.best_ask().unwrap_or(0.0)
                        } else {
                            fresh_book.best_bid().unwrap_or(0.0)
                        };

                        if aggressive_px <= 0.0 || recheck.shed_size <= 0.0 {
                            break;
                        }

                        match adapter
                            .place_ioc_order(
                                coin,
                                is_buy,
                                recheck.shed_size,
                                aggressive_px,
                            )
                            .await
                        {
                            Ok(Some(filled)) => {
                                total_shed += filled;
                                tracing::info!(
                                    coin = %coin,
                                    side = %shed_side,
                                    filled = filled,
                                    iteration = shed_iterations,
                                    "IOC shed filled"
                                );
                            }
                            Ok(None) => {
                                tracing::debug!(coin = %coin, "IOC shed filled instantly (full fill)");
                            }
                            Err(e) => {
                                tracing::warn!(coin = %coin, ?e, "IOC shed failed");
                            }
                        }

                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }

                    if total_shed.abs() < 0.001 {
                        let rounds = coin_state.increment_zero_shed(coin);
                        tracing::warn!(
                            coin = %coin,
                            rounds,
                            "IOC dry round {} — all IOC attempts failed",
                            rounds
                        );
                        if rounds >= 3 {
                            tracing::warn!(
                                coin = %coin,
                                "IOC dry 3+ cycles, GTC last resort triggered"
                            );
                            let mid = book.mid_price().unwrap_or(0.0);
                            let gtc_px = if shed_side == "SELL" {
                                (book.best_bid().unwrap_or(mid) * 0.95).max(0.0)
                            } else {
                                book.best_ask().unwrap_or(mid) * 1.05
                            };
                            let _tick = cfg.tick_for(coin);
                            let pos_sz = account.positions.iter()
                                .find(|p| &p.coin == coin)
                                .map(|p| p.size.abs())
                                .unwrap_or(0.0);
                            let gtc_sz = (pos_sz * mid * 0.5).max(cfg.min_order_size * mid);
                            if gtc_px > 0.0 && gtc_sz > 0.0 {
                                let gtc_qty = gtc_sz / gtc_px.max(0.000001);
                                match adapter
                                    .place_gtc_order(
                                        coin,
                                        shed_side == "BUY",
                                        gtc_qty,
                                        gtc_px,
                                        false,
                                    )
                                    .await
                                {
                                    Ok(OrderOutcome::Rested(oid)) => {
                                        tracing::info!(
                                            coin = %coin,
                                            oid,
                                            px = gtc_px,
                                            sz = gtc_qty,
                                            "GTC last-resort order placed"
                                        );
                                        active_orders.push((coin.clone(), oid));
                                    }
                                    Ok(OrderOutcome::FilledTaker) => {
                                        tracing::info!(coin = %coin, px = gtc_px, sz = gtc_qty, "GTC last-resort filled as taker");
                                    }
                                    Ok(OrderOutcome::Rejected(reason)) => {
                                        tracing::warn!(coin = %coin, ?reason, "GTC last-resort order rejected");
                                    }
                                    Err(e) => {
                                        tracing::error!(coin = %coin, ?e, "GTC last-resort order failed");
                                    }
                                }
                            }
                            coin_state.reset_zero_shed(coin);
                        }
                    } else {
                        coin_state.reset_zero_shed(coin);
                    }

                    metrics.shed_filled = total_shed;
                    coin_state.transition(coin, State::Cooldown);
                }

                State::Cooldown => {
                    if coin_state.cooldown_done(coin) {
                        let rt = cfg.roundtrip_bps();
                        let msp = rt + cfg.min_margin_bps;
                        if risk_out.net_spread_bps > msp && !portfolio_over_limit && risk.has_reserve(account.withdrawable) {
                            tracing::info!(coin=%coin, net_spread=risk_out.net_spread_bps, min_spread=msp, "cooldown elapsed, conditions favorable -> Active");
                            coin_state.transition(coin, State::Active);
                        } else {
                            let reason = if risk_out.net_spread_bps <= msp {
                                format!("net_spread {:.2} <= min {:.2} bps", risk_out.net_spread_bps, msp)
                            } else if portfolio_over_limit { "portfolio over limit".to_string() }
                            else { format!("reserve depleted (wd={})", account.withdrawable) };
                            tracing::warn!(coin=%coin, reason=%reason, "cooldown elapsed but conditions not met -> Waiting");
                            coin_state.enter_waiting(coin, &reason);
                        }
                    }
                }

                State::GateBlocked => {
                    // Orders already cancelled on entry; no-op until gate lifts
                    tracing::debug!(coin=%coin, "GateBlocked: holding");
                }

                State::Waiting => {
                    let rt = cfg.roundtrip_bps();
                    let msp = rt + cfg.min_margin_bps;
                    if risk_out.net_spread_bps > msp && !portfolio_over_limit && risk.has_reserve(account.withdrawable) {
                        if coin_state.tick_waiting(coin, cfg.spread_stable_cycles) {
                            tracing::info!(coin=%coin, favorable_cycles=cfg.spread_stable_cycles, net_spread=risk_out.net_spread_bps, "Waiting: conditions met -> Active");
                            coin_state.transition(coin, State::Active);
                        }
                    } else {
                        coin_state.get_or_init(coin).favorable_cycles = 0;
                    }
                }
            }

            // Tick post-UNWIND cooldown (after all state transitions)
            coin_state.tick_unwind_cooldown(coin);

            // ── V9 Directional Freeze: after flip, BAN opposite-side opens for N cycles ──
            let current_pos_sign = if pos.size > 0.001 { 1 }
                else if pos.size < -0.001 { -1 }
                else { 0 };
            if current_pos_sign != 0 {
                let is_frozen = coin_state.is_frozen(coin);
                let has_sign = last_pos_sign.get(coin).copied().unwrap_or(0) != 0;
                let sign_changed = current_pos_sign != last_pos_sign.get(coin).copied().unwrap_or(0);
                let did_flip = !is_frozen && sign_changed && has_sign;
                if did_flip {
                    tracing::warn!(
                        coin = %coin,
                        old_sign = last_pos_sign.get(coin).copied().unwrap_or(0),
                        new_sign = current_pos_sign,
                        pos_size = pos.size,
                        "FLIP DETECTED -> directional freeze 50 cycles (ban opposite side)"
                    );
                    coin_state.set_freeze(coin, current_pos_sign as i8);
                }
                last_pos_sign.insert(coin.clone(), current_pos_sign);
            }
            coin_state.tick_freeze(coin);

            // ── 5. P0: Place orders BEFORE cancel (eliminates 150ms bare window) ──
            // V12.4: Portfolio-over-limit no longer blocks position-reducing orders.
            // This aligns with the bootstrap design intent: "natural skew + quadratic
            // qty" progressive rebalancing IS a reducing action — blocking it creates
            // a deadlock where no coin enters UNWIND individually but aggregate is over limit.
            let can_place = coin_state.can_place_orders(coin);
            let portfolio_reduce = portfolio_over_limit && pos.size.abs() > 0.001;
            if portfolio_reduce {
                tracing::warn!(
                    coin = %coin,
                    pos_size = pos.size,
                    pos_ratio_pct = %format!("{:.1}", risk_out.position_ratio * 100.0),
                    "PORTFOLIO OVER LIMIT: reducing-only mode — block same-side, allow opposite-side"
                );
            }
            // UNWIND: skip profitability check (survival > profit)
            let is_unwind = coin_state.is_unwind(coin);
            // Gate: size_multiplier=0.0 → no orders placed (already handled)
            let roundtrip = cfg.roundtrip_bps();
            let min_spread = roundtrip + cfg.min_margin_bps;
            let spread_ok = risk_out.net_spread_bps > min_spread;
            if (can_place && spread_ok) || is_unwind || portfolio_reduce {
                // V9 Directional Freeze: ban opposite-side opens (Path B)
                let freeze_dir = coin_state.freeze_direction_for(coin);
                let is_frozen = coin_state.is_frozen(coin);
                let mut buy_sz = {
                let cs = coin_state.get_or_init(coin);
                risk_out.buy_sz * cs.size_multiplier
            };
                let mut sell_sz = {
                let cs = coin_state.get_or_init(coin);
                risk_out.sell_sz * cs.size_multiplier
            };
                if is_frozen {
                    // Freeze direction is the NEW direction after flip.
                    // If we flipped TO LONG (freeze_dir=1): ban SELL (which opens SHORT).
                    // If we flipped TO SHORT (freeze_dir=-1): ban BUY (which opens LONG).
                    if freeze_dir == -1 { buy_sz = 0.0; }  // SHORT freeze: no BUY
                    if freeze_dir == 1  { sell_sz = 0.0; } // LONG freeze: no SELL
                    tracing::info!(
                        coin = %coin,
                        freeze_dir,
                        buy_sz,
                        sell_sz,
                        "directional freeze: opposite side BANNED"
                    );
                }

                // V12.1: COARSE mode sensitive skew — block side after N same-side fills
                {
                    let cs = coin_state.get_or_init(coin);
                    let gate = cs.gate_mode.clone();
                    let buys = cs.buy_fills_tally;
                    let sells = cs.sell_fills_tally;
                    if gate == "COARSE" {
                        let max_side = cfg.coarse_tick_max_side_fills;
                        if buys >= max_side {
                            buy_sz = 0.0;
                            tracing::warn!(coin=%coin, buys, max_side,
                                "COARSE sensitive skew: BUY blocked after {buys} fills");
                        }
                        if sells >= max_side {
                            sell_sz = 0.0;
                            tracing::warn!(coin=%coin, sells, max_side,
                                "COARSE sensitive skew: SELL blocked after {sells} fills");
                        }
                        if buys == 0 && sells == 0 {
                            tracing::debug!(coin=%coin, "COARSE: both sides clear");
                        }
                    // V12.2: Asymmetric sizing for coarse-tick markets
                    // When position accumulates heavily on one side, shrink the adverse side and
                    // boost the favorable (unwind) side. Prevents free-option writing.
                    {
                        let gate_v122 = coin_state.get_or_init(coin).gate_mode.clone();
                        if gate_v122 == "COARSE" {
                            let pr_abs = risk_out.position_ratio.abs();
                            let favor_buy = pos.size < 0.0;
                            let favor_sell = pos.size > 0.0;
                            if pr_abs > cfg.coarse_pos_ratio_aggressive && pr_abs <= cfg.coarse_pos_ratio_hard_kill {
                                let boost = cfg.coarse_unwind_size_boost;
                                if favor_buy {
                                    buy_sz *= boost;
                                    sell_sz = sell_sz.min(buy_sz * 0.25);
                                    tracing::info!(coin=%coin, pr_pct=%format!("{:.1}", pr_abs*100.0),
                                        buy_sz=%format!("{:.4}", buy_sz), sell_sz=%format!("{:.4}", sell_sz),
                                        "COARSE asymmetric: SHORT pos -> boost BUY {:.1}x, cap SELL", boost);
                                } else if favor_sell {
                                    sell_sz *= boost;
                                    buy_sz = buy_sz.min(sell_sz * 0.25);
                                    tracing::info!(coin=%coin, pr_pct=%format!("{:.1}", pr_abs*100.0),
                                        buy_sz=%format!("{:.4}", buy_sz), sell_sz=%format!("{:.4}", sell_sz),
                                        "COARSE asymmetric: LONG pos -> boost SELL {:.1}x, cap BUY", boost);
                                }
                            } else if pr_abs > cfg.coarse_pos_ratio_hard_kill {
                                let boost = cfg.coarse_unwind_size_boost;
                                if favor_buy {
                                    buy_sz *= boost;
                                    sell_sz = 0.0;
                                    tracing::warn!(coin=%coin, pr_pct=%format!("{:.1}", pr_abs*100.0),
                                        buy_sz=%format!("{:.4}", buy_sz),
                                        "COARSE hard kill: SELL ZERO, BUY {:.1}x", boost);
                                } else if favor_sell {
                                    sell_sz *= boost;
                                    buy_sz = 0.0;
                                    tracing::warn!(coin=%coin, pr_pct=%format!("{:.1}", pr_abs*100.0),
                                        sell_sz=%format!("{:.4}", sell_sz),
                                        "COARSE hard kill: BUY ZERO, SELL {:.1}x", boost);
                                }
                            }
                        }
                    }

                    }
                }

                let coin_tick = cfg.tick_for(coin);

                let cooldown_left = coin_state.unwind_cooldown_left(coin);
                let freeze_buy = (is_unwind && pos.size > 0.0)
                    || (cooldown_left > 0 && pos.size > 0.0)
                    || (portfolio_over_limit && pos.size > 0.001);
                let freeze_sell = (is_unwind && pos.size < 0.0)
                    || (cooldown_left > 0 && pos.size < 0.0)
                    || (portfolio_over_limit && pos.size < -0.001);
                if cooldown_left > 0 {
                    tracing::debug!(coin=%coin, cooldown_left, "post-UNWIND cooldown active");
                }

                // P0: Collect new order OIDs in a separate vec to detect double-fill
                let mut new_oids: Vec<u64> = Vec::new();

                // Place BUY order
                if !freeze_buy && buy_sz > 0.0 {
                    let cs = coin_state.get_or_init(coin);
                metrics.size_multiplier = cs.size_multiplier;
                metrics.gate_mode = cs.gate_mode.clone();
                let cs_sz = coin_state.get_or_init(coin);
                metrics.size_multiplier = cs_sz.size_multiplier;
                metrics.gate_mode = cs_sz.gate_mode.clone();
                metrics.buy_sz = buy_sz;
                    if is_unwind {
                        // Unwind BUY: GTC non-Post-Only at best_ask to cross spread.
                        // place_limit_order uses Alo (rejected when crossing) → use GTC instead.
                        let aggressive_px = book.best_ask().unwrap_or(defensive_bid_px + coin_tick);
                        // 🩹 Residual size capping: prevent UNWIND over-rotation near zero position
                        let mid_for_cap = book.mid_price().unwrap_or(aggressive_px);
                        let current_pos_value = (pos.size.abs() * mid_for_cap).abs();
                        let unwind_buy_sz = if current_pos_value < cfg.base_order_notional * 1.5 {
                            let qty = current_pos_value / aggressive_px.max(0.000001);
                            let value = qty * aggressive_px;
                            if value < cfg.min_order_size {
                                tracing::info!(coin=%coin, current_pos_value, "UNWIND residual too small, skipping buy");
                                0.0
                            } else {
                                tracing::info!(coin=%coin, qty=qty, value=value, current_pos_value, "UNWIND precision cleanup buy");
                                qty
                            }
                        } else {
                            risk_out.buy_sz
                        };
                        if unwind_buy_sz > 0.0 {
                            match adapter
                                .place_gtc_order(coin, true, unwind_buy_sz, aggressive_px, false)
                            .await
                        {
                            Ok(OrderOutcome::Rested(oid)) => {
                                new_oids.push(oid);
                                metrics.placed_buy = true;
                            }
                            Ok(OrderOutcome::FilledTaker) => {
                                metrics.placed_buy = true;
                                tracing::info!(coin=%coin, "unwind BUY filled as taker");
                            }
                            Ok(OrderOutcome::Rejected(reason)) => {
                                tracing::warn!(coin=%coin, ?reason, "unwind BUY rejected");
                            }
                            Err(e) => tracing::warn!(coin=%coin, side="BUY", ?e, "unwind order error"),
                        }
                        }  // close if unwind_buy_sz > 0.0
                    } else {
                        let gate_mode = coin_state.get_or_init(coin).gate_mode.clone();
                        let mut shading_offset = calculate_bid_tick_offset(
                            coin_state.ask_rejection_count(coin),
                            risk_out.skew_bps,
                        );
                        // V12.1: COARSE mode — zero shading (can't retreat 1 tick in 1-tick spread)
                        if gate_mode == "COARSE" {
                            shading_offset = 0;
                        // V12: Coarse tick hard cap — 1-tick gross ≥30bps already fattens enough
                        } else if risk_out.gross_spread_bps >= 30.0 {
                            shading_offset = shading_offset.min(1);
                        }
                        let shaded_bid = apply_bid_shading(defensive_bid_px, shading_offset, coin_tick);
                        if shading_offset > 0 || toxic_active {
                            tracing::info!(
                                coin = %coin,
                                base_bid = defensive_bid_px,
                                shaded_bid,
                                offset_ticks = shading_offset,
                                ask_rejections = coin_state.ask_rejection_count(coin),
                                toxic_defense = toxic_active,
                                "bid shading applied"
                            );
                        }
                        rate_limit_delay().await;
                        
                        match adapter
                            .place_limit_order(coin, true, risk_out.buy_sz, shaded_bid, false)
                            .await
                        {
                            Ok(OrderOutcome::Rested(oid)) => {
                                new_oids.push(oid);
                                metrics.placed_buy = true;
                                coin_state.reset_ask_rejections(coin);
                            }
                            Ok(OrderOutcome::FilledTaker) => {
                                metrics.placed_buy = true;
                                coin_state.reset_ask_rejections(coin);
                            }
                            Ok(OrderOutcome::Rejected(reason)) => {
                                tracing::debug!(coin = %coin, ?reason, "buy order rejected");
                            }
                            Err(e) => tracing::warn!(coin = %coin, side = "BUY", ?e, "order error"),
                        }
                    }
                }

                // Place SELL order
                if !freeze_sell && sell_sz > 0.0 {
                    metrics.sell_sz = sell_sz;
                    if is_unwind && sell_sz > 0.0 {
                        // Unwind SELL: GTC non-Post-Only at best_bid to cross spread
                        let aggressive_px = book.best_bid().unwrap_or(defensive_ask_px - coin_tick);
                        // 🩹 Residual capping: prevent over-rotation near zero position
                        let mid_for_cap2 = book.mid_price().unwrap_or(aggressive_px);
                        let current_pos_value2 = (pos.size.abs() * mid_for_cap2).abs();
                        let unwind_sell_sz = if current_pos_value2 < cfg.base_order_notional * 1.5 {
                            let qty = current_pos_value2 / aggressive_px.max(0.000001);
                            if qty * aggressive_px < cfg.min_order_size {
                                tracing::info!(coin=%coin, pos_value=current_pos_value2,
                                    "UNWIND residual too small, skip sell");
                                0.0
                            } else {
                                tracing::info!(coin=%coin, qty=qty, pos_value=current_pos_value2,
                                    "UNWIND precision cleanup sell");
                                qty
                            }
                        } else {
                            risk_out.sell_sz
                        };
                        if unwind_sell_sz > 0.0 {
                            match adapter
                                .place_gtc_order(coin, false, unwind_sell_sz, aggressive_px, false)
                            .await
                        {
                            Ok(OrderOutcome::Rested(oid)) => {
                                new_oids.push(oid);
                                metrics.placed_sell = true;
                            }
                            Ok(OrderOutcome::FilledTaker) => {
                                metrics.placed_sell = true;
                                tracing::info!(coin=%coin, "unwind SELL filled as taker");
                            }
                            Ok(OrderOutcome::Rejected(reason)) => {
                                tracing::warn!(coin=%coin, ?reason, "unwind SELL rejected");
                            }
                            Err(e) => tracing::warn!(coin=%coin, side="SELL", ?e, "unwind order error"),
                        }
                        }  // close if unwind_sell_sz > 0.0
                    } else {
                        // ASK shading: retreat SELL upward when rejected
                        let ask_reject = coin_state.ask_rejection_count(coin);
                        let mut ask_offset = calculate_ask_tick_offset(ask_reject, risk_out.skew_bps);
                        // V12.1: COARSE mode — zero shading
                        let gate_mode_se = coin_state.get_or_init(coin).gate_mode.clone();
                        if gate_mode_se == "COARSE" {
                            ask_offset = 0;
                        // V12: Coarse tick hard cap
                        } else if risk_out.gross_spread_bps >= 30.0 {
                            ask_offset = ask_offset.min(1);
                        }
                        let shaded_ask = apply_ask_shading(defensive_ask_px, ask_offset, coin_tick);
                        if ask_offset > 0 {
                            tracing::info!(
                                coin = %coin,
                                base_ask = defensive_ask_px,
                                shaded_ask,
                                offset_ticks = ask_offset,
                                ask_rejections = ask_reject,
                                "ask shading applied"
                            );
                        }
                        match adapter
                            .place_limit_order(coin, false, sell_sz, shaded_ask, false)
                            .await
                        {
                            Ok(OrderOutcome::Rested(oid)) => {
                                new_oids.push(oid);
                                metrics.placed_sell = true;
                                coin_state.reset_ask_rejections(coin);
                            }
                            Ok(OrderOutcome::FilledTaker) => {
                                metrics.placed_sell = true;
                                coin_state.reset_ask_rejections(coin);
                            }
                            Ok(OrderOutcome::Rejected(Some(ref reason)))
                                if reason.to_lowercase().contains("would match") =>
                            {
                                let count = coin_state.increment_ask_rejections(coin);
                                let mut bid_offset = calculate_bid_tick_offset(count, risk_out.skew_bps);
                                let mut ask_offset2 = calculate_ask_tick_offset(count, risk_out.skew_bps);
                                // V12.1: COARSE mode — zero shading
                                let gate_mode_rr = coin_state.get_or_init(coin).gate_mode.clone();
                                if gate_mode_rr == "COARSE" {
                                    bid_offset = 0;
                                    ask_offset2 = 0;
                                // V12: Coarse tick hard cap on rejection-retry shading too
                                } else if risk_out.gross_spread_bps >= 30.0 {
                                    bid_offset = bid_offset.min(1);
                                    ask_offset2 = ask_offset2.min(1);
                                }
                                tracing::warn!(
                                    coin = %coin,
                                    count,
                                    bid_offset_ticks = bid_offset,
                                    ask_offset_ticks = ask_offset2,
                                    skew_bps = risk_out.skew_bps,
                                    "SELL would match -> bid+ask shading active"
                                );
                            }
                            Ok(OrderOutcome::Rejected(reason)) => {
                                tracing::debug!(coin = %coin, ?reason, "sell order rejected");
                            }
                            Err(e) => tracing::warn!(coin = %coin, side = "SELL", ?e, "order error"),
                        }
                    }
                }

                // ── P0: NOW cancel old orders (new orders already resting) ──
                cancel_active_orders(&mut adapter, coin, &mut active_orders).await;

                // P0: Merge new OIDs into active_orders
                // If any of these OIDs appear in the shock signal's recent_oids
                // (double-fill from the place-before-cancel micro-gap), trigger
                // defensive cooldown to prevent cascading exposure.
                if toxic_active && !shock.recent_oids.is_empty() {
                    let double_filled = new_oids.iter().any(|oid| shock.recent_oids.contains(oid));
                    if double_filled {
                        tracing::warn!(
                            coin = %coin,
                            new_oids = ?new_oids,
                            recent_fills = ?shock.recent_oids,
                            "DOUBLE-FILL DETECTED: old+new orders both filled in same cycle, triggering emergency cooldown"
                        );
                        coin_state.transition(coin, State::Cooldown);
                        // Cancel everything for this coin to reset
                        adapter.cancel_all_for_coin(coin).await.ok();
                        rate_limit_delay().await;
                        new_oids.clear();
                    }
                }

                for oid in new_oids {
                    active_orders.push((coin.clone(), oid));
                }
            } else if can_place {
                // Determine why we skipped
                let roundtrip = cfg.roundtrip_bps();
                let min_spread = roundtrip + cfg.min_margin_bps;
                let spread_ok = risk_out.net_spread_bps > min_spread;
                if !spread_ok && !is_unwind {
                    let reason = format!(
                        "net_spread {:.2} <= min {:.2} bps (roundtrip {} + margin {})",
                        risk_out.net_spread_bps, min_spread, roundtrip, cfg.min_margin_bps
                    );
                    tracing::info!(
                        coin = %coin,
                        net_spread = risk_out.net_spread_bps,
                        min_spread,
                        roundtrip,
                        "SKIPPING ORDERS: spread below profitability threshold"
                    );
                    if coin_state.state_of(coin) == State::Active {
                        coin_state.enter_waiting(coin, &reason);
                    }
                }
            } else if !can_place && coin_state.state_of(coin) == State::Active {
                let reason = if portfolio_over_limit {
                    "portfolio over limit".to_string()
                } else {
                    "can_place=false (state not orderable)".to_string()
                };
                tracing::warn!(coin = %coin, reason = %reason, "SKIPPING ORDERS: cannot place");
                coin_state.enter_waiting(coin, &reason);
            }

            // ── 6. Log metrics ──
            metrics.cycle_ms = cycle_start.elapsed().as_millis() as u64;
            log_cycle_metrics(&metrics);

            // ── 7. Label recording ──
            {
                let pos = account.positions.iter()
                    .find(|p| &p.coin == coin).cloned().unwrap_or_default();
                let book_guard = bus.books.read();
                let (bid_sz, ask_sz, mid_px) = book_guard.get(coin)
                    .map(|b| (b.total_bid_depth(), b.total_ask_depth(), b.mid_price().unwrap_or(0.0)))
                    .unwrap_or((0.0, 0.0, 0.0));
                drop(book_guard);

                let pos_notional = pos.size.abs() * mid_px;
                let now = chrono::Utc::now().to_rfc3339();
                labeler.record(CycleRecord {
                    ts: now,
                    coin: coin.to_string(),
                    state: metrics.state.to_string(),
                    pos_sz: pos.size,
                    pos_notional,
                    withdrawable: account.withdrawable,
                    equity: account.equity,
                    gross_spread_bps: metrics.gross_spread_bps,
                    net_spread_bps: metrics.net_spread_bps,
                    skew_bps: metrics.skew_bps,
                    position_ratio: metrics.position_ratio,
                    volatility: 0.0,
                    mid_px,
                    bid_sz,
                    ask_sz,
                    placed_buy_sz: metrics.buy_sz,
                    placed_sell_sz: metrics.sell_sz,
                });
            }

            // Drain and log fills
            let fills = bus.drain_fills();
            for fill in &fills {
                // V12.1: Tally fills per side for COARSE mode sensitive skew
                let fill_side = fill.side.to_lowercase();
                let tally = if fill_side.starts_with('b') {
                    coin_state.tally_buy_fill(&fill.coin)
                } else {
                    coin_state.tally_sell_fill(&fill.coin)
                };
                // V12.2: Toxicity momentum — track consecutive same-side fills
                let is_buy = fill_side.starts_with('b');
                let consecutive = coin_state.record_fill_side(&fill.coin, is_buy);
                if consecutive >= 3 {
                    tracing::warn!(
                        coin = %fill.coin,
                        side = if is_buy { "BUY" } else { "SELL" },
                        consecutive,
                        "TOXICITY: {} consecutive same-side fills — momentum alert", consecutive
                    );
                }
                tracing::info!(
                    coin = %fill.coin,
                    side = %fill.side,
                    sz = fill.sz,
                    px = fill.px,
                    fee = fill.fee,
                    side_tally = tally,
                    "fill processed"
                );
            }
        }

        // ── 8. Sleep with jitter + S1 shutdown check ──
        let base_sleep = rng.gen_range(cfg.cycle_sleep_min..cfg.cycle_sleep_max);
        let jitter = base_sleep * cfg.cycle_jitter * (rng.gen::<f64>() - 0.5) * 2.0;
        let sleep_duration = (base_sleep + jitter).max(1.0);
        let sleep = tokio::time::sleep(std::time::Duration::from_secs_f64(sleep_duration));
        tokio::pin!(sleep);

        tokio::select! {
            _ = &mut sigterm => {
                tracing::info!("SIGTERM received during sleep, cancelling all open orders...");
                graceful_shutdown(&mut adapter, &cfg, &active_orders).await;
                return Ok(());
            }
            _ = &mut sleep => {}
        }
    }
}

/// S1: Graceful shutdown — cancel all active orders before exit.
async fn graceful_shutdown<A: MarketAdapter>(
    adapter: &mut A,
    cfg: &Config,
    active_orders: &[(String, u64)],
) {
    for (coin, _) in active_orders {
        if let Err(e) = adapter.cancel_all_for_coin(coin).await {
            tracing::error!(%coin, ?e, "graceful shutdown: cancel_all failed");
        }
    }
    for coin in &cfg.coins {
        if let Err(e) = adapter.cancel_all_for_coin(coin).await {
            tracing::error!(%coin, ?e, "graceful shutdown: sweep cancel failed");
        }
    }
    tracing::info!("graceful shutdown: all orders cancelled");
}

/// B0: Bootstrap state recovery — sync exchange positions into engine FSM.
async fn bootstrap_recovery<A: MarketAdapter>(
    adapter: &mut A,
    cfg: &Config,
    bus: &SignalBus,
    coin_state: &mut CoinStateMachine,
) -> Result<()> {
    let account = adapter.fetch_state().await?;
    *bus.account.write() = account.clone();

    if account.positions.is_empty() {
        tracing::info!("bootstrap: no existing positions, starting from IDLE");
        return Ok(());
    }

    for pos in &account.positions {
        let coin = &pos.coin;
        if pos.size.abs() < 0.001 {
            continue;
        }

        rate_limit_delay().await;
                    let book = match adapter.fetch_l2(coin).await {
            Ok(b) => {
                bus.books.write().insert(coin.clone(), b.clone());
                b
            }
            Err(e) => {
                tracing::warn!(coin = %coin, ?e, "bootstrap: L2 fetch failed, skipping");
                continue;
            }
        };

        let mid = book.mid_price().unwrap_or(pos.entry_px.abs());
        if mid <= 0.0 {
            tracing::warn!(coin = %coin, mid, "bootstrap: zero mid, skipping");
            continue;
        }

        let hard_limit = if account.withdrawable > 0.0 {
            account.withdrawable * cfg.hard_limit_ratio
        } else {
            (account.equity * cfg.hard_limit_ratio).max(1.0)
        };
        let pos_notional = (pos.size.abs() * mid).abs();
        let pos_ratio = (pos_notional / hard_limit).min(1.0);

        // Bootstrap directly into Active. Natural cubic skew + quadratic qty
        // will handle inventory rebalancing progressively without forced
        // sell-then-rebuy round-trip. The cycle loop's risk assessment will
        // escalate to Unwind/Shedding only if position exceeds real thresholds.
        let init_state = State::Active;

        coin_state.init_coin(coin, init_state);
        tracing::info!(
            coin = %coin,
            pos = pos.size,
            pos_ratio,
            init_state = %init_state,
            "bootstrap: injected existing position (via natural skew, no forced sell-off)"
        );
    }

    Ok(())
}

/// Cancel active orders for a coin.
async fn cancel_active_orders<A: MarketAdapter>(
    adapter: &mut A,
    coin: &str,
    active_orders: &mut Vec<(String, u64)>,
) {
    let (ours, rest): (Vec<_>, Vec<_>) =
        std::mem::take(active_orders)
            .into_iter()
            .partition(|(c, _)| c == coin);

    *active_orders = rest;

    if ours.is_empty() {
        return;
    }

    for (c, oid) in ours {
        if let Err(e) = adapter.cancel_order(&c, oid).await {
            tracing::warn!(coin = %c, oid = oid, ?e, "cancel failed");
        }
    }
}

fn calculate_bid_tick_offset(ask_rejections: u32, skew_bps: f64) -> u32 {
    let base = if ask_rejections >= 6 {
        2
    } else if ask_rejections >= 3 {
        1
    } else {
        0
    };
    let skew_adjust = (skew_bps.max(0.0) * 1.5) as u32;
    base + skew_adjust
}

fn apply_bid_shading(base_bid_px: f64, offset: u32, tick_size: f64) -> f64 {
    let retreat = (offset as f64 * tick_size).min(tick_size * 10.0);
    (base_bid_px - retreat).max(base_bid_px * 0.90)
}

/// Returns how many ticks to RETREAT the ASK upward
/// when our SELL Post-Only orders keep getting rejected.
fn calculate_ask_tick_offset(sell_rejections: u32, skew_bps: f64) -> u32 {
    let base = if sell_rejections >= 6 { 2 } else if sell_rejections >= 3 { 1 } else { 0 };
    let skew_adjust = (skew_bps.max(0.0) * 0.5) as u32; // half as aggressive as bid side
    (base + skew_adjust).min(10) // cap at 10 ticks
}

/// Apply ask shading: raise the ask price by offset ticks.
fn apply_ask_shading(base_ask_px: f64, offset: u32, tick_size: f64) -> f64 {
    let advance = (offset as f64 * tick_size).min(tick_size * 10.0);
    base_ask_px + advance
}

/// Log cycle metrics as structured JSON.
fn log_cycle_metrics(m: &CycleMetrics) {
    if m.toxic_defense_active {
        tracing::warn!(
            coin = %m.coin,
            state = %m.state,
            gross_spread_bps = m.gross_spread_bps,
            net_spread_bps = m.net_spread_bps,
            skew_bps = m.skew_bps,
            skew_ticks = m.skew_ticks,
            pos_ratio = m.position_ratio,
            buy_sz = m.buy_sz,
            sell_sz = m.sell_sz,
            size_mult = m.size_multiplier,
            gate = %m.gate_mode,
            placed_buy = m.placed_buy,
            placed_sell = m.placed_sell,
            shed_filled = m.shed_filled,
            wd = m.withdrawable,
            eq = m.equity,
            cycle_ms = m.cycle_ms,
            toxic_defense = true,
            "cycle (TOXIC DEFENSE ACTIVE)"
        );
    } else {
        tracing::info!(
            coin = %m.coin,
            state = %m.state,
            gross_spread_bps = m.gross_spread_bps,
            net_spread_bps = m.net_spread_bps,
            skew_bps = m.skew_bps,
            skew_ticks = m.skew_ticks,
            pos_ratio = m.position_ratio,
            buy_sz = m.buy_sz,
            sell_sz = m.sell_sz,
            size_mult = m.size_multiplier,
            gate = %m.gate_mode,
            placed_buy = m.placed_buy,
            placed_sell = m.placed_sell,
            shed_filled = m.shed_filled,
            wd = m.withdrawable,
            eq = m.equity,
            cycle_ms = m.cycle_ms,
            "cycle"
        );
    }
}
