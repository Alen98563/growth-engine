#!/usr/bin/env python3
"""
V6 Patch: NORMAL态内单边买断锁 + IOC→GTC终极兜底

1. config.rs: add buy_disable_ratio (0.20), last_resort_gamble_discount (0.95)
2. engine.rs: freeze_buy when pos_ratio > buy_disable_ratio in NORMAL
3. engine.rs: Shedding→GTC last-resort when IOC returns 0.0 for 3+ rounds
4. executor.rs: add place_gamble_order (GTC market-crossing sell, no Post-Only)
"""

from pathlib import Path

BASE = Path("/tmp/growth-engine/src")

# ============================================================
# 1. config.rs
# ============================================================
cfg = BASE / "config.rs"
text = cfg.read_text()

text = text.replace(
    "    pub max_consecutive_cooldowns: u32,\n    pub ioc_timeout_secs: u64,",
    "    pub max_consecutive_cooldowns: u32,\n    /// NORMAL state: freeze BUY orders when position_ratio exceeds this.\n    pub buy_disable_ratio: f64,\n    pub ioc_timeout_secs: u64,"
)
text = text.replace(
    "    /// 0.0 = fully proportional, 1.0 = full limit at any vol\n    pub hard_limit_vol_bias: f64,\n}",
    "    /// 0.0 = fully proportional, 1.0 = full limit at any vol\n    pub hard_limit_vol_bias: f64,\n    /// When IOC shedding fails 3+ rounds, GTC sell at best_bid * discount (0.95 = 5% haircut).\n    pub last_resort_gamble_discount: f64,\n}"
)
text = text.replace(
    "            max_consecutive_cooldowns: 3,",
    "            max_consecutive_cooldowns: 3,\n            buy_disable_ratio: 0.20,\n            last_resort_gamble_discount: 0.95,"
)
text = text.replace(
    '            "HL_MAX_CONSECUTIVE_COOLDOWNS" => {',
    '            "HL_BUY_DISABLE_RATIO" => { cfg.buy_disable_ratio = v.parse().unwrap_or(0.20); }\n            "HL_LAST_RESORT_GAMBLE_DISCOUNT" => { cfg.last_resort_gamble_discount = v.parse().unwrap_or(0.95); }\n            "HL_MAX_CONSECUTIVE_COOLDOWNS" => {'
)
cfg.write_text(text)
print("[config.rs] buy_disable_ratio + last_resort_gamble_discount OK")


# ============================================================
# 2. engine.rs — freeze_buy + IOC→GTC
# ============================================================
eng = BASE / "engine.rs"
lines = eng.read_text().split("\n")
changes = []

# 2a. Add consecutive_zero_shed before total_shed
for i, line in enumerate(lines):
    if 'let mut total_shed = 0.0_f64;' in line:
        pre = "\n".join(lines[max(0,i-15):i])
        if "State::Shedding" in pre or ("total_shed" in pre and "shed" in pre.lower()):
            indent = line[:len(line)-len(line.lstrip())]
            lines.insert(i, indent + "let mut consecutive_zero_shed: u32 = 0;")
            changes.append(f"line {i}: consecutive_zero_shed counter")
            break

# 2b. After metrics.shed_filled, count zeros; before Cooldown transition
for i, line in enumerate(lines):
    if 'metrics.shed_filled = total_shed;' in line:
        indent = line[:len(line)-len(line.lstrip())]
        inner = indent + "    "
        count_block = [
            indent + "if total_shed == 0.0 {",
            inner + "consecutive_zero_shed += 1;",
            indent + "} else {",
            inner + "consecutive_zero_shed = 0;",
            indent + "}",
        ]
        for j, ins in enumerate(count_block):
            lines.insert(i+1+j, ins)
        changes.append(f"line {i}: zero-fill tracking")
        break

