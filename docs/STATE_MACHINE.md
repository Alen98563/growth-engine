# State Machine — Per-Coin Lifecycle & Gate System

## Overview

Each coin independently manages its own state lifecycle. This is critical — an HMSTR unwind should not stop MEME from making tight spreads.

Two-tier architecture:
- **State Machine**: Position-driven lifecycle (IDLE → NORMAL → UNWIND → EMERGENCY → COOLDOWN)
- **Gate System**: Spread-driven per-cycle permission (CROSSED → THIN_SPREAD → COARSE → TSUNAMI → SNIPER → BLOCKED)

## Gate Priority Chain (V12.3)

The gate runs every cycle BEFORE state transitions. Higher priority = checked first. A coin blocked by a higher gate never reaches lower gates.

```
1. CROSSED_BOOK       → size_mult=0.0  — REST best_ask ≤ best_bid (impossible to trade)
2. THIN_SPREAD        → size_mult=0.0  — gross_bps < maker_fee + min_margin (unprofitable)
3. COARSE_TICK        → size_mult=0.3  — 1-tick ≥ 15 bps (fat margin, safe harvest)
4. TSUNAMI_HARVEST    → size_mult=1.0  — gi ≥ tsunami_ticks (full fire)
5. SNIPER             → size_mult=0.4  — gi ≥ gate_block_ticks (tight spread, reduced size)
6. BLOCKED            → size_mult=0.0  — spread too narrow for any mode
```

### Gate Details

**1. CROSSED_BOOK** (`book_crossed=true`)
- Detected when REST L2 returns `best_ask ≤ best_bid`
- REST is authoritative (not WS incremental), no re-fetch needed
- Appears on HMSTR during active trading when takers cross faster than REST snapshot

**2. THIN_SPREAD** (`gross_bps < maker_fee_bps + min_margin_bps`, default 2.17 bps)
- **V12.3 addition**: Prevents fine-tick coins from trading at negative net spread
- Example: RESOLV 0.46 bps 1-tick → 0.46 < 0.17 + 2.0 = 2.17 → BLOCKED
- Example: HMSTR 53 bps → 53 > 2.17 → passes (continues to COARSE check)
- Automatically applies to all coins, no per-coin config needed

**3. COARSE_TICK_HARVEST** (`gross_bps ≥ 15 bps AND gi == 1`)
- V12.1: Dual-axis (tick count + bps), zero shading, configurable size (default 30%)
- V12.2: Asymmetric sizing + sensitive skew + hard kill for position accumulation
- Designed for coarse-tick coins: HMSTR (53 bps), MEME (18 bps)

**4. TSUNAMI_HARVEST** (`gi ≥ tsunami_ticks`, default ratio from config)
- Full-size bilateral quoting
- Normal shading applies
- "TSUNAMI" = spread is wide enough for aggressive quoting

**5. SNIPER** (`gi ≥ gate_block_ticks`)
- Reduced size (40%) for tight spreads
- Conservative: only harvests spread when it's barely viable

**6. BLOCKED** (fallback)
- Spread too narrow, no orders placed
- Waits for spread to widen naturally

## State Diagram

```
                         ┌─────────┐
                         │  IDLE   │
                         └────┬────┘
                              │ withdrawable > min_reserve OR has position
                         ┌────▼────┐
                  ┌──────│COLDSTART│──────┐ 2 cycles post-only
                  │      └─────────┘      │
                  │ done                  │ books stale
                  │                  ┌────▼────┐
                  │                  │ NORMAL   │ ◄───────────────────────────┐
                  │                  │ 0%-40%   │                            │
                  │                  │ bilateral│                            │
                  │                  └────┬────┘                            │
                  │                       │                                  │
                  │    pos_ratio >= 40%   pos_ratio >= 90%    conditions improved
                  │    (unwind_watermark) (shed_trigger)      (spread_stable_cycles)
                  │         │                 │                      │
                  │    ┌────▼──────────┐  ┌───▼──────────────┐  ┌───▼──────┐
                  │    │PASSIVE_UNWIND │  │  EMERGENCY_IOC   │  │ WAITING  │
                  │    │   40%-90%     │  │     90%+          │  │cond. not │
                  │    │ freeze same   │  │ IOC 50% takedown  │  │  met     │
                  │    │ retreat opp   │  │ GTC last-resort   │  └───┬──────┘
                  │    │    side       │  │ 500ms loop        │      │
                  │    └────┬──────────┘  └───┬───────────────┘      │
                  │         │                 │ shed done             │
                  │         │ pos ratio       ┌▼──────────┐           │
                  │         │ < 10%           │           │           │
                  │         │ (hysteresis)    │ COOLDOWN  │───────────┘
                  │         │                 │  30 sec   │ (conditional:
                  │         └────────────────►│ no orders │  pos < watermark)
                  │                           │           │
                  │                           └───────────┘
                  │
                  │ KILL_SWITCH (global): wd < $80
                  └──► cancel all + IOC liquidate all + HALT
```

