# Risk Control — Defense Layers

The Growth Engine implements **12 defense layers** designed for the unique risks of narrow-spread (4–6 bps) market-making on Hyperliquid Growth Mode.

## Defense Architecture

```
Layer 0  ── Cubic³ Price Skew (tick-discretized)
Layer 1  ── Quadratic² Asymmetric Quantity (min-lot guarded)
Layer 2  ── Adaptive IOC Shedding (500ms loop, 90% trigger)
Layer 3  ── Rate Limit Jitter (3-5s cycle, randomized)
Layer 4  ── Passive Unwind Watermark (40% trigger, 20% hysteresis)
Layer 5  ── Portfolio Hard Limit (40% of equity, cross-coin)
Layer 6  ── Kill Switch ($80 withdrawable floor)
Layer 7  ── Circuit Breaker (3 failures in 5s → halt)
Layer 8  ── Dynamic Position Cap (shrinks during volatility)
Layer 9  ── Liquidation Defense (direction-aware, 8% threshold)
Layer 10 ── Dust Filter ($20 threshold, cross-coin)
Layer 11 ── Anti-Ping-Pong (60s post-unwind BUY suppression)
Layer 12 ── GTC Last-Resort (95% bid when IOC fails 3+ rounds)
```

---

## Layer 0: Cubic³ Price Skew + Tick Discretization

### Purpose
Prevents position accumulation by asymmetrically widening the spread on the accumulating side. The cubic exponent (3.0) preserves tight spreads for the first 75% of position capacity.

### Formula
```
skew_bps = MAX_SKEW_BPS × position_ratio³
```
Then discretized to whole tick steps:
```
skew_ticks = floor(raw_skew_price / tick_size)
adjusted_px = base_px ± skew_ticks × tick_size
```

### Tick Discretization
On low-price coins like PUMP (tick=$0.000001), 1 tick ≈ 2–3 bps. Without discretization, micro-skews would be rounded away by exchange tick constraints, making the skew invisible. Discretization converts BPS skew to integer tick steps, clamped to [0.5×, 1.5×] of base price.

### Values
| Parameter | Default | Notes |
|-----------|---------|-------|
| `skew_power` | 3.0 | Cubic exponent |
| `max_skew_bps` | 2.0 | Max skew at 100% position |

### Example
Position at 50% capacity → skew = 2.0 × 0.125 = 0.25 bps (barely noticeable)
Position at 90% capacity → skew = 2.0 × 0.729 = 1.46 bps (strong discouragement)

---

## Layer 1: Quadratic² Asymmetric Quantity + Min-Lot Guard

### Purpose
Reduces order size on the position-accumulating side while keeping the non-accumulating side full-sized. Prevents ghost orders (below exchange minimum) by zeroing them out.

### Formula
```
qty_scale = max(0, 1 − position_ratio²)

LONG position:  buy_qty = base_qty × scale   SELL_qty = base_qty
SHORT position: buy_qty = base_qty            SELL_qty = base_qty × scale
```

### Min-Lot Guard
Orders below `min_order_size` (default 1.0 native unit) are zeroed entirely. Without this guard, a 90% position with base_qty=10 would produce scale=0.19 → qty=1.9, but if min_order_size=2.0, this order would be rejected by the exchange.

### Values
| Parameter | Default |
|-----------|---------|
| `qty_power` | 2.0 |
| `min_order_size` | 1.0 |

---

## Layer 2: Adaptive IOC Shedding (90% Trigger)

### Purpose
When position reaches 90% of hard limit, aggressively reduce using IOC (immediate-or-cancel) orders that cross the spread.

### Mechanism
1. **Trigger**: `position_ratio >= shed_trigger` (90% of hard limit)
2. **Size**: `shed_fraction × position` (50% of current position)
3. **Pricing**: Best opposing price (best_bid for sells, best_ask for buys)
4. **Loop**: 500ms cooldown, re-fetch L2, continue until safe or max 6 iterations
5. **Safe re-entry**: Position below `shed_safe_reentry` (70% of hard limit)

### Values
| Parameter | Default |
|-----------|---------|
| `shed_trigger` | 0.90 |
| `shed_fraction` | 0.50 |
| `shed_safe_reentry` | 0.70 |
| `max_iterations` | 6 |

