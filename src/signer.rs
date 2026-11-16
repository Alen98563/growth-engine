//! EIP-712 Signer — Hyperliquid order signing via Python subprocess.
//!
//! Delegates EIP-712 signing to the Python SDK (scripts/hl_sign.py),
//! which calls Hyperliquid's `sign_l1_action` directly for byte-perfect
//! signatures. This avoids the complexity of reimplementing EIP-712
//! typed structured data hashing in Rust.
//!
//! Architecture: Rust → subprocess `python3 hl_sign.py` → stdout JSON → ECDSA sig
//!
//! The signer returns px_str, sz_str, and asset_idx alongside the signature
//! so the executor can reconstruct the exact REST body used for signing,
//! preventing format mismatch between the signed action and the wire body.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

/// Signer wraps a private key and delegates to Python for EIP-712.
pub struct Signer {
    private_key: String,
    address: String,
    script_path: PathBuf,
    /// Pre-loaded asset indices (coin → idx), populated once at startup.
    asset_indices: std::collections::HashMap<String, i64>,
}

impl Signer {
    /// Create from hex-encoded private key (0x-prefixed).
    /// The address is derived from the key by the script.
    pub fn new(private_key_hex: String, address: String) -> Self {
        // Deployable path: the binary is scp'd to a host that does not carry the
        // repo, so the signer script is resolved from GE_SIGNER_SCRIPT when set.
        let script_path = std::env::var("GE_SIGNER_SCRIPT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts")
                    .join("hl_sign.py")
            });

        tracing::info!(%address, script = %script_path.display(), "signer initialized");
        Self {
            private_key: private_key_hex,
            address,
            script_path,
            asset_indices: std::collections::HashMap::new(),
        }
    }

    /// Pre-load asset indices via a one-time meta query.
    /// Call once at startup to avoid per-order /info API calls.
    pub fn load_asset_indices(&mut self, coins: &[String]) {
        use std::process::Command;
        let script_path = self.script_path.clone();
        let output = Command::new("python3")
            .arg(&script_path)
            .arg("meta-cache")
            .output();
        match output {
            Ok(out) if out.status.success() => {
                if let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    if let Some(universe) = parsed.as_array() {
                        for (i, item) in universe.iter().enumerate() {
                            if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                                self.asset_indices.insert(name.to_string(), i as i64);
                            }
                        }
                        tracing::info!(count = self.asset_indices.len(), "asset indices cached");
                    }
                }
            }
            _ => tracing::warn!("failed to load asset indices, will use per-call meta queries"),
        }
        // Also pre-load for each configured coin (ensure they exist)
        for c in coins {
            if !self.asset_indices.contains_key(c.as_str()) {
                tracing::warn!(coin = %c, "coin not found in universe, will query per-call");
            }
        }
    }

    /// Return the configured wallet address.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Sign an L1 order placement action.
    ///
    /// Calls `python3 hl_sign.py order --private-key ... --coin ... --is-buy ...`
    /// and parses the JSON output.
    pub fn sign_l1_order(
        &self,
        coin: &str,
        is_buy: bool,
        reduce_only: bool,
        limit_px: f64,
        sz: f64,
        nonce: u64,
        tif: &str,
    ) -> Result<SignedAction> {
        let output = Command::new("python3")
            .arg(&self.script_path)
            .arg("order")
            .arg("--private-key")
            .arg(&self.private_key)
            .arg("--coin")
            .arg(coin)
            .arg("--is-buy")
            .arg(if is_buy { "1" } else { "0" })
            .arg("--px")
            .arg(format!("{}", limit_px))
            .arg("--sz")
            .arg(format!("{}", sz))
            .arg("--reduce-only")
            .arg(if reduce_only { "1" } else { "0" })
            .arg("--tif")
            .arg(tif)
            .arg("--nonce")
            .arg(nonce.to_string())
            .arg("--asset-idx")
            .arg(self.asset_index_for(coin).to_string())
            .output()
            .context("failed to run hl_sign.py for order signing")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("hl_sign.py order failed: {}", stderr);
        }

        parse_signature_output(&output.stdout, "order")
    }

    /// Sign an L1 cancel action (single order by OID).
    pub fn sign_l1_cancel(
        &self,
        coin: &str,
        oid: u64,
        nonce: u64,
    ) -> Result<SignedAction> {
        let output = Command::new("python3")
            .arg(&self.script_path)
            .arg("cancel")
            .arg("--private-key")
            .arg(&self.private_key)
            .arg("--coin")
            .arg(coin)
            .arg("--oid")
            .arg(oid.to_string())
            .arg("--nonce")
            .arg(nonce.to_string())
            .arg("--asset-idx")
            .arg(self.asset_index_for(coin).to_string())
            .output()
            .context("failed to run hl_sign.py for cancel signing")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("hl_sign.py cancel failed: {}", stderr);
        }

        parse_signature_output(&output.stdout, "cancel")
    }

    /// Sign a cancel-by-cloid action (cancel all orders for a coin).
    pub fn sign_l1_cancel_by_cloid(
        &self,
        coin: &str,
        nonce: u64,
    ) -> Result<SignedAction> {
        let output = Command::new("python3")
            .arg(&self.script_path)
            .arg("cancel-by-cloid")
            .arg("--private-key")
            .arg(&self.private_key)
            .arg("--coin")
            .arg(coin)
            .arg("--nonce")
            .arg(nonce.to_string())
            .arg("--asset-idx")
            .arg(self.asset_index_for(coin).to_string())
            .output()
            .context("failed to run hl_sign.py for cancel-by-cloid signing")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("hl_sign.py cancel-by-cloid failed: {}", stderr);
        }

        parse_signature_output(&output.stdout, "cancel_by_cloid")
    }
    /// Sign a batch of orders in ONE EIP-712 signature (one python call, one
    /// signature, one REST request).
    ///
    /// `orders_json` is the array `hl_sign.py batch-order` expects:
    /// ```json
    /// [{"coin":"ETH","is_buy":true,"px":1752.5,"sz":0.0066,"reduce_only":false,"tif":"Alo"}]
    /// ```
    pub fn sign_l1_batch_order(&self, orders_json: &str, nonce: u64) -> Result<BatchSignedAction> {
        let output = Command::new("python3")
            .arg(&self.script_path)
            .arg("batch-order")
            .arg("--private-key")
            .arg(&self.private_key)
            .arg("--orders")
            .arg(orders_json)
            .arg("--nonce")
            .arg(nonce.to_string())
            .output()
            .context("failed to run hl_sign.py batch-order")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("hl_sign.py batch-order failed: {}", stderr);
        }

        parse_batch_signature_output(&output.stdout)
    }

    /// Sign an already-constructed action verbatim (used for multi-cancel and
    /// any future action shape). Keeps hl_sign.py as the single signing authority.
    pub fn sign_l1_action(&self, action_json: &str, nonce: u64) -> Result<SignedAction> {
        let output = Command::new("python3")
            .arg(&self.script_path)
            .arg("sign-action")
            .arg("--private-key")
            .arg(&self.private_key)
            .arg("--action")
            .arg(action_json)
            .arg("--nonce")
            .arg(nonce.to_string())
            .output()
            .context("failed to run hl_sign.py sign-action")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("hl_sign.py sign-action failed: {}", stderr);
        }

        parse_signature_output(&output.stdout, "action")
    }

    /// Cached asset index for a coin (0 when unknown).
    pub fn asset_index(&self, coin: &str) -> u32 {
        self.asset_index_for(coin).max(0) as u32
    }

    /// Get cached asset index for a coin (defaults to 0 if not found).
    fn asset_index_for(&self, coin: &str) -> i64 {
        self.asset_indices.get(coin).copied().unwrap_or(0)
    }
}

