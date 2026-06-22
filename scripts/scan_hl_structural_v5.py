#!/usr/bin/env python3
"""
🦈 STRUCTURAL TICK-SIZE SCANNER v5 — 四维漏斗
Four-stage funnel: Volume Gate → Trade Velocity Gate → Depth Gate → Coarse Tick Sort

漏斗 1: 24h 名义成交量 ≥ $50,000 (dayNtlVlm)
漏斗 2: ⚡ NEW — 过去 10 分钟真实成交笔数 > 0 (1m candle volume > 0 at least once)
漏斗 3: 盘口 1% 有效深度 ≥ $2,000 (min bid/ask notional within 1%)
漏斗 4: 存活币按 gross_bps 降序排列，GOLD/SILVER/BRONZE 分档

V5 新增: Trade Velocity Gate 解决"纸面点差幻觉"——币种有成交量和深度，但近 10 分钟
零笔成交 = 僵尸市场，直接剔除。

API:
  metaAndAssetCtxs → dayNtlVlm per coin
  candleSnapshot → 1m candle volume → velocity check
  l2Book → bid/ask levels → tick_size + depth
"""
import json, requests, time, sys, os
from datetime import datetime

HL_INFO = "https://api.hyperliquid.xyz/info"
TIMEOUT = 10

# ── 可调参数 ──
MIN_DAY_NTL_VLM = 50_000        # 漏斗1: 最低 24h 名义成交额
VELOCITY_WINDOW_MIN = 10         # 漏斗2: 回溯多少分钟检查成交
MIN_DEPTH_1PCT = 2_000          # 漏斗3: 最低 1% 纵深名义价值
MAX_COINS_TO_FETCH = 500        # 最多扫描币种数
BATCH_SIZE = 10                 # L2 批次
DEPTH_PCT = 0.01                # 深度纵深百分比 (1%)
OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__)) + "/../data"
OUTPUT_FILE = os.path.join(OUTPUT_DIR, "scan_structural_v5.json")


def fetch_with_retry(url, payload, retries=3):
    """带重试的 HTTP POST"""
    for attempt in range(retries):
        try:
            r = requests.post(url, json=payload, timeout=TIMEOUT)
            if r.status_code == 200:
                return r.json()
            print(f"  ⚠️ HTTP {r.status_code}, retry {attempt+1}/{retries}")
        except Exception as e:
            print(f"  ⚠️ {e}, retry {attempt+1}/{retries}")
        time.sleep(1.5)
    return None


def stage1_volume_gate():
    """
    漏斗 1: 24h 日成交量硬门禁
    """
    print("━" * 70)
    print("🟡 漏斗 1: 24h 日成交量门禁 (Volume Gate)")
    print(f"   阈值: dayNtlVlm ≥ ${MIN_DAY_NTL_VLM:,}")
    print("━" * 70)

    data = fetch_with_retry(HL_INFO, {"type": "metaAndAssetCtxs"})
    if not data:
        print("❌ 无法获取 metaAndAssetCtxs")
        sys.exit(1)

    universe_info = data[0]
    ctxs = data[1]
    universe = universe_info.get("universe", [])

    passed = {}
    killed = 0
    total_with_ctx = 0

    for i, entry in enumerate(universe[:MAX_COINS_TO_FETCH]):
        if not isinstance(entry, dict):
            continue
        name = entry.get("name", "")
        if not name:
            continue

        if i >= len(ctxs):
            continue
        total_with_ctx += 1

        ctx = ctxs[i]
        day_vlm = float(ctx.get("dayNtlVlm", 0))

        if day_vlm >= MIN_DAY_NTL_VLM:
            passed[name] = {
                "dayNtlVlm": day_vlm,
                "midPx": float(ctx.get("midPx", 0)),
                "ctx_idx": i,
            }
        else:
            killed += 1

    print(f"   扫描: {total_with_ctx} 币 → ✅ 通过: {len(passed)} | ❌ 成交量不足: {killed}")
    return passed


