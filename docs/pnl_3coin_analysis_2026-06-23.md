# Growth Engine V12.4 — 三币盈利分析报告

> 期: 2026-06-22 13:00 → 2026-06-22 22:51 (UTC) | 约 10 小时
>
> 版本: V12.3 (THIN_SPREAD) + V12.4 (portfolio-reduce)
>
> 标签记录: 6,701 条 (HMSTR 2,235 / MEME 2,233 / RESOLV 2,233)

---

## 账户级汇总

| 指标 | 始 | 终 | Δ |
|------|-----|-----|-----|
| Equity | $137.65 | $128.88 | **-$8.77 (-6.4%)** |
| Withdrawable | $156.11 | $89.23 | -$66.89 |
| 总名义成交 | — | — | **$4,651** churn |

---

## 逐币 PnL 分解

| Coin | 买入 | 卖出 | 交易 PnL | 未实现 | **总 PnL** | 名义周转 |
|------|------|------|----------|--------|------------|----------|
| HMSTR | $60.25 | $50.25 | -$9.99 | +$10.82 | **+$0.83** | $110 |
| MEME | $140.82 | $120.94 | -$19.88 | +$20.49 | **+$0.61** | $262 |
| RESOLV | $2,155.54 | $2,123.32 | -$32.22 | +$26.33 | **-$5.89** | **$4,279** |
| **合计** | | | | | **-$4.45** | $4,651 |

> 账户总亏损 $8.77；$4.45 来自以上三币 PnL，差额 ~$4.32 可能来自费率/funding/PUMP 残仓。

---

## HMSTR — 微盈 (+$0.83)

### 盈利原因 ✅
| 因素 | 值 | 分析 |
|------|-----|------|
| Gross Spread | 55 bps (p50) | 极宽，单边 27.5 bps 捕获 |
| Net Spread | 55 bps | Growth Mode：maker fee=0 |
| 下单率 | 90.0% | 几乎持续双边报价 |
| COARSE 保护 | 0.3x sizing | 小尺寸防爆仓 |
| Gate 模式 | COARSE / TSUNAMI | 始终活跃 |

**机制**: 55 bps 结构性粗 tick，Post-Only 限价单持续捕获 spread。双侧报价，90% 的下单率说明 Gate 几乎始终开闸。0.3x COARSE sizing 限制了单边累积风险。

### 亏损原因 ⚠️
| 因素 | 值 | 影响 |
|------|-----|------|
| 方向翻转 | **7 次** | 每次翻转 = 穿越 spread 付费 |
| 价格下跌 | 0.000182→0.000173 (-4.9%) | LONG 仓位承受价格磨损 |
| DirectionalFreeze | 翻转后单向封锁 | 不能双边报价，丧失 spread 收入 |
| 仓位累积 | -2,816→+59,602 | 从 SHORT 翻到 LONG，错失 SHORT 端 spread |

**根因**: 7 次翻转导致 DirectionalFreeze 封锁对应侧，每次冻结约 50 cycles (~5min)。翻转期间，止损/被吃单 → 反向开仓 → 市场回摆 → 再次止损。每次翻转损失约 spread (27.5 bps × position)。同时 LONG 仓位面临 5% 价格下跌磨损。

**判断**: 结构性盈利（spread capture），但被翻转损耗几乎抵消。净微盈说明粗 tick 做市模型成立。

---

## MEME — 微盈 (+$0.61)

### 盈利原因 ✅
| 因素 | 值 | 分析 |
|------|-----|------|
| 下单率 | **95.2%** | 极高活跃度 |
| COARSE/Gate | 持续开闸 | 几乎从不停摆 |
| Spread 波动 | 18.3→37.7 bps | 宽时切换 TSUNAMI 1.0x 获更大份额 |
| COARSE 不对称 | 单边硬杀 | 阻止不利侧累积，有利侧 1.2x |

**机制**: 极简做市 — 几乎 100% 时间在放置订单。COARSE 不对称 sizing 阻止了 MEME 在 SHORT 侧累积过量仓位（COARSE hard kill：BUY ZERO, SELL 1.2x）。Spread 偶发扩宽到 37.7 bps 时自动切换到 TSUNAMI 1.0x，增加收入。

### 亏损原因 ⚠️
| 因素 | 值 | 影响 |
|------|-----|------|
| 方向翻转 | **6 次** | 类似 HMSTR 的翻转损耗 |
| Spread 薄 | 18.3 bps (p50) | 单边 9.2 bps，利润空间窄 |
| COARSE 硬杀 | BUY ZERO 状态 | 单向报价失去 50% 收入 |
| 仓位累积 | +233→+38,610 | 单向累积不可逆 |

**根因**: MEME 在 COARSE hard kill 期间（SHORT 仓位 → BUY ZERO, SELL only），只收 SELL 侧 spread。但随着时间推移，SELL 订单被吃 → 积累了 MORE SHORT → 仓位加深。最终翻转后转为 LONG → 价格逆向。

**判断**: MEME 像 HMSTR 的缩小版 — spread 更薄但活跃度更高。净微盈说明 COARSE 不对称保护是有效的，但 6 次翻转仍然侵蚀了大部分利润。

---

## RESOLV — 严重亏损 (-$5.89) 🔴

