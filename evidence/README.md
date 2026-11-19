# Evidence layer

This directory holds measurements, not marketing numbers. Every row is either a
`bench --out` JSON report or a named prior measurement whose raw JSON was not
retained.

## What the timer actually covers

`src/bench.rs` starts its `last_attempt_ms` timer inside `post_exchange_raw`,
**after** signing and nonce construction and **before** the HTTP send. It stops
after the response JSON has been decoded. So the number covers:

```
strategy intent -> risk assess -> order params -> sign/nonce -> [timer start]
HTTP send -> venue queue -> venue match/ack -> HTTP response -> JSON decode [timer stop]
```

It excludes strategy, `RiskEngine::assess()`, order-parameter construction,
signing, nonce, and Python subprocess startup. `wall_median_s` includes those
costs; `wall_net_median_s` removes only the bench's own quota backoff, not
signing.

## Total batch ack vs per-order ack

A batch request is one HTTP action carrying N orders. The venue returns one ack
for the action. Therefore:

- `batch_place_total_net_ms` is the round-trip for **one action** carrying N orders.
- `batch_place_per_order_net_ms` is `total / N`, an **amortised** number.
- It is not a single-order response time. Batching removes N-1 HTTP round trips,
  signatures and nonces; it does not make the first order's ack faster.

This is why the ledger keeps both columns.

## Files

- `latency_ledger.csv` — one row per bench report.
- `raw/*.json` — the reports emitted by `growth-engine bench --out`.

## Provenance

- `host=seoul-vps` rows were produced on the Seoul VPS on 2026-11-19 against
  Hyperliquid mainnet, with $12 post-only ETH orders and cleanup to 0 open orders.
- The `virginia-vps` row is the 2026-11-16 prior measurement quoted in the
  `v12.8` README; its raw JSON was not retained, so it is marked `prior README`
  rather than passed off as a fresh run.
