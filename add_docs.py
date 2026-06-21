#!/usr/bin/env python3
"""Add missing doc comments to growth-engine source files."""
import os, sys

os.chdir("/root/growth-engine")

fixes = {
    "src/levels.rs": [
        (1, 'use std::collections::HashMap;',
         '//! Multi-Level Quote Grid - configurable N-level order placement per side.\n//!\n//! Each level has a sigma multiplier determining its distance from mid-price.\n//! Level 0 = nearest to mid, Level N-1 = farthest. Tracks per-level state\n//! (Idle -> Active -> Filled/Cancelled) and detects drift for re-quoting.\n\nuse std::collections::HashMap;'),
        (53, '    pub fn new() -> Self {',
         '    /// Create an empty level manager.\n    pub fn new() -> Self {'),
    ],
    "src/state.rs": [
        (146, '    pub fn new() -> Self {',
         '    /// Create a new per-coin state machine in Idle state.\n    pub fn new() -> Self {'),
        (202, '    pub fn new() -> Self {',
         '    /// Create a new coin state registry (manages all coins).\n    pub fn new() -> Self {'),
        (317, '    pub fn unwind_cooldown_left(&mut self, coin: &str) -> u32 {',
         '    /// Remaining anti-ping-pong cooldown cycles for a coin.\n    pub fn unwind_cooldown_left(&mut self, coin: &str) -> u32 {'),
        (321, '    pub fn tick_unwind_cooldown(&mut self, coin: &str) {',
         '    /// Decrement the post-unwind cooldown counter by one cycle.\n    pub fn tick_unwind_cooldown(&mut self, coin: &str) {'),
        (326, '    pub fn reset_unwind_cooldown(&mut self, coin: &str, cycles: u32) {',
         '    /// Set or reset the post-unwind BUY-suppression cooldown to a fixed number of cycles.\n    pub fn reset_unwind_cooldown(&mut self, coin: &str, cycles: u32) {'),
    ],
    "src/executor.rs": [
        (36, '    pub fn new(cfg: Config, signer: Signer) -> Self {',
         '    /// Build a new Executor with config, signer, and circuit breaker.\n    pub fn new(cfg: Config, signer: Signer) -> Self {'),
    ],
    "src/labeler.rs": [
        (63, '    pub fn new(path: PathBuf) -> Self {',
         '    /// Create a labeler that appends cycle snapshots to the given CSV path.\n    pub fn new(path: PathBuf) -> Self {'),
    ],
    "src/risk.rs": [
        (57, '    pub fn new(cfg: Config) -> Self {',
         '    /// Initialize the risk engine with pricing/quantity/position parameters.\n    pub fn new(cfg: Config) -> Self {'),
    ],
    "src/signer.rs": [
        (79, '    pub fn address(&self) -> &str {',
         '    /// Return the configured wallet address.\n    pub fn address(&self) -> &str {'),
    ],
    "src/types_proto.rs": [
        (42, '    pub fn from_str(s: &str) -> Option<Self> {',
         '    /// Parse order type from exchange wire format ("Limit", "StopMarket", etc.).\n    pub fn from_str(s: &str) -> Option<Self> {'),
        (78, '    pub fn from_bool(is_buy: bool) -> Self {',
         '    /// Convert boolean buy/sell flag to Side enum.\n    pub fn from_bool(is_buy: bool) -> Self {'),
        (82, '    pub fn is_buy(&self) -> bool { matches!(self, Self::Buy) }',
         '    /// Returns true if this is a buy side order.\n    pub fn is_buy(&self) -> bool { matches!(self, Self::Buy) }'),
        (83, '    pub fn is_sell(&self) -> bool { matches!(self, Self::Sell) }',
         '    /// Returns true if this is a sell side order.\n    pub fn is_sell(&self) -> bool { matches!(self, Self::Sell) }'),
        (99, "    pub fn tif_str(&self) -> &'static str {",
         "    /// Return the Time-In-Force as exchange wire format string (Gtc, Ioc, Alo).\n    pub fn tif_str(&self) -> &'static str {"),
        (459, '    pub fn new(alpha_name: &str, inst_id: &str) -> Self {',
         '    /// Create a new order wire format with required fields.\n    pub fn new(alpha_name: &str, inst_id: &str) -> Self {'),
        (504, '    pub fn new(inst_id: &str, book: &crate::types::L2Book) -> Self {',
         '    /// Build a cancel request for all open orders on the given instrument.\n    pub fn new(inst_id: &str, book: &crate::types::L2Book) -> Self {'),
    ],
    "src/types.rs": [
        (128, '    pub fn new(coin: Asset) -> Self {',
         '    /// Create a new L2 book for the given asset.\n    pub fn new(coin: Asset) -> Self {'),
        (137, '    pub fn best_bid(&self) -> Option<f64> {',
         '    /// Return the highest bid price, or None if book is empty.\n    pub fn best_bid(&self) -> Option<f64> {'),
        (141, '    pub fn best_ask(&self) -> Option<f64> {',
         '    /// Return the lowest ask price, or None if book is empty.\n    pub fn best_ask(&self) -> Option<f64> {'),
        (145, '    pub fn mid_price(&self) -> Option<f64> {',
         '    /// Compute mid-price from best bid/ask, or None if either side is empty.\n    pub fn mid_price(&self) -> Option<f64> {'),
    ],
}

for fpath, patches in fixes.items():
    with open(fpath) as fh:
        content = fh.read()
    lines = content.split("\n")
    for lineno, old, new in reversed(patches):
        idx = lineno - 1
        if lines[idx].strip() == old.strip():
            lines[idx] = new
        else:
            # Try fuzzy match — find the old text in nearby lines
            found = False
            for delta in range(-5, 6):
                if idx + delta >= 0 and idx + delta < len(lines):
                    if old.strip() in lines[idx + delta]:
                        lines[idx + delta] = new
                        found = True
                        break
            if not found:
                print(f"MISMATCH {fpath}:{lineno}: expected {old[:60]!r}")
                print(f"  nearby: {lines[max(0,idx-2):idx+2]}")
    with open(fpath, "w") as fh:
        fh.write("\n".join(lines))
    print(f"OK: {fpath} ({len(patches)} fixes)")

print("\nAll done!")
