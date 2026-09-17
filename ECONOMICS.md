# Economics — Why Fee Regime Is the Only Variable That Matters

> Companion to README §2. Run `python3 scripts/economics_model.py` to reproduce every number here.

## 1. The one equation

A market maker earns, per fill:

```
edge_per_fill  =  (quoted_spread / 2) × fill_efficiency  −  maker_fee
```

- `quoted_spread / 2` — a passive fill sits on one side of the book, so it captures half the spread.
- `fill_efficiency` — the haircut for adverse selection (you get filled when you're wrong) and queue position (you don't get filled when you're right).
- `maker_fee` — on Hyperliquid this can be **negative**. A rebate means the venue pays you per fill.

With the repo's conservative inputs:

```
(10.0 bps / 2) × 0.50  =  2.50 bps gross capture
2.50 bps − (−0.30 bps) =  2.80 bps net edge per fill
```

## 2. Why the project was shelved

The engine was shelved on **2026-06-23** for a purely economic reason. At Tier-0 standard fees:

```
maker_fee = 1.50 bps
```

Against the **coarse-tick** universe (median spread 11.33 bps, gross capture 2.50 bps) the edge was +1.00 bps — thin. Against **fine-tick** names it was fatal:

| Coin | 1-tick spread | Tier-0 maker fee | Verdict |
|:--|--:|--:|:--|
| RESOLV | 0.42 bps | 1.50 bps | **−1.08 bps → guaranteed loss** |
| FARTCOIN | 3.22 bps (observed p50) | 1.50 bps | ≈ break-even, negative after toxicity |

A market maker quoting into a 0.42 bps spread while paying 1.50 bps is **writing a structural loss**. No amount of engineering fixes arithmetic.

## 3. Why Growth Mode flips the sign

Growth Mode reduces protocol fees, rebates and volume contributions by **90%**. At deployer fee scale 0.1111:

```
maker:  1.5 bps × 1.1111 × 0.10  =  0.167 bps
taker:  4.5 bps × 1.1111 × 0.10  =  0.500 bps
```

The maker fee is now **9× below the break-even line** for the coarse-tick universe. Add the Tier-3 maker rebate (−0.30 bps) and the venue is *paying* for liquidity.

| Regime | Maker fee | Net edge/fill | Annual P&L on $1M @ 6× |
|:--|--:|--:|--:|
| Tier-0 Standard | +1.500 | +1.000 bps | $219k |
| Tier-0 + HIP-3 (2×) | +3.000 | **−0.500 bps** | **−$110k** |
| HIP-3 Growth Mode | +0.167 | +2.333 bps | $511k |
| GM + VIP Tier 4 | 0.000 | +2.500 bps | $548k |
| **GM + Rebate Tier 3** | **−0.300** | **+2.800 bps** | **$613k** |

Note the HIP-3 row: **naively deploying on a builder market at the default 2× schedule makes the strategy strictly worse than retail**. Fee scale selection is not a detail — it is the entire strategy.

## 4. Turnover is a multiplier, not a lever

Annual P&L scales **linearly** with turnover:

```
annual_P&L = capital × turnover × 365 × edge / 10,000
```

| Turnover | GM + Rebate T3 | Return on capital |
|:--|--:|--:|
| 1× | $102k | 10.2% |
| 3× | $307k | 30.7% |
| **6×** | **$613k** | **61.3%** |
| 12× | $1.23M | 122.6% |
| 24× | $2.45M | 245.3% |

Turnover is bounded in practice by **market share**, not by engine throughput.

## 5. Capacity, honestly

Base case needs **$6M/day** of maker volume. GE's scanner measured **$19.8M/day** across just 10 qualifying names → **~30% share**.

That is a large share of a small universe. Three honest paths to scale:
1. **Widen the universe** — 169 coins passed the volume gate; the 10 names are the *coarse-tick* subset only.
2. **Lower turnover** — 3× turnover still yields $307k/yr at 30.7% ROCE, at only 15% share.
3. **Multi-venue** — the risk engine is venue-agnostic; only `executor.rs` + `signer.rs` are exchange-specific.

## 6. What the model deliberately ignores

Being explicit about the omissions is what makes the model credible:

- **Funding P&L** — assumed flat. A market maker running near-zero net inventory pays/receives little, but this is not zero in volatile regimes.
- **Inventory risk** — the engine's job is to keep net exposure small; residual directional risk is not priced here.
- **Latency / adverse selection beyond the 50% haircut** — the 0.50 fill efficiency is a blunt instrument. A real deployment should measure it directly.
- **Fee tier dynamics** — the rebate tier requires > 3% of *platform* maker volume, which a $1M book alone will not reach. It is modelled as an *aspirational* regime.
- **Operational cost** — servers, monitoring, and the cost of being wrong.

## 7. The falsifiable claim

> **A market-making engine with a 10 bps average quoted spread and 50% fill efficiency is profitable on Hyperliquid if and only if its effective maker fee is below ~2.5 bps.**

That is the entire thesis. It is falsifiable, measurable, and independent of the code. Run `scripts/economics_model.py`, change one constant, watch the conclusion move.