### GTC Last-Resort (V6.2)
If 3+ consecutive IOC rounds yield zero fill, places a GTC limit sell at `best_bid × last_resort_gamble_discount` (95%). This breaks the NORMAL↔EMERGENCY_IOC↔COOLDOWN deadlock.

---

## Layer 3: Rate Limit Jitter

### Purpose
Prevents CloudFront 429 rate limiting by randomizing cycle duration.

### Mechanism
```
sleep = random(3.0, 5.0) + jitter × (random() − 0.5) × 2
```
Ensures no two cycles hit the API at predictable intervals.

---

## Layer 4: Passive Unwind Watermark (40% Trigger, 20% Hysteresis)

### Purpose
Gently reduce position at 40% of hard limit without panic-selling. Freezes same-side orders and tick-retreats the opposite side.

### Mechanism
- **Enter**: `position_ratio >= passive_unwind_watermark` (40% of hard limit)
- **Action**: Freeze position-accumulating side; tick-retreat reducing side by 3 ticks
- **Exit (hysteresis)**: `position_ratio < watermark − hysteresis` (40% − 20% = 20%)
- **Escalate**: After 5+ unwind attempts without progress → EMERGENCY_IOC

### Hysteresis Band
Without hysteresis, every minor price oscillation around 40% would toggle UNWIND↔NORMAL repeatedly. The 20% hysteresis buffer (40% → 20%) prevents this state flickering.

### Values
| Parameter | Default |
|-----------|---------|
| `passive_unwind_watermark` | 0.40 |
| `unwind_hysteresis` | 0.20 |
| `unwind_max_retreat_ticks` | 3 |
| `unwind_attempts_escalation` | 5 |

---

## Layer 5: Portfolio Hard Limit (40% of Equity)

### Purpose
Prevents total cross-coin position from exceeding 40% of account equity. Uses dust-filtered notional for accuracy.

### Mechanism
```
total_notional = Σ dust_filtered(position_i × mid_i)
limit = equity × hard_limit_ratio
OK = total_notional < limit
```

### Action
- **Exceeded**: BUY orders frozen (both coins), SELL orders allowed to reduce positions
- **First-cycle grace**: Inherited positions on startup skip the hard limit check to allow gradual unwinding

### Values
| Parameter | Default |
|-----------|---------|
| `hard_limit_ratio` | 0.40 |

---

## Layer 6: Kill Switch ($80 Withdrawable Floor)

### Purpose
Global emergency stop-loss. If withdrawable balance drops below $80, cancel everything and liquidate all positions.

### Trigger
`withdrawable < killswitch_min_wd` AND `withdrawable > 0`

### Action
1. Cancel all orders for all coins (cancelByCloid)
2. Emergency portfolio shed (IOC chunks with progressive price degradation)
3. Halt engine (exit main loop)

### Values
| Parameter | Default |
|-----------|---------|
| `killswitch_min_wd` | $80.00 |

---

## Layer 7: Circuit Breaker (3 Failures in 5s → Halt)

### Purpose
Prevent cascading API failures. If 3+ consecutive API errors (429, timeout, internal error) occur within 5 seconds, trip the breaker.

### Mechanism
- **Sliding window**: 5 seconds
- **Failure threshold**: 3 failures
- **On trip**: Cancel all orders, halt engine
- **On success**: Clear failure window (resets the breaker)

### Action on Trip
1. `cancel_all_for_coin()` for all coins
2. Engine loop breaks → clean exit

### Values
| Parameter | Default |
|-----------|---------|
| `circuit_breaker_window_secs` | 5.0 |
| `circuit_breaker_max_failures` | 3 |

---

## Layer 8: Dynamic Position Cap

### Purpose
Auto-shrinks the position cap during volatile markets. When volatility spikes, the engine should hold LESS inventory.

### Formula
```
effective_limit = max(dynamic_hard_limit_min, dynamic_hard_limit_base / vol_multiplier)
```

### Examples
| Vol Multiplier | Effective Cap | Notes |
|---------------|---------------|-------|
| 1.0 (quiet) | max(0.15, 0.40/1.0) = 0.40 | Full capacity |
| 2.0 (elevated) | max(0.15, 0.40/2.0) = 0.20 | Half capacity |
| 3.0 (turbulent) | max(0.15, 0.40/3.0) = 0.15 | Floor (minimum) |

### Values
| Parameter | Default |
|-----------|---------|
| `dynamic_hard_limit_base` | 0.40 |
| `dynamic_hard_limit_min` | 0.15 |

