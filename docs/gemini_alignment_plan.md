# Gemini 建议 vs 实际代码 · 对齐与落地计划

> 阅读时间 8 分钟 | 产出：立即可执行的 5 个 Step

---

## 一、现状诊断：我们手里到底有什么

```
growth-engine/src/ (P1, Rust, 10 文件, 4201 LOC, 编译 0 warning)
├── types.rs       ← ALL HL-specific (OrderRequest 用 HL API 字段名)
├── executor.rs    ← ALL HL-specific (place_limit_order 直接拼 JSON)
├── signer.rs      ← HL EIP-712 specific
├── ws.rs          ← HL WebSocket specific
├── config.rs      ← 做市策略参数 (skew/bps/qty)
├── engine.rs      ← 核心循环，直接调 executor，不经过 trait
├── state.rs       ← ✅ 唯一的通用组件 (State 枚举与市场无关)
├── risk.rs        ← 混合：通用风控逻辑 + HL 特定字段
├── order_book.rs  ← 通用 L2Book (并不含 HL 特定类型)
└── main.rs        ← 启动入口
```

**关键结论**: 10 个文件中，state.rs 和 order_book.rs 已经是通用组件；其余 8 个都直接或间接耦合 Hyperliquid。**Gemini 所有建议都依赖于一个前提：上层代码不直接调用交易所 API。我们还没有这个前提。**

---

## 二、Gemini 5 大建议 + 现实对齐

### 建议 1: L0/L1 — "泛化 MarketSnapshot + 能力契约 Mixins"

| Gemini 提的 | 我们现在的 | 差距 | 动作 |
|------------|-----------|------|------|
| 期货 Basis/OI/Expiry | 无 — 只有一个市场 | 远期 | 待有新市场时再加 |
| 期权 Greeks/IV Surface | 无 | 远期 | Phase 4 |
| 预测市场条件概率 | 无 | 可复用 PM bot 代码 | Phase 3 |
| IMarketAdapter Capabilities | executor.rs 直接拼 JSON | **核心差距** | **← 立即做** |
| 动态 Mixins 挂载 | 无 | 架构模式 | **← 先建基础** |

**实际动作**: 从 executor.rs 提取 `MarketAdapter` trait → 这是 Gemini 建议 1 的地基。

### 建议 2: 特征工程 — "MoE 路由 + 插件工厂"

| Gemini 提的 | 我们现在的 | 差距 | 动作 |
|------------|-----------|------|------|
| Plugin Feature Registry | 无特征工程 | 需要先有特征 | Phase 2 (Empirical Alpha) |
| MoE 动态路由 | 无 | 需要多市场+GPU | Phase 3-4 |
| 自动降级到经验特征 | 无 | 需要 GPU 推断流 | Phase 3 |

**现实判断**: 我们连 `FeatureEngine` 都没有（V8 蓝图已有的 Python 代码）。这应该在 Empirical Alpha Phase 2 时做，不是现在。

**实际动作**: 先从 OracleForge 移植 `MarketStateBuffer` → 这是 MoE 能工作的前提。

### 建议 3: AlphaCast/MCTS/MetaLabeler — DL/ML/RL 三层融合

| Gemini 提的 | 我们的现在 | 差距 | 动作 |
|------------|-----------|------|------|
| 多任务预测头 | 无 | 需要 GPU + 标签 | Phase 4 |
| 迁移学习冷启动 | 无 | 需要预训练模型 | Phase 4 |
| MCTS 动作空间升维 | 无 MCTS | 需要 AlphaCast 先工作 | Phase 4 |
| RL Policy+Value 引导 MCTS | 无 | 需要 RL 训练管线 | Phase 5 |
| MetaLabeler 过滤 | 无 | 需要 5K 标签 | Phase 3 |

**现实判断**: 这整个建议是 Phase 3-5 的。我们当前在 Phase 1。

**现在能做的准备**: 
- 开始打标签（CounterfactualLabeler 全量记录信号→成交映射） → ML 燃料积累
- 这会直接解锁 MetaLabeler

### 建议 4: Harness/风控 — "资产感知的分布式事务 + FSM"

| Gemini 提的 | 我们的现在 | 差距 | 动作 |
|------------|-----------|------|------|
| FSM 事务编排 | state.rs 已有 6 状态机 | **结构已有** | **← 形式化** |
| 多资产风控容器 | risk.rs 仅有单一市场 | 需要抽象 | **← 提取 RiskGate trait** |
| PDT/Greeks 监控 | 无 | 远期 | 新增市场时 |
| 动态 Kelly + σ 惩罚 | 硬上限比例 | 基本够用 | 优化非必须 |

**实际判断**: state.rs 已经是 Gemini 说的 FSM 引擎雏形。这是我们现在最接近 Gemini 建议的组件。

### 建议 5: 计算拓扑 — "GPU/CPU 分离 + 分布式总线"

