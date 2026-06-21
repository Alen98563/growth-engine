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
