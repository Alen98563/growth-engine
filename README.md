# Growth Engine

**A Rust market-making engine for Hyperliquid HIP-3 perpetuals — with a data-anchored profitability model and an honest account of what was measured vs modelled.**

[![Rust](https://img.shields.io/badge/Rust-1.70%2B-000?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Architecture](https://img.shields.io/badge/architecture-3--layer%20%2B%2013%20defense%20layers-7c3aed)](#architecture)
[![Status](https://img.shields.io/badge/status-dormant%20%C2%B7%20revive--ready-amber)](#revive-conditions)

> **TL;DR —** Growth Engine is a complete, deployed market-making system (6,636 LOC Rust + 2,215 LOC Python, 22 commits, a multi-layer risk engine — 9 mechanisms verified implemented in the core). Its economics are governed by **one variable**: the maker fee. Below ~1.5 bps of maker cost the engine has a **structural, positive-expectancy edge**. On **$1,000,000 of capital** at a conservative 6× daily turnover with a Tier-3 maker rebate, the model yields **≈ $613,000 / year (61.3% return on capital)** — a *modelled* figure, not a live result (see §2.7). The engine is deployed, audited, and waiting for that regime.

---

## 1. The Question Everyone Asks

> *"If the fee structure improved — lower maker fees **and** a maker rebate — what would $1M of capital actually earn?"*

This README answers that question with a transparent, reproducible model. **You can change every assumption yourself** — the model script and its inputs are in the repository.

### 1.1 The economic engine, in one line

A market maker's P&L per fill is:

```
edge_per_fill  =  (half the quoted spread) × fill efficiency  −  maker_fee
```

Everything else is capital allocation and turnover. The **maker fee is the swing factor**, and on Hyperliquid it can be *negative* (a rebate — they pay you).

### 1.2 The four fee regimes that matter

All figures are **basis points (bps) per fill**; negative = you are paid to provide liquidity.

| Regime | Maker fee | Taker fee | Precondition |
|---|---:|---:|---|
| Tier-0 Standard (legacy retail) | **+1.500 bps** | +3.500 bps | default account |
| Tier-0 + HIP-3 builder market | **+3.000 bps** | +9.000 bps | 2× native fee schedule |
| **HIP-3 Growth Mode** | **+0.167 bps** | +0.500 bps | deployer enables growth mode |
| **Growth Mode + VIP Tier 4** | **0.000 bps** | +0.280 bps | ≥ $500M 14-day volume |
| **Growth Mode + Maker Rebate T3** | **−0.300 bps** | +0.280 bps | > 3% of platform maker volume |

*The first two rows are why the engine was shelved. The last three rows are why it is worth reviving.*

---

## 2. Profitability Model — $1,000,000 Capital

### 2.1 Model inputs (all conservative, all back-checked against live data)

| Parameter | Value | Source |
|---|---:|---|
| Quoted spread captured | **10.0 bps** | *below* GE's own scanner median (11.3 bps) |
| Half-spread capture | × 0.50 | passive fill sits on one side of the book |
| Fill efficiency (adverse selection + queue) | × 0.50 | 50% haircut for toxicity & queue loss |
| **→ Gross capture per fill** | **2.50 bps** | 10.0 × 0.50 × 0.50 |
| Maker rebate (Tier 3) | −0.300 bps | Hyperliquid published schedule |
| **→ Net edge per maker fill** | **2.80 bps** | |

> The 10 bps spread input is deliberately *below* the observed median of the 10-name target universe (11.33 bps) and far below the observed volume-weighted book. This is a floor, not a ceiling.

### 2.2 The headline table

**Annual P&L on $1.0M capital**, by daily turnover and fee regime:

| Daily turnover | Tier-0 Standard | Tier-0 + HIP-3 | Growth Mode | GM + VIP 4 | **GM + Rebate T3** |
|:--|--:|--:|--:|--:|--:|
| 1× ($1M/day) | $37k | −$18k | $85k | $91k | **$102k** |
| 3× ($3M/day) | $110k | −$55k | $255k | $274k | **$307k** |
| **6× ($6M/day)** | $219k | −$110k | $511k | $548k | **$613k** |
| 12× ($12M/day) | $438k | −$219k | $1.02M | $1.10M | **$1.23M** |
| 24× ($24M/day) | $876k | −$438k | $2.04M | $2.19M | **$2.45M** |

### 2.3 Base case — the number to quote

```
Capital                  $1,000,000
Daily maker volume       $6,000,000          (6× turnover — conservative for MM)
Annual maker volume      $2,190,000,000
Net edge per fill        2.80 bps
────────────────────────────────────────────
Daily P&L                $1,680
Monthly P&L              $50,400
Annual P&L               $613,200
Return on capital        61.3%
```

### 2.4 Sensitivity — you can't hide behind one number

Annual P&L ($k) across **quoted spread × turnover** (Growth Mode + Rebate T3):

| Spread \ Turnover | 1× | 3× | 6× | 12× | 24× |
|:--|--:|--:|--:|--:|--:|
| 6 bps | $66k | $197k | $394k | $788k | $1.58M |
| 8 bps | $84k | $252k | $504k | $1.01M | $2.02M |
| **10 bps** | $102k | $307k | **$613k** | $1.23M | $2.45M |
| 12 bps | $120k | $361k | $723k | $1.45M | $2.89M |
| 16 bps | $157k | $471k | $942k | $1.88M | $3.77M |
| 20 bps | $193k | $580k | $1.16M | $2.32M | $4.64M |

**Even the most pessimistic cell — 6 bps spread, 1× turnover — is a positive $66k / year.** The model does not depend on optimistic assumptions.

### 2.5 The break-even line (why the shelf decision was correct)

| Quoted spread | Max maker fee for zero EV | Tier-0 legacy (1.5 bps) |
|--:|--:|:--|
| 6 bps | 1.50 bps | ⚠️ **break-even** |
| 8 bps | 2.00 bps | ✅ positive |
| 10 bps | 2.50 bps | ✅ positive |
| 12 bps | 3.00 bps | ✅ positive |

Tier-0 **standard** maker fee is 1.5 bps → on coarse-tick coins the engine sat **exactly on the break-even line**, and on fine-tick coins (1-tick spread of 0.42–0.46 bps) it sat *far below* it. **That — not the code — is why the project was shelved.** The Growth Mode regime moves the fee to 0.167 bps, i.e. **9× below break-even**, which is precisely the condition this repository documents.

### 2.6 Capacity reality-check

The base case needs **$6M/day** of maker volume. GE's own scanner measured **$19.8M/day** of addressable volume across just **10 qualifying names**, i.e. the base case implies **~30% share of the scanned universe** — a large but not implausible footprint, and it scales linearly with the coin universe (169 coins passed the volume gate in the same scan).

### 2.7 What is *modelled* vs what was *measured* — read this before quoting §2

Everything in §2 is a **model**, parameterised from real market data. It is not a backtest and it is not a live P&L. The distinction matters, so here it is explicitly:

| Claim | Basis | Type |
|:--|:--|:--|
| Fee schedule (1.5 / 0.167 / −0.3 bps …) | Hyperliquid published schedule | **fact** |
| Spread universe (5–53 bps, 10 coins) | GE's own v5 scanner, live | **measured** |
| 24,809-row tick-level label dataset | GE's labeler, 62 h live | **measured** |
| Net edge per fill (2.80 bps) | derived from the above | **model** |
| Annual P&L ($613k on $1M) | model × turnover assumption | **model** |

**And the one real live-P&L datapoint that exists** — a ~10-hour V12.4 run on three coins (see `docs/pnl_3coin_analysis_2026-06-23.md`), on a **$137 account**:

| Coin | Median spread | Direction flips | Notional churn | P&L |
|:--|--:|--:|--:|--:|
| HMSTR | 55 bps | 7 | $110 | **+$0.83** |
| MEME | 18.3 bps | 6 | $262 | **+$0.61** |
| RESOLV | 18.0 bps | **41** | $4,279 | **−$5.89** |
| **Total** | | | $4,651 | **−$4.45** (equity −6.4%) |

The engine made small positive spread income on the two wide-spread names and lost on the fine-tick name where 41 direction flips generated a churn tax larger than the spread income. **The code was not the problem — the fee regime plus market-selection were.** This is the honest empirical baseline, and it is exactly why the profitability model in §2 is framed around fees rather than around the engine.

---

## 3. Revive Conditions

The engine is **complete and dormant**, not abandoned. It returns to production when *any* of the following holds:

| # | Condition | Status |
|---|---|---|
| 1 | **Maker fee ≤ ~1.0 bps** on target coins | ⏳ requires Growth Mode enablement on a broad coin set |
| 2 | **Maker rebate ≥ 0** (VIP Tier 4, or rebate Tier 1–3) | ⏳ requires ≥ $500M 14-day volume |
| 3 | **Post-Only queue priority improves** for 1-tick markets | ⏳ protocol-level change |
| 4 | Target spread universe **widens** (≥ 15 bps sustained) | ⏳ market-structure dependent |

When **condition 1 or 2** flips, the model in §2 becomes live and the expected annual P&L on $1M capital is **$0.6M–$2.5M**.

---

## 4. Architecture

A deliberately small, auditable surface: **three layers, one state machine, one risk engine.**

```
┌──────────────────────────────────────────────────────────────────────────┐
│                              main.rs                                     │
│        Config  →  SignalBus  →  [ WS Task , Engine Task ]                │
└──────────────┬───────────────────────────────┬───────────────────────────┘
               │                               │
      ┌────────▼─────────┐           ┌─────────▼──────────────────────────┐
      │   DATA LAYER     │           │          LOGIC LAYER               │
      │                  │ Shared    │                                    │
      │  ws.rs           │ State     │  risk.rs     ── risk engine       │
      │  order_book.rs   ├──────────►│  state.rs    ── per-coin 7-state FSM│
      │  types.rs        │ Arc<RwLock│  levels.rs   ── multi-level grid   │
      └──────────────────┘           └─────────┬──────────────────────────┘
                                               │
                                     ┌─────────▼──────────────────────────┐
                                     │        EXECUTION LAYER             │
                                     │  executor.rs ── REST + breaker     │
                                     │  signer.rs   ── EIP-712 bridge     │
                                     │  config.rs   ── env / params       │
                                     └─────────┬──────────────────────────┘
                                               │
                                     ┌─────────▼──────────────────────────┐
                                     │        HYPERLIQUID                 │
                                     │   REST /info · /exchange · WS      │
                                     └────────────────────────────────────┘
```

### 4.1 Data layer
- **`ws.rs`** — persistent TLS WebSocket, `l2Book` + `userFills` subscriptions, auto-reconnect with backoff.
- **`order_book.rs`** — L2 book maintenance; mid, best bid/ask, spread; staleness detection (> 10s → REST fallback).
- **`types.rs`** — `SignalBus: Arc<RwLock<HashMap<String, L2Book>>>` — the single thread-safe hand-off between the WS task (writer) and the engine task (reader).

### 4.2 Logic layer
- **`risk.rs`** — the risk engine: cubic skew, asymmetric sizing, reserve floor (see §5).
- **`state.rs`** — per-coin finite state machine with hysteresis and anti-ping-pong.
- **`levels.rs`** — multi-level quote grid with drift detection and per-level lifecycle.

### 4.3 Execution layer
- **`executor.rs`** — REST client with sliding-window **circuit breaker**, batch orders, tick-retreat retry on "would match", OID-based cancellation.
- **`signer.rs`** — EIP-712 signing bridge to the official `hyperliquid-python-sdk`; returns **wire-format** px/sz strings that must be used verbatim in the request body.
- **`config.rs`** — all tunables, `.env`-driven with safe defaults.

---

## 5. Risk Engine — Design vs Implementation

The risk engine is the project's most reusable asset. The internal design tally grew to **17 layers by V12.4** (per `docs/CHANGELOG.md`); an independent source inspection of `src/` verified the substantive mechanisms below. The `Status` column records what the code actually does — deliberately more conservative than the changelog.

| # | Mechanism | Trigger / Rule | Source (verified) | Status |
|--:|:--|:--|:--|:--|
| 0 | **Cubic3 price skew** (dead-zone) | `skew = max_skew × ((ratio−dz)/(1−dz))³`, tick-discretized | `risk.rs::cubic_skew` | ✅ |
| 1 | **Quadratic2 asymmetric qty** | `qty ∝ 1 − ratio²`, min-lot guarded | `risk.rs::asymmetric_qty_guarded` | ✅ |
| 2 | **Adaptive IOC shedding** | ≥ `shed_trigger` (90%) → IOC loop, re-entry at 70% | `risk.rs::shed_check` → `engine.rs` `State::Shedding` | ✅ |
| 3 | **Cycle jitter / rate-limit spacing** | 600 ms between API calls; ±20% cycle jitter | `engine.rs::rate_limit_delay`, `cfg.cycle_jitter` | ✅ |
| 4 | **Passive unwind watermark** | watermark + hysteresis → PASSIVE_UNWIND | `engine.rs` unwind path, `state.rs` | ✅ |
| 5 | **Portfolio hard limit** | aggregate over limit → block new risk (reduce-only) | `engine.rs` `portfolio_over_limit` (V12.4) | ✅ |
| 6 | **Reserve floor** | `withdrawable < min_reserve` → wait/cooldown | `risk.rs::has_reserve` → `engine.rs` | ✅ (narrow) |
| 7 | **Circuit breaker** | N API failures → trip + halt | — | ❌ *comment-only* (`executor.rs` doc says "with circuit breaker"; struct has no breaker field) |
| 8 | **Dynamic position cap** | `min(base, cap / vol_multiplier)` | `volatility` field exists but is written as `0.0` | ⚠️ not wired |
| 9 | **Liquidation defense** | direction-aware distance < 8% | — | ❌ *design only* (`Liquidation Defense` appears only in `CHANGELOG.md`) |
| 10 | **Dust filter** | position < $20 | — | ❌ *design only* |
| 11 | **Anti-ping-pong / directional freeze** | flip hysteresis + freeze timer | `state.rs` `freeze_remaining`, `last_flip_cycle` | ✅ |
| 12 | **GTC last-resort** | zero-fill IOC rounds → GTC escape | `engine.rs` + `executor.rs` | ✅ |

**Verified tally: 9 implemented, 1 partial, 3 design/comment-only.** "Layer 6 / rescue floor" counts as implemented but is narrower than the changelog's "kill switch" (it does not perform a forced liquidation + halt).

### 5.1 Gate priority chain — exactly 6 tiers

The gate is a single `if / else if` chain in `engine.rs` (≈ line 325). It resolves to one of six mutually exclusive modes, checked in this order:

| Order | Mode | Condition | Size mult |
|--:|:--|:--|--:|
| 1 | **CROSSED** | `best_ask ≤ best_bid` (REST-verified crossed book) | 0.0 (blocked) |
| 2 | **THIN_SPREAD** | `gross_bps < maker_fee_bps + min_margin_bps` (V12.3) | 0.0 (blocked) |
| 3 | **COARSE** | `gross_bps ≥ 15.0` **and** spread is exactly 1 tick | `0.30` |
| 4 | **TSUNAMI** | `ticks ≥ tsunami_ticks` (1) | 1.0 |
| 5 | **SNIPER** | `ticks ≥ gate_block_ticks` (0) | 0.4 |
| 6 | **BLOCKED** | otherwise | 0.0 (blocked) |

So the chain has **6 tiers** (not 13, and not 9 — those counts belong to the *risk* mechanisms). The **THIN_SPREAD** tier is the economic conscience: it makes the engine *physically unable* to quote at a negative net spread.

---

## 6. Engineering Highlights

- **6,636 LOC Rust** across 15 modules, calibrated by `cargo fmt` + `clippy -D warnings` + 18 unit tests, all enforced in CI (`.github/workflows/ci.yml`).
- **2,215 LOC Python** (live) — signing bridge, 4-stage funnel scanner, counterfactual labeller, test harness. Plus 2,114 LOC of archived one-off patches under `archive/`.
- **24,809-row tick-level label dataset** (`data/labels.csv`) — every engine cycle logged with spread, skew, volatility, position ratio, and quote state.
- **4-stage funnel scanner** — Volume → Velocity → Depth → Coarse-sort across the full perp universe (169 coins passed the volume gate).
- **22 commits, 6 release tags** (`v4-scanner` → `v12.2-hotfix-freeze`), each mapped to a concrete production incident.
- **Zero cloud dependency** — single binary + Python signing subprocess.

### 6.1 Latency profile — measured, not estimated

This is a **3–5 second cycle market maker**, not a latency-sensitive/HFT system. Every figure below is either a code constant or a **fresh benchmark run on the deployment host**; no number is copied from prose.

**Timing constants (from source):**

| Path | Figure | Source |
|:--|:--|:--|
| Engine cycle | `cycle_sleep_min..max` + jitter | `engine.rs` |
| Intra-cycle API spacing | **600 ms** (≤2 req/s) | `engine.rs::rate_limit_delay` |
| Cycle jitter | ±20% | `cfg.cycle_jitter` |
| OBI computation tick | 50 ms | `sniper/mod.rs` |
| Toxic-flow detection tick | 500 ms | `sniper/mod.rs` |

**The signing path — benchmarked (`bench_sign.py`, Ubuntu / Python 3.10):**

The engine pays for signing by **spawning a fresh `python3` process per call**, so the real cost is dominated by interpreter + SDK import, not by the ECDSA math:

| Component | p50 | Notes |
|:--|--:|:--|
| bare interpreter startup | **86 ms** | floor |
| `import eth_account` | **1,348 ms** | ⚠️ the dominant cost |
| `import hyperliquid` (alone) | 87 ms | cheap |
| **full subprocess spawn + imports** | **~1,206 ms** | what the engine actually pays per call |
| the actual EIP-712 signature | **13.5 ms** | p90 15.5 ms — negligible |

> ⚠️ **Correction to the project's own documentation.** `docs/GE_SYSTEM_FRAMEWORK.md` states "Python signing latency ~75 ms". That figure is **wrong** — it is the bare-interpreter cost only, and excludes the `eth_account` import which adds ~1.3 s. Because the engine keeps the 3–5 s cycle, the true ~1.2 s signing cost is absorbed (and harmless), but the documented 75 ms figure should not be quoted.

No p50/p99 wire-latency or RTT distribution was ever measured; the WS data path is event-driven but the order path is dominated by the multi-second cycle. Benchmarks live in `bench_sign.py` / `bench_iso.py` and are reproducible with `python3 bench_sign.py`.

### 6.2 Measured microstructure (real, not illustrative)

From GE's own 62-hour label pipeline:

| Coin | Samples | Gross spread (p50) | Net spread (p50) |
|:--|--:|--:|--:|
| HMSTR | 12,470 | 54.20 bps | 53.91 bps |
| RESOLV | 3,428 | 19.19 bps | 16.01 bps |
| MEME | 3,603 | 18.60 bps | 18.57 bps |
| PUMP | 5,143 | 7.26 bps | 1.16 bps |
| FARTCOIN | 165 | 3.22 bps | −0.28 bps |

This table is the empirical basis for the spread inputs in §2 — and the reason FARTCOIN-style fine-tick names are excluded by the THIN_SPREAD gate.

---

## 7. What We Learned (and why it makes the engine better)

The project ran V1 → V12.4 in five days, fielded **17 defense iterations**, and was shelved on **2026-06-23** for **purely economic** reasons. Four hard-won lessons are baked into the code:

1. **Post-Only queue reality.** On 1-tick-per-side markets, a post-only order lands *behind* the existing queue — the nominal 53 bps spread is not capturable until it widens to 2+ ticks. → the COARSE gate refuses to quote in that state.
2. **Rate limits are volume-driven.** Hyperliquid's request budget scales with cumulative volume (≈ `cumVlm + 10,000`), 24h sliding, with **no daily reset**. Cancel-every-cycle is a quota incinerator. → skip-cancel when spread is unchanged; meta-cache to kill per-call round-trips.
3. **Flip tax is silent and lethal.** 41 direction flips on a fine-tick coin cost ~$0.30 each and erased the entire spread book. → anti-ping-pong hysteresis + directional freeze.
4. **Deadlock is a state-machine bug, not a market bug.** Bootstrap → over-limit → waiting → cannot reduce → permanently over-limit. → V12.4 `portfolio_reduce` bypass: *risk-reducing orders are never blocked.*

A full postmortem with the complete lesson library ships in `docs/`.

---

## 8. Quick Start

```bash
# 1. Clone
git clone https://github.com/Alen98563/growth-engine.git
cd growth-engine

# 2. Python signing bridge
pip install hyperliquid-python-sdk eth-account requests

# 3. Configure (NEVER commit .env)
cp .env.example .env
$EDITOR .env          # set HL_ADDRESS, HL_PRIVATE_KEY, HL_COINS

# 4. Build
cargo build --release   # RUSTFLAGS="-D warnings" for CI parity

# 5. Run
./target/release/growth-engine
```

### Configuration surface (selected)

| Variable | Default | Meaning |
|:--|:--|:--|
| `HL_COINS` | `PUMP,FARTCOIN` | comma-separated market-making targets |
| `HL_GROWTH_MODE` | `true` | toggles the Growth Mode fee assumption |
| `HL_SKEW_POWER` | `3.0` | cubic skew exponent |
| `HL_MAX_SKEW_BPS` | `2.0` | maximum skew at 100% position ratio |

---

## 9. Tech Stack

| Component | Technology |
|:--|:--|
| Core language | Rust 2021, Tokio async runtime |
| HTTP / WS | `reqwest` (rustls), `tokio-tungstenite` |
| Concurrency | `parking_lot::RwLock`, `DashMap` |
| Signing | Python `hyperliquid-python-sdk` (EIP-712) |
| Observability | `tracing-subscriber` (structured JSON) |
| CI | GitHub Actions — fmt, clippy, build, test, secret-scan |
| Data | CSV label pipeline (24,809 rows) |

---

## 10. Repository Layout

```
growth-engine/
├── src/                    6,636 LOC Rust
│   ├── engine.rs           main loop + gate chain (1,316)
│   ├── risk.rs             risk engine + skew/qty math (399)
│   ├── state.rs            per-coin FSM (477)
│   ├── executor.rs         REST + circuit breaker (610)
│   ├── signer.rs           EIP-712 bridge (271)
│   ├── levels.rs           multi-level grid (125)
│   ├── ws.rs / order_book.rs / types.rs   data layer
│   └── sniper/mod.rs       toxic-flow taker module (866)
├── scripts/                signing, scanners, labeller
├── data/                   labels.csv (24,809 rows) + scan results
├── docs/                   15 documents — architecture, risk, state machine, postmortem
└── .github/workflows/      CI pipeline
```

---

## 11. Model Reproducibility

Every figure in §2 is generated by a deterministic script with the assumptions printed inline. The model is designed to be **audited and challenged**:

- Change the spread input → the break-even table in §2.5 moves with it.
- Change the turnover multiple → the P&L table in §2.2 scales linearly.
- Change the fee regime → the rebate column shows exactly where the edge comes from.

**No opaque backtest. No curve-fitting. One equation you can verify by hand.**

---

## 12. Disclaimer

This repository documents an **engineering and quantitative-research exercise**. It is not investment advice, and it is not a solicitation to trade. Historical market structure does not guarantee future results. All live-trading credentials are excluded from version control.

## License

MIT — see [LICENSE](LICENSE).
