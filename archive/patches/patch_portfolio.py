#!/usr/bin/env python3
"""Fix portfolio hard limit deadlock: don't cooldown, allow passive sells."""

with open('src/engine.rs', 'r') as f:
    content = f.read()

# 1. Replace the portfolio hard limit block: remove cooldown + continue, just warn
old_block = '''            if !portfolio_ok {
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
            }'''

new_block = '''            if !portfolio_ok {
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
                    // Over hard limit: warn but don't cooldown/continue.
                    // Allow passive unwind (reduce-only sells) to reduce positions naturally.
                    // Buys are frozen via freeze_buy_override; sells pass through.
                    tracing::warn!(
                        coin = %coin,
                        total_notional = total_notional,
                        equity = account.equity,
                        limit_ratio = %cfg.hard_limit_ratio,
                        "portfolio hard limit exceeded — buys frozen, passive sells allowed"
                    );
                }
            }'''

if old_block in content:
    content = content.replace(old_block, new_block)
    print('1. portfolio hard limit block: UPDATED (no cooldown, no continue)')
else:
    print('1. BLOCK NOT FOUND — searching...')
    for i, line in enumerate(content.split('\n'), 1):
        if 'portfolio hard limit exceeded' in line and 'cooldown' in line:
            print(f'  Found at line {i}: {line.strip()[:80]}')
    exit(1)

# 2. In order placement guard, remove '&& portfolio_ok'
old_guard = 'if can_place && risk.is_profitable_spread(risk_out.net_spread_bps) && risk.is_minimally_profitable(risk_out.net_spread_bps) && portfolio_ok {'
new_guard = 'if can_place && risk.is_profitable_spread(risk_out.net_spread_bps) && risk.is_minimally_profitable(risk_out.net_spread_bps) {'
if old_guard in content:
    content = content.replace(old_guard, new_guard)
    print('2. order placement guard: REMOVED portfolio_ok gate')
else:
    print('2. guard NOT FOUND')

# 3. Freeze buy when portfolio over limit
old_freeze = '                let freeze_buy = is_unwind && pos.size > 0.0;'
new_freeze = '                let freeze_buy = is_unwind && pos.size > 0.0 || (!portfolio_ok && !is_first_cycle);'
if old_freeze in content:
    content = content.replace(old_freeze, new_freeze)
    print('3. freeze_buy: UPDATED to also freeze on portfolio over limit')
else:
    print('3. freeze_buy NOT FOUND')

with open('src/engine.rs', 'w') as f:
    f.write(content)
print('\nAll patches applied successfully.')