def stage2_velocity_gate(vol_survivors):
    """
    漏斗 2: ⚡ 成交速度门禁 (Trade Velocity Gate)
    对漏斗 1 存活币拉 1m candles，检查最近 N 分钟内是否有真实成交。
    如果全部 0 成交量 → ZOMBIE，一票否决。
    """
    print(f"\n━" * 70)
    print(f"⚡ 漏斗 2: 成交速度门禁 (Trade Velocity Gate)")
    print(f"   回溯: 过去 {VELOCITY_WINDOW_MIN} 分钟 | 条件: 至少有一根 1m candle volume > 0")
    print("━" * 70)

    end_time_ms = int(time.time() * 1000)
    start_time_ms = end_time_ms - VELOCITY_WINDOW_MIN * 60 * 1000

    coins = list(vol_survivors.keys())
    passed = {}
    zombies = []
    total = len(coins)

    for i, coin in enumerate(coins):
        try:
            payload = {
                "type": "candleSnapshot",
                "req": {
                    "coin": coin,
                    "interval": "1m",
                    "startTime": start_time_ms,
                    "endTime": end_time_ms,
                },
            }
            r = requests.post(HL_INFO, json=payload, timeout=TIMEOUT)
            if r.status_code != 200:
                zombies.append((coin, f"HTTP {r.status_code}"))
                continue

            candles = r.json()
            if not isinstance(candles, list):
                zombies.append((coin, "bad response"))
                continue

            # Check if any candle had non-zero volume
            has_trades = False
            total_volume = 0.0
            for c in candles:
                v = float(c.get("v", 0))
                if v > 0:
                    has_trades = True
                total_volume += v

            if has_trades:
                passed[coin] = vol_survivors[coin]
                passed[coin]["velocity_total_vol"] = total_volume
                passed[coin]["velocity_active_minutes"] = sum(
                    1 for c in candles if float(c.get("v", 0)) > 0
                )
            else:
                zombies.append((coin, f"0 volume in last {VELOCITY_WINDOW_MIN}min"))

        except Exception as e:
            zombies.append((coin, str(e)))

        if (i + 1) % 20 == 0:
            sys.stdout.write(f"\r  扫描: {i+1}/{total} ...")
            sys.stdout.flush()
        time.sleep(0.3)  # gentle rate limiting

    print(f"\r  扫描: {total}/{total} ...")
    print(f"  ✅ 活跃: {len(passed)} | ☠️ 僵尸: {len(zombies)}")

    for coin, reason in zombies[:10]:
        print(f"    ☠️ {coin}: {reason}")
    if len(zombies) > 10:
        print(f"    ... and {len(zombies)-10} more zombies")

    return passed, zombies


def compute_1pct_depth(levels, pct, mid_px):
    """计算距 BBO 指定百分比内的累计名义价值"""
    if not mid_px or mid_px <= 0:
        return 0.0
    depth_notional = 0.0
    for lvl in levels:
        px = float(lvl["px"])
        sz = float(lvl["sz"])
        deviation = abs(px - mid_px) / mid_px
        if deviation <= pct:
            depth_notional += sz * px
        else:
            break
    return depth_notional


