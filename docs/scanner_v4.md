# 🦈 Market Scanner — 三维漏斗筛选规则

> **版本**: V4 | **文件**: `scripts/scan_hl_structural_v4.py` | **更新**: 2026-06-22

---

## 1. 设计哲学

不扫垃圾。从 230 个 Hyperliquid 币种中，用三层漏斗逐级筛选出 **既有流动性支撑、又有粗刻度物理利差** 的做市黄金标的。

旧版 V3 只按 gross_bps 排序 → 高 bps 僵尸币（无成交量）也被排上去。V4 引入成交量 + 深度双重门禁，只让"活跃 + 有深度 + 粗 tick"的三冠王通过。

---

## 2. 数据源

| 数据 | API | 字段 |
|------|-----|------|
| 24h 成交量 | `metaAndAssetCtxs` | `dayNtlVlm` (名义成交额) |
| 盘口深度 | `l2Book` per coin | `levels[0]` bids, `levels[1]` asks |
| Coin 列表 | `metaAndAssetCtxs` | `universe[].name` |

---

## 3. 漏斗逐层详解

### 🟡 漏斗 1：24h 日成交量硬门禁（Volume Gate）

```
指标：dayNtlVlm（24h 名义总成交额）
阈值：≥ $50,000
来源：metaAndAssetCtxs → assetCtxs[i].dayNtlVlm
```

**淘汰逻辑**：日成交量低于 $50k 的币种直接致盲。无论 gross_bps 多高，没流量就是僵尸盘——即使挂单也无人来吃。

**阈值设计依据**：
- 单笔做市 Size ~$40-50 notional
- 50-100× 容纳量 = $2,000-$5,000
- $50,000 提供了充足的缓冲（1,000-1,250×）
- 低于此值的币种要么是垃圾币要么是刚上线无流动性

**BOME 案例分析**：
- V3 排名：第 3 名，21.7 bps（GOLD）
- V4 淘汰：dayNtlVlm = **$37,995** < $50k
- 结论：虽有粗 tick 利差，但日成交不足无法支撑做市，正确剔除

### 🟠 漏斗 2：盘口有效深度门禁（Depth Gate）

```
指标：min(bid_depth_1%, ask_depth_1%)
       = 距 BBO 1% 范围内的累计挂单名义价值
阈值：≥ $2,000
来源：l2Book → levels[0][:n] bids, levels[1][:n] asks
```

**计算公式**：
```python
def compute_1pct_depth(levels, pct=0.01, mid_px):
    depth = 0
    for lvl in levels:
        px, sz = float(lvl["px"]), float(lvl["sz"])
        if abs(px - mid_px) / mid_px <= pct:
            depth += sz * px  # 累计名义价值
        else:
            break  # 订单簿按距 BBO 距离排列，超出即停
    return depth
```

**淘汰逻辑**：纸片盘口——一个市价单就能砸穿几十个 tick——会导致引擎在开闭闸之间疯狂振荡，或 DRIFT 对账时遭遇毁灭性滑点。

**当 $2,000 不够时**：
- 做市 Size 增大 → 阈值需同时提高
- 经验法则：`min_depth_1pct ≥ 做市单笔 size × 100`

### 🟢 漏斗 3：粗精度收益率排序（Coarse Tick Sort）

```
指标：gross_bps = (tick_size / mid_price) × 10000
tick_size = min(相邻档位价差)  # 从 L2 前 10 档取最小步长
来源：l2Book → 价格列表 → 相邻差的最小值
```

**分档标准**：

| Tier | gross_bps | 做市模式 | 说明 |
|------|-----------|----------|------|
| 🟢 **GOLD** | ≥ 15 | TSUNAMI 全火力 | 1 tick 就赚钱，常态化开闸 |
| 🟡 **SILVER** | 8-14 | SNIPER 突袭 | 需 2 tick 或波动助攻 |
| 🟠 **BRONZE** | 4-7 | 极端行情突袭 | 精密 tick，竞争激烈 |
| ⚫ **DUST** | < 4 | 永不触达 | taker 费都覆盖不了 |

---

## 4. 完整漏斗流程图

