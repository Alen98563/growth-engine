# Growth Engine — 系统开发框架

**版本**: v6.3 (2026-06-19)
**语言**: Rust 2021 Edition + Python 3.10 (签名桥)
**运行时**: Tokio async + parking_lot
**部署**: VPS (Virginia: 47.253.152.167:2233)
**路径**: `/tmp/growth-engine/` (VPS) | `C:\Users\Administrator\.qclaw\workspace-hermes\` (本地)

---

## 1. 项目架构总览

```
┌──────────────────────────────────────────────────────────────┐
│                      main.rs (启动入口)                        │
│  .env → Config → Signer → Executor → Engine::run()            │
│                     │                  │                      │
│              ws::run() (并行任务)     Labeler (CSV 输出)       │
│                     │                                         │
│               SignalBus (共享内存总线)                          │
│          ┌──────┼──────┐                                      │
│          ▼      ▼      ▼                                      │
│      L2Book  Fills  AccountState                              │
└──────────────────────────────────────────────────────────────┘

引擎核心循环 (每 3-5 秒一次):
┌─────────────────────────────────────────────────────────────┐
│ for coin in coins:                                           │
│   1. fetch_state → SignalBus.account                         │
│   2. book ← WS(snapshot) or REST(fallback, if stale >10s)    │
│   3. cancel_active_orders(coin) → 清空上一轮挂单               │
│   4. RiskEngine::assess(book, account, coin) → RiskOutput     │
│   5. PerCoinStateMachine → 状态转换判断                        │
│   6. 按状态下单 (NORMAL/UNWIND/SHEDDING)                      │
│   7. Labeler::record(CycleRecord) → CSV                      │
│   8. sleep(3-5s + jitter)                                    │
└─────────────────────────────────────────────────────────────┘
```

## 2. 模块职责矩阵

| 文件 | 职责 | 输入 | 输出 | 依赖 |
|------|------|------|------|------|
| `main.rs` | 启动配置, 模块注册, 任务编排 | `.env` + CLI | tokio task spawn | 全部 |
| `config.rs` | 参数管理, 环境变量覆盖 | env vars | Config struct | 无 |
| `types.rs` | 共享数据结构 | — | L2Book, AccountState, SignalBus | parking_lot |
| `types_proto.rs` | 跨市场统一类型 (proto→Rust) | — | UnifiedOrder, OrderState, AlphaSignal | serde, chrono, uuid |
| `traits.rs` | 交易所适配器抽象接口 | — | MarketAdapter trait | async_trait |
| `state.rs` | PerCoin FSM (6 状态) | coin, transition | State 转换 | types |
| `engine.rs` | 核心循环, 风控编排, 标签记录 | Config, SignalBus, MarketAdapter, Labeler | 无 (无限循环) | config, risk, state, traits, labeler |
| `risk.rs` | 三次幂偏斜 + 非对称量控 | L2Book, AccountState | RiskOutput | config, types |
| `executor.rs` | HL REST API 交互 (下单/撤单/查询) | Config, Signer | Oid, fill_sz, AccountState | config, signer, types |
| `signer.rs` | EIP-712 签名 (Python 子进程) | coin, px, sz, nonce | SignedAction | Python hl_sign.py |
| `order_book.rs` | L2 盘口解析 (WS→结构化) | serde_json::Value | L2Book update | types |
| `ws.rs` | WebSocket 连接管理, 数据推送 | Config, SignalBus | 无 (写入 SignalBus) | types, order_book |
| `labeler.rs` | 周期→CSV 标签记录 | CycleRecord | CSV 文件 (append) | serde |
| `levels.rs` | 多级报价管理器 (未注册) | — | — | — |

## 3. 状态机规范

### 3.1 Engine 状态 (6态 FSM)

```
          ┌────────────┐
          │   IDLE     │ ← 启动默认
          └─────┬──────┘
                │ has_reserve + gross_spread > 0
                ▼
          ┌────────────┐
          │ COLD_START │ 2 周期限价单 (零仓位, 无库存门控)
          └─────┬──────┘
                │ advance_cold_start() == true
                ▼
          ┌────────────┐
    ┌────→│   ACTIVE   │←────────────────────────┐
    │     └──┬───┬───┬─┘                          │
    │        │   │   │                            │
    │ pos>40%│   │   │ pos>90%                    │
    │        │   │   │                            │
    │        ▼   │   ▼                            │
    │  ┌────────┐│ ┌──────────┐     ┌───────────┐ │
    │  │UNWIND  ││ │SHEDDING  │────→│ COOLDOWN  │─┤
    │  │(被动)  ││ │(IOC强平) │     │ (30s静默) │ │
    │  └───┬────┘│ └──────────┘     └───────────┘ │
    │      │     │                                  │
    │      ▼     │                                  │
    │  pos<20%   │                                  │
    │  (滞后)    │                                  │
    │      │     │                                  │
    └──────┴─────┘──────────────────────────────────┘
    │      │ reset + spread OK
    │      └─→ UNWIND→ACTIVE: 仓位降至 watermark-hysteresis
    │
    └────→ COOLDOWN→ACTIVE: 30秒到期 (无条件)
