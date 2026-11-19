# Growth Engine — Hyperliquid Growth Mode Market-Making

A production-grade Rust execution engine for market-making **PUMP** and **FARTCOIN** on Hyperliquid's HIP-3 Growth Mode (`feeScale=0.1111`, hyna deployer).

## Why This Exists

Hyperliquid's Growth Mode offers ultra-low fees:
- **Maker fee**: 0.17 bps
- **Taker fee**: 0.50 bps
- **Round-trip**: 0.67 bps

At these rates, 4–6 bps gross spreads are comfortably profitable. But you need a system that can:

1. **Price intelligently** — Stay at the book's edge without crossing
2. **Control risk** — Multiple defense layers prevent position runaway
3. **Survive API chaos** — Circuit breaker, rate limit aware, auto-reconnect
4. **Recover gracefully** — Passive unwind beats panic-selling; GTC last-resort beats deadlock

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust (edition 2021) |
| Async runtime | Tokio |
| HTTP client | reqwest (rustls-tls) |
| WebSocket | tokio-tungstenite |
| Signing | Python `hyperliquid-python-sdk` via subprocess |
| Logging | tracing-subscriber (structured JSON) |
| Concurrency | parking_lot RwLock, DashMap |

## Architecture (Three Layers)

```
Data Layer       Logic Layer         Execution Layer
──────────       ────────────        ───────────────
ws.rs ─────┐     risk.rs ──────┐     executor.rs ──── HL REST API
order_book  ├──► state.rs       ├──► signer.rs ────── hl_sign.py
types.rs ──┘     levels.rs ────┘     config.rs ────── .env
```

**Data Layer** ingests real-time L2 books and fills via WebSocket.
**Logic Layer** transforms raw data into trade decisions with 10+ defense layers.
**Execution Layer** sends signed orders to HL's REST API with circuit breaker protection.

## Quick Start

### Prerequisites

- Rust 1.70+
- Python 3.9+ with `hyperliquid-python-sdk` and `eth-account` installed
- A Hyperliquid wallet with funds and HIP-3 Growth Mode coins

### Setup

```bash
# Clone and configure
git clone <repo> growth-engine
cd growth-engine

# Install Python dependencies for signing
pip install hyperliquid-python-sdk eth-account requests

# Configure environment
cp .env.example .env
# Edit .env with your HL_PRIVATE_KEY and HL_ADDRESS

# Build
cargo build --release

# Run
./target/release/growth-engine
```

### Docker (Optional)

```bash
docker build -t growth-engine .
docker run -d --env-file .env --name growth-engine growth-engine
```

## Monitoring

The engine outputs structured JSON logs to stdout. Key metrics per cycle:

```json
{
  "coin": "FARTCOIN",
  "state": "NORMAL",
  "gross_spread_bps": 5.2,
  "net_spread_bps": 4.1,
  "skew_bps": 1.1,
  "pos_ratio": 0.15,
  "wd": 98.50,
  "eq": 102.30,
  "cycle_ms": 450
}
```

Use `jq` for quick analysis:
```bash
# Watch states
./growth-engine | jq '.fields | select(.message=="cycle") | {coin: .coin, state: .state, pos_ratio: .pos_ratio}'

# Alert on emergencies
./growth-engine | jq 'select(.fields.message | test("KILL_SWITCH|EMERGENCY|LIQUIDATION"))'
```

## Defense Layers

1. **Cubic³ Price Skew** — Tick-discretized, preserves spread for first 75% of position
2. **Quadratic² Asymmetric Qty** — Reduces position-accumulating side, min-lot guarded
3. **Adaptive IOC Shedding** — 500ms loop, re-fetches L2, stops at safe re-entry
4. **Passive Unwind** — Freeze same-side, tick-retreat opposite, 20% hysteresis
5. **Portfolio Hard Limit** — Cross-coin notional ≤ equity × 40%
6. **Kill Switch** — Global $80 withdrawable floor, cancel all + IOC liquidate
7. **Circuit Breaker** — 3+ API failures in 5s → halt
8. **Dynamic Position Cap** — Shrinks during volatility: `max(min, base / vol_mult)`  
9. **Liquidation Defense** — Direction-aware, triggers at <8% distance
10. **Dust Filter** — Positions <$20 ignored for risk assessment
11. **Anti-Ping-Pong** — 60s post-unwind BUY suppression
12. **GTC Last-Resort** — IOC fails 3+ rounds → GTC sell at 95% bid