---

## Layer 9: Liquidation Defense (Direction-Aware, 8% Threshold)

### Purpose
Detect when a position is approaching liquidation and trigger emergency exit BEFORE the exchange's liquidation engine does.

### Philosophy
"老子割肉自己切，不让交易所清算推土机碰我"
(I cut my own losses — never let the exchange's liquidation bulldozer touch me.)

### Mechanism
```
Direction-aware distance:
  LONG:  distance = (mark − liq) / mark   (danger = liq < mark)
  SHORT: distance = (liq − mark) / mark    (danger = liq > mark)

danger = distance > 0 AND distance < threshold (8%)
```

### Safety Guards
- Liquidation price must be > 0 (null/zero = no liquidation risk)
- Position must be > 0.01 size (negligible)
- Distance clamped to ≥ 0 (floating-point noise)
- Direction correctly handled for LONG vs SHORT

### Action
- Per-coin check (each coin in its own engine loop)
- Any coin triggers → global: cancel all + IOC liquidate all + halt
- Cross-coin NOT cascading (only the triggering coin's position is checked)

### Values
| Parameter | Default |
|-----------|---------|
| `liquidation_distance_threshold` | 0.08 (8%) |

---

## Layer 10: Dust Filter ($20 Threshold)

### Purpose
Prevent tiny residual positions from corrupting risk calculations. A $2 dust position should not trigger portfolio hard limit or unwinding.

### Mechanism
```
dust_filtered_notional = Σ max(0, |size_i × mid_i|) where |size × mid| >= $20
```
Positions with notional < $20 contribute 0 to the total.

### Values
| Parameter | Default |
|-----------|---------|
| `dust_threshold_notional` | $20.00 |

---

## Layer 11: Anti-Ping-Pong (60s Post-Unwind BUY Suppression)

### Purpose
After exiting PASSIVE_UNWIND → NORMAL, prevent immediately re-accumulating the same position. A 60-second cooldown suppresses all BUY orders while SELL orders remain active.

### Mechanism
- On UNWIND → NORMAL transition: set `post_unwind_until = now + 60s`
- While timer active: `freeze_buy = true`
- Ask side unchanged: SELL orders continue normally
- This prevents the engine from buying back what it just unwound

### Values
| Parameter | Default |
|-----------|---------|
| `post_unwind_cooldown_secs` | 60.0 |

---

## Layer 12: GTC Last-Resort (95% Bid When IOC Fails 3+ Rounds)

### Purpose
When IOC shedding fails (no takers in illiquid market), place a GTC (Good-Till-Cancelled) limit sell at 95% of best bid. This guarantees eventual fill by offering a discount to the market.

### Trigger
`zero_shed_rounds >= 3` (3+ IOC rounds with zero fill)

### Action
1. Fetch fresh L2 book
2. Price = `best_bid × last_resort_gamble_discount` (0.95)
3. Tick-round the price
4. Place GTC limit sell for remaining position at discounted price

### Real Recovery (V6.2)
FARTCOIN inherited 538 tokens at $0.13. IOC sells got zero fills for 3+ rounds. GTC last-resort placed at $0.1235 (95% of $0.13 bid) → filled over time → position 538→0, wd $92.80→$99.94.

### Values
| Parameter | Default |
|-----------|---------|
| `last_resort_gamble_discount` | 0.95 |

---

## Defense Layer Interaction

```
Position Ratio    Triggered Layers
────────────────  ─────────────────────────────────
   0% – 20%       Layer 0 (minimal skew), Layer 1 (symmetric), Layer 3 (jitter)
  20% – 40%       Layer 0 (moderate skew), Layer 1 (slight asymmetry)
  40% – 90%       Layer 4 (PASSIVE_UNWIND), Layer 8 (dynamic cap),
                  Layer 11 (post-unwind cooldown on recovery)
  90% – 100%      Layer 2 (IOC shedding), Layer 12 (GTC last-resort)
  Cross-coin:     Layer 5 (portfolio hard limit), Layer 10 (dust filter)
  Global:         Layer 6 (kill switch), Layer 7 (circuit breaker),
                  Layer 9 (liquidation defense)
```

The layers are designed to **escalate progressively**: gentle discouragement at low ratios becomes active position reduction at high ratios, with multiple fallback mechanisms when primary strategies fail.
