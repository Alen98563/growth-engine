# Deployment Guide — Growth Engine

## Prerequisites

### System Requirements
- **OS**: Linux (Ubuntu 22.04+), macOS, or Windows WSL2
- **RAM**: 256 MB minimum (the engine is lightweight)
- **Disk**: 100 MB for binary + logs
- **Network**: Stable connection to `api.hyperliquid.xyz`

### Software
- **Rust**: 1.70+ (install via [rustup](https://rustup.rs))
- **Python**: 3.9+ with packages:
  ```bash
  pip install hyperliquid-python-sdk eth-account requests
  ```
- **Git** (for version control)

---

## Step 1: Environment Setup

```bash
# Install Rust (if not installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install Python dependencies
pip install hyperliquid-python-sdk eth-account requests

# Clone the project
cd /root
git clone <repo-url> growth-engine
cd growth-engine
```

---

## Step 2: Configuration

```bash
# Copy and edit environment file
cp .env.example .env
nano .env
```

### Required Variables
```bash
HL_PRIVATE_KEY=0x_your_64_char_hex_private_key
HL_ADDRESS=0x_your_wallet_address
```

### Recommended Tweaks for FARTCOIN/PUMP
```bash
HL_COINS=FARTCOIN          # Single coin for testing
HL_COINS=PUMP,FARTCOIN     # Both coins for production
HL_PASSIVE_UNWIND_WATERMARK=0.40
HL_UNWIND_HYSTERESIS=0.20
HL_KILLSWITCH_MIN_WD=80.0
HL_MIN_PROFITABLE_SPREAD_BPS=1.5
HL_BASE_ORDER_NOTIONAL=10.0
```

### Log Level
```bash
RUST_LOG=growth_engine=info   # Production
RUST_LOG=growth_engine=debug  # Debugging
RUST_LOG=growth_engine=trace  # Maximum verbosity
```

---

## Step 3: Build

```bash
# Debug build (fast compile, slower execution)
cargo build

# Release build (slow compile, optimized execution) — RECOMMENDED
cargo build --release
```

Expected output: `target/release/growth-engine` (~10 MB binary)

### Verify Build
```bash
./target/release/growth-engine --version
# or check the binary exists:
ls -lh target/release/growth-engine
```

---

## Step 4: Verify Signing

Before running the engine, verify the Python signing helper works:

```bash
python3 scripts/hl_sign.py --help
# Should output: Hyperliquid EIP-712 signer v2.1.0
```

Test signing with your key:
```bash
python3 scripts/hl_sign.py order \
  --private-key 0xYOUR_KEY \
  --coin FARTCOIN \
  --is-buy 0 \
  --px 0.13 \
  --sz 1.0 \
  --nonce 1700000000000
# Should output JSON with r, s, v, px_str, sz_str, asset_idx
```

---

## Step 5: Run

### Foreground (for testing)
```bash
source .env
./target/release/growth-engine
```

### Background with log capture
```bash
source .env
nohup ./target/release/growth-engine > /var/log/growth-engine.log 2>&1 &
echo $! > /var/run/growth-engine.pid
```

### systemd Service (Recommended for Production)
Create `/etc/systemd/system/growth-engine.service`:

```ini
[Unit]
Description=Growth Engine - Hyperliquid Market Making
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/root/growth-engine
EnvironmentFile=/root/growth-engine/.env
ExecStart=/root/growth-engine/target/release/growth-engine
Restart=always
RestartSec=5
StandardOutput=journal
StandardError=journal
SyslogIdentifier=growth-engine

[Install]
WantedBy=multi-user.target
```

Enable and start:
```bash
systemctl daemon-reload
systemctl enable growth-engine
systemctl start growth-engine
```

---

## Step 6: Monitor

### View Logs
```bash
# systemd
journalctl -u growth-engine -f

# File-based
tail -f /var/log/growth-engine.log

# Filter for important events
journalctl -u growth-engine | grep -E "KILL_SWITCH|EMERGENCY|LIQUIDATION|state transition|CIRCUIT BREAKER"
```

### Health Metrics (from structured JSON logs)
```bash
# Extract cycle metrics
journalctl -u growth-engine -o json | jq 'select(.fields.message=="cycle") | {coin: .fields.coin, state: .fields.state, pos_ratio: .fields.pos_ratio, wd: .fields.wd}'

# Watch spreads
journalctl -u growth-engine -f -o json | jq 'select(.fields.message=="cycle") | "\(.fields.coin) state=\(.fields.state) gross=\(.fields.gross_spread_bps) net=\(.fields.net_spread_bps) wd=\(.fields.wd)"'

# Alert on danger
journalctl -u growth-engine -f -o json | jq 'select(.fields.message | test("KILL_SWITCH|EMERGENCY|LIQUIDATION|CIRCUIT"))'
```

### Process Check
```bash
# Check if running
ps aux | grep growth-engine | grep -v grep

# CPU/Memory
htop -p $(pgrep growth-engine)

# PID file
cat /var/run/growth-engine.pid
```

---

## Step 7: Stop

```bash
# Graceful (systemd)
systemctl stop growth-engine

# Graceful (PID file)
kill $(cat /var/run/growth-engine.pid)

# Force
kill -9 $(pgrep growth-engine)
```

Note: The engine handles SIGTERM cleanly — cancel all orders are processed before exit.

---

## Common Issues

### 1. "HL_PRIVATE_KEY must be set"
**Cause**: `.env` file not sourced or `HL_PRIVATE_KEY` missing.
**Fix**: 
```bash
source .env
echo $HL_PRIVATE_KEY  # verify it's set
```

### 2. "hl_sign.py: command not found" or Python errors
**Cause**: Python dependencies not installed.
**Fix**:
```bash
pip install hyperliquid-python-sdk eth-account requests
python3 scripts/hl_sign.py --help  # verify
```

### 3. "invalid signature" on orders
**Cause**: Signature-payload mismatch. This should not happen if using the Python signer.
**Debug**: Check that the wire-format `px_str`/`sz_str` from the signature are EXACTLY what goes into the REST body.

### 4. Orders getting "Alo order would match" repeatedly
**Cause**: Spread too narrow, book moved, or price not tick-discretized.
**Fix**: The tick-retreat mechanism should handle this. Check `unwind_max_retreat_ticks` is >= 3.

### 5. WebSocket disconnects frequently
**Cause**: Network issues or HL API instability.
**Fix**: Auto-reconnect is built in. Check `ws_reconnect_delay` and network connectivity.

### 6. High CPU usage
**Cause**: Release build not used, or too many coins.
**Fix**: Use `cargo build --release`. The engine is designed to be lightweight (~10-50MB RAM, <5% CPU).

### 7. Zero fills on IOC shedding
**Cause**: Illiquid market with no takers at the aggressive price.
**Fix**: V6.2's GTC last-resort will eventually fill. Monitor `zero_shed_rounds` counter.

### 8. Position not reducing in PASSIVE_UNWIND
**Cause**: Spread too wide, no takers at the retreated price, or only 3-tick retreat.
**Fix**: Unwind escalates to EMERGENCY_IOC after 5 attempts. Monitor `unwind_attempts` counter.

---

## VPS Deployment (Complete Script)

```bash
#!/bin/bash
# Deploy growth-engine to VPS

set -euo pipefail

VPS_HOST="your-vps-ip"
VPS_PORT="22"
VPS_USER="root"
SSH_KEY="~/.ssh/vps_key"

# 1. Create directories
ssh -i "$SSH_KEY" -p "$VPS_PORT" "$VPS_USER@$VPS_HOST" \
  "mkdir -p /root/growth-engine/src /root/growth-engine/scripts /root/growth-engine/docs"

# 2. Upload source
scp -i "$SSH_KEY" -P "$VPS_PORT" -r src/ "$VPS_USER@$VPS_HOST:/root/growth-engine/"
scp -i "$SSH_KEY" -P "$VPS_PORT" Cargo.toml "$VPS_USER@$VPS_HOST:/root/growth-engine/"
scp -i "$SSH_KEY" -P "$VPS_PORT" scripts/hl_sign.py "$VPS_USER@$VPS_HOST:/root/growth-engine/scripts/"

# 3. Upload .env (with secrets — never commit!)
scp -i "$SSH_KEY" -P "$VPS_PORT" .env "$VPS_USER@$VPS_HOST:/root/growth-engine/"

# 4. Build on VPS
ssh -i "$SSH_KEY" -p "$VPS_PORT" "$VPS_USER@$VPS_HOST" << 'EOF'
cd /root/growth-engine
source .env
cargo build --release
EOF

# 5. Start
ssh -i "$SSH_KEY" -p "$VPS_PORT" "$VPS_USER@$VPS_HOST" << 'EOF'
cd /root/growth-engine
source .env
nohup ./target/release/growth-engine > /var/log/growth-engine.log 2>&1 &
echo $! > /var/run/growth-engine.pid
echo "Growth Engine started with PID $(cat /var/run/growth-engine.pid)"
EOF

echo "Deployment complete. Monitor with:"
echo "  ssh VPS 'tail -f /var/log/growth-engine.log'"
```

---

## Docker Deployment (Alternative)

```dockerfile
# Dockerfile
FROM rust:1.77-slim-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

FROM python:3.11-slim-bookworm
RUN pip install hyperliquid-python-sdk eth-account requests
COPY --from=builder /app/target/release/growth-engine /usr/local/bin/
COPY scripts/hl_sign.py /app/scripts/
WORKDIR /app
ENTRYPOINT ["growth-engine"]
```

```bash
docker build -t growth-engine .
docker run -d --env-file .env --name growth-engine --restart always growth-engine
docker logs -f growth-engine
```

---

## Health Check URL (Optional)

If you add a simple HTTP health endpoint to the engine, you can monitor it:

```bash
# From monitoring system
curl -s http://localhost:9090/health
# Expected: {"status":"ok","state":"NORMAL","wd":98.50}
```
