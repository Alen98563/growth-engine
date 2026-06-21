# Architecture — Growth Engine

## Overview

The Growth Engine follows a **three-layer pipeline architecture**: Data → Logic → Execution, with a state machine coordinating transitions across layers.

```
┌─────────────────────────────────────────────────────────────────────┐
│                           main.rs                                   │
│     Config → SignalBus → [WS Task (async), Engine Task (async)]     │
└───────┬───────────────────────────┬─────────────────────────────────┘
        │                           │
        ▼                           ▼
┌───────────────┐           ┌───────────────────────────────┐
│  DATA LAYER   │           │         LOGIC LAYER            │
│               │           │                               │
│  ws.rs ─────┐ │  SignalBus │ risk.rs ───── state.rs       │
│  order_book ├─┼───────────┼─► cubic skew   per-coin FSM   │
│  types.rs ──┘ │  Arc<RwLock>   qty control  hysteresis    │
│               │               vol tracker  anti-ping-pong  │
└───────────────┘               shed check  cooldown deadlock│
                                liq defense                  │
                                levels.rs                   │
                                ─────────                   │
                                order grid management        │
                                └───────────────┬───────────┘
                                                │
                                                ▼
                                ┌───────────────────────────────┐
                                │       EXECUTION LAYER          │
                                │                               │
                                │  executor.rs ── signer.rs     │
                                │  REST client    Python subproc │
                                │  circuit breaker              │
                                │  batch orders                 │
                                │  tick retreat                 │
                                │  cancel-by-cloid              │
                                │         │                     │
                                │  config.rs                    │
                                │  env loading                  │
                                └───────────┬───────────────────┘
                                            │
                                            ▼
                                ┌───────────────────────────────┐
                                │    HYPERLIQUID EXCHANGE        │
                                │    POST /info  POST /exchange  │
                                │    WebSocket   l2Book stream    │
                                └───────────────────────────────┘
```

## Layer 1: Data (`ws.rs`, `order_book.rs`, `types.rs`)

**Responsibility**: Ingest real-time market data into shared state.

### ws.rs — WebSocket Client
- Manages persistent TLS WebSocket to `wss://api.hyperliquid.xyz/ws`
- Subscribes to `l2Book` for each monitored coin (full snapshot updates)
- Subscribes to `userFills` for own-trade monitoring
- Auto-reconnects on disconnect/error with configurable delay
- Pushes data into `SignalBus` (shared state)

### order_book.rs — L2 Book Maintenance
- Parses HL's `l2Book` message format: `levels[0]=bids, levels[1]=asks`
- Handles string→float parsing for px/sz values
- Provides mid price, best bid/ask, spread calculation
- Timestamp tracking for staleness detection (>10s → REST fallback)

### types.rs — Shared Data Structures
- **REST API types**: `OrderRequest`, `OrderResponse`, `OrderStatus`, etc.
- **L2 Book**: `L2Book`, `BookLevel` (px, sz, n)
- **Account State**: `AccountState`, `Position` (with liquidation price)
- **SignalBus**: `Arc<RwLock<HashMap<String, L2Book>>>` + account + fills
  - Thread-safe shared state between WS task (writer) and engine (reader)
  - Fills drained each cycle to prevent unbounded growth

## Layer 2: Logic (`risk.rs`, `state.rs`, `levels.rs`)

**Responsibility**: Transform raw market data into actionable trade decisions.

### risk.rs — Risk Engine
- **Cubic³ Price Skew**: `skew_bps = MAX_SKEW × ratio³` → tick-discretized
  - Tick discretization is critical: on low-price coins (PUMP tick=$0.000001),
    micro-skews must be rounded to valid tick increments
- **Quadratic² Asymmetric Qty**: `qty_scale = 1 − ratio²`, min-lot guarded
  - Accumulating side is reduced, non-accumulating side stays full
  - Min-lot guard: orders below `min_order_size` are zeroed (prevent rejections)
- **VolTracker**: Rolling mid-price standard deviation → volatility multiplier [1.0, cap]
- **IOC Shedding Check**: Triggers at `shed_trigger` (90% of hard limit)
- **Passive Unwind Check**: Triggers at `passive_unwind_watermark` (40% of hard limit)
- **Liquidation Defense**: Direction-aware distance calculation
- **Dynamic Position Cap**: `max(min, base / vol_multiplier)`
- **Dust Filter**: Positions <$20 excluded from risk calculations
- **Portfolio Hard Limit**: Total cross-coin notional vs equity

### state.rs — Per-Coin State Machine
- **7 states**: IDLE → COLD_START → NORMAL → PASSIVE_UNWIND → EMERGENCY_IOC → COOLDOWN → KILL_SWITCH
- **Per-coin independence**: Each coin has its own lifecycle tracking
  - Consecutive cooldown counter (max 3 → forced shed)
  - Zero-shed round counter (max 3 → GTC last-resort)
  - Unwind attempt counter (max 5 → escalate to EMERGENCY_IOC)