# 2c. Before Shedding→Cooldown, add GTC last-resort
for i, line in enumerate(lines):
    if 'coin_state.transition(coin, State::Cooldown);' in line:
        # Verify this is the Shedding path (near metrics.shed_filled / increment_cooldown)
        context = "\n".join(lines[max(0,i-25):i+5])
        if ('metrics.shed_filled' in context or 'shed_filled' in context) and 'increment_cooldown(coin);' in lines[i+1] if i+1<len(lines) else False:
            indent = line[:len(line)-len(line.lstrip())]
            inner = indent + "    "
            
            # Clean GTC last-resort block
            block = [
                indent + "// ── Last Resort: GTC sell when IOC is completely dry ──",
                indent + "if consecutive_zero_shed >= 3 && pos.size > 0.0 {",
                inner + "tracing::warn!(coin=%coin, consecutive_zero=%consecutive_zero_shed, \"IOC dry, GTC last resort\");",
                inner + "if let Ok(fresh) = executor.fetch_book(coin).await {",
                inner + "    if let Some(bid) = fresh.best_bid().filter(|&p| p > 0.0) {",
                inner + "        let px = ((bid * cfg.last_resort_gamble_discount) / cfg.tick_for(coin)).round() * cfg.tick_for(coin);",
                inner + "        if px > 0.0 {",
                inner + "            match executor.place_gamble_order(coin, false, pos.size, px).await {",
                inner + "                Ok(filled) => { total_shed += filled; metrics.shed_filled = total_shed; }",
                inner + "                Err(e) => tracing::error!(?e, coin=%coin, \"GTC last resort failed\"),",
                inner + "            }",
                inner + "        }",
                inner + "    }",
                inner + "}",
                indent + "}",
            ]
            # Insert before the transition line
            for j, blk in enumerate(block):
                lines.insert(i+j, blk)
            changes.append(f"line {i}: GTC last-resort before Shedding→Cooldown")
            break

# 2d. NORMAL态 freeze_buy: pos_ratio > buy_disable_ratio
for i, line in enumerate(lines):
    if 'let freeze_buy = is_unwind && pos.size > 0.0' in line:
        indent = line[:len(line)-len(line.lstrip())]
        new_line = indent + "|| (!is_unwind && !is_first_cycle && risk_out.position_ratio > cfg.buy_disable_ratio && coin_state.is_active(coin));"
        # Insert after existing line, before next condition line
        lines.insert(i+1, new_line)
        changes.append(f"line {i}: NORMAL buy_disable_ratio ({cfg.read_text().find('buy_disable_ratio:') and '0.20'})")
        break
else:
    print("WARNING: 2d freeze_buy marker not found")

eng.write_text("\n".join(lines))
print(f"[engine.rs] {len(changes)} changes:")
for c in changes:
    print(f"  {c}")


# ============================================================
# 3. executor.rs — place_gamble_order (simple GTC, no Post-Only)
# ============================================================
exe = BASE / "executor.rs"
text = exe.read_text()

# Add gamble_order method before the closing brace of impl Executor
# Find a good insertion point: after place_ioc_order or place_batch_orders
gamble_fn = """
    /// Last-resort GTC sell: accepts taker fee, crosses market, no Post-Only.
    pub async fn place_gamble_order(
        &mut self,
        coin: &str,
        _is_buy: bool,
        sz: f64,
        px: f64,
    ) -> Result<f64, String> {
        use crate::signer::sign_order;
        let (order_payload, _) = sign_order(
            &self.signer_key,
            self.signer_address.clone(),
            coin,
            false,  // sell only
            sz,
            px,
            "Gtc",  // Not Post-Only — accepts immediate fill at taker rate
            false,  // not reduce_only
        )
        .map_err(|e| format!("sign gamble: {}", e))?;
        let body = serde_json::json!({
            "type": "order",
            "orders": [order_payload],
            "grouping": "na"
        });
        let resp = self
            .client
            .post(format!("{}/exchange", self.api_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("gamble POST: {}", e))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("gamble reject {}: {}", status, text.truncate(200)));
        }
        // Parse fill amount from response
        let resp_json: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        if let Some(statuses) = resp_json.get("response")
            .and_then(|r| r.get("data"))
            .and_then(|d| d.get("statuses"))
            .and_then(|s| s.as_array())
        {
            for st in statuses {
                if let Some(filled) = st.get("filled")
                    .and_then(|f| f.get("totalSz"))
                    .and_then(|s| s.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                {
                    tracing::info!(coin=%coin, filled=filled, "gamble GTC fill");
                    return Ok(filled);
                }
            }
        }
        Ok(0.0) // No fill info in response
    }
"""
# Insert before the last closing brace of impl Executor
# Find "}" that closes impl Executor (look for one with only whitespace before it)
impl_start = text.find("impl Executor")
# Find matching closing brace (heuristic: look for the last "}\n" before the next pub/implicit)
lines_exe = text.split("\n")
insert_at = None
in_impl = False
for i, line in enumerate(lines_exe):
    if "impl Executor" in line:
        in_impl = True
    if in_impl and line.strip() == "}" and i > 0:
        # Check if next non-empty line is not part of impl
        insert_at = i
        break

if insert_at:
    lines_exe.insert(insert_at, gamble_fn)
    exe.write_text("\n".join(lines_exe))
    print("[executor.rs] place_gamble_order added")
else:
    print("WARNING: executor.rs insertion point not found")

print()
print("=== V6 PATCH COMPLETE ===")
