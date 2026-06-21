#!/usr/bin/env python3
"""hl_sign.py v2.1 — Batch-order support."""
import argparse, json, math, sys
from eth_account import Account
from hyperliquid.utils.signing import float_to_wire, sign_l1_action

__version__ = "2.1.0"
API_URL = "https://api.hyperliquid.xyz"
_asset_cache = {}

def _load_meta():
    import requests
    resp = requests.post(f"{API_URL}/info", json={"type": "meta"}, timeout=10)
    resp.raise_for_status()
    return resp.json().get("universe", [])

def _get_asset(coin, asset_idx=None):
    if asset_idx is not None:
        return asset_idx
    global _asset_cache
    if coin in _asset_cache:
        return _asset_cache[coin]["idx"]
    universe = _load_meta()
    for i, m in enumerate(universe):
        name = m.get("name", "")
        if name == coin:
            _asset_cache[coin] = {"idx": i, "szDecimals": m.get("szDecimals", 0)}
            return i
        if name and name not in _asset_cache:
            _asset_cache[name] = {"idx": i, "szDecimals": m.get("szDecimals", 0)}
    raise ValueError(f"coin '{coin}' not found in universe")

def _get_sz_decimals(coin):
    if coin not in _asset_cache:
        _get_asset(coin)
    return _asset_cache.get(coin, {}).get("szDecimals", 0)

def _wire_single_order(coin, is_buy, px, sz, reduce_only, tif="Alo", asset_idx=None):
    asset = _get_asset(coin, asset_idx)
    px = round(px, 6)
    px_str = float_to_wire(px)
    sd = _get_sz_decimals(coin)
    min_sz = (10.0 / px) * 1.005
    sz = max(sz, min_sz)
    scale = 10.0 ** sd
    sz_ceil = math.ceil(sz * scale) / scale + (1.0 / scale)
    sz_str = float_to_wire(sz_ceil)
    wire = {"a": asset, "b": is_buy, "p": px_str, "s": sz_str,
            "r": reduce_only, "t": {"limit": {"tif": tif}}}
    return wire, px_str, sz_str

def build_order_action(coin, is_buy, px, sz, reduce_only, tif="Alo", asset_idx=None):
    wire, px_str, sz_str = _wire_single_order(coin, is_buy, px, sz, reduce_only, tif, asset_idx)
    return {"type": "order", "orders": [wire], "grouping": "na"}, px_str, sz_str

def build_batch_action(order_list):
    wires, per_order = [], []
    for o in order_list:
        wire, px_str, sz_str = _wire_single_order(
            o["coin"], o["is_buy"], o["px"], o["sz"],
            o.get("reduce_only", False), o.get("tif", "Alo"))
        wires.append(wire)
        per_order.append({"px_str": px_str, "sz_str": sz_str,
                          "asset_idx": _get_asset(o["coin"]), "is_buy": o["is_buy"]})
    return {"type": "order", "orders": wires, "grouping": "na"}, per_order

def build_cancel_action(coin, oid, asset_idx=None):
    return {"type": "cancel", "cancels": [{"a": _get_asset(coin, asset_idx), "o": oid}]}

def build_cancel_by_cloid_action(coin, asset_idx=None):
    return {"type": "cancelByCloid", "asset": _get_asset(coin, asset_idx)}

def sign_action(action, private_key, nonce):
    wallet = Account.from_key(private_key)
    return sign_l1_action(wallet=wallet, action=action, active_pool=None,
                          nonce=nonce, expires_after=None, is_mainnet=True)

def main():
    parser = argparse.ArgumentParser(description=f"Hyperliquid EIP-712 signer v{__version__}")
    subs = parser.add_subparsers(dest="command", required=True)

    subs.add_parser("meta-cache", help="dump universe JSON for asset index cache")

    p = subs.add_parser("order")
    p.add_argument("--private-key", required=True)
    p.add_argument("--coin", required=True)
    p.add_argument("--is-buy", required=True, type=int, choices=[0, 1])
    p.add_argument("--px", required=True, type=float)
    p.add_argument("--sz", required=True, type=float)
    p.add_argument("--reduce-only", type=int, default=0, choices=[0, 1])
    p.add_argument("--tif", default="Alo", choices=["Alo", "Gtc", "Ioc"])
    p.add_argument("--nonce", required=True, type=int)
    p.add_argument("--asset-idx", type=int, default=None, help="preloaded asset index (skip meta API)")

    p = subs.add_parser("batch-order")
    p.add_argument("--private-key", required=True)
    p.add_argument("--orders", required=True)
    p.add_argument("--nonce", required=True, type=int)
    p.add_argument("--asset-idx", type=int, default=None, help="preloaded asset index (skip meta API)")

    p = subs.add_parser("cancel")
    p.add_argument("--private-key", required=True)
    p.add_argument("--coin", required=True)
    p.add_argument("--oid", required=True, type=int)
    p.add_argument("--nonce", required=True, type=int)
    p.add_argument("--asset-idx", type=int, default=None, help="preloaded asset index (skip meta API)")

    p = subs.add_parser("cancel-by-cloid")
    p.add_argument("--private-key", required=True)
    p.add_argument("--coin", required=True)
    p.add_argument("--nonce", required=True, type=int)
    p.add_argument("--asset-idx", type=int, default=None, help="preloaded asset index (skip meta API)")

    args = parser.parse_args()
    pk = getattr(args, "private_key", "0x")
    if not pk.startswith("0x"):
        pk = "0x" + pk
    per_order, px_str, sz_str = None, None, None
    try:
        if args.command == "meta-cache":
            import requests as _rq
            resp = _rq.post(f"{API_URL}/info", json={"type": "meta"}, timeout=15)
            resp.raise_for_status()
            data = resp.json()
            univ = data if isinstance(data, list) else data.get("universe", [])
            print(json.dumps(univ))
            sys.exit(0)
        if args.command == "order":
            action, px_str, sz_str = build_order_action(
                args.coin, bool(args.is_buy), args.px, args.sz, bool(args.reduce_only), args.tif,
                asset_idx=args.asset_idx)
        elif args.command == "batch-order":
            action, per_order = build_batch_action(json.loads(args.orders))
        elif args.command == "cancel":
            action = build_cancel_action(args.coin, args.oid, asset_idx=args.asset_idx)
        elif args.command == "cancel-by-cloid":
            action = build_cancel_by_cloid_action(args.coin, asset_idx=args.asset_idx)
    except Exception as e:
        print(json.dumps({"error": f"build: {e}"}), file=sys.stderr); sys.exit(1)
    try:
        sig = sign_action(action, pk, args.nonce)
    except Exception as e:
        print(json.dumps({"error": f"sign: {e}"}), file=sys.stderr); sys.exit(1)

    if args.command == "order":
        sig["px_str"] = px_str; sig["sz_str"] = sz_str
    if args.command in ("order", "cancel", "cancel-by-cloid"):
        if args.asset_idx is not None:
            sig["asset_idx"] = args.asset_idx
        else:
            try: sig["asset_idx"] = _get_asset(args.coin)
            except: sig["asset_idx"] = 0
    if args.command == "batch-order" and per_order:
        sig["per_order"] = per_order
    print(json.dumps(sig))

if __name__ == "__main__":
    main()
