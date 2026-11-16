//! growth-engine — Hyperliquid Growth Mode 做市执行引擎
//!
//! 专为 PUMP 和 FARTCOIN 的 4-6 bps 窄价差环境设计。
//! 四层风控架构：三次幂价格偏斜 + 非对称量控 + IOC 强平 + 429 限流。
//! P0: 添加 place-before-cancel + WS 毒性流实时检测 + defensive shading。
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │                    main()                           │
//! │  Config → SignalBus → [WS Task, Engine Task]        │
//! │                        ↑                            │
//! │              watch::channel(MarketShockSignal)       │
//! │              WS task writes, Engine reads            │
//! └──────────────┬──────────────────┬───────────────────┘
//!                │                  │
//!     ┌──────────▼──────────┐  ┌───▼────────────────────┐
//!     │   ws::WsClient      │  │   engine::Engine       │
//!     │   L2 book updates    │  │   State Machine        │
//!     │   User fills         │  │   Order lifecycle      │
//!     │   Shock signals →     │  │   → risk::RiskEngine   │
//!     │   → watch::Sender    │  │   → executor::Executor │
//!     └─────────────────────┘  └────────────────────────┘
//! ```

pub mod bench;
pub mod config;
pub mod engine;
pub mod executor;
pub mod labeler;
pub mod levels;
pub mod order_book;
pub mod quota;
pub mod replay;
pub mod risk;
pub mod signer;
pub mod sniper;
pub mod state;
pub mod traits;
pub mod types;
pub mod types_proto;
pub mod ws;

use crate::executor::Executor;
use crate::signer::Signer;
use anyhow::Result;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Offline replay sub-command — deterministic, no keys, no network:
    //   growth-engine replay --tape data/labels.csv --out demo/report.html
    if std::env::args().nth(1).as_deref() == Some("replay") {
        let rest: Vec<String> = std::env::args().skip(2).collect();
        return replay::run(&rest);
    }

    // V12.5 quota bench — live venue, tiny post-only orders, batched vs serial:
    //   growth-engine bench --n 5 --rounds 3 --coin ETH --notional 12
    if std::env::args().nth(1).as_deref() == Some("bench") {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| EnvFilter::new("growth_engine=warn")),
            )
            .json()
            .init();
        let rest: Vec<String> = std::env::args().skip(2).collect();
        return bench::run(&rest).await;
    }

    // Initialize structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("growth_engine=info")),
        )
        .json()
        .init();

    tracing::info!("growth-engine v{} starting (P0: place-before-cancel + toxic defense)", env!("CARGO_PKG_VERSION"));

    // Load config
    let cfg = config::Config::from_env()?;
    tracing::info!(coins=?cfg.coins, growth_mode=%cfg.growth_mode, "config loaded");

    // Build signal bus + shock receiver channel
    // SignalBus::new() returns (bus, shock_rx) — bus is shared, shock_rx goes to engine.
    let (bus, shock_rx) = types::SignalBus::new(&cfg);

    // Build market adapter (CryptoAdapter = Executor + Signer)
    let mut signer = Signer::new(cfg.private_key.clone(), cfg.address.clone());
    signer.load_asset_indices(&cfg.coins);
    let adapter = Executor::new(cfg.clone(), signer);
    // V12.6: in production the budget pre-check is on — a request that the
    // venue is going to refuse is not worth sending. `GE_QUOTA_GATE=0` turns
    // it off (used when we deliberately want to measure the venue's behaviour).
    let gate_on = std::env::var("GE_QUOTA_GATE")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true);
    adapter.set_quota_gate(gate_on);
    if let Ok(reserve) = std::env::var("GE_QUOTA_RESERVE") {
        if let Ok(n) = reserve.parse::<u64>() {
            adapter.set_quota_reserve(n);
        }
    }
    // V12.8: seed the ledger from the venue's own numbers
    // (`/info userRateLimit` → USED:CAP[:VOLUME]) instead of staying blind until
    // the first rejection. Without this the conserve mode has nothing to act on.
    if let Ok(v) = std::env::var("GE_QUOTA_SEED") {
        let parts: Vec<&str> = v.split(':').collect();
        if parts.len() >= 2 {
            if let (Ok(used), Ok(cap)) = (
                parts[0].trim().parse::<u64>(),
                parts[1].trim().parse::<u64>(),
            ) {
                let volume = parts.get(2).and_then(|s| s.trim().parse::<f64>().ok());
                adapter.seed_quota(used, cap, volume);
                tracing::info!(used, cap, volume = ?volume, "request budget seeded");
            } else {
                tracing::warn!(raw = %v, "GE_QUOTA_SEED not parseable, ignored");
            }
        }
    }
    tracing::info!(
        gate = gate_on,
        reserve = adapter.quota_gauge().line(),
        autofund = cfg.quota_autofund,
        "request-budget pre-check configured"
    );

    // Labeler: ML fuel pipeline (cycle snapshots → CSV)
    let labeler = labeler::Labeler::new(
        std::env::var("GE_LABEL_PATH")
            .unwrap_or_else(|_| "data/labels.csv".to_string())
            .into(),
    );

    // Spawn WebSocket task (receives cloned bus with shock_tx)
    let ws_bus = bus.clone();
    let ws_cfg = cfg.clone();
    let ws_handle = tokio::spawn(async move {
        if let Err(e) = ws::run(ws_cfg, ws_bus).await {
            tracing::error!(?e, "WebSocket task crashed");
        }
    });

    // Run engine (receives bus + shock_rx for real-time WS signal consumption)
    engine::run(cfg, bus, shock_rx, adapter, labeler).await?;

    let _ = ws_handle.await;
    Ok(())
}
