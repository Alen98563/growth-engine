# Changelog — Growth Engine

## V6.2 (2026-06-19) — GTC Last-Resort Deadlock Breaker

**Problem**: V6.1 was trapped in a NORMAL→EMERGENCY_IOC→COOLDOWN→NORMAL loop when IOC shedding produced zero fills. The engine tried to shed position via IOC (immediate-or-cancel) but the illiquid market had no takers. Position stayed at 100% (inherited), so every Cooldown exit immediately re-entered EMERGENCY_IOC.

**Changes**:
- Added **GTC last-resort**: When zero-shed counter ≥ 3, place a GTC limit sell at `best_bid × last_resort_gamble_discount` (95%)
- GTC orders rest on the book and fill over time, breaking the IOC-only deadlock
- `place_limit_order()` now accepts `force_gtc` parameter
- Tick-rounding on GTC price to ensure valid exchange price

**Result**: FARTCOIN inherited position of 538 tokens successfully unwound from 538→0. Withdrawable recovered from $92.80 → $99.94. The engine is now running healthy in NORMAL state with pos_ratio=0.0.

---

## V6.1 (2026-06-19) — Conditional Cooldown Regress

**Problem**: After cooldown expired, the engine blindly returned to NORMAL state without checking actual position ratio. If position was still at 100%, this triggered an immediate EMERGENCY_IOC → Cooldown loop.

**Changes**:
- **Conditional regress**: Cooldown → Active never happens blindly
  - `pos_ratio >= shed_trigger` → re-enter EMERGENCY_IOC
  - `pos_ratio >= passive_unwind_watermark` → enter PASSIVE_UNWIND
  - `pos_ratio < passive_unwind_watermark` → return to NORMAL
- Better logging for state transition reasons
- Consecutive cooldown tracking and forced shed at max

---

## V6.0 (2026-06-19) — Cross-Cycle State Persistence

**Problem**: Previous versions used local per-cycle variables that were reset each loop. Counters (cooldown, zero-shed) didn't persist across cycles, so the engine couldn't detect patterns.

**Changes**:
- Moved all per-coin state tracking to `CoinStateMachine`
- Counters persist across cycles: `consecutive_cooldowns`, `zero_shed_rounds`, `unwind_attempts`
- State machine tracks: `entered_at` timestamps, `post_unwind_until` anti-ping-pong timers
- Each coin independently manages its lifecycle

---

## V5.0 (2026-06-18) — Cooldown Deadlock Protection

**Problem**: When portfolio hard limit was exceeded, the engine entered Cooldown. On re-entry, the same condition triggered another Cooldown — an infinite loop with no position reduction.

**Changes**:
- **Cooldown counter**: Tracks consecutive Cooldown→Active→Cooldown loops
- **Forced shed**: At `max_consecutive_cooldowns` (3), force emergency portfolio shed
- **Emergency portfolio shed**: Chunks positions into $50 IOC orders with progressive price degradation
- Rescue mechanism prevents infinite cooldown loops

---

## V4.0 (2026-06-18) — Defense Barrier (Kill Switch + Circuit Breaker + Dynamic Cap + Liq Defense)

**Problem**: The engine had no global safety mechanisms. API failures, liquidation risk, and market volatility were unhandled.

**Changes**:
- **P0: Kill Switch** — $80 withdrawable floor → cancel all + liquidate all + halt
- **P1: Circuit Breaker** — 3+ API failures in 5s → cancel all + halt
- **P0: Dynamic Position Cap** — `max(min, base / vol_multiplier)` auto-shrinks during volatility
- **P0: Liquidation Defense** — Direction-aware, 8% threshold, emergency exit
- **VolTracker**: Rolling mid-price standard deviation for vol-adaptive spread
- All defense layers logged on engine start

---

## V3.0 (2026-06-18) — Batch Order System + Portfolio Hard Limit

**Problem**: Each order required a separate Python subprocess call (signing) + HTTP request. With multi-level quoting (6 orders per cycle), this was 6× the latency.

**Changes**:
- **Batch signing**: `sign_l1_batch_order` calls Python once for all orders
- **Batch submission**: Single `POST /exchange` for all 6+ orders
- **Portfolio hard limit**: Total cross-coin notional capped at 40% of equity
- **First-cycle grace**: Inherited positions skip hard limit to avoid instant dump
- **Dust filter**: Positions <$20 excluded from risk calc
- `place_batch_orders()` returns per-order OIDs

---

## V2.0 (2026-06-18) — Multi-Level Quoting + Unwind Hysteresis

**Problem**: Single-level quoting (one buy, one sell) left most of the spread unutilized. No hysteresis caused state flickering at the unwind boundary.

**Changes**:
- **Multi-level quoting**: `num_levels` orders per side with configurable sigma multipliers
- **LevelManager**: Tracks per-level order state, drift detection, re-quote logic
- **Unwind hysteresis**: Enter at 40%, exit at 20% (watermark − hysteresis)
- **Anti-ping-pong**: 60s post-unwind BUY suppression
- **Volatility-adaptive spread**: Sigma ticks scaled by vol multiplier
- Tick-retreat for unwind sells to prevent "would match" rejections

---

## V1.0 (2026-06-18) — Initial Release

**Background**: The project was originally a Polymarket + Hyperliquid perp market-making bot. After discovering no maker rebate on either platform, it pivoted to HL Growth Mode (HIP-3, `feeScale=0.1111`) for PUMP and FARTCOIN.

**Core architecture**:
- Three-layer design: Data (WS) → Logic (Risk) → Execution (REST)
- Cubic³ price skew: `skew_bps = MAX_SKEW × ratio³`
- Quadratic² asymmetric qty: `qty_scale = 1 − ratio²`
- IOC shedding at 90% position
- Passive unwind at 40% position
- Python EIP-712 signing via `hl_sign.py`
- WebSocket for live L2 books and user fills
- 3-5s cycle with jitter to avoid 429 rate limits

**Abandoned directions**:
- Polymarket CLOB integration (no maker rebate)
- HL perp standard market-making (maker+taker = 6.0 bps unprofitable)
- Generic multi-exchange architecture (focused on HL Growth Mode)

---

## Evolution Summary

| Version | Key Addition | Defense Layers |
|---------|-------------|----------------|
| V1.0 | Cubic skew, IOC shed, Python signing | 4 |
| V2.0 | Multi-level quoting, hysteresis | 6 |
| V3.0 | Batch orders, portfolio hard limit, dust filter | 8 |
| V4.0 | Kill switch, circuit breaker, dynamic cap, liq defense | 11 |
| V5.0 | Cooldown deadlock protection, emergency portfolio shed | 11 |
| V6.0 | Cross-cycle state persistence, per-coin independence | 11 |
| V6.1 | Conditional cooldown regress | 11 |
| V6.2 | GTC last-resort deadlock breaker | **12** |

**Current (V6.2)**: 12 defense layers, batch orders with 3-level grid, per-coin state machines, GTC last-resort. Engine PID 117335 running healthy in NORMAL state with pos_ratio=0.0, wd=$99.94.
