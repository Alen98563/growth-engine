#!/usr/bin/env python3
"""
PGN Pegging Engine v2 — Direct WhaleState Consumer
==================================================
直接读 /tmp/whale_sense_state.json，不经过单独桥接。
HMSTR 白名单已确认在 whale_sense_v2 WATCHLIST 中。
"""
import json, os, sys, time, math, signal
from datetime import datetime, timezone
import requests as rq

# ── LangFuse L1 Tracing ──
LANGFUSE_HOST = "http://100.104.34.54:3000"
LANGFUSE_PUBLIC_KEY = "pk-lf-main-hermes-dev-0000000001"
LANGFUSE_SECRET_KEY = "sk-lf-main-hermes-dev-0000000001"
langfuse = None
lf_ok = False
try:
    from langfuse import Langfuse
    langfuse = Langfuse(
        public_key=LANGFUSE_PUBLIC_KEY,
        secret_key=LANGFUSE_SECRET_KEY,
        host=LANGFUSE_HOST,
    )
    lf_ok = True
    print(f"LangFuse connected: {LANGFUSE_HOST}")
except Exception as e:
    print(f"LangFuse unavailable ({e}), tracing disabled")

SIGNAL_PATH = "/tmp/whale_sense_state.json"  # direct
FILLS_PATH = "/tmp/pgn_fills.jsonl"
STATE_PATH = "/tmp/pgn_state.json"
HL_API = "https://api.hyperliquid.xyz"

# ── HardBound (from loop_constraints.yaml) ──
MAX_TOTAL_NOTIONAL_PCT = 0.80
INDIVIDUAL_COIN_CAP = 0.20
MAX_POSITIONS = 3
MIN_ORDER_NOTIONAL = 2.0
MAX_HOLD_HOURS = 24.0

# ── SearchSpace defaults ──
WHALESCORE_THRESHOLD = 60
SIZE_PCT = 0.05
VOL_GATE_STD_MULT = 2.5
SLEEP_COOLDOWN_S = 300
MAX_FLIPS = 3
SL_ATR_MULT = 2.5
TP_ATR_MULT = 4.0
TRAILING_ATR_MULT = 2.0

# ── Dynamic coin list (loaded from coin_scan.json at startup) ──
def load_coins():
    """Load recommended coins from scan_pgn_coins.py output.
    Falls back to ['BTC','ETH','SOL'] if coin_scan.json is missing or corrupt."""
    try:
        scan_path = os.path.join(os.path.dirname(__file__), "coin_scan.json")
        with open(scan_path) as f:
            data = json.load(f)
        coins = data.get("recommended_coins", [])
        if coins and len(coins) > 0:
            print(f"  Coins loaded from coin_scan.json: {coins}")
            return coins
    except Exception as e:
        print(f"  coin_scan.json load failed ({e}), using fallback")
    fallback = ["BTC", "ETH", "SOL"]
    print(f"  Coins fallback: {fallback}")
    return fallback

COINS = load_coins()
CYCLE_INTERVAL = 30

position = {}          # coin → {size, entry_px, side, entry_ts, highest_px, lowest_px}
realized_pnl = 0.0
equity = 170.0
cycle_count = 0
in_cooldown = False
cooldown_until = 0.0
flip_count_100 = 0
flip_window_start = 0
consecutive_losses = 0
metrics_window = []     # (cycle, equity+realized) for sharpe/drawdown

def fetch_l2(coin):
    try:
        r = rq.post(f"{HL_API}/info", json={"type":"l2Book","coin":coin}, timeout=10)
        d = r.json(); lv = d.get("levels",[[],[]])
        if lv[0] and lv[1]:
            bb, ba = float(lv[0][0]["px"]), float(lv[1][0]["px"])
            mid = (bb+ba)/2; sp = (ba-bb)/mid*10000
            return mid, bb, ba, sp, float(lv[0][0]["sz"]), float(lv[1][0]["sz"])
    except: pass
    return None,None,None,None,None,None

def read_whale():
    if not os.path.exists(SIGNAL_PATH): return {}
    try:
        with open(SIGNAL_PATH) as f: d = json.load(f)
        return d.get("coins",{})
    except: return {}