### 亏损原因（核心伤亡）

| 因素 | 值 | 严重性 |
|------|-----|--------|
| **方向翻转** | **41 次** | 🔴🔴🔴 灾难级 |
| UNWIND 时间 | **61.1%** (1,365/2,233) | 🔴🔴 被套牢 |
| 交易笔数 | **383 笔** | 🔴 过度交易 |
| 名义周转 | **$4,279** (30x 账户) | 🔴🔴 摩擦致死 |
| 下单率 | **40.8%** | 大部分时间不下单 |
| GATE_BLOCKED | 42 条 | THIN_SPREAD 触发 |
| Spread 中位 | 18.0 bps | 窄 spread 恶化翻转成本 |

### 死亡螺旋机制

```
窄 spread (18 bps) → TSUNAMI 1.0x → 双边下单 → 单侧被吃
→ 仓位倾斜 → 达到 40% UNWIND 阈值 → PASSIVE_UNWIND
→ spread 同时压缩到 <2.17 bps → THIN_SPREAD → GATE_BLOCKED
→ UNWIND 中不下单 → 仓位不能减 → 继续被吃 → 翻转
→ DirectionalFreeze 50 cycles → 无法下单 → 仓位漂移
→ UNWIND 退出 → spread 扩大 → TSUNAMI → 重新开仓
→ 仓位回到 46% → 再次进入 UNWIND → 循环
```

**关键数据点**:
- 41 次翻转，每次 flip 成本 ≈ spread (18 bps median) × position ≈ $0.20-0.50
- 383 次交易 × $5-15 avg size = $4,279 名义成交
- 费率估计: $4,279 × 0.015% (taker fee during UNWIND GTC exits) ≈ $0.64
- 翻转税: 41 × ~$0.30 = ~$12.30 (大部分损失)

### 为什么 RESOLV 独特？

RESOLV 不同于 HMSTR/MEME 的关键特征:

1. **Tick size 极小 (0.000001, ~0.5 bps)** — spread 结构不稳定，易剧烈波动 (3→113 bps)
2. **THIN_SPREAD 反复触发** — 42 次 Gate 封锁，封锁期间不下单但仓位继续漂移
3. **高频微波动** — 价格区间 $0.021-0.023，微小价格变动 (0.000001 tick) 就能穿越 spread
4. **V12.1 COARSE 保护缺失** — COARSE 需要 1-tick ≥ 15 bps，RESOLV 1-tick = 0.46 bps 远低于阈值
5. **V12.3 THIN_SPREAD 到位前** — 引擎以 TSUNAMI 1.0x 全力做市在负净 spread 下，加速了前期亏损

### 盈利原因（微弱）

| 因素 | 值 |
|------|-----|
| Spread 偶尔扩宽 | p90 = 36 bps，最高 113 bps |
| TSUNAMI 1.0x | 宽 spread 时全尺寸双边下单 |
| UNWIND GTC 逃生 | 紧急时以吃单价减仓 |

RESOLV 的总亏损几乎完全由高频翻转驱动。当 spread 宽时 (36-113 bps)，TSUNAMI 1.0x 模式捕获了可观的 spread 收入（$2,123 卖出收入 - $2,155 买入成本）。但 41 次翻转 + $4,279 名义周转的摩擦成本远超 spread 收入。

---

## 总结矩阵

| Coin | 收费模式 | median spread | 翻转 | 下单率 | PnL | 判断 |
|------|---------|---------------|------|--------|-----|------|
| HMSTR | 粗 tick 55bps | COARSE 0.3x | 7 次 | 90.0% | +$0.83 | ✅ 结构性盈利，被翻转磨损 |
| MEME | 中 tick 18bps | COARSE 0.3x | 6 次 | 95.2% | +$0.61 | ✅ 微盈，COARSE 保护有效 |
| RESOLV | **细 tick 0.5bps** | TSUNAMI 1.0x | **41 次** | 40.8% | **-$5.89** | 🔴 高频翻转毁灭 |

### 根因层级

1. **一级**: RESOLV 精密 tick → 无法利用 COARSE 保护 → 全火力 TSUNAMI → 过多交易（383 笔 vs HMSTR 11 笔）
2. **二级**: 翻转税 — 每翻转一次损失 spread (18 bps median)，41 次翻转税 ≈ $12
3. **三级**: 费率摩擦 — $4,279 名义周转产生 taker fee (UNWIND GTC 吃单)
4. **缓解**: V12.3 THIN_SPREAD 到位（部署后），阻止了 RESOLV 在负净 spread 下交易。但 42 次 GATE_BLOCKED 也意味着 42 次错失减仓窗口

### 建议

| Priority | 动作 | 预期效果 |
|----------|------|----------|
| P0 | 降低 RESOLV max_size_pct 到 0 | **立即止损**（停做 RESOLV） |
| P1 | 提高 THIN_SPREAD min_margin 到 5 bps | RESOLV 永久闭闸（0.46 < 1.5+5=6.5） |
| P2 | 加入 flip_rate gate (flip/hour > 2 → BLOCKED) | 阻止 flip 驱动型亏损币 |
| P3 | UNWIND 加入 PnL 感知（亏损时加速减仓） | 降低 UNWIND 期间的摩擦成本 |
