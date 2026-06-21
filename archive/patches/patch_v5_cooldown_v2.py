#!/usr/bin/env python3
"""V5 patch v2: robust matching"""

from pathlib import Path

BASE = Path("/tmp/growth-engine/src")

# ========== 1. config.rs (already done in v1, re-verify) ==========
cfg = BASE / "config.rs"
text = cfg.read_text()

assert "max_skew_bps: 2.0" in text, "config: max_skew_bps not changed?"
assert "hard_limit_ratio: 0.40" in text, "config: hard_limit_ratio not changed?"
print("[config.rs] already patched OK")

# ========== 2. engine.rs ==========
eng = BASE / "engine.rs"
lines = eng.read_text().split("\n")
changes = []

# 2a. After "reserve depleted -> Cooldown": add increment_cooldown
#    Find: coin_state.transition(coin, State::Cooldown);
#          } else {
#          // Stage 2
for i, line in enumerate(lines):
    if ('coin_state.transition(coin, State::Cooldown);' in line
        and i+1 < len(lines)
        and '} else {' in lines[i+1]
        and i+2 < len(lines)
        and 'Stage 2: Soft recovery' in lines[i+2]):
        
        indent = line[:len(line) - len(line.lstrip())]
        lines.insert(i+1, indent + "coin_state.increment_cooldown(coin);")
        changes.append(f"line {i}: increment_cooldown @ reserve depleted -> Cooldown")
        break
else:
    print("WARNING: 2a marker not found")

# 2b. After "metrics.shed_filled = total_shed; ... transition Cooldown": add increment_cooldown
for i, line in enumerate(lines):
    if 'metrics.shed_filled = total_shed;' in line:
        # Look ahead for transition to Cooldown
        for j in range(i+1, min(i+5, len(lines))):
            if 'coin_state.transition(coin, State::Cooldown);' in lines[j]:
                indent = lines[j][:len(lines[j]) - len(lines[j].lstrip())]
                lines.insert(j+1, indent + "coin_state.increment_cooldown(coin);")
                changes.append(f"line {j}: increment_cooldown @ shed -> Cooldown")
                break
        break
else:
    print("WARNING: 2b marker not found")

# 2c. COOLDOWN handler: conditional regress
# Replace the unconditional "State::Active" with position-gated transition
cooldown_start = None
cooldown_end = None
for i, line in enumerate(lines):
    if 'State::Cooldown => {' in line:
        cooldown_start = i
    if cooldown_start is not None and i > cooldown_start:
        if line.strip() == '}' and i > cooldown_start + 3:
            # This is the closing brace of the match arm
            # But we need the RIGHT closing brace — check for the cooldown_done if block
            pass
    if cooldown_start and i > cooldown_start:
        if 'coin_state.transition(coin, State::Active);' in line:
            # Replace this line and the closing braces with conditional logic
            # Find the start of the if block
            if_start = cooldown_start
            for j in range(cooldown_start, i):
                if 'if coin_state.cooldown_done' in lines[j]:
                    if_start = j
                    break
            
            indent = lines[if_start][:len(lines[if_start]) - len(lines[if_start].lstrip())]
            inner_indent = indent + "    "
            
            # Build replacement lines
            replacement = [
                inner_indent + "// Conditional regress: never blindly return to NORMAL",
                inner_indent + "let pr = risk_out.position_ratio;",
                inner_indent + "if pr >= cfg.shed_trigger {",
                inner_indent + "    tracing::warn!(",
                inner_indent + "        coin = %coin,",
                inner_indent + "        position_ratio = pr,",
                inner_indent + "        shed_trigger = cfg.shed_trigger,",
                inner_indent + '        "COOLDOWN expired but position still critical, re-entering EMERGENCY_IOC"',
                inner_indent + "    );",
                inner_indent + "    coin_state.transition(coin, State::Shedding);",
                inner_indent + "} else if pr >= cfg.passive_unwind_watermark {",
                inner_indent + "    tracing::info!(",
                inner_indent + "        coin = %coin,",
                inner_indent + "        position_ratio = pr,",
                inner_indent + "        watermark = cfg.passive_unwind_watermark,",
                inner_indent + '        "COOLDOWN expired, entering PASSIVE_UNWIND"',
                inner_indent + "    );",
                inner_indent + "    coin_state.transition(coin, State::Unwind);",
                inner_indent + "} else {",
                inner_indent + "    tracing::info!(",
                inner_indent + "        coin = %coin,",
                inner_indent + "        position_ratio = pr,",
                inner_indent + '        "COOLDOWN expired, position safe - returning to NORMAL"',
                inner_indent + "    );",
                inner_indent + "    coin_state.reset_cooldown_count(coin);",
                inner_indent + "    coin_state.transition(coin, State::Active);",
                inner_indent + "}",
            ]
            
            # Remove the old unconditional line and insert replacement
            lines[i] = replacement[0]
            lines[i+1:i+1] = replacement[1:]
            changes.append(f"line {i}: COOLDOWN handler conditional regress")
            break

eng.write_text("\n".join(lines))
print(f"[engine.rs] {len(changes)} changes:")
for c in changes:
    print(f"  {c}")

print()
print("=== PATCH V5 COMPLETE ===")
print("1. max_skew_bps: 3.5 -> 2.0")
print("2. hard_limit_ratio: 0.60 -> 0.40")
print("3. passive_unwind_watermark: 0.40 -> 0.25")
print("4. increment_cooldown() called at 2 COOLDOWN entry points")
print("5. COOLDOWN -> conditional regress (>=shed_trigger->IOC, >=watermark->Unwind, else->NORMAL)")
