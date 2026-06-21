#!/usr/bin/env python3
"""Scan HL for wide-spread coins (potential London targets)."""
import json, requests, time

# Get meta
r = requests.post("https://api.hyperliquid.xyz/info",
    json={"type": "meta"}, timeout=10)
data = r.json()
universe = data if isinstance(data, list) else data.get("universe", [])
coins = []
for m in universe:
    if isinstance(m, dict):
        name = m.get("name")
        if name:
            coins.append(name)
    elif isinstance(m, list) and len(m) > 1:
        coins.append(m[0])
print(f"Total: {len(coins)} coins")

# Get L2 for each (batch to avoid rate limit)
results = []
batch_size = 10
for i in range(0, min(len(coins), 80), batch_size):
    batch = coins[i:i+batch_size]
    for c in batch:
        try:
            r2 = requests.post("https://api.hyperliquid.xyz/info",
                json={"type": "l2Book", "req": {"coin": c}}, timeout=8)
            if r2.status_code == 200:
                d2 = r2.json()
                levels = d2.get("levels", [[],[]])
                if len(levels) >= 2 and levels[0] and levels[1]:
                    bb = float(levels[0][0]["px"])
                    ba = float(levels[1][0]["px"])
                    mid = (bb + ba) / 2
                    spread_bps = (ba - bb) / mid * 10000
                    if spread_bps > 0:
                        bid_depth = sum(float(l["sz"]) * float(l["px"]) for l in levels[0][:3])
                        ask_depth = sum(float(l["sz"]) * float(l["px"]) for l in levels[1][:3])
                        results.append((c, spread_bps, mid, len(levels[0]), len(levels[1]), bid_depth, ask_depth))
        except Exception as e:
            pass
    if i + batch_size < len(coins):
        time.sleep(1.5)  # rate limit

results.sort(key=lambda x: -x[1])
print(f"\n{'Coin':16s} {'spread':>8s} {'tick_est':>8s} {'mid':>10s} {'bids':>5s} {'asks':>5s} {'bid_dep$':>8s}")
print("-" * 75)
for c, s, m, nb, na, bd, ad in results[:20]:
    tick_est = (m * s / 10000) / m * 10000  # approximate tick = spread
    if m > 0:
        tick_est = s  # spread_bps is our tick equivalent
    print(f"{c:16s} {s:8.1f} bps {tick_est:8.1f} {m:10.6f} {nb:5d} {na:5d} ${bd:7.0f}")

# Filter: spread >= 5bps AND bid_depth >= $100
print(f"\n{'='*60}")
print("CANDIDATES (spread >= 5bps, bid_depth >= $100):")
candidates = [(c,s,m,bd,ad) for c,s,m,nb,na,bd,ad in results if s >= 5.0 and bd >= 100]
for c, s, m, bd, ad in candidates[:15]:
    print(f"  {c:16s} spread={s:5.1f}bps  mid={m:.6f}  depth=${bd:.0f}")
