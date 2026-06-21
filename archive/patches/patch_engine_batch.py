#!/usr/bin/env python3
"""Patch engine.rs to use batch order placement (1 POST instead of N)."""
import os

EXEC = "/tmp/growth-engine/src/engine.rs"
with open(EXEC) as f:
    e = f.read()

# ── Find the old BUY+SELL loops and replace with batch version ──
# The old section starts at "if can_place && risk" and ends just before "// ── 7. Log metrics"

old_block = """            if can_place && risk.is_profitable_spread(risk_out.net_spread_bps) && portfolio_ok {
                let is_unwind = coin_state.is_unwind(coin);
                let coin_tick = cfg.tick_for(coin);

                // Determine freeze direction from position sign
                let freeze_buy = is_unwind && pos.size > 0.0;   // LONG → freeze BUY, SELL to unwind
                let freeze_sell = is_unwind && pos.size < 0.0;  // SHORT → freeze SELL, BUY to unwind

    // ── Multi-level BUY orders ──
                if !freeze_buy {
                    let mgr = level_managers.entry(coin.clone()).or_insert_with(crate::levels::LevelManager::new);
                    let sigma_ticks = if risk_out.net_spread_bps > 0.0 {
                        (risk_out.net_spread_bps / 10000.0 * mid) / coin_tick
                    } else {
                        2.0
                    };
                    for lq in &risk_out.levels_buy {
                        if lq.sz <= 0.0 { continue; }
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;

                        if !mgr.needs_requote(lq.level, lq.px, coin_tick, sigma_ticks) {
                            continue; // Passive — leave resting order alone
                        }

                        // Cancel old order for this level if active
                        if let Some(old) = mgr.orders.get(&lq.level) {
                            if old.state == crate::levels::LevelState::Active {
                                let _ = executor.cancel_order(coin, old.oid).await;
                                mgr.mark_cancelled(lq.level);
                            }
                        }

                        // Place new order
                        match executor
                            .place_limit_order(coin, true, lq.sz, lq.px, false)
                            .await
                        {
                            Ok(Some(oid)) => {
                                mgr.register(lq.level, oid, lq.px, lq.sz, now_ms);
                                metrics.placed_buy = true;
                            }
                            Ok(None) => tracing::debug!(coin = %coin, level = lq.level, "buy level filled/rejected"),
                            Err(e) => tracing::warn!(coin = %coin, level = lq.level, side = "BUY", ?e, "order error"),
                        }
                    }
                }

    // ── Multi-level SELL orders ──
                if !freeze_sell {
                    let mgr = level_managers.entry(coin.clone()).or_insert_with(crate::levels::LevelManager::new);
                    let sigma_ticks = if risk_out.net_spread_bps > 0.0 {
                        (risk_out.net_spread_bps / 10000.0 * mid) / coin_tick
                    } else {
                        2.0
                    };
                    for lq in &risk_out.levels_sell {
                        if lq.sz <= 0.0 { continue; }
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;

                        if !mgr.needs_requote(lq.level, lq.px, coin_tick, sigma_ticks) {
                            continue;
                        }

                        if let Some(old) = mgr.orders.get(&lq.level) {
                            if old.state == crate::levels::LevelState::Active {
                                let _ = executor.cancel_order(coin, old.oid).await;
                                mgr.mark_cancelled(lq.level);
                            }
                        }

                        match executor
                            .place_limit_order(coin, false, lq.sz, lq.px, false)
                            .await
                        {
                            Ok(Some(oid)) => {
                                mgr.register(lq.level, oid, lq.px, lq.sz, now_ms);
                                metrics.placed_sell = true;
                            }
                            Ok(None) => tracing::debug!(coin = %coin, level = lq.level, "sell level filled/rejected"),
                            Err(e) => tracing::warn!(coin = %coin, level = lq.level, side = "SELL", ?e, "order error"),
                        }
                    }
                }
            }"""

new_block = """            if can_place && risk.is_profitable_spread(risk_out.net_spread_bps) && portfolio_ok {
                let is_unwind = coin_state.is_unwind(coin);
                let coin_tick = cfg.tick_for(coin);
                let freeze_buy = is_unwind && pos.size > 0.0;
                let freeze_sell = is_unwind && pos.size < 0.0;

                let sigma_ticks = if risk_out.net_spread_bps > 0.0 {
                    (risk_out.net_spread_bps / 10000.0 * mid) / coin_tick
                } else {
                    2.0
                };
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;

                let mgr = level_managers.entry(coin.clone())
                    .or_insert_with(crate::levels::LevelManager::new);

                // ── Phase 1: Cancel stale + collect batch requests ──
                let mut batch: Vec<(bool, f64, f64, usize)> = Vec::new();  // (is_buy,sz,px,level)

                for lq in &risk_out.levels_buy {
                    if lq.sz <= 0.0 { continue; }
                    if freeze_buy { continue; }
                    if !mgr.needs_requote(lq.level, lq.px, coin_tick, sigma_ticks) { continue; }
                    if let Some(old) = mgr.orders.get(&lq.level) {
                        if old.state == crate::levels::LevelState::Active {
                            let _ = executor.cancel_order(coin, old.oid).await;
                            mgr.mark_cancelled(lq.level);
                        }
                    }
                    batch.push((true, lq.sz, lq.px, lq.level));
                }

                for lq in &risk_out.levels_sell {
                    if lq.sz <= 0.0 { continue; }
                    if freeze_sell { continue; }
                    if !mgr.needs_requote(lq.level, lq.px, coin_tick, sigma_ticks) { continue; }
                    if let Some(old) = mgr.orders.get(&lq.level) {
                        if old.state == crate::levels::LevelState::Active {
                            let _ = executor.cancel_order(coin, old.oid).await;
                            mgr.mark_cancelled(lq.level);
                        }
                    }
                    batch.push((false, lq.sz, lq.px, lq.level));
                }

                // ── Phase 2: Batch place (1 POST for all levels) ──
                if !batch.is_empty() {
                    let orders: Vec<(bool, f64, f64)> = batch.iter()
                        .map(|(is_buy, sz, px, _)| (*is_buy, *sz, *px))
                        .collect();

                    match executor.place_batch_orders(coin, &orders).await {
                        Ok((oids, _nonce)) => {
                            for (i, (is_buy, _sz, px, level)) in batch.iter().enumerate() {
                                if let Some(oid) = oids[i] {
                                    mgr.register(*level, oid, *px, orders[i].1, now_ms);
                                    if *is_buy { metrics.placed_buy = true; }
                                    else { metrics.placed_sell = true; }
                                }
                            }
                        }
                        Err(e) => tracing::warn!(coin = %coin, ?e, "batch place failed"),
                    }
                }
            }"""

if old_block in e:
    e = e.replace(old_block, new_block)
    with open(EXEC, "w") as f:
        f.write(e)
    print("✅ engine.rs patched — batch order placement")
else:
    print("❌ OLD BLOCK NOT FOUND — engine.rs NOT modified")
    # Debug: look around
    lines = e.split("\n")
    for i, l in enumerate(lines):
        if "if can_place && risk" in l:
            print(f"  found at line {i+1}: {l.strip()}")
            print(f"  context: {lines[i+1].strip()[:80]}")
