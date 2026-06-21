"""
Counterfactual Labeling (CFL) — batch processor for Growth Engine labels.

Pipeline:
  data/labels.csv  ──read──>  forward-window equity Δ  ──label──>  labeled/*.csv

Labels (three-class):
  win   > +threshold   (equity gain)
  lose  < -threshold   (equity loss)
  flat  otherwise      (neutral / breakeven)

Derived features appended per row:
  ewma_spread, state_duration, delta_pos_notional, ewma_vol, rolling_win_rate

Usage:
  python scripts/label_cfl.py [--csv data/labels.csv] [--out labeled/] [--window 5] [--threshold 0.001]
"""

import argparse
import csv
import os
import sys
from collections import deque
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Optional


@dataclass
class Row:
    ts: str
    coin: str
    state: str
    pos_sz: float
    pos_notional: float
    withdrawable: float
    equity: float
    gross_spread_bps: float
    net_spread_bps: float
    skew_bps: float
    position_ratio: float
    volatility: float
    mid_px: float
    bid_sz: float
    ask_sz: float
    placed_buy_sz: float
    placed_sell_sz: float


@dataclass
class LabeledRow:
    # original fields
    ts: str
    coin: str
    state: str
    pos_sz: float
    pos_notional: float
    withdrawable: float
    equity: float
    gross_spread_bps: float
    net_spread_bps: float
    skew_bps: float
    position_ratio: float
    volatility: float
    mid_px: float
    bid_sz: float
    ask_sz: float
    placed_buy_sz: float
    placed_sell_sz: float
    # derived features
    delta_equity: float          # equity[t+W] - equity[t]
    delta_equity_pct: float      # (equity[t+W] - equity[t]) / equity[t]
    ewma_spread_5: float         # EWMA spread over trailing 5 cycles
    state_duration: int          # consecutive cycles in current state
    delta_pos_notional: float    # pos_notional change over window
    # label
    label: int                   # +1=win, 0=flat, -1=lose


def read_csv(path: str) -> list[Row]:
    rows = []
    with open(path, newline="", encoding="utf-8") as f:
        reader = csv.DictReader(f)
        for r in reader:
            try:
                rows.append(Row(
                    ts=r["ts"],
                    coin=r["coin"],
                    state=r["state"],
                    pos_sz=float(r["pos_sz"]),
                    pos_notional=float(r["pos_notional"]),
                    withdrawable=float(r["withdrawable"]),
                    equity=float(r["equity"]),
                    gross_spread_bps=float(r["gross_spread_bps"]),
                    net_spread_bps=float(r["net_spread_bps"]),
                    skew_bps=float(r["skew_bps"]),
                    position_ratio=float(r["position_ratio"]),
                    volatility=float(r["volatility"]),
                    mid_px=float(r["mid_px"]),
                    bid_sz=float(r["bid_sz"]),
                    ask_sz=float(r["ask_sz"]),
                    placed_buy_sz=float(r["placed_buy_sz"]),
                    placed_sell_sz=float(r["placed_sell_sz"]),
                ))
            except (ValueError, KeyError):
                continue
    return rows


def compute_ewma(values: list[float], alpha: float = 0.3) -> float:
    """Exponentially weighted moving average, oldest-first."""
    if not values:
        return 0.0
    result = values[0]
    for v in values[1:]:
        result = alpha * v + (1.0 - alpha) * result
    return result


