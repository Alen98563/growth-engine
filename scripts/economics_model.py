#!/usr/bin/env python3
"""
Growth Engine — HIP-3 market-making economics model.

Deterministic, dependency-free. Every assumption is a named constant so the
model can be audited and re-run by anyone:

    python3 scripts/economics_model.py

Outputs the tables quoted in README.md sections 2.2 - 2.6.
"""

CAPITAL = 1_000_000          # USD
SPREAD_BPS = 10.0            # quoted spread captured (bps) — BELOW observed median 11.3
HALF_CAPTURE = 0.50          # passive fill sits on one side of the book
FILL_EFFICIENCY = 0.50       # survival vs adverse selection + queue loss

# Maker fee per fill in bps. Negative = rebate (you are paid to quote).
REGIMES = {
    "Tier-0 Standard":        +1.500,
    "Tier-0 + HIP-3 (2x)":    +3.000,
    "HIP-3 Growth Mode":      +0.167,
    "Growth Mode + VIP 4":     0.000,
    "Growth Mode + Rebate T3": -0.300,
}
BEST = "Growth Mode + Rebate T3"
TURNOVERS = [1, 3, 6, 12, 24]   # daily volume as multiple of capital

# Observed target universe (GE v5 velocity-funnel scanner, 2026-06-22)
UNIVERSE = [
    ("HMSTR", 53.33,    302_230),
    ("MEME",  18.07,    124_717),
    ("ACE",   12.20,  1_391_892),
    ("XAI",   11.93,    268_183),
    ("TURBO", 11.33,     71_373),
    ("W",      8.46,  2_002_854),
    ("PUMP",   6.47, 12_652_846),
    ("SAGA",   6.31,  2_352_488),
    ("DOOD",   6.24,    121_253),
    ("DYM",    5.24,    500_535),
]


def gross_capture(spread_bps: float = SPREAD_BPS) -> float:
    """Edge per fill before fees, in bps."""
    return spread_bps * HALF_CAPTURE * FILL_EFFICIENCY


def annual_pnl(turnover: float, maker_fee_bps: float, spread_bps: float = SPREAD_BPS) -> float:
    edge = gross_capture(spread_bps) - maker_fee_bps
    return CAPITAL * turnover * 365 * edge / 10_000.0


def money(v: float) -> str:
    return ("-$" if v < 0 else "$") + f"{abs(v)/1000:,.0f}k"


def section(t: str) -> None:
    print("\n" + t + "\n" + "-" * len(t))


def main() -> None:
    print("=" * 74)
    print("GROWTH ENGINE — HIP-3 MARKET-MAKING ECONOMICS")
    print(f"capital=${CAPITAL:,}  spread={SPREAD_BPS}bps  "
          f"half={HALF_CAPTURE}  fill_eff={FILL_EFFICIENCY}")
    print("=" * 74)

    # ---- Universe ----
    section("Observed target universe (10 qualifying names)")
    print(f"{'Coin':<8}{'Spread(bps)':>13}{'Daily volume':>18}")
    for coin, sp, vol in sorted(UNIVERSE, key=lambda x: -x[1]):
        print(f"{coin:<8}{sp:>13.2f}{'$'+format(vol, ','):>18}")
    adv = sum(v for _, _, v in UNIVERSE)
    vw = sum(v * s for _, s, v in UNIVERSE) / adv
    med = sorted(s for _, s, _ in UNIVERSE)[len(UNIVERSE) // 2]
    print(f"\naggregate ADV      : ${adv:,.0f}/day")
    print(f"volume-weighted    : {vw:.2f} bps")
    print(f"median spread      : {med:.2f} bps")

    # ---- Capture ----
    section("Capture model (per maker fill)")
    print(f"quoted spread          {SPREAD_BPS:>7.2f} bps")
    print(f"x half spread          {SPREAD_BPS*HALF_CAPTURE:>7.2f} bps")
    print(f"x fill efficiency      {SPREAD_BPS*HALF_CAPTURE*FILL_EFFICIENCY:>7.2f} bps  (gross capture)")
    print(f"\n{'Fee regime':<26}{'Maker':>10}{'Net edge/fill':>16}")
    for name, fee in REGIMES.items():
        print(f"{name:<26}{fee:>+10.3f}{gross_capture()-fee:>+16.3f} bps")

    # ---- Annual P&L ----
    section(f"Annual P&L on ${CAPITAL:,} — turnover x fee regime")
    hdr = f"{'Turnover':<12}" + "".join(f"{n[:14]:>16}" for n in REGIMES)
    print(hdr)
    for t in TURNOVERS:
        row = f"{str(t)+'x':<12}" + "".join(f"{money(annual_pnl(t, f)):>16}" for f in REGIMES.values())
        print(row)

    # ---- Base case ----
    section(f"Base case — {TURNOVERS[2]}x turnover, {BEST}")
    fee = REGIMES[BEST]
    edge = gross_capture() - fee
    vol = TURNOVERS[2] * CAPITAL
    daily = vol * edge / 10_000
    print(f"capital              ${CAPITAL:,.0f}")
    print(f"daily maker volume   ${vol:,.0f}")
    print(f"annual maker volume  ${vol*365:,.0f}")
    print(f"net edge / fill      {edge:.2f} bps")
    print(f"daily P&L            ${daily:,.0f}")
    print(f"monthly P&L          ${daily*30:,.0f}")
    print(f"ANNUAL P&L           ${daily*365:,.0f}")
    print(f"return on capital    {daily*365/CAPITAL*100:.1f}%")

    # ---- Sensitivity ----
    section("Sensitivity — annual P&L ($k) vs spread x turnover (best regime)")
    print(f"{'Spread':<10}" + "".join(f"{str(t)+'x':>12}" for t in TURNOVERS))
    for sp in [6, 8, 10, 12, 16, 20]:
        print(f"{str(sp)+' bps':<10}" + "".join(
            f"{annual_pnl(t, fee, sp)/1000:>11,.0f}k" for t in TURNOVERS))

    # ---- Break-even ----
    section("Break-even maker fee (zero EV)")
    print(f"{'Spread':<10}{'Max maker fee':>16}{'Tier-0 (1.5bps)':>20}")
    for sp in [6, 8, 10, 12, 16, 20]:
        be = gross_capture(sp)
        verdict = "break-even" if abs(be - 1.5) < 1e-9 else ("positive EV" if be > 1.5 else "NEGATIVE EV")
        print(f"{str(sp)+' bps':<10}{be:>14.2f} bps{verdict:>20}")

    # ---- Capacity ----
    section("Capacity reality-check")
    print(f"addressable ADV (10 names) : ${adv:,.0f}/day")
    print(f"required maker volume      : ${vol:,.0f}/day")
    print(f"implied share of universe  : {vol/adv*100:.1f}%")

    print("\n" + "=" * 74)
    print("Model ends. Adjust the constants at the top of this file to re-run.")
    print("=" * 74)


if __name__ == "__main__":
    main()
