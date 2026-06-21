import re
from pathlib import Path

BASE = Path("/tmp/growth-engine/src")

# === config.rs ===
cfg = BASE / "config.rs"
text = cfg.read_text()
text = text.replace(
    "    pub max_consecutive_cooldowns: u32,\n    pub ioc_timeout_secs: u64,",
    "    pub max_consecutive_cooldowns: u32,\n    pub buy_disable_ratio: f64,\n    pub ioc_timeout_secs: u64,",
)
text = text.replace(
    "    pub hard_limit_vol_bias: f64,\n}",
    "    pub hard_limit_vol_bias: f64,\n    pub last_resort_gamble_discount: f64,\n}",
)
text = text.replace("max_consecutive_cooldowns: 3,", "max_consecutive_cooldowns: 3,\n            buy_disable_ratio: 0.20,\n            last_resort_gamble_discount: 0.95,")
text = text.replace(
    '"HL_MAX_CONSECUTIVE_COOLDOWNS" => {',
    '"HL_BUY_DISABLE_RATIO" => { cfg.buy_disable_ratio = v.parse().unwrap_or(0.20); }\n            "HL_LAST_RESORT_GAMBLE_DISCOUNT" => { cfg.last_resort_gamble_discount = v.parse().unwrap_or(0.95); }\n            "HL_MAX_CONSECUTIVE_COOLDOWNS" => {',
)
cfg.write_text(text)
print("[config] OK")

# === engine.rs ===
eng = BASE / "engine.rs"
lines = eng.read_text().split("\n")

# 1. Add consecutive_zero_shed counter before total_shed in Shedding
for i, line in enumerate(lines):
    if line.strip() == "let mut total_shed = 0.0_f64;":
        pre = "\n".join(lines[max(0,i-10):i])
        if "State::Shedding" in pre or "shed" in pre:
            indent = line[:len(line)-len(line.lstrip())]
            lines.insert(i, indent + "let mut consecutive_zero_shed: u32 = 0;")
            print(f"[eng 1] consecutive_zero_shed @ line {i}")
            break

# 2. Track zero-fill after metrics.shed_filled
for i, line in enumerate(lines):
    if line.strip() == "metrics.shed_filled = total_shed;":
        indent = line[:len(line)-len(line.lstrip())]
        lines.insert(i+1, indent + "if total_shed == 0.0 { consecutive_zero_shed += 1; } else { consecutive_zero_shed = 0; }")
        print(f"[eng 2] zero tracking @ {i}")
        break

# 3. GTC last-resort before Shedding->Cooldown
for i, line in enumerate(lines):
    if line.strip() == "coin_state.transition(coin, State::Cooldown);":
        ctx = "\n".join(lines[max(0,i-25):i+5])
        if "metrics.shed_filled" in ctx or "shed_filled" in ctx:
            indent = line[:len(line)-len(line.lstrip())]
            inner = indent + "    "
            block = [
                indent + '// GTC last-resort when IOC dry 3+ rounds',
                indent + 'if consecutive_zero_shed >= 3 && pos.size > 0.0 {',
                inner + 'tracing::warn!(coin=%coin, consecutive=%consecutive_zero_shed, "IOC dry 3+, GTC last resort");',
                inner + 'if let Ok(fresh) = executor.fetch_book(coin).await {',
                inner + '    if let Some(bid) = fresh.best_bid().filter(|&p| p > 0.0) {',
                inner + '        let raw_px = bid * cfg.last_resort_gamble_discount;',
                inner + '        let px = (raw_px / cfg.tick_for(coin)).round() * cfg.tick_for(coin);',
                inner + '        if px > 0.0 {',
                inner + '            match executor.place_limit_order(coin, false, pos.size, px, true).await {',
                inner + '                Ok(Some(filled_oid)) => { total_shed += pos.size; metrics.shed_filled = total_shed; tracing::warn!(coin=%coin, oid=filled_oid, "GTC last resort placed"); }',
                inner + '                Ok(None) => { total_shed += pos.size; metrics.shed_filled = total_shed; tracing::warn!(coin=%coin, "GTC last resort filled instantly"); }',
                inner + '                Err(e) => tracing::error!(?e, coin=%coin, "GTC last resort failed"),',
                inner + '            }',
                inner + '        }',
                inner + '    }',
                inner + '}',
                indent + '}',
            ]
            for j, bl in enumerate(block):
                lines.insert(i+j, bl)
            print(f"[eng 3] GTC last-resort @ {i}")
            break

# 4. NORMAL buy_disable: freeze_buy when pos_ratio > 0.20
for i, line in enumerate(lines):
    if "let freeze_buy = is_unwind && pos.size > 0.0" in line:
        indent = line[:len(line)-len(line.lstrip())]
        lines.insert(i+1, indent + "    || (!is_unwind && !is_first_cycle && risk_out.position_ratio > cfg.buy_disable_ratio && coin_state.is_active(coin));")
        print(f"[eng 4] buy_disable @ {i}")
        break

eng.write_text("\n".join(lines))
print("[engine] all 4 patches OK")