/// Parse JSON output from hl_sign.py.
fn parse_signature_output(stdout: &[u8], action_type: &str) -> Result<SignedAction> {
    #[derive(Deserialize)]
    struct SigOut {
        r: String,
        s: String,
        v: u32,
        #[serde(default)]
        px_str: Option<String>,
        #[serde(default)]
        sz_str: Option<String>,
        #[serde(default)]
        asset_idx: Option<u32>,
        #[serde(default)]
        action: Option<String>,
    }

    let out: SigOut = serde_json::from_slice(stdout)
        .context("failed to parse hl_sign.py output")?;

    Ok(SignedAction {
        r: out.r,
        s: out.s,
        v: out.v,
        px_str: out.px_str,
        sz_str: out.sz_str,
        asset_idx: out.asset_idx.unwrap_or(0),
        action_json: out.action,
        action_type: action_type.to_string(),
    })
}

/// One batch signature covering N orders, plus the exact wire strings that
/// were signed for each order (so the REST body stays byte-identical).
#[derive(Debug, Clone)]
pub struct BatchSignedAction {
    pub r: String,
    pub s: String,
    pub v: u32,
    /// Canonical action JSON the signature covers; must be POSTed verbatim.
    pub action_json: Option<String>,
    pub per_order: Vec<SignedOrderWire>,
}