```
230 coins (全 HL)
    │
    ▼
┌─────────────────────────────────────┐
│ 🟡 漏斗1: Volume Gate               │
│ dayNtlVlm ≥ $50,000                 │
├─────────────────────────────────────┤
│ ✅ 通过: ~170 coins                  │
│ ❌ 淘汰: ~60 coins (僵尸/新币)      │
└─────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────┐
│ 🟠 漏斗2: Depth Gate                │
│ min(bid_1%, ask_1%) ≥ $2,000        │
├─────────────────────────────────────┤
│ ✅ 通过: ~170 coins                  │
│ ❌ 淘汰: ~0 coins (当前阈值保守)    │
│ ⚠️ 做市 size↑ → threshold↑ 同步调  │
└─────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────┐
│ 🟢 漏斗3: Coarse Tick Sort          │
│ gross_bps = tick/mid × 10000         │
├─────────────────────────────────────┤
│ 🟢 GOLD    ≥15 bps  → 部署          │
│ 🟡 SILVER  8-14 bps → 监控          │
│ 🟠 BRONZE  4-7 bps  → 极端行情       │
│ ⚫ DUST    <4 bps    → 永不          │
└─────────────────────────────────────┘
    │
    ▼
  Deploy List (GOLD coins)
```

---

## 5. V4 实测结果（2026-06-22 08:30 CST）

```
漏斗1: 169/230 通过 (73.5%)
漏斗2: 169/169 通过 (100% — $2k 门槛偏低)
漏斗3:
  🟢 GOLD:   3 coins — HMSTR, NOT, MEME
  🟡 SILVER: 4 coins — ACE, XAI, TURBO, W
  🟠 BRONZE: 4 coins — PUMP, DOOD, SAGA, DYM
  ⚫ DUST: 158 coins (91% 被粗 tick 筛掉)
```

### GOLD 部署配置

```toml
[[coins]]
name = "HMSTR"
tsunami_ticks = 1  # 1 tick = 53.9 bps → 全火力
# depth_1%=$10,743  vol_24h=$283,429

[[coins]]
name = "NOT"
tsunami_ticks = 1  # 1 tick = 24.3 bps → 全火力
# depth_1%=$11,959  vol_24h=$51,512

[[coins]]
name = "MEME"
tsunami_ticks = 1  # 1 tick = 18.3 bps → 全火力
# depth_1%=$22,373  vol_24h=$118,414
```

---

## 6. 运行方式

```bash
cd /root/growth-engine
python3 scripts/scan_hl_structural_v4.py
```

输出：
- 终端：漏斗逐层详情 + 部署建议
- JSON：`data/scan_structural_v4.json`（完整扫描快照）

可调参数（脚本顶部）：
```python
MIN_DAY_NTL_VLM = 50_000   # $50k → 调整此值收紧/放宽
MIN_DEPTH_1PCT = 2_000     # $2k  → 随做市 size 同步调高
DEPTH_PCT = 0.01           # 1%   → 纵深百分比
```

---

## 7. 版本演化

| 版本 | 日期 | 变更 |
|------|------|------|
| V2 | 2026-06-21 | 初版，仅按 gross_bps 排序 |
| V3 | 2026-06-21 | 修复 `gross_ticks as u32` 浮点截断 → `.round()` |
| **V4** | **2026-06-22** | **三维漏斗：Volume Gate + Depth Gate + Coarse Sort** |

---

## 8. 与引擎闸门的对接

扫描器输出 → 引擎配置：

```
扫描器 GOLD list → engine .env: HL_COINS=HMSTR,NOT,MEME
扫描器 gross_bps → engine config: tsunami_ticks per coin
扫描器 dayNtlVlm → 用户判断：资金是否够覆盖所有 GOLD 币
扫描器 depth_1%  → 用户判断：做市 size 是否超出盘口容纳力
```

引擎的 `tsunami_ticks` 含义：**触发 TSUNAMI 1.0x 全火力所需的最小 gross_ticks 数**。
对于 GOLD 币（1 tick ≥ 15 bps），`tsunami_ticks = 1`（1 tick 即开闸）。