```

**关键数值**:
- `watermark = 0.25` (25%) — 进入 PASSIVE_UNWIND
- `hysteresis = 0.20` — 退出阈值 = 25% - 20% = 5%
- `shed_trigger = 0.90` (90%) — 进入 EMERGENCY_IOC
- `shed_safe_reentry = 0.70` (70%) — IOC shed 停止条件
- `cooldown = 30s` — COOLDOWN 等待时间
- MAX_SHED_ITERATIONS = 6 — IOC shed 最大尝试次数

### 3.2 下单行为矩阵

| 状态 | BUY | SELL | TIF | 价格 | 备注 |
|------|-----|------|-----|------|------|
| NORMAL | ✅ | ✅ | Alo (Post-Only) | bid/ask ± skew | 双边做市 |
| PASSIVE_UNWIND(LONG) | ❌ | ✅ (tick retreat) | Alo | ask − tick×n | 仅SELL, 退tick防拒 |
| PASSIVE_UNWIND(SHORT) | ✅ (tick retreat) | ❌ | Alo | bid + tick×n | 仅BUY |
| SHEDDING(LONG) | ❌ | ✅ | Ioc | best_bid (吃单) | 折扣5%交叉价差 |
| SHEDDING(SHORT) | ✅ | ❌ | Ioc | best_ask | 同上 |
| COOLDOWN | ❌ | ❌ | — | — | 静默等待 |

## 4. 风险模型 (四层)

### Layer 0: 三次幂偏斜 + Tick 离散化

```
skew_bps = MAX_SKEW_BPS × (position_ratio)³
skew_ticks = floor(skew_bps × base_px / 10000 / tick_size)

LONG:  bid_px = best_bid - skew_ticks×tick
       ask_px = best_ask - skew_ticks×tick

SHORT: bid_px = best_bid + skew_ticks×tick
       ask_px = best_ask + skew_ticks×tick
```

- `MAX_SKEW_BPS = 2.0` (0.02% 最大偏斜)
- `skew_power = 3.0` (三次幂)
- 0.50 ratio → skew = 2.0 × 0.125 = 0.25 bps (几乎不可见)
- 0.90 ratio → skew = 2.0 × 0.729 = 1.46 bps (可见排斥)
- Tick 离散化确保微型偏斜 (≤0.05 bps) 被舍入为 0 tick

### Layer 1: 二次幂非对称量控

```
scale = 1.0 - position_ratio²

LONG:  buy_sz  = base_qty × scale    (减少买入)
       sell_sz = base_qty            (全额卖出)

SHORT: buy_sz  = base_qty            (全额买入)
       sell_sz = base_qty × scale    (减少卖出)
```

- `qty_power = 2.0` (二次幂)
- Min Lot Guard: scaled < min_order_size → 归零

### Layer 2: IOC 自适应强平

- 触发: pos_ratio ≥ 0.90
- 执行: 循环 IOC 市价单, 500ms 间隔
- 停止: pos_ratio < 0.70 OR 6次后强制 Cooldown
- 风险: 在浅深度市场无效 — 需 GTC last-resort 兜底 (B1 待修复)

### Layer 3: 硬上限 + 周期防护

- `hard_limit_ratio = 0.40` (40% wd 硬上限)
- 组合端口: total_notional < equity × 0.40
- 429 防护: 周期 3-5s + 30% 抖动

## 5. 数据流

```
WS (l2Book updates) ──→ SignalBus.books ──┬──→ engine (read)
                                          │