def label_rows(
    rows: list[Row],
    window: int = 5,
    threshold: float = 0.001,  # 0.1% equity change
    spread_hist: int = 5,
) -> list[LabeledRow]:
    """Forward-window counterfactual labeling."""
    n = len(rows)
    labeled: list[LabeledRow] = []

    for i in range(n):
        # Forward window: equity[t+window] - equity[t]
        fwd_idx = min(i + window, n - 1)
        delta_eq = rows[fwd_idx].equity - rows[i].equity
        eq_i = rows[i].equity if rows[i].equity > 0 else 1.0
        delta_pct = delta_eq / eq_i

        # Label assignment
        if delta_pct > threshold:
            label = 1   # win
        elif delta_pct < -threshold:
            label = -1  # lose
        else:
            label = 0   # flat

        # Derived feature: EWMA spread (trailing)
        start = max(0, i - spread_hist + 1)
        trailing_spreads = [r.gross_spread_bps for r in rows[start:i + 1]]
        ewma_spread = compute_ewma(trailing_spreads)

        # Derived feature: state duration (how many consecutive cycles in current state)
        current_state = rows[i].state
        dur = 1
        j = i - 1
        while j >= 0 and rows[j].state == current_state:
            dur += 1
            j -= 1

        # Derived feature: delta_pos_notional (window forward)
        delta_pos = rows[fwd_idx].pos_notional - rows[i].pos_notional

        labeled.append(LabeledRow(
            ts=rows[i].ts,
            coin=rows[i].coin,
            state=rows[i].state,
            pos_sz=rows[i].pos_sz,
            pos_notional=rows[i].pos_notional,
            withdrawable=rows[i].withdrawable,
            equity=rows[i].equity,
            gross_spread_bps=rows[i].gross_spread_bps,
            net_spread_bps=rows[i].net_spread_bps,
            skew_bps=rows[i].skew_bps,
            position_ratio=rows[i].position_ratio,
            volatility=rows[i].volatility,
            mid_px=rows[i].mid_px,
            bid_sz=rows[i].bid_sz,
            ask_sz=rows[i].ask_sz,
            placed_buy_sz=rows[i].placed_buy_sz,
            placed_sell_sz=rows[i].placed_sell_sz,
            delta_equity=round(delta_eq, 6),
            delta_equity_pct=round(delta_pct, 8),
            ewma_spread_5=round(ewma_spread, 2),
            state_duration=dur,
            delta_pos_notional=round(delta_pos, 6),
            label=label,
        ))

    return labeled


def write_csv(labels: list[LabeledRow], out_path: str) -> None:
    os.makedirs(os.path.dirname(out_path) or ".", exist_ok=True)
    fieldnames = list(LabeledRow.__dataclass_fields__.keys())
    with open(out_path, "w", newline="", encoding="utf-8") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()
        for lr in labels:
            writer.writerow(asdict(lr))


def print_stats(labels: list[LabeledRow]) -> None:
    total = len(labels)
    wins = sum(1 for l in labels if l.label == 1)
    loses = sum(1 for l in labels if l.label == -1)
    flats = sum(1 for l in labels if l.label == 0)

    by_state: dict[str, list[int]] = {}
    for l in labels:
        by_state.setdefault(l.state, []).append(l.label)

    print(f"\n{'='*60}")
    print(f"  Counterfactual Labeling — Summary")
    print(f"{'='*60}")
    print(f"  Total rows:    {total}")
    print(f"  Win  (+1):      {wins:>5}  ({wins/total*100:5.1f}%)" if total else "")
    print(f"  Flat ( 0):      {flats:>5}  ({flats/total*100:5.1f}%)" if total else "")
    print(f"  Lose (-1):      {loses:>5}  ({loses/total*100:5.1f}%)" if total else "")
    print(f"  Win/Lose ratio: {wins/max(loses,1):.2f}")
    print(f"\n  Per-state breakdown:")
    for state, tags in sorted(by_state.items()):
        sw = sum(1 for t in tags if t == 1)
        sf = sum(1 for t in tags if t == 0)
        sl = sum(1 for t in tags if t == -1)
        print(f"    {state:<20}  W={sw:>3}  F={sf:>3}  L={sl:>3}")
    print(f"{'='*60}\n")


def main():
    parser = argparse.ArgumentParser(description="Counterfactual Labeling for Growth Engine")
    parser.add_argument("--csv", default="data/labels.csv", help="Input CSV path")
    parser.add_argument("--out", default="data/labeled.csv", help="Output labeled CSV path")
    parser.add_argument("--window", type=int, default=5, help="Forward window (cycles)")
    parser.add_argument("--threshold", type=float, default=0.001, help="Equity change threshold (0.001 = 0.1%)")
    parser.add_argument("--spread-hist", type=int, default=5, help="Trailing cycles for EWMA spread")
    args = parser.parse_args()

    if not Path(args.csv).exists():
        print(f"ERROR: Input CSV not found: {args.csv}", file=sys.stderr)
        print(f"Run the engine first to generate data/labels.csv", file=sys.stderr)
        sys.exit(1)

    print(f"Reading: {args.csv}")
    rows = read_csv(args.csv)
    print(f"  → {len(rows)} valid rows")

    if len(rows) < args.window + 1:
        print(f"WARNING: Only {len(rows)} rows — need > {args.window} for window labeling.")
        print("  Labels may be computed against last row as anchor.")

    labels = label_rows(rows, window=args.window, threshold=args.threshold, spread_hist=args.spread_hist)

    print(f"Writing: {args.out}")
    write_csv(labels, args.out)
    print(f"  → {len(labels)} labeled rows")

    print_stats(labels)


if __name__ == "__main__":
    main()
