# V12 Engine — Crossed-Book Gate, Shading Cap, THIN_SPREAD, Portfolio-Reduce

> 版本: V12.4 (portfolio-reduce) | 日期: 2026-06-22 | 前身: V11.6
>
> Git: `0cc8974`(V12) → `dc149f0`(V12.1) → `a8671d8`(V12.2) → `e4b1c28`(V12.3) → `c5ff094`(V12.4)

## 演化动机

V11.6 引擎在 HMSTR 上跑了 11 小时（3,624 cycles），**0 成交**。equity 从 $130.38 → $126.04（全为 -235,438 HMSTR SHORT 浮亏）。根因分两层：

### 层 1: Crossed Book（交叉盘口）

REST L2 返回 `best_ask (0.000188) < best_bid (0.000189)`。Post-Only 限价单只能挂在 spread 外侧，无法触及成交价。但 `_ => 999.0` fallback 把 crossed book 误判为 TSUNAMI 1.0x，引擎继续以 Post-Only 挂单。

### 层 2: Shading 自杀式隔离

V11.6 的 bid shading 会为 Post-Only rejection 堆 6 ticks 撤退。在 HMSTR (tick=0.000001, 60 bps) 上这等于移动 6 倍 gross spread 的距离 — 完全脱离成交区。

---

## V12 三项改造

### 改造 1: Crossed-Book Gate (`engine.rs:289-302`)

```rust
// Before (V11.6): crossed book → _ => 999.0 → TSUNAMI 1.0x
let gross_ticks = match (book.best_bid(), book.best_ask()) {
    (Some(bid), Some(ask)) if ask > bid => (ask - bid) / coin_tick.max(1e-9),
    _ => 999.0,
};

// After (V12): crossed book → book_crossed=true → CROSSED/BLOCKED
let (gross_ticks, book_crossed) = match (book.best_bid(), book.best_ask()) {
    (Some(bid), Some(ask)) if ask > bid => ((ask - bid) / coin_tick.max(1e-9), false),
    (Some(_), Some(_)) => (0.0, true),  // crossed!
    _ => (999.0, false),                // no data
};

let (gate_mode_str, size_multiplier) = if book_crossed {
    tracing::warn!("CROSSED_BOOK: best_ask <= best_bid (REST-verified), forcing GATE_BLOCKED");
    ("CROSSED", 0.0)
} else if gi >= cfg.tsunami_ticks { ... }
```

**关键设计决定**: 不重拉 REST。HMSTR crossed book 来自交易所 REST API 原文，不是 WS 增量污染。重拉 = 死循环。

### 改造 2: Coarse-Tick Shading Cap (`engine.rs`)

```rust
// 三处统一逻辑：
let mut shading_offset = calculate_bid_tick_offset(rejections, skew_bps);
if risk_out.gross_spread_bps >= 30.0 {
    shading_offset = shading_offset.min(1);  // hard cap
}
```

**约束**: 仅在 `gross_bps >= 30` 时生效。精密 tick 币（如 PUMP 6.5bps）不受限。

### 改造 3: V5 Scanner Velocity Gate

详见 `docs/scanner_v5.md`。引擎侧不需要改动 — scanner 的输出决定 `.env` 配置哪些币。

---

## V12.1 — COARSE Dual-Axis Gate

> Git: `dc149f0`

**问题**: Gate 系统只用 tick count（`gi >= tsunami_ticks`）。粗精度币 1 tick = 53 bps 与精密币 1 tick = 0.5 bps 无法区分。

**改造**:
- `gross_spread_bps` 从 bid/ask mid-point 计算 → 双轴闸门
- COARSE 模式：`gross_bps >= coarse_tick_bps_threshold` (15 bps) AND `gi == 1`
- Zero shading in COARSE mode（shading_offset = 0，无需 tick 撤退）
- 可配置尺寸：`coarse_tick_size_pct`（默认 0.3x = 30%）

