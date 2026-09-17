#!/usr/bin/env python3
"""Measure real EIP-712 signing latency for hl_sign.py (the GE signing bridge).

Times: python interpreter+import startup, key derivation, and signing,
over N iterations. Reports p50/p90/min/max.
"""
import subprocess, sys, time, statistics, shutil, os

SCRIPT = os.path.join(os.path.dirname(__file__), "hl_sign.py")
N = int(sys.argv[1]) if len(sys.argv) > 1 else 12

# check deps available
try:
    import eth_account  # noqa
    from hyperliquid.utils.signing import sign_l1_action  # noqa
    ok = True
except Exception as e:
    ok = False
    print("deps missing locally: %s" % e)

print("python: %s" % shutil.which("python"))
print("script: %s (exists=%s)" % (SCRIPT, os.path.exists(SCRIPT)))

# 1) interpreter + import startup cost (process spawn baseline)
times_import = []
for _ in range(5):
    t0 = time.perf_counter()
    subprocess.run([sys.executable, "-c", "import eth_account, hyperliquid.utils.signing"], capture_output=True)
    times_import.append(time.perf_counter() - t0)
print("\ninterpreter+imports startup: p50=%.0f ms  min=%.0f ms" % (
    statistics.median(times_import) * 1000, min(times_import) * 1000))

# 2) if deps OK, time an in-process sign_l1_action
if ok:
    from eth_account import Account
    from hyperliquid.utils.signing import float_to_wire, sign_l1_action
    acct = Account.create()
    # build a minimal L1 order action
    action = {
        "type": "order",
        "orders": [{
            "a": 200, "b": True,
            "p": float_to_wire(1.0), "s": float_to_wire(1.0),
            "r": False, "t": {"limit": {"tif": "Alo"}},
        }],
        "grouping": "na",
    }
    times_sign = []
    for i in range(N):
        nonce = int(time.time() * 1000) + i
        t0 = time.perf_counter()
        sign_l1_action(acct, action, None, nonce, None, False)
        times_sign.append(time.perf_counter() - t0)
    times_sign.sort()
    print("\nin-process sign_l1_action (N=%d):" % N)
    print("  p50 = %.2f ms" % (statistics.median(times_sign) * 1000))
    print("  p90 = %.2f ms" % (times_sign[int(N * 0.9)] * 1000))
    print("  min = %.2f ms  max = %.2f ms" % (min(times_sign) * 1000, max(times_sign) * 1000))

    # 3) full subprocess invocation (what the Rust engine actually pays)
    times_proc = []
    for i in range(5):
        nonce = int(time.time() * 1000) + i
        t0 = time.perf_counter()
        r = subprocess.run([sys.executable, SCRIPT, "--help"], capture_output=True, text=True)
        times_proc.append(time.perf_counter() - t0)
    print("\nfull subprocess spawn+import (hl_sign.py --help), N=5:")
    print("  p50 = %.0f ms" % (statistics.median(times_proc) * 1000))
    print("  this is the dominant per-call cost the engine pays")
else:
    print("\ncannot time signing: hyperliquid SDK not installed locally")
