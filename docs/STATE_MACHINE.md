# State Machine — Per-Coin Lifecycle

## Overview

Each coin independently manages its own state lifecycle. This is critical — a FARTCOIN unwind should not stop PUMP from making tight spreads.

## State Diagram

```
                         ┌─────────┐
                         │  IDLE   │
                         │ waiting │
                         └────┬────┘
                              │ conditions met:
                              │ • withdrawable > min_reserve OR has position
                              │ • gross_spread_bps > 0
                         ┌────▼────┐
                  ┌──────│COLDSTART│──────┐
                  │      │ 2 cycles│      │
                  │      └─────────┘      │
                  │ done                  │ books stale
                  │                  ┌────▼────┐
                  │                  │ NORMAL   │ ◄──────────────────┐
                  │                  │ 0%-40%   │                    │
                  │                  │ bilateral│                    │
                  │                  │ post-only│                    │
                  │                  └────┬────┘                    │
                  │                       │                          │
                  │    pos_ratio >=      pos_ratio >=             pos_ratio
                  │    unwind_watermark   shed_trigger            < hysteresis
                  │    (40%)              (90%)                   exit
                  │         │                 │                      │
                  │    ┌────▼──────────┐  ┌───▼──────────────┐      │
                  │    │PASSIVE_UNWIND │  │  EMERGENCY_IOC   │      │
                  │    │   40%-90%     │  │     90%+          │      │
                  │    │ freeze same   │  │ IOC 50% takedown  │      │
                  │    │ retreat opp   │  │ 500ms loop        │      │
                  │    │    side       │  │ GTC last-resort   │      │
                  │    └────┬──────────┘  └───┬───────────────┘      │
                  │         │                 │ shed done             │
                  │         │ 5+ unwind      ┌▼──────────┐           │
                  │         │ attempts ─────►│           │           │
                  │         │                 │ COOLDOWN  │           │
                  │         │                 │  30 sec   │───────────┘
                  │         │                 │ no orders │ (conditional:
                  │         │                 │           │  pos < watermark)
                  │         │                 └───────────┘
                  │         │
                  │         │ pos_ratio < hysteresis (watermark − hysteresis)
                  │         │ → NORMAL (with 60s anti-ping-pong)
                  │         └────────────────────────────────────────────┘
                  │
                  │ KILL_SWITCH (global)
                  │ withdrawable < $80
                  └──► cancel all + IOC liquidate all + HALT
```

## Transition Conditions

### IDLE → COLD_START
- **Trigger**: `withdrawable >= min_reserve` (OR already holding a position) AND `gross_spread_bps > 0`
- **Purpose**: Wait for sufficient account balance before starting
- **Notes**: Coin with existing position can enter even with low balance (to begin unwinding)

### COLD_START → NORMAL
- **Trigger**: 2 complete engine cycles (no inventory gate during cold start)
- **Purpose**: Brief warming period with post-only quotes, no inventory restrictions
- **Notes**: Ensures the book is established before risk controls activate

### NORMAL → PASSIVE_UNWIND
- **Trigger**: `position_ratio >= passive_unwind_watermark` (default 40% of hard limit)
- **Action**: Freeze same-side orders, tick-retreat opposite side by 3 ticks
- **Purpose**: Gently reduce position without panic-selling
- **Hysteresis exit**: Position must drop below `watermark − hysteresis` (default 20% of hard limit) to exit

### NORMAL → EMERGENCY_IOC
- **Trigger**: `position_ratio >= shed_trigger` (default 90% of hard limit) AND NOT first cycle
- **First-cycle grace**: Inherited positions on startup skip IOC shed
- **Action**: IOC sell at best bid (LONG) or IOC buy at best ask (SHORT), 50% of position
- **Loop**: 500ms cooldown, re-fetch L2, continue until safe or max 6 iterations
- **GTC last-resort**: If 3+ IOC rounds yield zero fill, place GTC limit sell at bid × 0.95

### NORMAL → COOLDOWN
- **Trigger**: `withdrawable < min_reserve` (reserve depleted)
- **Action**: 30s pause, no orders placed
- **Conditional regress**: After cooldown, check position ratio before returning to NORMAL

### PASSIVE_UNWIND → NORMAL
- **Trigger**: `position_ratio < unwind_watermark − unwind_hysteresis` (default 20% of hard limit)
- **Side effect**: Start 60s anti-ping-pong timer (BUY orders suppressed)
- **Purpose**: Prevent immediately re-accumulating the unwound position

### PASSIVE_UNWIND → EMERGENCY_IOC
- **Trigger**: `unwind_attempts >= 5` without progress (position didn't drop)
- **Purpose**: If passive unwind can't reduce position after 5 cycles, escalate to IOC
- **Notes**: Only triggers if `should_shed` is true (position_ratio >= shed_trigger)

### EMERGENCY_IOC → COOLDOWN
- **Trigger**: Position in safe zone, max iterations reached, or IOC error
- **Side effect**: Increment cooldown counter; increment zero-shed counter if no fill
- **GTC check**: If zero-shed ≥ 3, GTC last-resort sell executed

### COOLDOWN → NORMAL / PASSIVE_UNWIND / EMERGENCY_IOC
- **Trigger**: 30s elapsed since entering cooldown
- **Conditional regress** (never blindly return to NORMAL):
  - `pos_ratio >= shed_trigger` → re-enter EMERGENCY_IOC
  - `pos_ratio >= passive_unwind_watermark` → enter PASSIVE_UNWIND
  - `pos_ratio < passive_unwind_watermark` → return to NORMAL
- **Deadlock protection**: If `consecutive_cooldowns >= max_consecutive_cooldowns` (default 3), force emergency shed before returning

### Any → KILL_SWITCH
- **Trigger**: `withdrawable < killswitch_min_wd` (global, all coins)
- **Action**: Cancel all orders for all coins, IOC liquidate all positions, halt engine
- **Notes**: This is a terminal state — engine exits the main loop

### Liquidation Defense (emergency exit)
- **Trigger**: Any coin's mark-to-liquidation distance < 8% (direction-aware)
- **Action**: Cancel all orders, IOC liquidate all positions, halt engine
- **Details**: LONG checks liq < mark; SHORT checks liq > mark. Handles direction correctly.

## Real Recovery Example (V6.2, 2026-06-19)

FARTCOIN recovery from inherited position of 538 tokens:

```
Initial: POS=538 LONG, wd=$92.80, state=NORMAL(100% ratio)
↓
Cycle 1: IDLE → COLD_START (position detected, wd=$92.80)
Cycle 2-3: COLD_START → NORMAL (2 cycles complete)
↓
Cycle 4+: NORMAL → PASSIVE_UNWIND (position_ratio >= 40% water-mark)
  - Freeze BUY, SELL with 3-tick retreat
  - 5+ attempts, position barely reduced (illiquid spread)
↓
Cycle ~10: PASSIVE_UNWIND → EMERGENCY_IOC (5+ attempts, escalated)
  - 6 iterations of IOC sell at best_bid
  - Zero fills (no takers in illiquid market)
  - GTC last-resort placed at bid × 0.95
↓
GTC fills over several cycles → position unwound
Final: POS=0, wd=$99.94
```

**Key insight**: V6.1 was trapped in a NORMAL↔EMERGENCY_IOC↔COOLDOWN loop because position was always at 100% (inherited), so every Cooldown→NORMAL immediately re-entered EMERGENCY_IOC. V6.2's GTC last-resort broke this deadlock by placing a resting order that could fill over time.