| Gemini 提的 | 我们的现在 | 差距 | 动作 |
|------------|-----------|------|------|
| TensorRT/ONNX serving | 无 GPU | 没有硬件 | 远期 |
| Kafka/ZeroMQ 总线 | SignalBus (内存 RwLock) | 单进程够用 | 多进程时才升级 |
| gRPC 推断分离 | 无 | 没有 GPU | 远期 |

**现实判断**: 单 VPS、单进程、$120 本金。分布式总线是过度设计。SignalBus 内存版现在是正确选择（YAGNI）。

---

## 三、优先级矩阵

```
                    紧迫性
                  低      高
             ┌─────────┬─────────┐
         高  │ MoE路由  │ Trait   │ ← 立即做
重要性       │ RL+MCTS  │ 提取    │
             ├─────────┼─────────┤
         低  │ GPU拓扑  │ 标签打点 │ ← 等积累
             │ 分布式总线│ FSM形式化│
             └─────────┴─────────┘
```

---

## 四、立即可执行的 5 个 Step

### Step 1: 提取 MarketAdapter trait (30 min)

**为什么是第一步**: 这是 Gemini 建议 1 的地基，也是我们架构文档的核心抽象。不改行为，只改结构。

**当前代码**:
```rust
// executor.rs: 紧耦合 — engine 直接调 HL API
executor.place_limit_order(coin, is_buy, sz, px, tif)?;
executor.cancel_order(coin, oid)?;
executor.fetch_account_state()?;
```

**目标代码**:
```rust
// core/traits.rs: 解耦 — engine 只与 trait 交互
trait MarketAdapter {
    fn market_type(&self) -> MarketType;
    fn place_order(&self, order: &UnifiedOrder) -> Result<OrderAck>;
    fn cancel_order(&self, asset: &str, oid: u64) -> Result<()>;
    fn fetch_state(&self) -> Result<AccountState>;
    fn tick_size(&self, asset: &str) -> f64;
    fn min_notional(&self, asset: &str) -> f64;
    fn maker_fee_bps(&self) -> f64;
    fn taker_fee_bps(&self) -> f64;
}

enum MarketType { CryptoPerp, Prediction, Equity, Forex, Options, Futures }

struct CryptoAdapter { executor: Executor, signer: Signer }
impl MarketAdapter for CryptoAdapter { ... }
```

**具体操作**:
1. 新建 `src/traits.rs` 写 trait 定义
2. 在 `executor.rs` 上实现 `impl MarketAdapter for CryptoAdapter`
3. `engine.rs` 改为接受 `Box<dyn MarketAdapter>`
4. `cargo check` 确认 0 错误

### Step 2: 统一类型定义 (20 min)

**为什么**: Gemini 的 MarketSnapshot 泛化要求所有市场共享基础类型。

```rust
// 新建 core/unified.rs 或在 types.rs 中新增

/// 统一订单 — 所有市场用同一结构
struct UnifiedOrder {
    market: MarketType,
    symbol: String,
    side: OrderSide,
    qty: f64,             // 基础单位（股/张/币），适配器负责换算
    price: Option<f64>,   // None = 市价
    tif: TimeInForce,
    reduce_only: bool,
    client_id: String,
}

/// 市场能力声明 — Gemini 建议的 Capability 简化版
struct MarketCapabilities {
    supports_margin: bool,
    supports_options: bool,
    supports_post_only: bool,
    settlement_mode: Settlement,
    session_24_7: bool,
}
```

### Step 3: 形式化状态机 FSM (20 min)

**为什么**: Gemini 建议 4 的核心是 FSM 事务编排。state.rs 已经是 6 状态 FSM，只需形式化接口。

```rust
// 当前已有状态: Idle → ColdStart → Active → Unwind → Shedding → Cooldown
// 形式化为 trait

trait StateMachine {
    fn current(&self) -> State;
    fn transition_allowed(&self, from: State, to: State) -> bool;
    fn tick(&mut self, ctx: &TickContext) -> Option<State>;
    fn on_enter(&mut self, state: State);
    fn on_exit(&mut self, state: State);
}
```

这一步基本零成本 — state.rs 已经包含了所有转换逻辑，只需加 trait 声明。

### Step 4: 标签记录路径 (30 min)

**为什么**: 这是 Phase 2 (Empirical Alpha) 和 Phase 3 (MetaLabeler) 的唯一燃料。Gemini 的 DL/ML/RL 全栈都需要标签数据。现在不打，两个月后还是零。

```rust
// 在 engine.rs 每个信号决策点增加一行记录
struct SignalLabel {
    timestamp: u64,
    market: MarketType,
    symbol: String,
    features: HashMap<String, f64>,  // 当前已知特征
    signal: AlphaSignal,              // 引擎输出
    decision: Decision,               // EXECUTED / REJECTED_GATE / REJECTED_RISK
    outcome: Option<FillOutcome>,     // 成交后回溯填入
}

// 写入 SQLite 或 CSV (简单方案): 每行一个 JSON
fn record_label(label: &SignalLabel) {
    append_to_csv("labels.csv", serde_json::to_string(label));
}
```