## Documentation

- [Architecture](docs/ARCHITECTURE.md) — Module relationships and data flow
- [State Machine](docs/STATE_MACHINE.md) — Full state diagram with transition conditions
- [Risk Control](docs/RISK_CONTROL.md) — All defense layers with thresholds
- [Deployment](docs/DEPLOYMENT.md) — Setup, monitor, debug guide
- [Changelog](docs/CHANGELOG.md) — V1 → V6.2 evolution

## License

MIT

## Request budget & batched placement (v12.5–v12.8)

Hyperliquid meters `/exchange` **per order**, and the cap grows with traded volume.
Both numbers are readable from `GET /info {"type":"userRateLimit"}` → `cumVlm`,
`nRequestsUsed`, `nRequestsCap`. Measured on 2026-11-16: a four-order action moved
`nRequestsUsed` by 4 while `cumVlm` grew by $60.08 — so an action carrying N orders costs
N units, and one dollar of taker volume buys back roughly one unit.

What this engine does about it:

- **Batching buys latency, not quota.** Both quote sides of a coin go out as one action
  (`MarketAdapter::place_batch`, non-unwind path in `engine.rs`). Same order count for the
  meter, one HTTP round trip, one signature, one nonce.
- **`src/quota.rs` keeps a local ledger**, seeded from the venue's own numbers
  (`GE_QUOTA_SEED=USED:CAP[:VOLUME]`) or corrected by every rejection sentence. Placement
  paths consult it before sending; cancel paths deliberately do not.
- **Conserve mode** (`GE_QUOTA_CONSERVE_BELOW=N`): when estimated headroom falls below N,
  stop quoting and keep sweeping, so the remaining requests go to risk reduction.
- **Periodic telemetry** (`GE_QUOTA_GAUGE_EVERY=N`) logs
  `QUOTA state=… headroom=… used=… limit=… ratio=… need_usdc=…` every N cycles.
- **Auto-refill is off by default** (`GE_QUOTA_AUTOFUND`) and not wired to any trading
  path: it spends real money (~$1 taker volume per request freed).

Measured on a live test account (Virginia VPS, $12 post-only orders, ETH, gate on):

| metric | serial | batch | ratio |
|---|---|---:|---:|
| HTTP requests per round (3 orders) | 6 | 2 | 3.0× fewer |
| wall time per round | 11.81 s | 3.82 s | — |
| place ack, venue round-trip only | **529 ms / order** | **642 ms / batch ack → 214 ms / order** | 2.5× per order |
| cancel ack, venue round-trip only | **872 ms / order** | **879 ms / batch ack → 293 ms / order** | 3.0× per order |

Read the batch column carefully: 214 ms is **642 ms divided by 3**, not a
214 ms single-order response. One batch ack covers three orders; the venue
round-trip itself is about 642 ms. Batching removes two HTTP round trips, two
signatures and two nonces, but it does **not** make the first order's ack faster.

Seoul rerun on 2026-11-19 (same $12 post-only ETH, 5 orders × 3 rounds):

| metric | serial | batch |
|---|---:|---:|
| `/exchange` requests per round | 10 | 2 |
| wall time per round | 9.88 s | 1.98 s |
| place ack, venue round-trip only | **434 ms / order** | **428 ms / batch ack → 86 ms / order** |
| cancel ack, venue round-trip only | **735 ms / order** | **776 ms / batch ack → 155 ms / order** |

Again, 86 ms is **428 ms divided by 5**, not an 86 ms single-order response.
The batch ack itself is the same order of magnitude as a single serial ack;
batching removes four HTTP round trips, four signatures and four nonces.
Raw rows: `evidence/latency_ledger.csv`; timer boundary: `docs/MEASUREMENT_PROTOCOL.md`.

Reproduce:
```
growth-engine bench --n 3 --rounds 1 --coin ETH --notional 12 --max-notional 15 --px-tick 0.1 --quota-gate
```
`bench` also accepts `--seed-quota USED:CAP[:VOLUME]`, `--quota-reserve N`, and
`--out PATH`. `--out` writes the full `RESULT_JSON` report to a file so it can
be appended to `evidence/latency_ledger.csv` by `scripts/latency_ledger.py`.

`place_ack_net_median_ms` starts **after** signing/nonce and covers only the
HTTP round trip plus JSON decode. `wall_net_median_s` removes the bench's own
quota backoff; it does **not** remove signing cost.