def log_fill(coin, side, px, sz, slippage, mid_px, close_pnl=0.0):
    fill = {"ts":datetime.now(timezone.utc).isoformat(),"coin":coin,"side":side,
            "px":px,"sz":sz,"slippage_bps":slippage,"mid_px":mid_px,
            "realized_pnl":close_pnl}
    try:
        with open(FILLS_PATH,"a") as f: f.write(json.dumps(fill)+"\n")
    except: pass
    # LangFuse fill span (v4 API)
    if lf_ok:
        try:
            s = langfuse.start_observation(name=f"pgn-fill-{coin}", as_type="span", metadata={
                "coin":coin,"side":side,"px":px,"sz":sz,
                "slippage_bps":slippage,"mid_px":mid_px,"realized_pnl":close_pnl
            })
            s.update(output=fill)
            s.end()
        except: pass
    return fill

def trace_cycle(coin, ws_score, state, decision, z_p, z_oi, mode, mid, bb, ba, spread_bps, limit_px, slippage, vol_dev):
    """LangFuse span for each PGN decision cycle (v4 API)"""
    if not lf_ok:
        return
    try:
        s = langfuse.start_observation(name=f"pgn-cycle-{coin}", as_type="span", metadata={
            "whale_score": ws_score, "state": state, "decision": str(decision),
            "z_p": z_p, "z_oi": z_oi,
            "mode": mode, "limit_px": limit_px, "slippage_bps": slippage,
            "mid": mid, "best_bid": bb, "best_ask": ba, "spread_bps": spread_bps,
            "vol_deviation_pct": round(vol_dev, 3)
        })
        s.update(output={
            "coin":coin,"cycle":cycle_count,"mode":mode,
            "whale_score":ws_score,"state":state,"decision":str(decision),
            "limit_px":limit_px,"slippage_bps":round(slippage,2)
        })
        s.end()
    except: pass

def write_state():
    total_pnl = realized_pnl + sum(p.get("unrealized",0) for p in position.values())
    s = {"ts":datetime.now(timezone.utc).isoformat(),"cycle":cycle_count,
         "positions":{c:{"size":v["size"],"side":v["side"],"entry_px":v["entry_px"]}
                      for c,v in position.items()},
         "realized_pnl":realized_pnl,"equity":equity+total_pnl,
         "in_cooldown":in_cooldown}
    try:
        with open(STATE_PATH,"w") as f: json.dump(s,f,indent=2)
    except: pass

