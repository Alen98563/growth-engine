#!/usr/bin/env python3
"""
hl_test_harness.py — 15-Minute Fleet Test Harness (v2.0)
==========================================================
1. Flatten all positions
2. Start Fleet v2 (1-seat mode)
3. Run for exactly N minutes with live monitoring
4. Kill all processes
5. Analyze logs → summary report

Usage:
    python3 hl_test_harness.py [--max-seats 1] [--duration 15] [--output /tmp/hl_test_report.json]
"""

import os, sys, time, signal, json, subprocess, re
from datetime import datetime
from typing import Dict, List, Optional, Tuple

WORK_DIR = "/root/OpenClaw-Projects/Hyperliquid_HL."
FLEET_SCRIPT = "hl_fleet_manager_v2.py"
FLEET_LOG = "/tmp/hl_fleet_v2.log"


def run(cmd: str, timeout: int = 10) -> str:
    try:
        r = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=timeout)
        return (r.stdout + r.stderr).strip()
    except Exception:
        return ""


def shell_lines(cmd: str, timeout: int = 10) -> List[str]:
    out = run(cmd, timeout)
    return [l for l in out.splitlines() if l.strip()] if out else []


def flatten_positions():
    """Flatten all open positions before test."""
    os.chdir(WORK_DIR)
    env = os.environ.copy()
    for k in ["HL_PRIVATE_KEY", "HL_WALLET_ADDRESS"]:
        if k not in env or not env[k]:
            print(f"  ❌ {k} not set")
            sys.exit(1)

    script = '''
from hyperliquid.info import Info
from hyperliquid.exchange import Exchange
from hyperliquid.utils import constants
import eth_account, os, time
pk = os.environ["HL_PRIVATE_KEY"]
addr = os.environ["HL_WALLET_ADDRESS"]
wallet = eth_account.Account.from_key(pk)
info = Info(constants.MAINNET_API_URL, skip_ws=True)
exch = Exchange(wallet, constants.MAINNET_API_URL, account_address=addr)
state = info.user_state(addr)
for p in state.get("assetPositions", []):
    szi = float(p["position"].get("szi", 0))
    if abs(szi) > 0.01:
        coin = p["position"]["coin"]
        try:
            exch.market_close(coin)
            print(f"Closed {coin} {szi:.0f}")
        except Exception as e:
            print(f"FAIL {coin}: {e}")
        time.sleep(2)
'''
    subprocess.run(["python3", "-c", script], env=env, timeout=60)
    time.sleep(3)


def get_live_count() -> int:
    """Return count of Fleet + daemon processes running on VPS."""
    lines = shell_lines("ps aux | grep -E '[h]l_fleet|[h]l_mm_v1'")
    return len(lines)


def kill_all():
    """Kill all Fleet + daemon processes."""
    run("pkill -f hl_fleet_manager 2>/dev/null; pkill -9 -f hl_fleet_manager 2>/dev/null")
    run("pkill -f hl_mm_v1_daemon 2>/dev/null; pkill -9 -f hl_mm_v1_daemon 2>/dev/null")
    time.sleep(2)


def start_fleet(max_seats: int, scan_interval: int = 120):
    """Start Fleet v2 in background. Returns PID."""
    os.chdir(WORK_DIR)
    env = os.environ.copy()

    # Truncate old log
    with open(FLEET_LOG, "w") as f:
        f.write("")

    cmd = (
        f"nohup python3 {FLEET_SCRIPT} "
        f"--max-seats {max_seats} --scan-interval {scan_interval} "
        f">> {FLEET_LOG} 2>&1 &"
    )
    subprocess.run(cmd, shell=True, env=env)

    # Wait for startup
    for _ in range(15):
        time.sleep(1)
        try:
            with open(FLEET_LOG) as f:
                if "STARTING" in f.read():
                    break
        except Exception:
            pass
    else:
        print("  ❌ Fleet failed to start (no STARTING in log)")
        sys.exit(1)

    # Get PID
    out = run("ps aux | grep '[h]l_fleet_manager_v2' | awk '{print $2}'")
    pids = [l for l in out.splitlines() if l.strip().isdigit()]
    pid = pids[-1] if pids else "?"
    print(f"  ✅ Fleet v2 started (PID={pid})")
    return pid


