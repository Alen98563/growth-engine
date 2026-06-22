# 统一多市场交易架构设计

## 已完成
- 阅读 Polymarket 7-Layer 思维导图 (JS node graph)
- 阅读 V8 融合蓝图 (ResNet + AlphaCast + MCTS + Empirical Alpha + Harness)
- 输出 `docs/HERMES_QUANT_ARCHITECTURE.md` (15KB) — 从当前 Growth Engine v6.2 演化的统一架构

## 核心设计

### 七层架构 (市场无关核心)
L0(多源数据) → L1(适配器,Trait隔离) → L2(标准化+Redis) → L3(Alpha引擎并行) → L4(决策风控) → L5(路由) → L6(多路执行) → L7(统一会计)

### 最关键抽象: MarketAdapter Trait
```rust
trait MarketAdapter { connect, subscribe, place_order, cancel, fetch_orderbook, tick_size, maker_fee, is_session_open, settlement_mode }
```
所有上层逻辑永不碰交易所特定字段。新增市场 = 实现一个 Adapter。

### ML 集成 (四阶段渐进)
Phase 1(当前): Spread Capture 做市 ↔ Growth Engine v6.2
Phase 2(1-2月): Empirical Alpha (Buffer→Feature→CrossSection→HardGating) — 需积累标签
Phase 3(3-6月): ResNet 深度嵌入 + MetaLabeler (LightGBM) — 需5K标签+GPU
Phase 4(远期): AlphaCast Transformer + MCTS 路径推演

### 代码结构
core/ (市场无关) → adapters/ (每个市场一个crate) → engines/ (每个Alpha一个crate) → decision/ → execution/ → settlement/

### 迁移路线
Step0(今天可做): 从executor.rs抽取MarketAdapter trait
Step1: 提取AlphaEngine trait
Step2: Empirical Alpha P1 (从OracleForge移植)
Step3: 积累标签→激活MetaLabeler
Step4+: 按需新增市场适配器