## Transition Conditions

### IDLE → COLD_START
- **Trigger**: `withdrawable >= min_reserve` OR holding existing position, AND `gross_spread_bps > 0`
- **Purpose**: Wait for account balance; existing positions can enter to begin unwinding

### COLD_START → NORMAL
- **Trigger**: 2 complete cycles (no inventory gate during cold start)
- **Purpose**: Brief warming with post-only quotes before risk controls activate

### NORMAL → PASSIVE_UNWIND
- **Trigger**: `position_ratio >= passive_unwind_watermark` (40%)
- **Action**: Freeze same-side orders, tick-retreat opposite side, GTC crossing
- **Exit**: Position < `watermark − hysteresis` (40% − 30% = 10%)

### NORMAL → EMERGENCY_IOC
- **Trigger**: `position_ratio >= shed_trigger` (90%) or `unwind_attempts >= 5`
- **Action**: IOC at best bid/ask, 50% position, 500ms loop, max 6 iterations
- **GTC last-resort**: 3+ zero-fill IOC rounds → GTC at 5% discount

### NORMAL → WAITING
- **Trigger**: `portfolio_over_limit` (aggregate notional > 60% equity) or spread collapsed
- **Exit**: All of `spread > min`, `!portfolio_over_limit`, `has_reserve` for `spread_stable_cycles` consecutive cycles
- **Portfolio-reduce bypass (V12.4)**: Position-reducing orders are allowed even from Waiting state via `portfolio_reduce` path

### Any → GATE_BLOCKED
- **Trigger**: Gate system returns size_multiplier = 0.0 (CROSSED or THIN_SPREAD or BLOCKED)
- **Action**: No orders placed; wait for spread to widen
- **Exit**: Gate re-evaluated every cycle; auto-transition when condition clears

### PASSIVE_UNWIND → NORMAL
- **Trigger**: `position_ratio < 10%` (watermark − hysteresis)
- **Side effect**: 5-cycle post-unwind cooldown (anti-ping-pong)

### EMERGENCY_IOC → COOLDOWN
- **Trigger**: Position in safe zone, max iterations, or IOC error
- **Conditional regress**: After 30s, check position ratio → appropriate state

## Portfolio Limit & Restart Behavior (V12.4)

### The Bootstrap Promise
On restart, existing positions are injected into NORMAL state with the promise that "natural skew + quadratic qty" will progressively rebalance them — no forced sell-off.

### The Deadlock (pre-V12.4)
When aggregate notional > 60% portfolio limit but no single coin > 40% unwind threshold:
1. Bootstrap → NORMAL
2. Portfoalio over limit → NORMAL → WAITING
3. WAITING requires `!portfolio_over_limit` to exit
4. No orders can reduce positions → **permanent deadlock**

### The Fix (V12.4)
The `portfolio_reduce` bypass allows position-reducing orders through even when portfolio is over limit:

```
Portfolio over limit?
├── pos.size > 0 (LONG)  → freeze BUY, allow SELL  (reducing)
├── pos.size < 0 (SHORT) → freeze SELL, allow BUY  (reducing)
└── pos.size = 0 (FLAT)  → freeze BOTH             (safe)
```

This works alongside the existing `can_place_orders` state gate and `spread_ok` profitability check:

```rust
if (can_place && spread_ok) || is_unwind || portfolio_reduce {
    // place orders (with per-direction freeze active)
}
```

Total protection when portfolio is over limit:
- No new positions opened (flat coins blocked)
- Existing positions can only be reduced
- Skew pricing still applies (favorable pricing on the reducing side)
- Spread profitability still checked (unless in UNWIND or portfolio_reduce)

## Key Design Decisions

1. **Gate runs BEFORE state transitions**: Spread conditions are evaluated per-cycle; state transitions are evaluated after gate
2. **Size multiplier persists across cycles**: Gate mode doesn't change until next gate evaluation
3. **Fill tallies reset on gate mode change**: New market regime → fresh fill statistics
4. **UNWIND bypasses spread_ok**: Survival > profit in position-reduction mode
5. **Portfolio-reduce bypasses spread_ok**: Same philosophy — get back under limit is the priority
6. **Shading cap at 1 tick for coarse-tick coins**: Prevents 6-tick isolation on 60-bps spreads