REST (fetch_account_state) ──→ bus.account ──→ engine
                                          │
REST (fetch_l2_snapshot) ──→ bus.books ───┘
                                          │
WS (userFills) ──→ bus.fills ──→ engine: drain_fills() (每周期消费)
                                          │
engine: labeler::record() ──→ CSV ──→ label_cfl.py ──→ labeled.csv
```

## 6. 部署检查清单

- [ ] `cp .env.sample .env` + 填写 `HL_PRIVATE_KEY`, `HL_ADDRESS`
- [ ] `HL_COINS` 用逗号分隔: `"FARTCOIN,PUMP"` (仅支持 HIP-3 Growth Mode 币种)
- [ ] `HL_GROWTH_MODE=true` (非 Growth Mode 费率 6bps 往返, 不可盈利)
- [ ] `HL_CYCLE_SLEEP_MIN=3.0` `HL_CYCLE_SLEEP_MAX=5.0`
- [ ] `cargo build` → `target/debug/growth-engine` (不要用 `--release`, VPS 编译 OOM)
- [ ] 启动前: `python3 scripts/liquidate.py` 平仓残留仓位 (可选)
- [ ] 启动: `RUST_LOG=info ./target/debug/growth-engine`
- [ ] 监控: `tail -f logs/labels.csv` 实时标签 + `journalctl -f` 看 tracing 日志
- [ ] 停止: `pkill growth-engine` (待 B1 修复前需手动 cancel_all)

## 7. 标签管道 (ML Fuel)

```
引擎每周期 → labeler → CSV (实时追加)
                        │
                        ▼
              label_cfl.py (离线批处理)
                        │
                        ▼
              计算反事实标签 (CFL):
              W(win)  : 连续 3 周期 forward_equity_delta > 0
              F(flat) : Δ ≤ ± spread 噪音
              L(loss) : forward_equity_delta < −spread
                        │
                        ▼
              labeled.csv (带标签)
```

### CycleRecord 字段

| 字段 | 类型 | 说明 |
|------|------|------|
| ts | ISO-8601 | UTC 时间戳 |
| coin | String | 币种 |
| state | String | 引擎状态 (NORMAL/UNWIND/…) |
| pos_sz | f64 | 净仓位 (正=多, 负=空) |
| pos_notional | f64 | 仓位名义价值 (=|sz|×mid_px) |
| withdrawable | f64 | 可提取余额 |
| equity | f64 | 账户总权益 |
| gross_spread_bps | f64 | 原始买卖价差 |
| net_spread_bps | f64 | 偏斜后有效价差 |
| skew_bps | f64 | 施加的偏斜 |
| position_ratio | f64 | 仓位占比 [0,1] |
| volatility | f64 | 波动率 (TODO: VolTracker) |
| mid_px | f64 | 中间价 |
| bid_sz | f64 | 买盘深度 (top N levels) |
| ask_sz | f64 | 卖盘深度 |
| placed_buy_sz | f64 | 本轮挂买单数量 |
| placed_sell_sz | f64 | 本轮挂卖单数量 |

## 8. 关键参数速查表

```bash
# .env 有效参数 (所有 HL_ 前缀)
HL_PRIVATE_KEY=0x...           # EOA 私钥 (必填)
HL_ADDRESS=0x...              # 钱包地址 (必填, 与私钥匹配)
HL_COINS=FARTCOIN,PUMP        # 做市币种列表 (逗号分隔)
HL_GROWTH_MODE=true           # HIP-3 Growth Mode 开关
HL_API_URL=https://api.hyperliquid.xyz  # API 端点
HL_WS_URL=wss://api.hyperliquid.xyz    # WebSocket 端点

