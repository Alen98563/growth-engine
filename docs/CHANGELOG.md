# Changelog — Growth Engine

## V12.4 (2026-06-22) — Portfolio-Reduce: Restart Deadlock Fix

**Problem**: When restarting with existing positions where aggregate notional exceeded 60% portfolio limit but no single coin exceeded 40% unwind watermark, the engine deadlocked:
1. Bootstrap → Active (by design: "natural skew + quadratic qty" progressive rebalancing)
2. Portfolio-over-limit → Active coins pushed to Waiting
3. Waiting exit requires `!portfolio_over_limit`
4. No orders can be placed → positions don't change → portfolio stays over limit → **permanent deadlock**

This violated the bootstrap design intent: position-reducing orders from natural skew rebalancing were blocked by the same gate that was supposed to protect against NEW position accumulation.

**Changes** (`engine.rs:685-697`):
- Removed `!portfolio_over_limit` from `can_place` — state-based gating only
- Added `portfolio_reduce` bypass: when portfolio is over limit AND coin has non-zero position, order placement is allowed
- Added per-direction freeze for portfolio-over-limit: LONG positions can only SELL; SHORT positions can only BUY
- Added `portfolio_reduce` to the `(can_place && spread_ok) || is_unwind || portfolio_reduce` placement gate (parallel to UNWIND bypass)
- Log `"PORTFOLIO OVER LIMIT: reducing-only mode"` warning when active

**Effect**: The "natural skew + quadratic qty" mechanism can now work as designed — rebalancing orders that reduce total exposure are never blocked, while new position-opening orders remain gated.

---

## V12.3 (2026-06-22) — Net-Spread Safety Gate

**Problem**: Fine-tick coins (e.g. RESOLV at 0.46 bps 1-tick) bypassed COARSE protection because their tick spread was below the 15 bps threshold. They fell through to TSUNAMI 1.0x and traded at negative net spread — accumulating positions that couldn't be profitable even with 100% maker fill rate.

**Root cause**: Gate priority chain had no check for absolute spread profitability. COARSE (1-tick ≥ 15 bps) was the only dual-axis gate, and coins with spreads below that threshold had no net-spread protection.

**Changes** (`engine.rs:309-325`):
- Inserted **THIN_SPREAD** gate at priority #2 (between CROSSED and COARSE)
- Condition: `gross_bps < maker_fee_bps + min_margin_bps` (default 2.17 bps)
- Blocked coins get `"THIN_SPREAD"` gate mode with 0.0× multiplier
- Gate priority chain becomes: CROSSED → **THIN_SPREAD** → COARSE → TSUNAMI → SNIPER → BLOCKED

**Effect**: All fine-tick coins automatically blocked when their spread can't cover costs. No per-coin configuration needed.

---

## V12.2 (2026-06-22) — COARSE Asymmetric Sizing & Toxicity Detection

**Problem**: COARSE-mode coins accumulated position on one side because spreads were wide enough to cross. Without fill-side tracking, the engine kept buying into a rising LONG position — writing free options to the market.

**Changes**:
- Per-side fill tally (`buy_fills_tally`, `sell_fills_tally`) — track how many fills each side got since last reset
- **Sensitive skew**: When one side reaches `coarse_tick_max_side_fills`, block that side entirely
- **Asymmetric sizing**: When position exceeds `coarse_pos_ratio_aggressive`, boost the favorable (unwind) side by `coarse_unwind_size_boost` and cap the adverse side at 25%
- **Hard kill**: When position exceeds `coarse_pos_ratio_hard_kill`, zero the adverse side entirely
- **Consecutive same-side fill tracking** for toxicity momentum detection
- Flip hysteresis persistent counter (hotfix: `last_flip_cycle` + `freeze_remaining`)

**Effect**: COARSE coins self-regulate position accumulation. No more free-option writing.

---

## V12.1 (2026-06-22) — COARSE Dual-Axis Gate

**Problem**: Gate system used tick count only (`gi ≥ tsunami_ticks`). COARSE-tick coins (HMSTR 1 tick = 53 bps) were correctly classified at 1 tick, but the gate couldn't distinguish between a 1-tick 60 bps spread and a 1-tick 0.5 bps spread.

**Changes**:
- `gross_spread_bps` calculated from bid/ask mid-point for dual-axis gate
- COARSE mode gated by `gross_bps >= coarse_tick_bps_threshold` (15 bps) AND `gi == 1`
- Zero shading in COARSE mode (shading_offset = 0 — no tick retreat)
- COARSE size multiplier configurable via `coarse_tick_size_pct` (default 0.3× aka 30%)
- Separate COARSE mode logging with `"COARSE_TICK_HARVEST"` message

**Effect**: 1-tick coins with fat margin correctly enter low-risk COARSE harvest mode. Fine-tick coins with narrow spreads fall through to TSUNAMI/SNIPER (later fixed by V12.3).

---

## V12 (2026-06-22) — Crossed-Book Gate & Shading Cap & V5 Scanner

**Problem**: HMSTR ran V11.6 for 11 hours (3,624 cycles) with **zero fills** despite active taker volume. Two root causes:

1. **Crossed book**: REST L2 returned `best_ask (0.000188) < best_bid (0.000189)`. The `_ => 999.0` fallback treated this as TSUNAMI 1.0×, and Post-Only orders could never execute in a crossed book.
2. **Shading over-isolation**: 6+ Post-Only rejections triggered 6-tick bid retreat (6 × 60 bps = 360 bps). Orders were 6× the spread away from any taker.

