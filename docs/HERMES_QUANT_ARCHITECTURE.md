# Hermes Quant System · 统一多市场交易架构 V1

> **设计原则**: 市场无关核心 | ML 即插即用 | 适配器隔离 | 风控强制门 | 渐进上线
>
> 继承自 [Polymarket 7-Layer](polymarket_system_mindmap.html) + [V8 Fusion](quant_system_v8_blueprint.html)  
> 从当前 Growth Engine v6.2 演化

---

## 1. 架构全景

```
                          ┌──────────────────────────────────┐
                          │        🤖 AI/ML 推演域           │
                          │  ResNet → AlphaCast → MCTS       │
                          │  (Phase 3+ 阶段接入，非必须)     │
                          └──────────────┬───────────────────┘
                                         │ 融合特征向量
  ┌──────────────────────────────────────┼──────────────────────────────────────┐
  │                                      ▼                                      │
  │  ┌─────────┐   ┌──────────────┐   ┌──────────┐   ┌──────────┐   ┌───────┐ │
  │  │ L0 数据  │──▶│ L1 市场适配器 │──▶│ L2 标准化 │──▶│ L3 Alpha │──▶│  L4   │ │
  │  │ 多源聚合  │   │ 统一接口隔离  │   │ +Redis总线│   │ 引擎并行  │   │决策风控│ │
  │  └─────────┘   └──────────────┘   └──────────┘   └──────────┘   └───┬───┘ │
  │                                                                      │     │
  │  ┌─────────┐   ┌──────────────┐   ┌──────────┐   ┌──────────┐        │     │
  │  │ L7 结算  │◀──│ L6 多路执行   │◀──│ L5 订单路由│◀──│  融合决策  │◀───────┘     │
  │  │ 统一会计  │   │ 4市场并行通道  │   │ 智能分发   │   │          │              │
  │  └─────────┘   └──────────────┘   └──────────┘   └──────────┘              │
  │                                                                             │
  │  ♻️ 反馈回路: Label → Model · Importance → Feature · Gate Stats → Param   │
  └─────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. 七层详解 + 扩展点

### L0 · 多市场数据源

```
  Polymarket        Crypto Perps        Equity/Options       Forex
  ┌──────────┐    ┌──────────────┐    ┌──────────────┐    ┌──────────┐
  │ WS实时盘口 │    │ Binance WS   │    │ Alpaca REST  │    │ MT4/5 ZMQ│
  │ REST 500+  │    │ OKX WS       │    │ IB TWS API   │    │ DWX EA   │
  │ CLOB 链上  │    │ Bybit WS     │    │ Options Chain │    │ Tick流   │
  └──────────┘    └──────────────┘    └──────────────┘    └──────────┘
                         │                    │                  │
              ┌──────────┴────────────────────┴──────────────────┘
              │         新闻/情绪/NLP: Twitter · RSS · NewsAPI       │
              └─────────────────────────────────────────────────────┘
```

**扩展点**: 新增市场 = 新增一个 L0 数据源 + 对应 L1 适配器，其余 6 层零改动。

---

### L1 · 市场适配器层 ⭐ (最关键抽象)

```rust
/// 统一接口 — 全系统唯一与市场交互的合约
trait MarketAdapter: Send + Sync {
    // ── 生命周期 ──
    fn connect(&mut self) -> Result<()>;
    fn disconnect(&mut self) -> Result<()>;
    fn is_connected(&self) -> bool;
    fn market_type(&self) -> MarketType;

    // ── 数据 ──
    fn subscribe(&self, symbols: &[String]) -> Result<()>;
    fn fetch_orderbook(&self, symbol: &str) -> Result<UnifiedBook>;
    fn fetch_positions(&self) -> Result<Vec<UnifiedPosition>>;
    fn fetch_balance(&self) -> Result<UnifiedBalance>;

    // ── 交易 ──
    fn place_order(&self, order: UnifiedOrder) -> Result<OrderAck>;
    fn cancel_order(&self, order_id: &str) -> Result<()>;
    fn cancel_all(&self, symbol: &str) -> Result<()>;
    fn get_order_status(&self, order_id: &str) -> Result<OrderState>;

