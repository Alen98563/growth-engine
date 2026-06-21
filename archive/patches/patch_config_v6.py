#!/usr/bin/env python3
"""Surgical config.rs edits only (engine.rs already done by patch_v6b.py)"""

from pathlib import Path

cfg_path = Path("/tmp/growth-engine/src/config.rs")
text = cfg_path.read_text()

lines = text.split("\n")
new_lines = []
changes = 0

for i, line in enumerate(lines):
    new_lines.append(line)
    
    # 1. After "pub max_consecutive_cooldowns: u32," insert buy_disable_ratio
    if line.strip() == "pub max_consecutive_cooldowns: u32,":
        indent = "    "
        new_lines.append(f"{indent}/// NORMAL state: freeze BUY orders when position_ratio exceeds this.")
        new_lines.append(f"{indent}pub buy_disable_ratio: f64,")
        changes += 1
        print(f"[struct] buy_disable_ratio after line {i}")
    
    # 2. After hard_limit_ratio insert last_resort_gamble_discount
    if line.strip() == "pub hard_limit_ratio: f64,":
        indent = "    "
        new_lines.append(f"{indent}/// When IOC shedding fails 3+ rounds, GTC sell at best_bid * discount.")
        new_lines.append(f"{indent}pub last_resort_gamble_discount: f64,")
        changes += 1
        print(f"[struct] last_resort_gamble_discount after line {i}")

# Write back
cfg_path.write_text("\n".join(new_lines))
print(f"[struct] {changes} fields added")

# Now add defaults
text = cfg_path.read_text()
text = text.replace(
    "max_consecutive_cooldowns: 3,",
    "max_consecutive_cooldowns: 3,\n            buy_disable_ratio: 0.20,\n            last_resort_gamble_discount: 0.95,",
)
cfg_path.write_text(text)
print("[defaults] buy_disable_ratio=0.20, last_resort_gamble_discount=0.95")

# Add env parsing
text = cfg_path.read_text()
text = text.replace(
    'if let Ok(v) = std::env::var("HL_MAX_CONSECUTIVE_COOLDOWNS") {',
    'if let Ok(v) = std::env::var("HL_BUY_DISABLE_RATIO") { cfg.buy_disable_ratio = v.parse().unwrap_or(0.20); }\n        if let Ok(v) = std::env::var("HL_LAST_RESORT_GAMBLE_DISCOUNT") { cfg.last_resort_gamble_discount = v.parse().unwrap_or(0.95); }\n        if let Ok(v) = std::env::var("HL_MAX_CONSECUTIVE_COOLDOWNS") {',
)
cfg_path.write_text(text)
print("[env] HL_BUY_DISABLE_RATIO + HL_LAST_RESORT_GAMBLE_DISCOUNT")
print("\n=== DONE ===")