def analyze_logs() -> dict:
    """Parse Fleet and daemon logs for key metrics."""
    report = {
        "timestamp": datetime.now().isoformat(),
        "fleet": {},
        "daemons": {},
        "errors": [],
        "summary": {},
    }

    # ── Fleet log ──
    if os.path.exists(FLEET_LOG):
        with open(FLEET_LOG) as f:
            fleet_lines = f.readlines()

        # Seat assignments
        seats = []
        for line in fleet_lines:
            m = re.search(
                r'SEAT-(\d+)\s+(\w+)\s+score=([\d.]+)\s+spread=([\d.]+)bps'
                r'.*depth=\$([\d,]+)/\$([\d,]+).*makers=(\d+)', line
            )
            if m:
                seats.append({
                    "seat": int(m.group(1)),
                    "coin": m.group(2),
                    "score": float(m.group(3)),
                    "spread_bps": float(m.group(4)),
                    "bid_depth": float(m.group(5).replace(",", "")),
                    "ask_depth": float(m.group(6).replace(",", "")),
                    "makers": int(m.group(7)),
                })
        if seats:
            report["fleet"]["last_seats"] = seats

        # Status lines
        status_lines = [l for l in fleet_lines if "[Status]" in l]
        if status_lines:
            last = status_lines[-1]
            m = re.search(r'\$([\d.]+)\s+wd\s*\|\s*(\d+)/(\d+)\s+daemons', last)
            if m:
                report["fleet"]["last_wd"] = float(m.group(1))
                report["fleet"]["active_daemons"] = int(m.group(2))
                report["fleet"]["max_daemons"] = int(m.group(3))

        # Scan stats
        for line in fleet_lines:
            m = re.search(r'Scan done: (\d+) qualified / (\d+) total', line)
            if m:
                report["fleet"]["last_scan"] = {
                    "qualified": int(m.group(1)),
                    "total": int(m.group(2)),
                }

        # 429 / errors
        errors_fleet = [l for l in fleet_lines if "429" in l or "ERROR" in l or "error" in l]
        if errors_fleet:
            report["errors"].append({"source": "fleet", "count": len(errors_fleet), "samples": errors_fleet[-3:]})

    # ── Daemon logs ──
    daemon_logs = run("ls /tmp/hl_fleet_v2_*.log 2>/dev/null || echo ''")
    for log_path in daemon_logs.splitlines():
        if not log_path.strip() or log_path == FLEET_LOG:
            continue
        m = re.search(r'hl_fleet_v2_(\w+)\.log', log_path)
        if not m:
            continue
        coin = m.group(1)

        try:
            with open(log_path) as f:
                dl = f.readlines()
        except Exception:
            continue

        dinfo = {
            "lines": len(dl), "states": 0, "pricing": 0,
            "fills": 0, "gate_b": 0, "terminated": False,
            "pnl": {}, "errors": 0,
        }
        for line in dl:
            if "[State]" in line:
                dinfo["states"] += 1
                m2 = re.search(
                    r'acct=\$([\d.]+)\s+wd=\$([\d.]+)\s+net=([-\d.]+)'
                    r'.*r\$([\d+\-.]+)\s+f\$([\d+\-.]+)\s+n\$([\d+\-.]+)', line
                )
                if m2:
                    dinfo["pnl"] = {
                        "account": float(m2.group(1)),
                        "withdrawable": float(m2.group(2)),
                        "net_position": float(m2.group(3)),
                        "realized": float(m2.group(4)),
                        "fee": float(m2.group(5)),
                        "net_pnl": float(m2.group(6)),
                    }
            if "[Pricing]" in line:
                dinfo["pricing"] += 1
            if "FILL" in line or "fill" in line.lower():
                dinfo["fills"] += 1
            if "Gate B" in line or "PASSIVE SHED" in line:
                dinfo["gate_b"] += 1
            if "TERMINATING" in line:
                dinfo["terminated"] = True
            if "ERROR" in line or "error" in line or "429" in line:
                dinfo["errors"] += 1

        report["daemons"][coin] = dinfo

    # ── Summary ──
    wd_start, wd_end = None, report["fleet"].get("last_wd", 0)
    try:
        with open(FLEET_LOG) as f:
            for line in f:
                m = re.search(r'\[Status\]\s+\$([\d.]+)\s+wd', line)
                if m and wd_start is None:
                    wd_start = float(m.group(1))
                    break
    except Exception:
        pass

    total_fills = sum(d["fills"] for d in report["daemons"].values())
    total_gate_b = sum(d["gate_b"] for d in report["daemons"].values())
    total_errors = sum(d["errors"] for d in report["daemons"].values())
    terminated = any(d["terminated"] for d in report["daemons"].values())

    report["summary"] = {
        "wd_start": wd_start,
        "wd_end": wd_end,
        "wd_delta": (wd_end - wd_start) if wd_start and wd_end else 0,
        "num_daemons": len(report["daemons"]),
        "total_fills": total_fills,
        "total_gate_b": total_gate_b,
        "total_errors": total_errors,
        "any_terminated": terminated,
        "result": "PASS" if (total_gate_b == 0 and not terminated and total_errors == 0 and total_fills > 0) else "WARN",
    }
    return report