    // ── 市场属性 ──
    fn min_notional(&self, symbol: &str) -> Result<f64>;
    fn tick_size(&self, symbol: &str) -> Result<f64>;
    fn maker_fee_bps(&self) -> f64;
    fn taker_fee_bps(&self) -> f64;
    fn is_session_open(&self) -> bool;          // 24/7 vs NYSE vs 24/5
    fn settlement_mode(&self) -> Settlement;     // Instant vs T+2 vs OnChain
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MarketType { Prediction, CryptoPerp, Equity, Forex, Options, Futures }

// 统一订单 — 上层逻辑永不碰交易所有关字段
struct UnifiedOrder {
    symbol: String,
    side: OrderSide,
    qty: f64,               // 总是基础单位 (股/张/币)
    price: Option<f64>,     // None = 市价
    tif: TimeInForce,
    reduce_only: bool,
    client_id: String,      // 幂等键
}

// 各适配器负责: qty换算(手数/cotracts/shares)、价格精度、交易所特定字段
```

**已有实现对照**: 当前 `executor.rs` 的 `place_limit_order()` 是 CryptoAdapter 的雏形。

**适配器实现清单**:
| 适配器 | 状态 | 复杂度 | 依赖 |
|--------|------|--------|------|
| `CryptoAdapter` (Hyperliquid) | ✅ 已有 Growth Engine | 低 — 直接提取 | `hl_sign.py` |
| `PredictionAdapter` (Polymarket) | 📦 已有 PM bot 代码可复用 | 中 — CLOB签名 | `py_order_utils` |
| `EquityAdapter` (Alpaca) | 📋 待建 | 低 — REST简单 | `alpaca-py` |
| `ForexAdapter` (MT5) | 📋 待建 | 中 — ZMQ桥接 | MetaTrader5 |
| `OptionsAdapter` | 📋 远期 | 高 — Greeks计算 | IB API |

---

### L2 · 数据标准化 + Redis 总线

```
  ┌─────────────┐    ┌─────────────────────────────────────────┐
  │ DataNormalizer│───▶│  Redis 统一命名空间                     │
  │ Tick→OHLCV   │    │  poly:*  crypto:*  equity:*  forex:*   │
  │ 全P&L→USD    │    │  {market}:book:{symbol}                 │
  │ 时间戳对齐   │    │  {market}:trades:{symbol}               │
  │ 复权因子    │    │  {market}:positions                     │
  └─────────────┘    │  {market}:signals                       │
                     │  portfolio:* (P&L汇总)                   │
                     │  features:* (P1)  labels:* (P2)          │
                     └─────────────────────────────────────────┘
```

**当前对照**: `SignalBus` (types.rs) = 内存版 Redis 总线，单进程够用。

**迁移路径**: `SignalBus` → `RedisBus`（多进程/跨VPS时才升级，YAGNI）

---

### L3 · Alpha 引擎层

```
  ┌───────────────────────────────────────────────────┐
  │  Alpha Engine Registry (可插拔引擎)                │
  ├───────────────┬───────────────┬───────────────────┤
  │ 通用引擎       │ 市场专属引擎    │ ML 增强引擎        │
  ├───────────────┼───────────────┼───────────────────┤
  │ Spread Capture│ Funding Arb   │ ResNet 深度信号    │
  │ OBI v2        │ IV Surface     │ AlphaCast 时序预测 │
  │ OFI Engine    │ Event Shock    │ MCTS 路径推演      │
  │ Momentum      │ FX Carry       │ MetaLabeler 过滤   │
  └───────────────┴───────────────┴───────────────────┘
```

**引擎接口**:
```rust
trait AlphaEngine: Send + Sync {
    fn name(&self) -> &str;
    fn applicable_markets(&self) -> &[MarketType];
    fn evaluate(&self, ctx: &AlphaContext) -> Vec<AlphaSignal>;
    fn is_healthy(&self) -> bool;
}

struct AlphaSignal {
    engine: String,
    market: MarketType,
    symbol: String,
    direction: f64,       // [-1, +1] 方向强度
    confidence: f64,      // [0, 1] 置信度
    horizon_seconds: u64, // 预期持仓时间
    features: HashMap<String, f64>,  // 特征快照(用于后续标签)
}
```

**当前对照**: 当前 Growth Engine 的 `risk.rs` + DAO 定价 = 单引擎 Spread Capture (买1卖1做市)。可以外挂更多引擎。

**扩展优先级**:
1. **P0** — Spread Capture（已有）
2. **P1** — OBI v2（订单簿失衡检测，5行代码）
3. **P2** — Funding Rate Arb（Crypto专属）
4. **P3** — Empirical Alpha 三阶段（Buffer→Feature→Labeler）— 需要 5K+ 标签积累
5. **远期** — ResNet/AlphaCast/MCTS（需要 GPU + 大量标签数据）

---

### L4 · 决策与风控层 ⭐ (强制门)

```
  AlphaSignals ──▶ ┌──────────────────────────────┐
                   │  Signal Fusion Engine         │
                   │  动态权重 × 市场类型 × 置信度   │
                   └──────────────┬───────────────┘
                                  ▼
                   ┌──────────────────────────────┐
                   │  RISK GATES (顺序，不可跳过)   │
                   │  G0: 启停开关                   │
                   │  G1: 组合硬上限 (equity×0.6)    │
                   │  G2: 单市场上限 (per-market cap)│
                   │  G3: 流动性门 (depth/spread)    │
                   │  G4: Volatility 门 (vol>阈值拒) │
                   │  G5: PDT/保证金/清算距离         │
                   │  G6: Kill Switch (wd<$阈值)     │
                   │  G7: Meta-Label (ML模型分数)    │
                   └──────────────┬───────────────┘
                                  ▼
                   ┌──────────────────────────────┐
                   │  Position Sizer               │
                   │  Kelly Criterion × conf × cap  │
                   └──────────────────────────────┘
```

**当前对照**: Growth Engine 已有 G1(硬上限)、水位线 Unwind、Kill Switch、Cooldown、CircuitBreaker。缺少 G3-G5-G7（多市场专属）。

---

### L5 · 智能订单路由

```
  UnifiedOrder ──▶ ┌──────────────────────────────┐
                   │  Smart Order Router          │
                   │  Order → MarketAdapter 映射    │
                   │  Limit优先 · 改单>撤单+重下    │
                   │  订单状态机: NEW→POSTED→FILLED  │
                   └──────────────────────────────┘
```

**状态机**（当前已有雏形在 `PerCoinState`）: `NEW → POSTED → PARTIAL → FILLED | CANCELED | REJECTED`

---

### L6 · 多路并行执行

```
  ┌───────────┐  ┌───────────┐  ┌───────────┐  ┌───────────┐
  │ Poly Exec  │  │Crypto Exec│  │Equity Exec│  │Forex Exec │
  │ 住宅代理池  │  │直连/代理  │  │Alpaca/IB  │  │ZMQ→MT4   │
  │ Session绑定│  │资金费率监控│  │PDT守护    │  │手数换算   │
  │ Polygon RPC│  │强平预警   │  │T+2追踪    │  │过夜利息   │
  └───────────┘  └───────────┘  └───────────┘  └───────────┘
```

**当前对照**: `executor.rs` = Crypto Exec。需为每个市场实现独立执行通道。

---

### L7 · 统一结算与会计

```
  ┌──────────────────────────────────────────────┐
  │  Unified Accounting Engine                   │
  │  - 跨市场 P&L 汇总 (统一 USD)                 │
  │  - 实时 Sharpe / Sortino / MaxDD             │
  │  - 每日自动快照 → SQLite/CSV                 │
  │  - AlphaCast 预测误差追踪 (远期)              │
  └──────────────────────────────────────────────┘
```

---

## 3. ML/AI 集成路线图 (Phase 驱动)

```
  Phase 1 (当前)           Phase 2 (1-2月)          Phase 3 (3-6月)         Phase 4 (远期)
  ┌─────────────┐        ┌──────────────┐        ┌──────────────┐        ┌──────────────┐
  │ Growth Engine│        │ + Empirical  │        │ + ResNet     │        │ + AlphaCast  │
  │ v6.2         │        │   Alpha 三阶  │        │ + MetaLabeler│        │ + MCTS       │
  │              │        │              │        │              │        │              │
  │ 做市策略      │   ──▶  │ Buffer/Feature│  ──▶  │ 128d深度嵌入  │  ──▶  │ Transformer  │
  │ 状态机风控    │        │ CrossSection │        │ LightGBM过滤  │        │ N步仿真推演   │
  │ 实时盘口WS   │        │ HardGating   │        │ Top-Decile   │        │ 奖励校准回溯  │
  └─────────────┘        └──────────────┘        └──────────────┘        └──────────────┘
  ✅ Done                 ⬜ 需5K标签积累          ⬜ 需GPU + 10K标签      ⬜ 需成熟Phase 3
```

**Phase 1→2 的关键路径**: 积累 CounterfactualLabel 标签（所有 Alpha 信号无论执行与否都打标），目标 5K 条后激活 MetaLabeler。

---

## 4. 代码目录结构（目标态）

```
hermes-quant/
├── Cargo.toml                    # workspace: core + adapters + engines
├── README.md
├── docs/                         # 架构文档
│   ├── ARCHITECTURE.md
│   ├── ADAPTERS.md               # 各适配器实现指南
│   └── ML_INTEGRATION.md         # ML 集成路线
│
├── core/                         # 市场无关核心
│   ├── types.rs                  # UnifiedOrder, UnifiedBook, UnifiedPosition...
│   ├── traits.rs                 # MarketAdapter, AlphaEngine, RiskGate
│   ├── bus.rs                    # SignalBus → RedisBus(远期)
│   ├── normalizer.rs             # DataNormalizer: Tick→OHLCV, P&L→USD
│   └── calendar.rs               # 多市场交易日历 (NYSE/24-7/24-5)
│
├── adapters/                     # 市场适配器 (每个一个 crate)
│   ├── crypto/
│   │   ├── mod.rs
│   │   ├── hyperliquid.rs        # ← 从当前 executor.rs/signer.rs 提取
│   │   └── binance.rs            # (Phase 2)
│   ├── prediction/
│   │   ├── mod.rs
│   │   └── polymarket.rs         # ← 从 PM bot 代码提取
│   ├── equity/
│   │   ├── mod.rs
│   │   └── alpaca.rs             # (Phase 3)
│   └── forex/
│       ├── mod.rs
│       └── mt5_bridge.rs         # (Phase 3)
│
├── engines/                      # Alpha 引擎 (每个一个 crate)
│   ├── spread_capture/           # ← 从当前 engine.rs 提取
│   ├── obi/                      # 订单簿不平衡 (Phase 2)
│   ├── funding_arb/              # 资金费率套利 (Phase 2)
│   ├── empirical_alpha/          # Buffer + Feature + CrossSection + Gating (Phase 2)
│   │   ├── buffer.rs
│   │   ├── feature.rs
│   │   ├── cross_section.rs
│   │   ├── gating.rs
│   │   └── labeler.rs
│   ├── resnet/                   # 深度特征提取 (Phase 3)
│   ├── alphacast/                # 时序预测 (Phase 4)
│   └── mcts/                     # 路径推演 (Phase 4)
│
├── decision/                     # 决策与风控
│   ├── fusion.rs                 # 多信号融合 (动态权重)
│   ├── gates.rs                  # G0-G7 风控门
│   ├── sizer.rs                  # Kelly 仓位计算
│   └── portfolio.rs              # 组合 VaR + 相关矩阵
│
├── execution/                    # 执行层
│   ├── router.rs                 # Smart Order Router
│   ├── order_state.rs            # 统一订单状态机
│   └── humanizer.rs              # 行为拟人化 (Poisson, 延迟注入)
│
├── settlement/                   # 结算与会计
│   ├── pnl.rs                    # 统一 P&L 计算
│   ├── ledger.rs                 # 交易账本 (SQLite)
│   └── reports.rs                # 日报/周报生成
│
├── monitor/                      # 监控
│   ├── api.rs                    # FastAPI metrics 端点
│   ├── dashboard.rs              # WebSocket SPA
│   └── alerts.rs                 # Telegram/DingTalk
│
├── harness/                      # 测试框架
│   ├── backtest.rs               # 多市场联合回测
│   └── preflight.rs              # 部署前校验 (← 已有 HL test harness)
│
└── scripts/
    ├── hl_sign.py                # ← 已有
    └── deploy.sh
```

---

## 5. 当前 → 目标态的迁移路线

### Step 0 (今天): 提取通用接口

从当前 `types.rs` 和 `executor.rs` 抽取 `MarketAdapter` trait，将 Hyperliquid 特定逻辑封装为 `CryptoAdapter`。

**工作量**: ~2h，不改行为，只重构结构。

```rust
// 当前 (紧耦合)
executor.place_limit_order(coin, is_buy, sz, px, tif)

// 目标 (解耦)
let crypto: CryptoAdapter = adapters.get(MarketType::CryptoPerp);
crypto.place_order(UnifiedOrder { symbol: coin, side, qty: sz, price: px, .. })?;
```

### Step 1 (本周): 提取 Alpha Engine 接口

```rust
trait AlphaEngine {
    fn evaluate(&self, ctx: &AlphaContext) -> Vec<AlphaSignal>;
}

// 当前做市策略 = SpreadCaptureEngine
struct SpreadCaptureEngine { config: SpreadCaptureConfig }
impl AlphaEngine for SpreadCaptureEngine { ... }
```

### Step 2 (1-2周): Empirical Alpha P1

- Port `MarketStateBuffer`, `FeatureEngine`, `CrossSectionEngine` from OracleForge
- 积累 Buffer 数据（不阻塞，后台异步）

### Step 3 (积累期): 等待标签

- 正常运行做市策略，CounterfactualLabeler 全量记录信号→成交映射
- 达到 5K 后激活 MetaLabeler (Phase 3)

### Step 4+ (按需): 新增市场适配器

每增加一个市场 = 新增一个 Adapter + 一个 Exec channel，其余代码零改动。

---

## 6. 不可变设计约束

| 约束 | 原因 |
|------|------|
| 上层代码只与 trait 交互，不 import 任何适配器内部类型 | 防止耦合泄漏 |
| 风控门必须在决策层之前执行 | 当前 Growth Engine 已经有 G1-G2 |
| 所有 Alpha 信号无论执行与否都打标 | 为 ML 积累标签 |
| 每个适配器独立进程/线程 | 一个市场崩溃不拖垮全局 |
| Post-Only 始终优先，GTC 仅用于紧急减仓 | 费率和滑点教训 |
| 所有 P&L 统一换算 USD | 跨市场净额计算基础 |

---

## 7. 与当前 Growth Engine 的映射

```
  当前 Growth Engine v6.2          统一架构
  ──────────────────────         ──────────
  main.rs                          main.rs (不变)
  config.rs                        core/config.rs
  types.rs + SignalBus             core/types.rs + core/bus.rs
  signer.rs + hl_sign.py           adapters/crypto/signer.rs
  executor.rs                      adapters/crypto/hyperliquid.rs
  engine.rs                        engines/spread_capture/  + decision/
  risk.rs                          decision/gates.rs + decision/portfolio.rs
  state.rs                         core/state.rs
  ws.rs + order_book.rs            adapters/crypto/ws.rs
  levels.rs                        engines/spread_capture/levels.rs
```

**迁移策略**: 逐个文件提取，每次 extract 后立即 build 验证（不是一次性重构）。

---

## 8. 下一步

1. **确认架构方向** — 用户 review 本文档
2. **Step 0 实施** — 从当前 Growth Engine 提取 `MarketAdapter` trait
3. **适配器开发优先级** — 先 Crypto(已有)、再 Prediction(PM 代码复用)、再 Equity/Forex(按需)
4. **Phase 2 触发条件** — 做市策略稳定运行 2 周 + 积累足够标签后开始 Empirical Alpha