**门禁决策矩阵更新**:
```
1. CROSSED_BOOK    → size=0.0   (REST ask ≤ bid)
2. COARSE_TICK     → size=0.3   (1-tick ≥ 15 bps)  ← 新增
3. TSUNAMI         → size=1.0   (gi ≥ 4)
4. SNIPER          → size=0.4   (gi ≥ 1)
5. BLOCKED         → size=0.0   (spread too narrow)
```

---

## V12.2 — COARSE Asymmetric Sizing & Toxicity Detection

> Git: `a8671d8` + hotfix `3830b9c`

**问题**: COARSE 模式币积累单边仓位（wide spread → taker cross → free option writing）。

**改造**:
- Per-side fill tally (`buy_fills_tally`, `sell_fills_tally`)
- **Sensitive skew**: 单侧 fill 达到 `coarse_tick_max_side_fills` → 禁该侧
- **Asymmetric sizing**: 仓位超过 `coarse_pos_ratio_aggressive` → 放大有利侧（`coarse_unwind_size_boost`），限制不利侧到 25%
- **Hard kill**: 仓位超过 `coarse_pos_ratio_hard_kill` → 不利侧直接置零
- Flip hysteresis 持久计数修复 (`last_flip_cycle` + `freeze_remaining`)

---

## V12.3 — THIN_SPREAD Net-Spread Safety Gate

> Git: `e4b1c28`

**问题**: 精密 tick 币（RESOLV 0.46 bps 1-tick）绕过了 COARSE 保护（阈值 15 bps），落入 TSUNAMI 1.0x，在负净 spread 下持续成交、积仓、被动解仓。

**根因**: Gate 优先链没有绝对 spread 盈利能力检查。COARSE（1-tick ≥ 15 bps）是唯一双轴闸门，低于该阈值的币没有净 spread 保护。

**RESOLV 真实案例**:
```
tick_size  = 0.000001
1-tick spread = 0.46 bps
maker fee     = 1.50 bps (non-Growth)
min_margin    = 2.00 bps
gate check: 0.46 < 1.50 + 2.00 = 3.50 → THIN_SPREAD → BLOCKED
```

**改造**:
- 在 Gate 优先链中插入 **THIN_SPREAD**（优先级 #2）
- 条件：`gross_bps < maker_fee_bps + min_margin_bps`（默认 2.17 bps）
- 封锁币的 gate mode = `"THIN_SPREAD"`, size_mult = 0.0

**Gate 优先链（V12.3 最终版）**:
```
1. CROSSED_BOOK    → size=0.0   (REST ask ≤ bid)
2. THIN_SPREAD     → size=0.0   (gross < maker_fee + min_margin)  ← 新增
3. COARSE_TICK     → size=0.3   (1-tick ≥ 15 bps)
4. TSUNAMI         → size=1.0   (full fire)
5. SNIPER          → size=0.4   (tight spread)
6. BLOCKED         → size=0.0   (fallback)
```

**效果**: 所有精密 tick 币自动闭闸，无需 per-coin 配置。

---

## V12.4 — Portfolio-Reduce（Restart Deadlock Fix）

> Git: `c5ff094`

**问题**: 引擎重启时，若已有仓位（如 HMSTR 13.4%、RESOLV 43.1%、MEME 0.2%），聚合名义值超过 60% 组合硬限，但没有任何单独币种超过 40% unwind 阈值，导致死锁：

```
1. Bootstrap → Active（按设计："natural skew + quadratic qty" 渐进再平衡）
2. Portfolio over limit → Active 币被推到 Waiting
3. Waiting 退出需要 !portfolio_over_limit
4. 无法下单 → 仓位不变 → 组合永远超限 → 永久死锁
```

这违背了 bootstrap 的设计意图：自然偏斜再平衡本身就是一种减仓行为，不应被用来保护新仓位积累的同一道闸门封锁。

