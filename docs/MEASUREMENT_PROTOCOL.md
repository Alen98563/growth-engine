# Measurement protocol — execution-path latency

This document defines what the latency numbers in this repository mean, how to
reproduce them, and what they do not prove. It exists because the most common
misreading of the `v12.8` numbers is to treat a batch's amortised per-order ack
as a single-order response time.

## Three evidence layers

| Layer | Artifact | Reproducible without keys? |
|:--|:--|:--|
| L1 — offline replay | `growth-engine replay --tape data/labels_public.csv --out demo/index.html` | yes |
| L2 — live execution | `v12.8` branch, `bench` sub-command against Hyperliquid mainnet | no — needs a funded test account |
| L3 — latency ledger | `evidence/latency_ledger.csv`, `evidence/raw/*.json` | raw rows are shipped; rerun needs a key |

## What `bench` measures

`src/bench.rs` runs two strategies in the same process against the same venue and
signer:

- **serial** — N × `POST /exchange` to place, then N × `POST /exchange` to cancel;
- **batch** — one `POST /exchange` carrying N orders, then one carrying N cancels.

Every order is **post-only (`Alo`)**, priced at `--px-off` below mid, and its
notional is checked against `--max-notional` before send. The bench cancels
everything it placed and asserts zero open orders at the end.

### Timer boundary

```
strategy intent -> risk assess -> order params -> sign/nonce -> [timer start]
HTTP send -> venue queue -> venue match/ack -> HTTP response -> JSON decode [timer stop]
```

The timer is `last_attempt_ms`, set inside `post_exchange_raw`. It starts after
the signature and nonce are built and before `client.post().send()`. It stops
after the response body is parsed.

Therefore it **excludes**:

- strategy and quote construction;
- `RiskEngine::assess()`;
- order-parameter formatting;
- EIP-712 signing and nonce construction;
- Python subprocess startup (`hl_sign.py`);
- the bench's own quota backoff (removed separately as `wall_net`).

It **includes**:

- the HTTP request write and TLS round trip;
- venue queueing, matching and ack generation;
- the HTTP response read and JSON decode.

`wall_median_s` includes the excluded costs above. `wall_net_median_s` only
removes the bench's quota backoff; it does **not** remove signing.

## Definitions

| Field | Meaning |
|:--|:--|
| `serial_place_net_ms` | median, over rounds, of the median single-order place ack (venue RTT only) |
| `serial_cancel_net_ms` | same, for a single cancel |
| `batch_place_total_net_ms` | median ack for **one** batch action carrying N placements |
| `batch_place_per_order_net_ms` | `batch_place_total_net_ms / N` — amortised, not a single-order latency |
| `batch_cancel_total_net_ms` | median ack for **one** batch action carrying N cancels |
| `batch_cancel_per_order_net_ms` | `batch_cancel_total_net_ms / N` — amortised |
| `serial_round_wall_s` | one round's wall time for 2N serial requests, including signing |
| `batch_round_wall_s` | one round's wall time for 2 batch requests, including signing |

The two batch columns must never be collapsed into one "batch latency" number.
`642 ms / 3 = 214 ms` is arithmetic, not a measurement of a 214 ms order.

## 2026-11-19 Seoul run

Environment:

- Alibaba Cloud Seoul VPS, 2 vCPU / 2 GiB / 40 GiB, 200 Mbps;
- Hyperliquid mainnet, ETH;
- $12 post-only orders, `--px-off 0.96`, `--px-tick 0.1`;
- account value ≈ $121; no positions opened; 0 open orders after each round;
- `--quota-gate` enabled; `--quota-reserve 3`.

Commands:

```bash
cd /root/ge-v128
set -a; . /root/hl.env; set +a
export GE_HOST=seoul-vps
export GE_GIT_COMMIT=$(git rev-parse --short HEAD)

# smoke: 2 orders, 1 round
./target/release/growth-engine bench \
  --n 2 --rounds 1 --coin ETH --notional 12 --max-notional 15 \
  --px-tick 0.1 --px-off 0.96 --quota-gate \
  --out evidence/raw/seoul_smoke.json

# main: 5 orders, 3 rounds
./target/release/growth-engine bench \
  --n 5 --rounds 3 --coin ETH --notional 12 --max-notional 15 \
  --px-tick 0.1 --px-off 0.96 --quota-gate \
  --out evidence/raw/seoul_n5_r3.json
```

Results (p50 of round medians):

| Metric | n=2, 1 round | n=5, 3 rounds |
|:--|--:|--:|
| Serial place ack | 519 ms | **434 ms** |
| Serial cancel ack | 776 ms | **735 ms** |
| Batch place ack, total | 404 ms | **428 ms** |
| Batch place ack, per order | 202 ms | **86 ms** |
| Batch cancel ack, total | 673 ms | **776 ms** |
| Batch cancel ack, per order | 336 ms | **155 ms** |
| Requests per round, serial | 4 | 10 |
| Requests per round, batch | 2 | 2 |
| Round wall, serial | 4.26 s | 9.88 s |
| Round wall, batch | 1.89 s | 1.98 s |

The important result is the **total** batch ack: it stays in the same
400–800 ms range as a single serial order. Batching does not make the venue
answer faster. It removes N−1 HTTP round trips, signatures and nonces, and it
lets one ack cover N orders. The per-order improvement is amortisation.

## What this does not prove

- **Not an HFT system.** The engine cycle is 3–5 seconds; the Python signing
  subprocess costs ~1.2 s per call. No colocation, no kernel bypass, no FPGA.
- **Not production size.** $12 orders on a ~$121 account. Queue position and
  market impact at size are not measured.
- **Not multi-venue.** One venue, one region (Seoul), one coin (ETH).
- **Not a P&L result.** The bench places and cancels; it does not hold a
  position. The 10-hour pilot P&L is separate and is labelled model/pilot in the
  main README.
- **Not the full order path.** Strategy, risk assessment, order parameters,
  signing, nonce and Python startup are outside the timer.

## Re-run checklist

1. Fund a **test account**; never use a production key.
2. Put `HL_PRIVATE_KEY` and `HL_ADDRESS` in `.env` or export them; never commit
   them. `.gitignore` already excludes `.env`.
3. Build: `cargo build --release --offline`.
4. Start with `--n 2 --rounds 1`; confirm `cleanup: 0 open orders left`.
5. Run the main measurement with `--out`.
6. Append: `python3 scripts/latency_ledger.py evidence/latency_ledger.csv evidence/raw/<file>.json`.
7. Verify the account still has 0 open orders and 0 positions.
