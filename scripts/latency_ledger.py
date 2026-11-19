#!/usr/bin/env python3
"""Append `growth-engine bench --out` JSON reports to evidence/latency_ledger.csv.

Usage:
    python3 scripts/latency_ledger.py evidence/latency_ledger.csv evidence/raw/*.json

The CSV keeps the total batch ack and the per-order amortised ack separate.
They are not interchangeable: one batch ack covers N orders.
"""

import csv
import json
import sys
from pathlib import Path

FIELDS = [
    "measured_at", "host", "git_commit", "coin", "n", "rounds",
    "serial_place_net_ms", "serial_cancel_net_ms",
    "batch_place_total_net_ms", "batch_place_per_order_net_ms",
    "batch_cancel_total_net_ms", "batch_cancel_per_order_net_ms",
    "serial_round_wall_s", "batch_round_wall_s",
    "serial_requests_per_round", "batch_requests_per_round",
    "timer_boundary", "provenance", "source_file",
]


def row_from(path: Path) -> dict:
    d = json.loads(path.read_text())
    serial = d.get("serial", {})
    batch = d.get("batch", {})
    derived = d.get("derived", {})
    measurement = d.get("measurement", {})
    return {
        "measured_at": d.get("ts", ""),
        "host": measurement.get("host", ""),
        "git_commit": measurement.get("git_commit", ""),
        "coin": d.get("coin", ""),
        "n": d.get("n", ""),
        "rounds": d.get("rounds", ""),
        "serial_place_net_ms": serial.get("place_ack_net_median_ms", ""),
        "serial_cancel_net_ms": serial.get("cancel_ack_net_median_ms", ""),
        "batch_place_total_net_ms": derived.get("batch_place_total_ack_ms", ""),
        "batch_place_per_order_net_ms": derived.get("batch_place_per_order_ack_ms", ""),
        "batch_cancel_total_net_ms": derived.get("batch_cancel_total_ack_ms", ""),
        "batch_cancel_per_order_net_ms": derived.get("batch_cancel_per_order_ack_ms", ""),
        "serial_round_wall_s": serial.get("wall_median_s", ""),
        "batch_round_wall_s": batch.get("wall_median_s", ""),
        "serial_requests_per_round": derived.get("requests_per_round_serial", ""),
        "batch_requests_per_round": derived.get("requests_per_round_batch", ""),
        "timer_boundary": measurement.get("timer", ""),
        "provenance": d.get("provenance", "growth-engine bench --out"),
        "source_file": str(path),
    }


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    out = Path(sys.argv[1])
    files = [Path(p) for p in sys.argv[2:]]
    rows = [row_from(p) for p in files]
    exists = out.exists()
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("a" if exists else "w", newline="") as fh:
        w = csv.DictWriter(fh, fieldnames=FIELDS)
        if not exists:
            w.writeheader()
        w.writerows(rows)
    print(f"appended {len(rows)} rows -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