def main():
    global position, realized_pnl, cycle_count, in_cooldown, cooldown_until
    global consecutive_losses, flip_count_100, equity

    print("PGN Pegging Engine v2 — Direct WhaleState")
    print(f"  Coins: {COINS}  Interval: {CYCLE_INTERVAL}s  Equity: ${equity:.2f}")
    print(f"  WS Threshold: {WHALESCORE_THRESHOLD}  Size%: {SIZE_PCT*100:.0f}%")

    while True:
        cycle_count += 1
        now = time.time()

        if in_cooldown and now < cooldown_until:
            remaining = int(cooldown_until-now)
            if cycle_count % 5 == 0: print(f"  COOLDOWN {remaining}s")
            time.sleep(CYCLE_INTERVAL)
            continue
        in_cooldown = False

        whale = read_whale()
        if not whale:
            time.sleep(1); continue

        for coin in COINS:
            ws = whale.get(coin,{})
            if not ws: continue
            ws_score = ws.get("whale_score",0)
            state = ws.get("state","随机游走")
            decision = ws.get("decision","HOLD")
            z_p = ws.get("z_p",0)
            z_oi = ws.get("z_oi",0)

            # L2
            mid, bb, ba, spread_bps, bid_sz, ask_sz = fetch_l2(coin)
            if mid is None: continue

            # Vol gate
            vol_dev = abs(ws.get("price",mid)-mid)/mid*100
            if vol_dev > 5:
                print(f"  VOL GATE {coin}: {vol_dev:.1f}% → cooldown {SLEEP_COOLDOWN_S}s")
                trace_cycle(coin, ws_score, state, decision, z_p, z_oi, "SLEEP", mid, bb, ba, spread_bps, 0, 0, vol_dev)
                in_cooldown = True; cooldown_until = now+SLEEP_COOLDOWN_S; break

            # Gate logic
            if ws_score < WHALESCORE_THRESHOLD and state == "随机游走":
                if cycle_count % 10 == 0: print(f"  SLEEP {coin} ws={ws_score:.0f} {state}")
                time.sleep(CYCLE_INTERVAL if len(COINS)==1 else 0)
                continue
            elif ws_score < WHALESCORE_THRESHOLD:
                if cycle_count % 10 == 0: print(f"  HOLD {coin} ws={ws_score:.0f} {state}")
                trace_cycle(coin, ws_score, state, decision, z_p, z_oi, "HOLD", mid, bb, ba, spread_bps, 0, 0, vol_dev)
                continue

            # Determine direction
            if state in ("主力吸筹","主升拉升") and decision.startswith("EXECUTE"):
                mode = "PEG_LONG"
                side = "BUY"
            elif state in ("高位派发","动能衰变") and decision.startswith("EXECUTE"):
                mode = "PEG_SHORT"
                side = "SELL"
            elif ws_score >= 80:
                # Extreme whale_score in IDLE → lean direction from z_p
                mode = "PEG_LONG" if z_p > 0 else "PEG_SHORT"
                side = "BUY" if z_p > 0 else "SELL"
            else:
                if cycle_count % 10 == 0: print(f"  HOLD {coin} ws={ws_score:.0f} {state}/{decision.value if hasattr(decision,'value') else decision}")
                trace_cycle(coin, ws_score, state, decision, z_p, z_oi, "HOLD", mid, bb, ba, spread_bps, 0, 0, vol_dev)
                time.sleep(CYCLE_INTERVAL if len(COINS)==1 else 0)
                continue

            # Peg price
            limit_px = bb*1.0002 if side=="BUY" else ba*0.9998
            slippage = (limit_px-mid)/mid*10000

            # Trace every active cycle
            trace_cycle(coin, ws_score, state, decision, z_p, z_oi, mode, mid, bb, ba, spread_bps, limit_px, slippage, vol_dev)

            # Size
            order_notional = equity * SIZE_PCT
            order_notional = max(order_notional, MIN_ORDER_NOTIONAL)
            order_sz = order_notional / limit_px

            # Check position caps
            total_notional = sum(abs(p.get("size",0))*mid for p in position.values())
            if total_notional + order_notional > equity * MAX_TOTAL_NOTIONAL_PCT:
                print(f"  PORTFOLIO CAP: total {total_notional/equity*100:.0f}% → skip")
                time.sleep(CYCLE_INTERVAL); continue

            pos = position.get(coin)
            if pos and pos["side"] == side:
                coin_notional = abs(pos["size"]) * mid
                if coin_notional > equity * INDIVIDUAL_COIN_CAP:
                    print(f"  COIN CAP {coin}: {coin_notional/equity*100:.0f}% → skip")
                    time.sleep(CYCLE_INTERVAL); continue

            # Close opposite
            if pos and pos["side"] and pos["side"] != side and pos["size"] != 0:
                close_pnl = (limit_px-pos["entry_px"])*abs(pos["size"])
                if pos["side"]=="SELL": close_pnl = -close_pnl
                realized_pnl += close_pnl
                log_fill(coin, "CLOSE_"+pos["side"], limit_px, abs(pos["size"]), slippage, mid, close_pnl)
                print(f"  CLOSE {coin} {pos['side']} PnL=${close_pnl:+.2f}")
                position.pop(coin,None)
                pos = None

            # Enter
            if not pos:
                position[coin] = {"size":order_sz,"entry_px":limit_px,"side":side,
                                  "entry_ts":datetime.now(timezone.utc).isoformat(),
                                  "unrealized":0.0,"highest_px":limit_px,"lowest_px":limit_px}
                log_fill(coin, side, limit_px, order_sz, slippage, mid)
                total_val = equity+realized_pnl
                print(f"  [{cycle_count}] {mode} | ws={ws_score:.0f} {state} | "
                      f"{side} {order_sz:.1f}@{limit_px:.6f} slip={slippage:.1f}bps | "
                      f"eq=${total_val:.2f}",flush=True)

        write_state()
        time.sleep(CYCLE_INTERVAL)


if __name__=="__main__":
    signal.signal(signal.SIGINT, lambda s,f: sys.exit(0))
    signal.signal(signal.SIGTERM, lambda s,f: sys.exit(0))
    main()