- **Hysteresis**: Enter UNWIND at watermark, exit at watermark − hysteresis (20% band)
- **Anti-ping-pong**: Post-unwind 60s timer suppresses BUY orders
- **Conditional regress**: Cooldown→NORMAL only if position safely below watermark

### levels.rs — Multi-Level Order Grid
- Manages N price levels per side with configurable sigma multipliers
- Tracks per-level state: Idle → Active → Filled/Cancelled
- Drift detection: re-quote if price moved >0.5σ from placed price
- Provides active OIDs for cancellation

## Layer 3: Execution (`executor.rs`, `signer.rs`, `config.rs`)

**Responsibility**: Execute decisions safely against the exchange.

### executor.rs — REST API Client
- **Account state**: `POST /info` (clearinghouseState)
- **L2 snapshot**: `POST /info` (l2Book) — REST fallback when WS stale
- **Order placement**: Single limit (Alo/Gtc), IOC (Ioc), batch orders
- **Cancellation**: By OID or by client-order-ID (cancel-all)
- **Tick retreat**: Retries Post-Only order with retreating price on "would match"
- **CircuitBreaker**: Sliding-window API failure detection (3 failures in 5s → trip)
- **Signature integrity**: Uses wire-format px_str/sz_str from Python signer

### signer.rs — EIP-712 Signing Bridge
- Delegates to `scripts/hl_sign.py` (Python subprocess)
- Uses official `hyperliquid-python-sdk` for byte-accurate EIP-712 signatures
- Supports: single order, batch order, cancel-by-OID, cancel-by-cloid
- Returns wire-format values that MUST be used in REST body

### config.rs — Configuration
- All tunable parameters with sensible defaults
- Loads from `.env` via `dotenvy`, overridden by environment
- Per-coin tick sizes, fee schedules (Growth vs Standard)

## Data Flow — One Engine Cycle

```
1. Fetch account state  ──────────────────► executor.fetch_account_state()
   │                                        POST /info (clearinghouseState)
   ▼
2. Global checks: Kill Switch, Circuit Breaker
   ├─ wd < $80 → cancel all + IOC liquidate + halt
   └─ breaker tripped → halt
   ▼
3. For each coin:
   ├─ Get L2 book (WS snapshot or REST fallback if >10s stale)
   ├─ Liquidation defense check (direction-aware)
   ├─ Cancel stale orders (active OIDs from last cycle)
   ├─ Risk.assess(book, account, coin, vol_mult)
   │   ├─ Compute position_ratio with dust filter + dynamic cap
   │   ├─ Cubic skew → tick-discretized prices
   │   ├─ Asymmetric qty → min-lot guarded sizes
   │   ├─ Shed/unwind checks
   │   └─ Multi-level quote grid
   ├─ State transitions (based on position_ratio + state counters)
   ├─ Portfolio hard limit check (cross-coin)
   ├─ Place orders (batch submit via single POST /exchange)
   │   ├─ Freeze logic (unwind, anti-ping-pong, buy_disable)
   │   ├─ Collect levels needing re-quote
   │   └─ Batch: cancel stale → place new → register OIDs
   └─ Log metrics (structured JSON)
   ▼
4. Sleep (3-5s + jitter) → next cycle
```

## Key Design Decisions

1. **Python signing subprocess**: Guarantees byte-identical EIP-712 signatures to the official SDK. Maintaining a Rust-native implementation in lockstep with SDK updates would be fragile.

2. **Full-snapshot L2 updates**: HL sends complete order book snapshots (not deltas) on each `l2Book` message. This simplifies local book maintenance at the cost of slightly larger messages — acceptable at 3–5s cycle rate.

3. **Batch order submission**: Single `POST /exchange` for all levels (both sides combined) reduces API calls and Python subprocess invocations by N×.

4. **Per-coin independence**: Each coin manages its own lifecycle. One coin's emergency doesn't cascade to others. A FARTCOIN unwind doesn't stop PUMP from tight quoting.

5. **First-cycle grace**: Inherited positions on startup skip EMERGENCY_IOC to allow passive Unwind to reduce naturally. Prevents immediately dumping inventory at startup prices.

6. **GTC last-resort**: When IOC fails 3+ consecutive rounds, place a GTC limit sell at 95% of best bid. This breaks the NORMAL↔EMERGENCY_IOC↔COOLDOWN deadlock that trapped V6.1.

## Module Dependency Graph

```
main.rs
  ├── config.rs (independent)
  ├── types.rs (independent)
  ├── ws.rs
  │     ├── config.rs
  │     ├── types.rs
  │     └── order_book.rs
  ├── engine.rs
  │     ├── config.rs
  │     ├── types.rs
  │     ├── state.rs
  │     ├── risk.rs
  │     │     ├── config.rs
  │     │     ├── types.rs
  │     │     └── levels.rs
  │     ├── executor.rs
  │     │     ├── config.rs
  │     │     ├── types.rs
  │     │     ├── signer.rs
  │     │     └── order_book.rs
  │     ├── signer.rs
  │     └── levels.rs
  └── (signal bus connects ws ↔ engine)
```