def stage3_depth_gate(velocity_survivors):
    """
    漏斗 3: 盘口有效深度门禁
    """
    print(f"\n━" * 70)
    print(f"🟠 漏斗 3: 盘口 1% 有效深度门禁 (Depth Gate)")
    print(f"   阈值: min(bid_depth_1%, ask_depth_1%) ≥ ${MIN_DEPTH_1PCT:,}")
    print("━" * 70)

    coins = list(velocity_survivors.keys())
    total = len(coins)
    results = []

    for i in range(0, total, BATCH_SIZE):
        batch = coins[i:i + BATCH_SIZE]
        for coin in batch:
            try:
                r = requests.post(HL_INFO,
                    json={"type": "l2Book", "coin": coin},
                    timeout=TIMEOUT)
                if r.status_code != 200:
                    continue

                d = r.json()
                levels = d.get("levels", [[], []])
                if len(levels) < 2 or not levels[0] or not levels[1]:
                    continue

                asks = levels[0]
                bids = levels[1]
                bb = float(bids[0]["px"])
                ba = float(asks[0]["px"])
                mid = (bb + ba) / 2
                if mid <= 0:
                    continue

                # V5: crossed book check (best_ask <= best_bid → skip)
                if ba <= bb:
                    continue

                # ── 计算 tick_size ──
                all_pxs = []
                for lvl in bids[:10]:
                    all_pxs.append(float(lvl["px"]))
                for lvl in asks[:10]:
                    all_pxs.append(float(lvl["px"]))
                all_pxs = sorted(set(all_pxs))
                if len(all_pxs) < 2:
                    continue
                tick_size = min(abs(all_pxs[j] - all_pxs[j-1]) for j in range(1, len(all_pxs)))
                if tick_size <= 0:
                    continue

                gross_bps = (tick_size / mid) * 10000

                # ── 计算 1% 纵深 ──
                bid_depth_1pct = compute_1pct_depth(bids, DEPTH_PCT, mid)
                ask_depth_1pct = compute_1pct_depth(asks, DEPTH_PCT, mid)
                min_depth = min(bid_depth_1pct, ask_depth_1pct)

                bid_depth_top3 = sum(float(l["sz"]) * float(l["px"]) for l in bids[:3])
                ask_depth_top3 = sum(float(l["sz"]) * float(l["px"]) for l in asks[:3])

                depth_pass = min_depth >= MIN_DEPTH_1PCT

                vmeta = velocity_survivors[coin]
                results.append({
                    "coin": coin,
                    "gross_bps": round(gross_bps, 2),
                    "tick_size": tick_size,
                    "mid": mid,
                    "best_bid": bb,
                    "best_ask": ba,
                    "dayNtlVlm": vmeta["dayNtlVlm"],
                    "velocity_active_min": vmeta.get("velocity_active_minutes", 0),
                    "bid_depth_1pct": round(bid_depth_1pct, 0),
                    "ask_depth_1pct": round(ask_depth_1pct, 0),
                    "min_depth_1pct": round(min_depth, 0),
                    "bid_depth_top3": round(bid_depth_top3, 0),
                    "ask_depth_top3": round(ask_depth_top3, 0),
                    "depth_pass": depth_pass,
                    "n_bids": len(bids),
                    "n_asks": len(asks),
                })
            except Exception:
                pass

        progress = min(i + BATCH_SIZE, total)
        sys.stdout.write(f"\r  扫描: {progress}/{total} ...")
        sys.stdout.flush()
        if i + BATCH_SIZE < total:
            time.sleep(1.5)

    print()
    depth_pass = [r for r in results if r["depth_pass"]]
    depth_fail = [r for r in results if not r["depth_pass"]]
    print(f"   L2 获取: {len(results)} 币 → ✅ 深度通过: {len(depth_pass)} | ❌ 纸片盘口: {len(depth_fail)}")

    return results


def stage4_sort_and_tier(candidates):
    """
    漏斗 4: 按 gross_bps 降序排列，分档输出
    """
    print(f"\n━" * 70)
    print(f"🟢 漏斗 4: 粗精度收益率排序 (Coarse Tick Sort)")
    print("━" * 70)

    survivors = [c for c in candidates if c["depth_pass"]]
    survivors.sort(key=lambda x: -x["gross_bps"])

    gold = [c for c in survivors if c["gross_bps"] >= 15.0]
    silver = [c for c in survivors if 8.0 <= c["gross_bps"] < 15.0]
    bronze = [c for c in survivors if 4.0 <= c["gross_bps"] < 8.0]
    dust = [c for c in survivors if c["gross_bps"] < 4.0]

    def print_tier(emoji, label, tier_list):
        print(f"\n  {emoji} {label}: {len(tier_list)} coins")
        if tier_list:
            print(f"  {'Coin':14s} {'gross_bps':>8s} {'tick_sz':>12s}  {'mid':>10s}  "
                  f"{'depth_1%':>9s}  {'vol_24h':>12s}  {'active_min':>10s}")
            print(f"  {'-'*90}")
            for c in tier_list[:15]:
                print(f"  {c['coin']:14s} {c['gross_bps']:7.1f} bps  {c['tick_size']:12.8f}  "
                      f"{c['mid']:10.8f}  ${c['min_depth_1pct']:7.0f}  ${c['dayNtlVlm']:10,.0f}  "
                      f"{c.get('velocity_active_min', '?'):>9}")

    print_tier("🟢", "GOLD   (gross≥15 bps, 常态化开闸)", gold)
    print_tier("🟡", "SILVER (8≤gross<15 bps, 突袭模式)", silver)
    print_tier("🟠", "BRONZE (4≤gross<8 bps, 精密市场)", bronze)
    if dust:
        names = ", ".join(c["coin"] for c in dust[:8])
        print(f"\n  ⚫ DUST (gross<4): {len(dust)} coins ({names}...)")

    return survivors, gold, silver, bronze


