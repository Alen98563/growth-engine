#!/usr/bin/env python3
"""
🦈 STRUCTURAL TICK-SIZE SCANNER v3
Finds coins where 1 tick gross_bps >= 15.0 (coarse tick → fat margin)
gross_bps = (tick_size / mid_price) * 10000
"""
import json, requests, time

# Step 1: Get meta
print("=== Fetching meta... ===")
r = requests.post("https://api.hyperliquid.xyz/info",
    json={"type": "meta"}, timeout=10)
data = r.json()
universe = data if isinstance(data, list) else data.get("universe", [])
coins = []
for m in universe:
    if isinstance(m, dict):
        name = m.get("name")
        if name: coins.append(name)
    elif isinstance(m, list) and len(m) > 1:
        coins.append(m[0])
print(f"Total: {len(coins)} coins\n")

# Step 2: Scan ALL coins
results = []
total = len(coins)
batch_size = 10

for i in range(0, total, batch_size):
    batch = coins[i:i+batch_size]
    for c in batch:
        try:
            r2 = requests.post("https://api.hyperliquid.xyz/info",
                json={"type": "l2Book", "coin": c}, timeout=8)  # FIXED: coin not req
            if r2.status_code != 200: continue
            d2 = r2.json()
            levels = d2.get("levels", [[], []])
            if len(levels) < 2: continue
            if not levels[0] or not levels[1]: continue

            bids = levels[0]
            asks = levels[1]
            bb = float(bids[0]["px"])
            ba = float(asks[0]["px"])
            mid = (bb + ba) / 2
            if mid <= 0: continue

            # Tick size: minimum price gap between adjacent levels
            all_pxs = []
            for lvl in bids[:5]: all_pxs.append(float(lvl["px"]))
            for lvl in asks[:5]: all_pxs.append(float(lvl["px"]))
            all_pxs = sorted(set(all_pxs))
            if len(all_pxs) < 2: continue
            tick_size = min(abs(all_pxs[j] - all_pxs[j-1]) for j in range(1, len(all_pxs)))
            if tick_size <= 0: continue

            gross_bps = (tick_size / mid) * 10000

            bid_depth = sum(float(l["sz"]) * float(l["px"]) for l in bids[:3])
            ask_depth = sum(float(l["sz"]) * float(l["px"]) for l in asks[:3])

            results.append({
                "coin": c, "gross_bps": gross_bps, "tick_size": tick_size,
                "mid": mid, "bid_depth": bid_depth, "ask_depth": ask_depth,
            })
        except Exception:
            pass

    progress = min(i + batch_size, total)
    if progress % 50 == 0 or progress == total:
        print(f"  scanned {progress}/{total}...", flush=True)
    if i + batch_size < total:
        time.sleep(1.5)

# Step 3: Sort and tier
results.sort(key=lambda x: -x["gross_bps"])
t1 = [r for r in results if r["gross_bps"] >= 15.0]
t2 = [r for r in results if 8.0 <= r["gross_bps"] < 15.0]
t3 = [r for r in results if 4.0 <= r["gross_bps"] < 8.0]
t4 = [r for r in results if r["gross_bps"] < 4.0]

print(f"\n{'='*75}")
print(f"🦈 STRUCTURAL TICK-SIZE SCAN — gross_bps = tick/mid × 10000")
print(f"{'='*75}")
print(f"Total scannable: {len(results)} coins\n")

def print_tier(emoji, name, tier):
    print(f"  {emoji} {name}: {len(tier)} coins")
    if tier:
        print(f"  {'Coin':16s} {'gross_bps':>8s}  {'tick':>12s}  {'mid':>12s}  {'bid$':>9s}")
        print(f"  {'-'*60}")
        for r in tier[:20]:
            print(f"  {r['coin']:16s} {r['gross_bps']:7.1f} bps  {r['tick_size']:12.8f}  {r['mid']:12.8f} ${r['bid_depth']:8.0f}")

print_tier("🟢", "GOLD    (gross≥15 — 常态开闸)", t1)
print()
print_tier("🟡", "SILVER  (8≤gross<15 — 突袭可行)", t2)
print()
print_tier("🟠", "BRONZE  (4≤gross<8 — 精密市场)", t3)
print()
if t4:
    sample = ", ".join(r["coin"] for r in t4[:5])
    print(f"  ⚫ DUST (gross<4): {len(t4)} coins  (sample: {sample}...)")

# Top 10 by gross_bps
print(f"\n{'='*75}")
print(f"TOP 10:")
print(f"  {'Coin':16s} {'gross_bps':>8s}  {'tick':>12s}  {'mid':>12s}")
print(f"  {'-'*50}")
for r in results[:10]:
    print(f"  {r['coin']:16s} {r['gross_bps']:7.1f} bps  {r['tick_size']:12.8f}  {r['mid']:12.8f}")

# Save to file
out_path = "/root/growth-engine/data/scan_structural_v3.json"
with open(out_path, "w") as f:
    json.dump(results, f, indent=2)
print(f"\nSaved: {out_path}")
print("=== DONE ===")
