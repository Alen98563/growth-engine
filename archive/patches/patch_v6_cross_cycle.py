#!/usr/bin/env python3
"""Fix consecutive_zero_shed: local → persistent in CoinStateMachine"""

from pathlib import Path

# === state.rs ===
state_path = Path("/tmp/growth-engine/src/state.rs")
state_text = state_path.read_text()

# 1. Add field to PerCoinState
state_text = state_text.replace(
    "pub consecutive_cooldowns: u32,\n    /// Counter for Unwind state",
    "pub consecutive_cooldowns: u32,\n    /// How many consecutive rounds with zero IOC/limit fill\n    pub zero_shed_rounds: u32,\n    /// Counter for Unwind state",
)

# 2. Add to new() initializer
state_text = state_text.replace(
    "consecutive_cooldowns: 0,\n            unwind_attempts: 0,",
    "consecutive_cooldowns: 0,\n            zero_shed_rounds: 0,\n            unwind_attempts: 0,",
)

# 3. Add methods after increment_cooldown (before reset_cooldown_count)
state_text = state_text.replace(
    '/// Reset cooldown counter (after successful recovery or forced shed).\n    pub fn reset_cooldown_count',
    '/// Increment zero-shed counter (no fill in one EMERGENCY_IOC/Shedding round).\n    pub fn increment_zero_shed(&mut self, coin: &str) {\n        let cs = self.get_or_init(coin);\n        cs.zero_shed_rounds += 1;\n        tracing::warn!(\n            coin = %coin,\n            zero_shed_rounds = cs.zero_shed_rounds,\n            "zero-shed counter incremented"\n        );\n    }\n\n    /// Get current zero-shed round count.\n    pub fn zero_shed_count(&mut self, coin: &str) -> u32 {\n        self.get_or_init(coin).zero_shed_rounds\n    }\n\n    /// Reset zero-shed counter (after a fill or GTC last-resort).\n    pub fn reset_zero_shed(&mut self, coin: &str) {\n        let cs = self.get_or_init(coin);\n        if cs.zero_shed_rounds > 0 {\n            tracing::info!(coin = %coin, prev = cs.zero_shed_rounds, "zero-shed counter reset");\n            cs.zero_shed_rounds = 0;\n        }\n    }\n\n    /// Reset cooldown counter (after successful recovery or forced shed).\n    pub fn reset_cooldown_count',
)

state_path.write_text(state_text)
print("[state.rs] field + 3 methods added")

# === engine.rs ===
eng_path = Path("/tmp/growth-engine/src/engine.rs")
eng_text = eng_path.read_text()

# 4. Remove local `let mut consecutive_zero_shed`
eng_text = eng_text.replace(
    "let mut consecutive_zero_shed: u32 = 0;\n                    let mut total_shed = 0.0_f64;",
    "let mut total_shed = 0.0_f64;",
)

# 5. Replace local usage with coin_state method
eng_text = eng_text.replace(
    "if total_shed == 0.0 { consecutive_zero_shed += 1; } else { consecutive_zero_shed = 0; }",
    "if total_shed == 0.0 { coin_state.increment_zero_shed(coin); } else { coin_state.reset_zero_shed(coin); }",
)

# 6. Replace consecutive_zero_shed >= 3 with coin_state.zero_shed_count(coin) >= 3
eng_text = eng_text.replace(
    "if consecutive_zero_shed >= 3 && pos.size > 0.0 {",
    "if coin_state.zero_shed_count(coin) >= 3 && pos.size > 0.0 {",
)

# 7. When GTC last-resort fires, reset the counter
eng_text = eng_text.replace(
    'tracing::warn!(coin=%coin, consecutive=%consecutive_zero_shed, "IOC dry 3+, GTC last resort");',
    'let zs = coin_state.zero_shed_count(coin); tracing::warn!(coin=%coin, consecutive=%zs, "IOC dry 3+, GTC last resort");',
)

eng_path.write_text(eng_text)
print("[engine.rs] local → persistent counter, 4 edits")