def print_funnel_summary(vol_total, velocity_count, l2_count, survivors, gold, silver, bronze, zombies):
    """打印完整四维漏斗摘要"""
    print(f"\n{'='*70}")
    print(f"📊 四维漏斗 Funnel Summary (V5)")
    print(f"{'='*70}")
    print(f"  🟡 漏斗 1 (Volume Gate ≥ ${MIN_DAY_NTL_VLM:,}):        {vol_total} coins 进入")
    print(f"  ⚡ 漏斗 2 (Trade Velocity Gate {VELOCITY_WINDOW_MIN}min):  {velocity_count} 活跃 | ☠️ {len(zombies)} 僵尸")
    print(f"  🟠 漏斗 3 (Depth Gate ≥ ${MIN_DEPTH_1PCT:,}):          {l2_count} coins L2扫描 → {len(survivors)} 存活")
    print(f"  🟢 漏斗 4 (Coarse Tick Sort):")
    print(f"       GOLD    (≥15 bps): {len(gold)}  — 🎯 立即部署")
    print(f"       SILVER  (8-14 bps): {len(silver)}  — 候选监控")
    print(f"       BRONZE  (4-7 bps):  {len(bronze)}  — 极端行情突袭")
    print(f"       -----------------------------------------------------------------")
    print(f"       Total survivors:    {len(survivors)} coins (穿透全部四关)")

    if gold:
        print(f"\n🎯 立即可部署 GOLD (全部启用):")
        for c in gold:
            ticks = max(1, round(c["gross_bps"] / 15.0))
            print(f"   [[coins]]  name=\"{c['coin']}\"  tsunami_ticks={ticks}  "
                  f"# gross={c['gross_bps']:.1f}bps  d1%=${c['min_depth_1pct']:,.0f}  "
                  f"v24h=${c['dayNtlVlm']:,.0f}  active={c.get('velocity_active_min','?')}min")

    if silver:
        print(f"\n👁️  候选监控 SILVER (等波动开闸):")
        for c in silver[:5]:
            print(f"   {c['coin']}: gross={c['gross_bps']:.1f}bps  d1%=${c['min_depth_1pct']:,.0f}  "
                  f"v24h=${c['dayNtlVlm']:,.0f}  active={c.get('velocity_active_min','?')}min")


def main():
    print(f"🦈 HL Structural Scanner V5 — 四维漏斗 (含成交速度)")
    print(f"   时间: {datetime.now().isoformat()}")
    print(f"   漏斗1: Volume Gate (dayNtlVlm ≥ ${MIN_DAY_NTL_VLM:,})")
    print(f"   漏斗2: ⚡ Velocity Gate (过去 {VELOCITY_WINDOW_MIN}min 成交笔数 > 0)")
    print(f"   漏斗3: Depth Gate  (1%深度 ≥ ${MIN_DEPTH_1PCT:,})")
    print(f"   漏斗4: Coarse Tick Sort (gross_bps 降序)")
    print()

    # ── 阶段 1 ──
    vol_survivors = stage1_volume_gate()
    if not vol_survivors:
        print("❌ 无币种通过成交量门禁，退出")
        return

    # ── 阶段 2: ⚡ NEW velocity gate ──
    velocity_survivors, zombies = stage2_velocity_gate(vol_survivors)
    if not velocity_survivors:
        print("❌ 无币种通过成交速度门禁（全部僵尸），退出")
        return

    # ── 阶段 3 + 4 ──
    candidates = stage3_depth_gate(velocity_survivors)
    survivors, gold, silver, bronze = stage4_sort_and_tier(candidates)

    # ── 汇总 ──
    l2_count = len(candidates)
    print_funnel_summary(len(vol_survivors), len(velocity_survivors),
                         l2_count, survivors, gold, silver, bronze, zombies)

    # ── 保存 ──
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    output = {
        "timestamp": datetime.now().isoformat(),
        "version": "v5-velocity-funnel",
        "params": {
            "min_dayNtlVlm": MIN_DAY_NTL_VLM,
            "velocity_window_min": VELOCITY_WINDOW_MIN,
            "min_depth_1pct": MIN_DEPTH_1PCT,
            "depth_pct": DEPTH_PCT,
        },
        "funnel_stats": {
            "vol_pass": len(vol_survivors),
            "velocity_pass": len(velocity_survivors),
            "zombies": len(zombies),
            "l2_scanned": l2_count,
            "depth_pass": len(survivors),
            "gold": len(gold),
            "silver": len(silver),
            "bronze": len(bronze),
        },
        "zombies": [{"coin": c, "reason": r} for c, r in zombies],
        "gold": gold,
        "silver": silver,
        "bronze": bronze,
    }
    with open(OUTPUT_FILE, "w") as f:
        json.dump(output, f, indent=2)
    print(f"\n💾 Saved: {OUTPUT_FILE}")
    print("🏁 V5 扫描完成")


if __name__ == "__main__":
    main()
