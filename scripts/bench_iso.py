#!/usr/bin/env python3
import subprocess, sys, time, statistics
def t(cmd, n=7):
    xs=[]
    for _ in range(n):
        t0=time.perf_counter()
        subprocess.run(cmd, capture_output=True)
        xs.append(time.perf_counter()-t0)
    xs.sort()
    return xs[len(xs)//2]*1000, min(xs)*1000
print("bare interpreter          : p50=%.0f ms min=%.0f ms" % t([sys.executable,"-c","pass"]))
print("+ eth_account             : p50=%.0f ms min=%.0f ms" % t([sys.executable,"-c","import eth_account"]))
print("+ hyperliquid SDK         : p50=%.0f ms min=%.0f ms" % t([sys.executable,"-c","import hyperliquid"]))
print("+ both (as hl_sign does)  : p50=%.0f ms min=%.0f ms" % t([sys.executable,"-c","import eth_account, hyperliquid.utils.signing"]))
