#!/usr/bin/env python3
"""Patch vol_multiplier into risk.assess() — connects VolTracker to spread widening."""
import sys

def patch(path):
    with open(path) as f: code = f.read()

    # 1. Add vol_multiplier param to assess() signature
    old_sig = """    pub fn assess(
        &self,
        book: &L2Book,
        account: &AccountState,
        coin: &str,
    ) -> Option<RiskOutput> {"""
    new_sig = """    pub fn assess(
        &self,
        book: &L2Book,
        account: &AccountState,
        coin: &str,
        vol_multiplier: f64,
    ) -> Option<RiskOutput> {"""
    code = code.replace(old_sig, new_sig)

    # 2. Apply vol_multiplier to sigma_ticks (widen all levels during high vol)
    old_sigma = """        let sigma_ticks = if net_spread_bps > 0.0 && mid > 0.0 && tick > 0.0 { ((net_spread_bps / 10000.0 * mid) / tick).max(1.0) } else { 2.0 };"""
    new_sigma = """        let raw_sigma = if net_spread_bps > 0.0 && mid > 0.0 && tick > 0.0 { ((net_spread_bps / 10000.0 * mid) / tick).max(1.0) } else { 2.0 };
        // Vol-adaptive: widen multi-level grid during high volatility
        let sigma_ticks = (raw_sigma * vol_multiplier).max(1.0);"""
    code = code.replace(old_sigma, new_sigma)

    with open(path, 'w') as f: f.write(code)
    print(f"[OK] {path} — vol_multiplier wired")

if __name__ == "__main__":
    base = sys.argv[1] if len(sys.argv) > 1 else "/tmp/growth-engine/src"
    patch(f"{base}/risk.rs")
    # Also update engine.rs: pass vol_multiplier to risk.assess()
    with open(f"{base}/engine.rs") as f: ec = f.read()
    old_call = "let risk_out = match risk.assess(&book, &account, coin) {"
    new_call = "            let vol_mult = vol_tracker.multiplier(cfg.vol_threshold_bps, cfg.vol_multiplier_cap);\n            let risk_out = match risk.assess(&book, &account, coin, vol_mult) {"
    ec = ec.replace(old_call, new_call)
    with open(f"{base}/engine.rs", 'w') as f: f.write(ec)
    print("[OK] engine.rs — passes vol_multiplier to assess()")