**Changes**:
- **Crossed-book detection**: REST L2 `best_ask ≤ best_bid` → `book_crossed=true` → CROSSED/BLOCKED gate
- Not re-fetched (REST is authoritative, WS is incremental)
- **Coarse-tick shading cap**: On coins with `gross_bps ≥ 30`, shading offset capped at 1 tick maximum
- Fine-tick coins (gross < 30 bps) retain full shading (not affected)
- **V5 Velocity Scanner**: 4-stage (Vol→Depth→Active→CoarseSort) replacing old 3-stage V4

**Effect**: Crossed-book coins no longer waste cycles. Coarse-tick coins stay near-the-money without suicidal isolation.

---

## V11.6 (2026-06-22) — Dynamic Tick Query & Floating Point Precision

**Problem**: Tick sizes were hardcoded per coin. RESOLV (0.000001 tick) was getting the PUMP hardcoded tick, causing spread calculation errors.

**Changes**:
- `tick_for(coin)` queries the exchange's actual tick size dynamically
- Floating-point precision fix for tick rounding
- PUMP → HMSTR strategic switch: 6.8 bps → 60.8 bps structural coarse tick

---

## V4 Scanner (2026-06-22) — 3-Stage Funnel Scanner

**Background**: Needed systematic coin selection to replace manual `.env` editing.

**Stages**:
1. **Volume Gate**: 24h volume ≥ $50k
2. **Depth Gate**: Top-of-book depth ≥ $2k
3. **Coarse Sort**: Sort by gross spread bps descending

Result: HMSTR 53.3 bps / NOT 24.3 bps / MEME 18.3 bps identified as GOLD-tier market-making targets.

---

## V6.2 (2026-06-19) — GTC Last-Resort Deadlock Breaker

**Problem**: IOC shedding produced zero fills in illiquid markets. Engine trapped in EMERGENCY_IOC→COOLDOWN→NORMAL→EMERGENCY_IOC loop.

**Changes**:
- GTC last-resort: zero-shed ≥ 3 → GTC limit sell at `best_bid × 0.95`
- GTC orders rest on book and fill over time
- `place_limit_order()` accepts `force_gtc` parameter

---

## V6.1 (2026-06-19) — Conditional Cooldown Regress

**Problem**: Cooldown→Active was unconditional, re-entering EMERGENCY_IOC immediately.

**Changes**: After cooldown, check position ratio before state transition — enter appropriate state (NORMAL/UNWIND/EMERGENCY_IOC) based on actual position.

---

## V6.0 (2026-06-19) — Cross-Cycle State Persistence

**Problem**: Local per-cycle variables reset each loop. Counters couldn't detect patterns.

**Changes**: Moved all per-coin state tracking to `CoinStateMachine` — counters persist across cycles.

---

## V5.0 (2026-06-18) — Cooldown Deadlock Protection

**Problem**: Portfolio hard limit → Cooldown → same condition → infinite loop.

**Changes**: Cooldown counter + forced emergency portfolio shed at 3 consecutive cooldowns.

---

## V4.0 (2026-06-18) — Defense Barrier (4 Global Protections)

**Changes**:
- Kill Switch: wd < $80 → liquidate all + halt
- Circuit Breaker: 3+ API failures → cancel all + halt
- Dynamic Position Cap: auto-shrinks during volatility
- Liquidation Defense: direction-aware, 8% threshold

---

## V3.0 (2026-06-18) — Batch Order System + Portfolio Hard Limit

**Changes**: Batch signing (one Python call for all orders), single POST for all orders, portfolio total notional capped at 40% equity.

---

## V2.0 (2026-06-18) — Multi-Level Quoting + Unwind Hysteresis

**Changes**: Multi-level order grid, unwind hysteresis (enter 40%, exit 20%), anti-ping-pong timer.

---

## V1.0 (2026-06-18) — Initial Release

**Core**: Cubic³ skew pricing, quadratic² qty, IOC shedding, passive unwind, Python EIP-712 signing, WebSocket L2 books.

---

## Evolution Summary

| Version | Key Addition | Defense Layers |
|---------|-------------|----------------|
| V1.0 | Cubic skew, IOC shed, Python signing | 4 |
| V2.0 | Multi-level quoting, hysteresis | 6 |
| V3.0 | Batch orders, portfolio hard limit | 8 |
| V4.0 | Kill switch, circuit breaker, dynamic cap | 11 |
| V5.0 | Cooldown deadlock protection | 11 |
| V6.0 | Cross-cycle state persistence | 11 |
| V6.1 | Conditional cooldown regress | 11 |
| V6.2 | GTC last-resort | 12 |
| V11.6 | Dynamic tick query, HMSTR switch | 12 |
| V12 | Crossed-book gate, shading cap, V5 scanner | 14 |
| V12.1 | COARSE dual-axis gate + zero shading | 15 |
| V12.2 | COARSE asymmetric sizing + toxicity detection | 16 |
| V12.3 | THIN_SPREAD net-spread safety gate | **17** |
| V12.4 | Portfolio-reduce restart deadlock fix | **17** |

**Current (V12.4)**: 17 defense layers. Gate priority: CROSSED → THIN_SPREAD → COARSE → TSUNAMI → SNIPER → BLOCKED. Portfolio-over-limit allows position-reducing orders only. Supports HMSTR/MEME/RESOLV three-coin market-making.
