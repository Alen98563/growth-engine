#!/usr/bin/env python3
"""Patch engine.rs: full batch refactor. Replace the entire order placement block."""

with open("/tmp/growth-engine/src/engine.rs") as f:
    e = f.read()

# Old block: from "if can_place && risk" to just before "// ── 7. Log metrics ──"
old_start = "            if can_place && risk.is_profitable_spread(risk_out.net_spread_bps) && portfolio_ok {"
old_end_marker = "            }\n\n            // ── 7. Log metrics ──"

# Find indices
si = e.find(old_start)
ei = e.find(old_end_marker, si)

if si == -1 or ei == -1:
    print(f"ERROR: si={si}, ei={ei}")
    # Debug: print surrounding lines
    for i, line in enumerate(e.split('\n')):
        if "if can_place" in line or "7. Log metrics" in line:
            print(f"  L{i+1}: {line.strip()[:100]}")
    import sys; sys.exit(1)

print(f"Found block: L{si//80}→L{ei//80}, {ei-si} chars")

# New block
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

                // ── BATCH Phase 1: Cancel stale + collect ──
                let mut batch: Vec<(bool, f64, f64, usize)> = Vec::new(); // (is_buy,sz,px,level)

                if !freeze_buy {
                    for lq in &risk_out.levels_buy {
                        if lq.sz <= 0.0 { continue; }
                        if !mgr.needs_requote(lq.level, lq.px, coin_tick, sigma_ticks) { continue; }
                        if let Some(old) = mgr.orders.get(&lq.level) {
                            if old.state == crate::levels::LevelState::Active {
                                let _ = executor.cancel_order(coin, old.oid).await;
                                mgr.mark_cancelled(lq.level);
                            }
                        }
                        batch.push((true, lq.sz, lq.px, lq.level));
                    }
                }

                if !freeze_sell {
                    for lq in &risk_out.levels_sell {
                        if lq.sz <= 0.0 { continue; }
                        if !mgr.needs_requote(lq.level, lq.px, coin_tick, sigma_ticks) { continue; }
                        if let Some(old) = mgr.orders.get(&lq.level) {
                            if old.state == crate::levels::LevelState::Active {
                                let _ = executor.cancel_order(coin, old.oid).await;
                                mgr.mark_cancelled(lq.level);
                            }
                        }
                        batch.push((false, lq.sz, lq.px, lq.level));
                    }
                }

                // ── BATCH Phase 2: 1 POST /exchange for all levels ──
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
"""

e = e[:si] + new_block + e[ei:]

with open("/tmp/growth-engine/src/engine.rs", "w") as f:
    f.write(e)
print("✅ engine.rs patched successfully")