**关键**: 即使现在只有做市策略，也要记录每一次 BUY/SELL 决策 + 当时的盘口特征 + 最终成交结果。这是 ML 训练的**唯一原料**。

### Step 5: 引擎注册表 (15 min)

**为什么**: 为多 Alpha 引擎并行 (Gemini 建议 3 的前提) 做准备。

```rust
// engines/registry.rs
trait AlphaEngine {
    fn name(&self) -> &str;
    fn applicable_markets(&self) -> &[MarketType];
    fn evaluate(&self, ctx: &AlphaContext) -> Vec<AlphaSignal>;
}

struct EngineRegistry {
    engines: Vec<Box<dyn AlphaEngine>>,
}

impl EngineRegistry {
    fn register(&mut self, engine: Box<dyn AlphaEngine>);
    fn evaluate_all(&self, ctx: &AlphaContext) -> Vec<AlphaSignal>;
}

// 当前唯一引擎 = SpreadCaptureEngine (提取自 engine.rs)
struct SpreadCaptureEngine { config: SpreadCaptureConfig }
impl AlphaEngine for SpreadCaptureEngine { ... }
```

---

## 五、做完 5 个 Step 后的架构

```
                 Engine Registry
                 ┌─────────────┐
                 │SpreadCapture │ ← 当前唯一引擎
                 │ OBI (空壳)   │ ← 预留
                 └──────┬──────┘
                        │ AlphaSignal
                 ┌──────▼──────┐
                 │Decision/Gates│ ← 风控门
                 └──────┬──────┘
                        │ UnifiedOrder
                 ┌──────▼──────┐
                 │MarketAdapter│ ← trait (当前仅 CryptoAdapter)
                 │  trait       │
                 └──────┬──────┘
                        │
          ┌─────────────┼─────────────┐
          ▼             ▼             ▼
    CryptoAdapter  PolyAdapter   EquityAdapter
      (已有)        (PM代码复用)    (空壳)
```

**关键**: 架构上已经能接纳 PM 和 Equity 适配器，只需实现 trait。

---

## 六、什么不要做（反 KPI 清单）

| 不要做的 | 为什么 |
|---------|--------|
| 搭建 GPU 推理服务 | 没有 GPU，没有标签，$120 本金不需要 |
| 实现 MoE 路由 | 先有多个特征类型再说 |
| 升级到 Kafka/ZeroMQ | SignalBus 内存版单进程完全够用 |
| 实现 MCTS | 先有 AlphaCast（先有标签数据） |
| 实现 RL Policy Network | Phase 5，先积累 10K+ 标签 |
| ResNet 深度嵌入 | 需要 GPU + 50+ 特征（Phase 2 才有） |
| 多腿期权组合下单 | 没有期权市场，没有 IB 账户 |

---

## 七、时间估计

| Step | 内容 | 时间 | 风险 |
|------|------|------|------|
| 1 | MarketAdapter trait 提取 | 30 min | 低 — 纯重构，不改行为 |
| 2 | 统一类型定义 | 20 min | 低 — 新增文件 |
| 3 | FSM 形式化 | 20 min | 极低 — state.rs 已有全部逻辑 |
| 4 | 标签记录路径 | 30 min | 低 — 新增 CSV 写入 |
| 5 | 引擎注册表 | 15 min | 低 — 新增 trait |
| **合计** | | **< 2h** | |

全部 5 步做完后 `cargo check` 应 0 错误 0 警告，行为与现在完全一致。

---

## 八、Gemini 建议的价值评估

| 建议 | 方向正确性 | 当前可落地度 | 价值 |
|------|----------|------------|------|
| 1. 泛化多态引擎 | ✅ | 🟢 80% (trait提取) | 地基级 |
| 2. MoE 特征路由 | ✅ | 🔴 5% (缺特征+GPU) | Phase 3 |
| 3. DL/ML/RL 三层 | ✅ | 🔴 0% (缺标签+GPU) | Phase 4-5 |
| 4. FSM + 风控容器 | ✅ | 🟡 50% (FSM已有) | 近期 |
| 5. 分布式计算拓扑 | ⚠️ 过度设计 | 🔴 0% | 至少一年后 |

**结论**: Gemini 的建议架构方向完全正确，但时间线严重超前。我们现在需要的是**地基**（trait 提取 + 类型定义）和**燃料积累**（打标签），而不是直接搭第五层。

---

## 九、立即动手

要我现在开始 Step 1 (提取 MarketAdapter trait) 吗？这是所有后续扩展的唯一阻塞点。不改行为，30 分钟，纯结构重构。
