#!/usr/bin/env python3
"""auto_rescan.py — hourly coin re-ranking. Writes coin_scan.json only.
pgn_engine_v2.py reads json at startup — no source file patching needed."""
import subprocess, sys, os, json, time

SRC = os.path.dirname(__file__)
SCAN = os.path.join(SRC, "scan_pgn_coins.py")
ENGINE_PY = os.path.join(SRC, "pgn_engine_v2.py")
LOG = "/tmp/pgn_engine_v2.log"
SCAN_OUT = os.path.join(SRC, "coin_scan.json")

def restart_engine():
    os.system("pkill -f pgn_engine_v2 2>/dev/null")
    time.sleep(2)
    os.system(f"nohup python3 -u {ENGINE_PY} > {LOG} 2>&1 &")
    time.sleep(1.5)
    try:
        pid = subprocess.check_output("pgrep -f pgn_engine_v2 || echo 0", shell=True).decode().strip()
    except:
        pid = "?"
    print(f"[auto_rescan] Engine restarted, PID={pid}")

def main():
    print("[auto_rescan] Running scan_pgn_coins...")
    r = subprocess.run(["python3", SCAN], capture_output=True, text=True, timeout=120)
    print(r.stdout.strip())

    with open(SCAN_OUT) as f:
        data = json.load(f)
    coins = data.get("recommended_coins", [])
    print(f"[auto_rescan] coin_scan.json updated → {coins}")

    with open(SCAN_OUT) as f:
        new_scan = json.load(f)
    new_coins = new_scan.get("recommended_coins", [])

    # Check if coins changed vs last scan state file
    state_file = os.path.join(SRC, ".auto_rescan_state")
    try:
        with open(state_file) as f:
            old_coins = json.load(f)
    except:
        old_coins = []

    if sorted(new_coins) != sorted(old_coins):
        print(f"[auto_rescan] Coins changed: {old_coins} → {new_coins}")
        with open(state_file, "w") as f:
            json.dump(new_coins, f)
        restart_engine()
        print(f"[auto_rescan] Done — engine restarted with {new_coins}")
    else:
        print(f"[auto_rescan] No change ({new_coins}). Skipping restart.")

if __name__ == "__main__":
    main()