# 风控参数 (可选覆盖)
HL_HARD_LIMIT_RATIO=0.40      # 组合硬上限 (wd 的 40%)
HL_SKEW_POWER=3.0             # 偏斜幂 (3=三次幂)
HL_MAX_SKEW_BPS=2.0           # 最大偏斜 (0.02%)
HL_QTY_POWER=2.0              # 量控幂 (2=二次幂)
HL_SHED_TRIGGER=0.90          # IOC 强平触发水位
HL_SHED_SAFE_REENTRY=0.70     # IOC 强平安全返回水位
HL_PASSIVE_UNWIND_WATERMARK=0.25  # 被动减仓入场水位
HL_UNWIND_HYSTERESIS=0.20     # 被动减仓退出滞后

# 执行参数
HL_CYCLE_SLEEP_MIN=3.0        # 最小周期休眠 (秒)
HL_CYCLE_SLEEP_MAX=5.0        # 最大周期休眠 (秒)
HL_CYCLE_JITTER=0.30          # 抖动比例
HL_COOLDOWN_SECS=30           # COOLDOWN 等待秒数
HL_COLD_START_CYCLES=2        # 冷启动限价单周期数
```

## 9. 错误码速查

| 错误信息 | 含义 | 修复 |
|----------|------|------|
| `order rejected: Order has invalid price` | 价格不满足 tick_size 整数倍 | 确保 `round_to_tick(px)` |
| `order rejected: Would have immediately matched` | Post-Only 冲突 | tick retreat 机制自动处理 (最多 3 次) |
| `User does not exist` | 签名地址与账户不匹配 | 检查 `.env` 中 `HL_ADDRESS` |
| `Nonce must increase monotonically` | nonce 回拨 | 等待 1-2 秒或重启 (待 B6 修复) |
| `429 Too Many Requests` | CloudFront 限流 | 自动退避 + 缓存降级, 增加 cycle_sleep |
| `Exchange rejected action: failed to parse` | 签名体格式错误 | 检查 `build_exchange_body()` 字段对齐 |

## 10. 扩展新市场 (5 步配方)

1. **实现 MarketAdapter trait** (新建 `src/adapters/polymarket.rs`)
2. **添加 MarketType 变体** (如 `Prediction`) — `traits.rs`
3. **注册到 main.rs**: `pub mod adapters` + `let adapter = PolyAdapter::new(cfg)`
4. **填充 MarketCapabilities** (费率, 交易时间, 结算模式)
5. **实现 8 个 trait 方法**: fetch_state, fetch_l2, place_limit_order, place_ioc_order, place_with_tick_retreat, cancel_order, cancel_all_for_coin, tick_for, min_order_size

## 11. 已知局限

- **无 GTC last-resort**: B1 待修复, COOLDOWN→Active 循环时 IOC 全部失败则死锁
- **Python 签名延迟**: 每次 ~75ms, 已通过 3-5s 周期容忍
- **单市场覆盖**: 仅 Hyperliquid Perps, 多市场适配器接口已就绪 (traits.rs)
- **VolTracker 未接线**: 标签中 volatility 字段恒为 0.0, 待 Step 3b
- **无持久化冷却**: COOLDOWN 等待在重启时丢失 (内存级)
- **无集群支持**: 单进程, 不支持跨 VPS 协同 (future: Fleet Manager pattern)

---

## 12. 版本历史摘要

| 版本 | 日期 | 变更 |
|------|------|------|
| V3 (Heimdall) | 06-19 | Kill Switch + 防乒乓 + 波动率自适应 + 多层报价 |
| V4 | 06-19 | 通信熔断器 + 动态上限 + 清算距离 |
| V5 | 06-19 | COOLDOWN 条件回归 + 参数调优 (被 .env 覆盖) |
| V6 | 06-19 | 冻结买单 + GTC 兜底 + zero_shed_rounds |
| V6.1 | 06-19 | place_limit_order 函数签名扩展 (force_gtc) |
| V6.2 | 06-19 | GTC last-resort 验证 (仓位 538→0, wd +$7.14) |
| V6.3 | 06-19 | 代码清理, 注释补全, 模块重组 (当前版本) |

---

**文件路径**: `docs/GE_SYSTEM_FRAMEWORK.md`
**维护者**: Hermes Agent
**最后更新**: 2026-06-19 11:30 GMT+8
