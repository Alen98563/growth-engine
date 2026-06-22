# V12 Engine — Crossed-Book Gate + Shading Cap

> 版本: V12 | 日期: 2026-06-22 | 前身: V11.6

## 演化动机

V11.6 引擎在 HMSTR 上跑了 11 小时（3,624 cycles），**0 成交**。equity 从 $130.38 → $126.04（全为 -235,438 HMSTR SHORT 浮亏）。根因分两层：

### 层 1: Crossed Book（交叉盘口）

REST L2 返回 `best_ask (0.000188) < best_bid (0.000189)`。Post-Only 限价单只能挂在 spread 外侧，无法触及成交价。但 `_ => 999.0` fallback 把 crossed book 误判为 TSUNAMI 1.0x，引擎继续以 Post-Only 挂单。

### 层 2: Shading 自杀式隔离

V11.6 的 bid shading 会为 Post-Only rejection 堆 6 ticks 撤退。在 HMSTR (tick=0.000001, 60 bps) 上这等于移动 6 倍 gross spread 的距离 — 完全脱离成交区。

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

### 改造 2: Coarse-Tick Shading Cap (`engine.rs:750, 835, 869`)

```rust
// 三处统一逻辑：
let mut shading_offset = calculate_bid_tick_offset(rejections, skew_bps);
if risk_out.gross_spread_bps >= 30.0 {
    shading_offset = shading_offset.min(1);  // hard cap
}
```

**约束**: 仅在 `gross_bps >= 30` 时生效。精密 tick 币（如 PUMP 6.5bps）不受限。

**设计理由**: 粗精度币一次 tick 已经是 30-60 bps 肥利润，2-6 ticks 的 shading 撤退会造成自杀式隔离。1-tick 撤退仍然能让引擎在 taker 靠近时被成交，但不会跳到 6x spread 之外。

### 改造 3: V5 Scanner Velocity Gate

详见 `docs/scanner_v5.md`。引擎侧不需要改动 — scanner 的输出决定 `.env` 配置哪些币。

## V5 扫描结果（2026-06-22 10:25 CST）

### GOLD (≥15 bps)
| Coin | gross_bps | 1% depth | 24h vol | active/10min |
|------|-----------|----------|---------|-------------|
| HMSTR | 53.3 | $6,896 | $302k | **11** (⛔ crossed book!) |
| MEME | 18.1 | $24,072 | $125k | 2 |

### ZOMBIE (Velocity Gate killed)
| Coin | Reason |
|------|--------|
| NOT | 0 volume in last 10min |
| PURR | 0 volume in last 10min |

### HMSTR 怪象

HMSTR `active=11min`（每根 1m candle 都有成交量）但 crossed book！这意味着 taker 在 crossed 区成交，而 Post-Only maker 永远吃不到。V12 的 CROSSED/BLOCKED 正确拦截了这种情况。

## 门禁决策矩阵（V12 完整闸门优先级）

```
1. Crossed Book?      → CROSSED/BLOCKED (0.0x)  — REST-verified
2. gross_ticks < 2?   → GATE_BLOCKED (0.0x)     — 统一 2-tick 下限
3. 2 <= ticks < 4?    → SNIPER (0.4x)           — 微利做市
4. ticks >= 4?        → TSUNAMI (1.0x)          — 全量挂单
```

## Shading 行为矩阵

| gross_bps | rejection count | V11.6 offset | V12 offset | 变化 |
|-----------|----------------|-------------|------------|------|
| 60 | 0 | 0 | 0 | — |
| 60 | 3 | `1 + 90 = 91` → capped 10 | `min(1, 1)` = 1 | **-90 ticks** |
| 60 | 6 | `2 + 90 = 92` → capped 10 | `min(1, 1)` = 1 | **-90 ticks** |
| 10 | 3 | 1 + 0 = 1 | 1 (no cap, <30) | — |
| 10 | 6 | 2 + 0 = 2 | 2 (no cap, <30) | — |

## 文件变更

```
src/engine.rs          — 3 patches (crossed book + shading cap ×3)
scripts/scan_hl_structural_v5.py  — 新增 4-stage scanner
docs/scanner_v5.md     — scanner 文档
docs/v12_engine.md     — 本文档
```

Git: `0cc8974` (`engine.rs`), `99a221e` (`scanner fix + docs`)