**改造** (`engine.rs:685-697`):

```rust
// 旧：硬锁 — 组合超限时所有下单均禁止（除 UNWIND）
let can_place = coin_state.can_place_orders(coin)
    && (!portfolio_over_limit || coin_state.is_unwind(coin));

// 新：三条通路，portfolio_reduce 并行于 UNWIND
let can_place = coin_state.can_place_orders(coin);
let portfolio_reduce = portfolio_over_limit && pos.size.abs() > 0.001;

if (can_place && spread_ok) || is_unwind || portfolio_reduce {
    // 下单（配合方向冻结）
}
```

**方向冻结（`freeze_buy`/`freeze_sell`）**:
```rust
let freeze_buy = (is_unwind && pos.size > 0.0)
    || (cooldown_left > 0 && pos.size > 0.0)
    || (portfolio_over_limit && pos.size > 0.001);   // ← 新增
let freeze_sell = (is_unwind && pos.size < 0.0)
    || (cooldown_left > 0 && pos.size < 0.0)
    || (portfolio_over_limit && pos.size < -0.001);  // ← 新增
```

**组合超限时的下单行为矩阵**:
```
pos.size  | freeze_buy | freeze_sell | 效果
> 0 (LONG)   | true       | false       | 只能 SELL（减仓）
< 0 (SHORT)  | false      | true        | 只能 BUY（减仓）
≈ 0 (FLAT)   | true       | true        | 双边封锁（安全）
```

**效果**: "natural skew + quadratic qty" 渐进再平衡机制现已完全恢复。减仓订单永不封锁，开仓订单保持闸门。

---

## Shading 行为矩阵

| gross_bps | rejection count | V11.6 offset | V12 offset | V12.1 COARSE offset |
|-----------|----------------|-------------|------------|---------------------|
| 60 | 0 | 0 | 0 | 0 |
| 60 | 3 | `1 + 90 = 91` → capped 10 | `min(1, 1)` = 1 | **0** (COARSE zero shading) |
| 60 | 6 | `2 + 90 = 92` → capped 10 | `min(1, 1)` = 1 | **0** (COARSE zero shading) |
| 10 | 3 | 1 + 0 = 1 | 1 (no cap, <30) | 1 (COARSE not engaged, <15) |
| 10 | 6 | 2 + 0 = 2 | 2 (no cap, <30) | 2 (COARSE not engaged, <15) |

---

## COARSE Asymmetric Sizing Matrix (V12.2)

| pos_ratio | 行为 |
|-----------|------|
| < aggressive | symmetric: buy_sz = sell_sz |
| [aggressive, hard_kill) | asymmetric: favor side × boost, adverse ≤ 25% |
| ≥ hard_kill | hard kill: adverse side = 0, favor side × boost |

---

## 防御层演进

| Version | 新增防御 | 累计 |
|---------|---------|------|
| V12 | Crossed-book gate, shading cap | 14 |
| V12.1 | COARSE dual-axis gate, zero shading | 15 |
| V12.2 | COARSE asymmetric sizing, toxicity momentum | 16 |
| V12.3 | THIN_SPREAD net-spread safety gate | **17** |
| V12.4 | Portfolio-reduce restart deadlock fix | **17** |

---

## 文件变更总览

```
src/engine.rs          — V12 crossed book + shading cap ×3
                        + V12.1 COARSE dual-axis + zero shading
                        + V12.2 asymmetric sizing + toxicity
                        + V12.3 THIN_SPREAD gate
                        + V12.4 portfolio-reduce bypass
src/state.rs           — gate_mode field, fill tallies, COARSE helpers
scripts/scan_hl_structural_v5.py  — 4-stage scanner
docs/scanner_v5.md
docs/v12_engine.md     — 本文档
docs/CHANGELOG.md      — 全版本 changelog
docs/STATE_MACHINE.md  — 完整状态机 + gate 优先链
```
