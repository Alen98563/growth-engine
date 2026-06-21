#!/usr/bin/env python3
"""Fix place_limit_order: add force_gtc param and replace hardcoded Alo"""

from pathlib import Path

exec_path = Path("/tmp/growth-engine/src/executor.rs")
text = exec_path.read_text()

# 1. Add force_gtc to function signature
text = text.replace(
    "    pub async fn place_limit_order(\n        &mut self,\n        coin: &str,\n        is_buy: bool,\n        sz: f64,\n        limit_px: f64,\n        reduce_only: bool,\n    )",
    "    pub async fn place_limit_order(\n        &mut self,\n        coin: &str,\n        is_buy: bool,\n        sz: f64,\n        limit_px: f64,\n        reduce_only: bool,\n        force_gtc: bool,\n    )",
)

# 2. Use force_gtc in tif
text = text.replace(
    '            "t": {"limit": {"tif": "Alo"}}',
    '            "t": {"limit": {"tif": if force_gtc { "Gtc" } else { "Alo" }}}',
)

exec_path.write_text(text)
print("[executor.rs] force_gtc param added + tif conditional")

# 3. Update engine.rs call site
eng_path = Path("/tmp/growth-engine/src/engine.rs")
eng_text = eng_path.read_text()
eng_text = eng_text.replace(
    "executor.place_limit_order(coin, false, pos.size, px, true).await",
    "executor.place_limit_order(coin, false, pos.size, px, true, true).await",
)
eng_path.write_text(eng_text)
print("[engine.rs] force_gtc=true passed to place_limit_order")
