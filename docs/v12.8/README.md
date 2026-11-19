# v12.8 — what this branch is (and what its diff means)

This branch carries the commits that were **compiled and run on a live Hyperliquid account**:

| commit | content |
|---|---|
| `f1b9171` | batched placement in the quote loop, request-budget ledger (`src/quota.rs`), net-latency accounting, `bench` sub-command, README section |
| `e50996d` | single-leg guard extracted into `reconcile_single_leg` + 5 mock-adapter tests |

**Read the diff against `aeeeebf`, not against `main`.**

```
git diff aeeeebf..v12.8
git show f1b9171 e50996d
```

## Why the base is `aeeeebf` and not `main`

`main` is the *published* lineage; the deployment tree is the *live-trading* lineage. They diverged
before `aeeeebf` — 22 commits on `main` are absent here, 15 here are absent there — and `main`'s
`engine.rs` has since been restructured, so `git diff main..v12.8` renders a whole-tree rewrite that
means nothing.

Nothing here has been merged into that restructured loop, deliberately: a hand-merge would produce
code that no run ever produced, presented as the engine's state. Porting V12.7 onto `main`'s cycle is
a separate task, and it must be re-run on the venue before it can be called done.

## What this branch proves, and what it does not

Proven on the venue (Virginia VPS, live account, $12 post-only orders, ETH):

- one action per cycle instead of two removed **3.0× the HTTP requests** (6 → 2 per round),
- per-order venue round-trip: **214 ms place / 293 ms cancel** (was 529 / 872),
- batched actions are accepted and self-cancelled cleanly; end state 0 orders, 0 positions,
- conserve mode withholds placement and keeps sweeping when the request budget is below the floor.

Not proven: the single-leg guard has **unit-test coverage only** — no live "one leg rejected" event
was captured, because the venue's budget checks lag. `min_margin_bps` is still hard-coded `2.0`.

## Test status

`cargo test --offline --release` → 29 passed, 3 failed. All 3 failures are in `risk::tests`
(cubic-skew expectations); `src/risk.rs` is untouched by the commits above, and was last changed in
`483ce0f`.

## Budget accounting changed a strategy decision

`/exchange` is metered **per order**, and the cap grows with traded volume
(`POST /info {"type":"userRateLimit"}` → `cumVlm`, `nRequestsUsed`, `nRequestsCap`). A four-order
action moved `nRequestsUsed` by 4 while `cumVlm` grew by $60.08.

**Batching therefore saves round trips, signatures and nonces — not quota.** An earlier conclusion
recorded elsewhere in this project claimed batching was a way to spend fewer requests; that was
wrong (it was an artefact of the venue's lagging checks) and it is corrected in the README section
added by `f1b9171`.
