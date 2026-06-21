#!/usr/bin/env python3
"""Patch engine.rs: add `is_first_cycle` flag to skip portfolio hard limit on inherited positions."""

with open("/tmp/growth-engine/src/engine.rs") as f:
    src = f.read()

# 1. Add is_first_cycle initialization before the per-coin loop
old = """    loop {
        // ── Per-coin lifecycle ──
        // Each coin manages its own state independently via CoinStateMachine.
        for tick in &cfg.coins {"""
new = """    loop {
        // ── Per-coin lifecycle ──
        // First cycle flag: if engine wakes up with inherited positions,
        // skip portfolio hard limit so the state machine (Unwind/Cooldown)
        // can manage them naturally instead of emergency-dumping.
        let mut is_first_cycle = true;
        // Each coin manages its own state independently via CoinStateMachine.
        for tick in &cfg.coins {"""

if new not in src and old in src:
    src = src.replace(old, new, 1)
    src = src.replace("// ── Per-coin lifecycle ──", "// ── Per-coin lifecycle ──\n        // First cycle flag: skip hard limit on inherited positions so Unwind handles them naturally.\n        let mut is_first_cycle = true;")
    print("[OK] added is_first_cycle flag")
else:
    # Alternative: find the exact insertion point
    idx = src.find("        // Each coin manages its own state")
    if idx >= 0:
        # Find the previous newline to insert before
        nl = src.rfind("\n", 0, idx)
        insert = """        // First cycle flag: if engine wakes up with inherited positions,
        // skip portfolio hard limit so the state machine (Unwind/Cooldown)
        // can manage them naturally instead of emergency-dumping.
        let mut is_first_cycle = true;

"""
        src = src[:nl+1] + insert + src[nl+1:]
        print("[OK] added is_first_cycle flag (method 2)")
    else:
        print("[WARN] could not find insertion point for is_first_cycle")

# 2. Replace the portfolio_ok block: add grace skip for first cycle
old_block = """            if !portfolio_ok {
                tracing::warn!(
                    coin = %coin,
                    total_notional = total_notional,
                    equity = account.equity,
                    limit_ratio = %cfg.hard_limit_ratio,
                    "portfolio hard limit exceeded, emergency IOC shed all positions"
                );
                // Emergency shed: IOC all positions before entering cooldown
                let shed_total = emergency_portfolio_shed(&mut executor, &account, &cfg).await;
                tracing::warn!(
                    coin = %coin,
                    shed_total = shed_total,
                    "portfolio emergency shed complete"
                );
                // Transition to Cooldown and increment deadlock counter
                coin_state.transition(coin, State::Cooldown);
                coin_state.increment_cooldown(coin);
            }"""

new_block = """            if !portfolio_ok {
                if is_first_cycle && total_notional > 0.0 {
                    // Grace: first cycle with inherited positions — let state machine handle them.
                    // Skip hard limit to avoid immediately dumping base inventory;
                    // the Unwind/Cooldown mechanism will reduce positions naturally over time.
                    tracing::warn!(
                        coin = %coin,
                        total_notional = total_notional,
                        equity = account.equity,
                        limit_ratio = %cfg.hard_limit_ratio,
                        "portfolio hard limit on startup (inherited positions) — grace skip, relying on Unwind/Cooldown"
                    );
                } else {
                    tracing::warn!(
                        coin = %coin,
                        total_notional = total_notional,
                        equity = account.equity,
                        limit_ratio = %cfg.hard_limit_ratio,
                        "portfolio hard limit exceeded — cooldown"
                    );
                    coin_state.transition(coin, State::Cooldown);
                    coin_state.increment_cooldown(coin);
                    continue;  // skip order placement for this cycle
                }
            }"""

if old_block in src:
    src = src.replace(old_block, new_block, 1)
    print("[OK] portfolio hard limit: added first-cycle grace")
else:
    print("[WARN] old_block not found, trying partial match")
    # Search for "portfolio excess hard limit" with surrounding context
    idx = src.find("portfolio hard limit exceeded, emergency IOC shed all positions")
    if idx >= 0:
        print(f"  Found at idx {idx}")
    else:
        # Try finding "portfolio hard limit exceeded" 
        idx2 = src.find("portfolio hard limit exceeded")
        if idx2 >= 0:
            print(f"  Found alternative at idx {idx2}")

# 3. Set is_first_cycle = false at the END of the first per-coin iteration
old2 = """        // ── Step 7: Sleep ──
        let sleep_secs = cfg.cycle_sleep_min"""
new2 = """        // ── End of first cycle ──
        is_first_cycle = false;

        // ── Step 7: Sleep ──
        let sleep_secs = cfg.cycle_sleep_min"""

if new2 not in src and old2 in src:
    src = src.replace(old2, new2, 1)
    print("[OK] added is_first_cycle=false at cycle end")
else:
    # Alternative: find "Step 7: Sleep"
    idx = src.find("Step 7: Sleep")
    if idx >= 0:
        nl = src.rfind("\n", 0, idx)
        src = src[:nl+1] + "        is_first_cycle = false;\n\n" + src[nl+1:]
        print("[OK] added is_first_cycle=false (method 2)")
    else:
        print("[WARN] could not find Step 7")

with open("/tmp/growth-engine/src/engine.rs", "w") as f:
    f.write(src)

print("[DONE] engine.rs patched with first-cycle grace for inherited positions")