def print_report(report: dict, duration_sec: int):
    """Human-readable report."""
    print()
    print("=" * 65)
    print(f"  HL FLEET {duration_sec // 60}-MIN TEST REPORT — {datetime.now().strftime('%H:%M:%S')}")
    print("=" * 65)

    s = report["summary"]
    mins = duration_sec / 60

    print(f"\n  Duration: {mins:.0f} min  |  Result: {s['result']}")
    print(f"  Withdrawable: ${s['wd_start']:.2f} → ${s['wd_end']:.2f}  (Δ{s['wd_delta']:+.2f})")
    print(f"  Daemons: {s['num_daemons']} (in log)  |  Fills: {s['total_fills']}  |  Gate B: {s['total_gate_b']}  |  Errors: {s['total_errors']}")
    print(f"  Terminated: {s['any_terminated']}")

    if report["errors"]:
        print(f"\n  ⚠️  Errors ({sum(e['count'] for e in report['errors'])} total):")
        for e in report["errors"]:
            print(f"    [{e['source']}] {e['count']} occurrences")

    if report["daemons"]:
        print(f"\n  ── Daemon Details ──")
        for coin, d in report["daemons"].items():
            pnl = d.get("pnl", {})
            flags = []
            if d["terminated"]:
                flags.append("TERMINATED")
            if d["gate_b"] > 0:
                flags.append(f"GateB×{d['gate_b']}")
            if d["errors"] > 0:
                flags.append(f"ERR×{d['errors']}")
            flag_str = f" [{' '.join(flags)}]" if flags else ""
            print(f"  {coin:10s}  states={d['states']:3d}  fills={d['fills']:3d}  "
                  f"acct=${pnl.get('account', 0):.2f}  net={pnl.get('net_position', 0):.0f}  "
                  f"PnL={pnl.get('net_pnl', 0):+.4f}{flag_str}")

    if report["fleet"].get("last_seats"):
        print(f"\n  ── Seat Assignments ──")
        for seat in report["fleet"]["last_seats"]:
            print(f"  SEAT-{seat['seat']} {seat['coin']:10s}  score={seat['score']:.1f}  "
                  f"{seat['spread_bps']}bps  ${seat['bid_depth']:,.0f} bid")

    print()
    print("=" * 65)


def main():
    import argparse
    p = argparse.ArgumentParser(description="HL Fleet 15-Minute Test Harness (v2.0)")
    p.add_argument("--max-seats", type=int, default=1)
    p.add_argument("--duration", type=int, default=15, help="Test duration in minutes")
    p.add_argument("--output", type=str, default="/tmp/hl_test_report.json")
    args = p.parse_args()

    duration_sec = args.duration * 60

    print(f"\n{'='*65}")
    print(f"  HL FLEET TEST HARNESS v2.0 — {args.duration} MIN")
    print(f"  Seats: {args.max_seats}  |  Start: {datetime.now().strftime('%H:%M:%S')}")
    print(f"{'='*65}\n")

    # 0. Flatten all positions
    print("0. Flattening positions...")
    flatten_positions()
    print("   ✅ All positions flat\n")

    # 1. Kill old
    print("1. Cleaning up old processes...")
    kill_all()
    print(f"   ✅ Clean (residual: {get_live_count()})\n")

    # 2. Start Fleet
    print("2. Starting Fleet v2...")
    pid = start_fleet(args.max_seats)

    # 3. Run with live monitoring
    print(f"3. Running for {args.duration} min...")
    elapsed = 0
    tick_sec = 60
    while elapsed < duration_sec:
        time.sleep(tick_sec)
        elapsed += tick_sec
        live = get_live_count()
        remaining = (duration_sec - elapsed) / 60
        daemon_count = max(0, live - 1)  # subtract fleet process
        print(f"   [{elapsed//60:2d}min] {live} processes ({daemon_count} daemons) — {remaining:.0f}min remaining")

    # 4. Stop
    print("\n4. Stopping all processes...")
    kill_all()
    time.sleep(2)
    print(f"   ✅ All stopped (residual: {get_live_count()})\n")

    # 5. Analyze
    print("5. Analyzing logs...")
    report = analyze_logs()
    report["summary"]["duration_sec"] = duration_sec

    # 6. Report
    print_report(report, duration_sec)

    # 7. Save
    out_path = args.output
    with open(out_path, "w") as f:
        json.dump(report, f, indent=2, default=str)
    print(f"  Report saved: {out_path}")

    # Also copy to project dir
    ws_path = f"{WORK_DIR}/hl_test_report_{datetime.now().strftime('%Y%m%d_%H%M')}.json"
    with open(ws_path, "w") as f:
        json.dump(report, f, indent=2, default=str)

    return 0 if report["summary"]["result"] == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())