/// Per-order wire values as produced by `float_to_wire` inside hl_sign.py.
#[derive(Debug, Clone)]
pub struct SignedOrderWire {
    pub px_str: String,
    pub sz_str: String,
    pub asset_idx: u32,
    pub is_buy: bool,
}

fn parse_batch_signature_output(stdout: &[u8]) -> Result<BatchSignedAction> {
    #[derive(Deserialize)]
    struct WireOut {
        px_str: String,
        sz_str: String,
        #[serde(default)]
        asset_idx: u32,
        #[serde(default)]
        is_buy: bool,
    }

    #[derive(Deserialize)]
    struct BatchOut {
        r: String,
        s: String,
        v: u32,
        #[serde(default)]
        per_order: Vec<WireOut>,
        #[serde(default)]
        action: Option<String>,
    }

    let out: BatchOut = serde_json::from_slice(stdout)
        .context("failed to parse hl_sign.py batch-order output")?;

    Ok(BatchSignedAction {
        r: out.r,
        s: out.s,
        v: out.v,
        action_json: out.action,
        per_order: out
            .per_order
            .into_iter()
            .map(|w| SignedOrderWire {
                px_str: w.px_str,
                sz_str: w.sz_str,
                asset_idx: w.asset_idx,
                is_buy: w.is_buy,
            })
            .collect(),
    })
}

/// A signed action ready for the HL REST API.
#[derive(Debug, Clone)]
pub struct SignedAction {
    pub r: String,
    pub s: String,
    pub v: u32,
    /// Exact price string used in the signed action (from float_to_wire)
    pub px_str: Option<String>,
    /// Exact size string used in the signed action
    pub sz_str: Option<String>,
    /// Asset index for this coin (e.g. PUMP=200, FARTCOIN=165)
    pub asset_idx: u32,
    /// Canonical action JSON the signature covers; must be POSTed verbatim.
    pub action_json: Option<String>,
    pub action_type: String,
}

/// EIP-712 signing recipe for Hyperliquid.
///
/// The Python SDK's `sign_l1_action` handles the full chain:
///   action_dict → msgpack → keccak256 → phantom_agent → EIP-712 typed data → ECDSA
///
/// DO NOT reimplement this in Rust. Keep `scripts/hl_sign.py` as the
/// single source of truth for byte-perfect signature generation.
pub mod recipes {
    //! # EIP-712 Signing Recipe (reference only)
    //!
    //! ## Nonce
    //! The nonce is the current timestamp in **milliseconds** (matches
    //! the Hyperliquid Python SDK's `get_timestamp_ms()`).
    //!
    //! ## Domain
    //! ```python
    //! domain = {
    //!     "chainId": 1337,
    //!     "name": "Exchange",
    //!     "version": "1",
    //!     "verifyingContract": "0x0000000000000000000000000000000000000000",
    //! }
    //! ```
    //!
    //! ## Order action
    //! ```python
    //! action = {
    //!     "type": "order",
    //!     "orders": [{"a": asset_idx, "b": is_buy, "p": px_str, "s": sz_str,
    //!                  "r": reduce_only, "t": {"limit": {"tif": "Alo"}}}],
    //!     "grouping": "na",
    //! }
    //! ```
    //!
    //! ## Cancel action
    //! ```python
    //! action = {
    //!     "type": "cancel",
    //!     "cancels": [{"a": asset_idx, "o": oid}],
    //! }
    //! ```
    //!
    //! ## CancelByCloid action
    //! ```python
    //! action = {
    //!     "type": "cancelByCloid",
    //!     "asset": asset_idx,
    //! }
    //! ```
    //!
    //! ## Phantom agent (envelope)
    //! ```python
    //! phantom_agent = {"source": "a", "connectionId": hash}
    }