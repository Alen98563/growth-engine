# V5 Scanner — 四维漏斗

> 版本: V5 | 日期: 2026-06-22 | 前身: V4 (三维漏斗)

## 演化动机

V4 发现 HMSTR（gross=53.9 bps，GOLD）引擎跑 11 小时 **0 成交**。根因：L2 订单簿持续交叉（best_ask < best_bid），但 V4 只计算名义 tick_size → 理论价差，未检测：
1. **是否有真实 taker 在交易**（交叉盘口 = 幽灵流动性）
2. **盘口是否 crossed**（bid 压在 ask 之上时，Post-Only 永远无法成交）

V5 在 V4 的三维漏斗中嵌入两道新防线。

## 漏斗结构对比

```
V4:  Volume Gate → Depth Gate → Coarse Sort (3-stage)
V5:  Volume Gate → ⚡Velocity Gate → Depth Gate → Coarse Sort (4-stage)
                    + crossed-book skip
```

| 阶段 | V4 | V5 |
|------|-----|-----|
| 1. Volume | dayNtlVlm ≥ $50k | 不变 |
| **2. Velocity** | — | ⚡ 新增：过去 10min 1m candle volume > 0 |
| 3. Depth | 1% 纵深 ≥ $2k | 不变，另新增 crossed-book 前置检查 |
| 4. Coarse Sort | gross_bps 降序 | 不变 |

## 各阶段详解

### 漏斗 1: Volume Gate (`dayNtlVlm >= $50,000`)

```
API: POST /info {"type":"metaAndAssetCtxs"}
输入: 全量 230 coins
输出: dayNtlVlm >= $50k 的币种
```

BOME 案例：gross=21.7 bps（V3 排第三），但 dayNtlVlm=$37,995 < $50k → 被 V4 正确剔除。

### 漏斗 2: ⚡ Velocity Gate（过去 10 分钟至少 1 根 1m candle 有成交）

```
API: POST /info {"type":"candleSnapshot","req":{"coin":"XXX","interval":"1m",
     "startTime":now-10min,"endTime":now}}
条件: 任意 1 根 candle.volume > 0
否则: ZOMBIE → 标记但不进入漏斗 3
```

**设计理由**: `dayNtlVlm=$1M` 但过去 10 分钟完全无成交 = 僵尸盘口。做市引擎即使 Post-Only 挂单也无 taker 来吃，最终只会因仓位被动通胀而爆仓。

**时间窗口选择**: 10 分钟足够捕获正常波动。如果连一根 1m candle 都没有成交，说明 market maker 和 taker 都已撤离该币种。

### 漏斗 3: Depth Gate（1% 纵深 ≥ $2,000 + crossed-book skip）

```
API: POST /info {"type":"l2Book","coin":"XXX"}
交叉检查: best_ask <= best_bid → 直接跳过（V5 新增）
深度计算: 距 BBO 1% 范围内累计名义价值 sz×px
门禁条件: min(bid_depth_1pct, ask_depth_1pct) >= $2,000
```

**Crossed book skip（V5 新增）**: 当 `best_ask <= best_bid` 时直接 continue，不浪费 API 调用。这种盘口等价于没有可用的 spread，无论 gross_bps 多大都无法做市。

### 漏斗 4: Coarse Tick Sort

```
分组:
  GOLD    (gross >= 15 bps)   → 🎯 立即部署
  SILVER  (8-14 bps)           → 👁️ 候选监控
  BRONZE  (4-7 bps)            → ⚡ 极端行情突袭
  DUST    (<4 bps)             → 不推荐
```

新增字段 `velocity_active_min`：过去 10 分钟内有多少分钟有成交。

## 运行

```bash
cd /root/growth-engine
python3 scripts/scan_hl_structural_v5.py
```

输出：
- 终端：四维漏斗摘要 + GOLD/SILVER/BRONZE 分档 + zombie 列表
- JSON：`data/scan_structural_v5.json`

## 参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `MIN_DAY_NTL_VLM` | $50,000 | 24h 最低名义成交量 |
| `VELOCITY_WINDOW_MIN` | 10 | 回溯分钟数 |
| `MIN_DEPTH_1PCT` | $2,000 | 最低 1% 纵深 |
| `DEPTH_PCT` | 0.01 | 纵深百分比 |
| `BATCH_SIZE` | 10 | L2 并发批次 |

## 版本历史

| 版本 | 日期 | 变化 |
|------|------|------|
| V3 | 06-21 | 单层 gross_bps 排序 |
| V4 | 06-22 08:00 | 三维漏斗：Volume + Depth + Sort |
| V5 | 06-22 10:00 | 四维漏斗：+Velocity Gate + crossed-book skip |

## 关联引擎变更

V5 扫描器与 V12 引擎同步上线：

| 扫描器检测 | 引擎防御 |
|-----------|---------|
| Velocity Gate → ZOMBIE | 引擎不部署该币 |
| crossed-book 跳过 | `engine.rs` CROSSED/BLOCKED 闸门 |
| — | Shading cap >=30bps → max 1 tick |
