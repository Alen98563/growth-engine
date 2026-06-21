#!/usr/bin/env python3
"""V5 patch: max_skew_bps down + hard_limit down + COOLDOWN conditional regress + increment_cooldown"""

from pathlib import Path

BASE = Path("/tmp/growth-engine/src")

# ========== 1. config.rs ==========
cfg = BASE / "config.rs"
text = cfg.read_text()
changes_cfg = []

# 1a. max_skew_bps: 3.5 to 2.0
old, new = "max_skew_bps: 3.5", "max_skew_bps: 2.0"
assert old in text, f"NOT FOUND: {old}"
text = text.replace(old, new)
changes_cfg.append("max_skew_bps: 3.5 -> 2.0")

# 1b. hard_limit_ratio: 0.60 to 0.40
old, new = "hard_limit_ratio: 0.60", "hard_limit_ratio: 0.40"
assert old in text, f"NOT FOUND: {old}"
text = text.replace(old, new)
changes_cfg.append("hard_limit_ratio: 0.60 -> 0.40")

# 1c. passive_unwind_watermark: 0.40 to 0.25 (proportional to new hard_limit)
old, new = "passive_unwind_watermark: 0.40", "passive_unwind_watermark: 0.25"
assert old in text, f"NOT FOUND: {old}"
text = text.replace(old, new)
changes_cfg.append("passive_unwind_watermark: 0.40 -> 0.25")

cfg.write_text(text)
print(f"[config.rs] {' | '.join(changes_cfg)}")
print("             unwind hysteresis @ 0.20 -> exit at 0.05 position_ratio")

# ========== 2. engine.rs ==========
eng = BASE / "engine.rs"
text = eng.read_text()
changes_eng = []

# 2a. COOLDOWN transition from "reserve depleted": add increment_cooldown
marker = '''                        coin_state.transition(coin, State::Cooldown);
                    } else {
                        // Stage 2: Soft recovery — passive unwind at 40%'''
needle = '''                        coin_state.transition(coin, State::Cooldown);
                        coin_state.increment_cooldown(coin);
                    } else {
                        // Stage 2: Soft recovery — passive unwind at 40%'''
assert marker in text, "NOT FOUND: reserve depleted COOLDOWN marker"
text = text.replace(marker, needle)
changes_eng.append("increment_cooldown @ reserve depleted path")

# 2b. Shedding to COOLDOWN transition: add increment_cooldown
marker2a = "metrics.shed_filled = total_shed;"
marker2b = "coin_state.transition(coin, State::Cooldown);"
# Find "metrics.shed_filled = total_shed;\n                    coin_state.transition(coin, State::Cooldown);"
idx = text.find(marker2a)
assert idx > 0, "NOT FOUND: shed_filled line"
# Find next "State::Cooldown" after that
snippet = text[idx:idx+200]
assert marker2b in snippet, "NOT FOUND: shed->cooldown transition near shed_filled"
old2 = marker2a + "\n                    " + marker2b
new2 = marker2a + "\n                    " + marker2b + "\n                    coin_state.increment_cooldown(coin);"
assert old2 in text, f"NOT FOUND: shed->cooldown exact pattern"
text = text.replace(old2, new2)
changes_eng.append("increment_cooldown @ shed->cooldown path")

# 2c. COOLDOWN handler: conditional regress
old_cooldown = '''                State::Cooldown => {
                    if coin_state.cooldown_done(coin) {
                        // Check if we've been stuck in Cooldown->Active->Cooldown loop
                        if coin_state.consecutive_cooldown_count(coin) >= cfg.max_consecutive_cooldowns {
                            tracing::warn!(
                                coin = %coin,
                                consecutive = coin_state.consecutive_cooldown_count(coin),
                                max = %cfg.max_consecutive_cooldowns,
                                "max consecutive cooldowns reached, forcing IOC shed"
                            );
                            let forced_shed = emergency_portfolio_shed(&mut executor, &account, &cfg).await;
                            tracing::warn!(
                                coin = %coin,
                                forced_shed = forced_shed,
                                "forced cooldown shed complete"
                            );
                            coin_state.reset_cooldown_count(coin);
                        }
                        coin_state.transition(coin, State::Active);
                    }
                }'''

new_cooldown = '''                State::Cooldown => {
                    if coin_state.cooldown_done(coin) {
                        // Check if we've been stuck in Cooldown->Active->Cooldown loop
                        let cons = coin_state.consecutive_cooldown_count(coin);
                        if cons >= cfg.max_consecutive_cooldowns {
                            tracing::warn!(
                                coin = %coin,
                                consecutive = cons,
                                max = %cfg.max_consecutive_cooldowns,
                                "max consecutive cooldowns reached, forcing IOC shed"
                            );
                            let forced_shed = emergency_portfolio_shed(&mut executor, &account, &cfg).await;
                            tracing::warn!(
                                coin = %coin,
                                forced_shed = forced_shed,
                                "forced cooldown shed complete"
                            );
                            coin_state.reset_cooldown_count(coin);
                        }
                        // Conditional regress: never blindly return to NORMAL
                        let pr = risk_out.position_ratio;
                        let sz = pos.size;
                        if pr >= cfg.shed_trigger {
                            // Position still critical, re-enter IOC shed
                            tracing::warn!(
                                coin = %coin,
                                position_ratio = pr,
                                shed_trigger = cfg.shed_trigger,
                                "COOLDOWN expired but position still critical, re-entering EMERGENCY_IOC"
                            );
                            coin_state.transition(coin, State::Shedding);
                        } else if pr >= cfg.passive_unwind_watermark {
                            // Still over unwind threshold, enter passive unwind
                            tracing::info!(
                                coin = %coin,
                                position_ratio = pr,
                                watermark = cfg.passive_unwind_watermark,
                                "COOLDOWN expired, entering PASSIVE_UNWIND"
                            );
                            coin_state.transition(coin, State::Unwind);
                        } else {
                            // Safe - return to Active
                            tracing::info!(
                                coin = %coin,
                                position_ratio = pr,
                                "COOLDOWN expired, position safe - returning to NORMAL"
                            );
                            coin_state.reset_cooldown_count(coin);
                            coin_state.transition(coin, State::Active);
                        }
                    }
                }'''

assert old_cooldown in text, "NOT FOUND: COOLDOWN handler block"
text = text.replace(old_cooldown, new_cooldown)
changes_eng.append("COOLDOWN handler: conditional regress (>=shed_trigger->IOC, >=watermark->Unwind, else->NORMAL)")

eng.write_text(text)
print(f"[engine.rs] {' | '.join(changes_eng)}")

# ========== Summary ==========
print()
print("=== PATCH SUMMARY ===")
print("1. max_skew_bps: 3.5 -> 2.0  (net spread back to positive)")
print("2. hard_limit_ratio: 0.60 -> 0.40  (prevent position inflation)")
print("3. passive_unwind_watermark: 0.40 -> 0.25  (proportional to new hard_limit)")
print("4. increment_cooldown() called at 2 COOLDOWN entry points  (fix dead code)")
print("5. COOLDOWN -> conditional regress: >=90%->IOC, >=25%->Unwind, <25%->NORMAL")
