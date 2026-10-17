"""Does buying the coins that are "trending" or "looking good" pay?

basket_study.py chose satellites once a year by volume or by a 2-year screen,
and both lost to BTC/ETH/SOL alone. A trend screen is usually shorter and
faster than that, so this re-picks the three satellites EVERY MONTH, from
coins liquid at that moment (top 20 by trailing 90-day dollar volume, at
least 180 days listed, LUNA/FTT/EOS included), by:

  hot 30d         highest trailing 30-day return
  hot 90d         highest trailing 90-day return
  strong vs BTC   highest 90-day return relative to BTC, among coins whose
                  own 200-day rule says bull (fewer than three if fewer qualify)

Each runs through basket.py's engine with the live weights (BTC 44 / ETH 22 /
SOL 12 + 3 x 5%): a coin that drops out is sold, a new one is bought, and the
trend rule applies to satellites as to the rest. Compared over every monthly
start date against no satellites and the live LINK/XRP/INJ.

The universe is today's list of large coins (plus LUNA/FTT/EOS), which
flatters any rule that buys winners; watchlist.py shows what the screens see
now. Run: `python -m crypto.opt.trending`.
"""

from __future__ import annotations

import numpy as np
import pandas as pd

from crypto.opt.basket import (
    DATA,
    LIVE_V3,
    Panel,
    Policy,
    build_panel,
    simulate,
    summarize,
)
from crypto.opt.basket_data import SYMBOLS
from crypto.opt.basket_study import MAJORS, NO_SATS, start_dates

OUT = DATA / "basket_results"
MAJOR_W = {"BTCUSD": 0.44, "ETHUSD": 0.22, "SOLUSD": 0.12}
SAT_W = 0.05


def liquid(panel: Panel, i: int, pool: int = 20, min_history: int = 180) -> list[int]:
    """Coins tradeable on day i, by trailing 90-day dollar volume (to d-1)."""
    js = []
    for j, p in enumerate(panel.pairs):
        if p in MAJORS or not panel.listed[i, j]:
            continue
        first = int(np.argmax(panel.listed[:, j]))
        if i - first >= min_history:
            js.append(j)
    vol = {j: panel.volume[max(0, i - 90) : i, j].mean() for j in js}
    return sorted(js, key=lambda j: -vol[j])[:pool]


def trailing(panel: Panel, i: int, j: int, days: int) -> float:
    a, b = panel.close[i - 1 - days, j], panel.close[i - 1, j]
    return (
        float(b / a - 1) if a and not np.isnan(a) and not np.isnan(b) else float("-inf")
    )


def pick(panel: Panel, i: int, screen: str, n: int = 3) -> list[str]:
    js = liquid(panel, i)
    btc = panel.pairs.index("BTCUSD")
    if screen == "hot 30d":
        score = {j: trailing(panel, i, j, 30) for j in js}
    elif screen == "hot 90d":
        score = {j: trailing(panel, i, j, 90) for j in js}
    elif screen == "strong vs BTC":
        b90 = trailing(panel, i, btc, 90)
        score = {
            j: trailing(panel, i, j, 90) - b90 for j in js if panel.bull[i, j] == 1.0
        }
    else:
        raise ValueError(screen)
    ranked = sorted(score, key=lambda j: -score[j])
    return [panel.pairs[j] for j in ranked[:n] if np.isfinite(score[j])]


class MonthlyRule:
    """Base weights for day i: the majors, plus satellites re-picked monthly."""

    def __init__(self, panel: Panel, screen: str):
        self.panel, self.screen = panel, screen
        self.cache: dict[tuple[int, int], dict[str, float]] = {}

    def __call__(self, i: int) -> dict[str, float]:
        d = self.panel.index[i]
        key = (d.year, d.month)
        if key not in self.cache:
            w = dict(MAJOR_W)
            for p in pick(self.panel, i, self.screen):
                w[p] = SAT_W
            self.cache[key] = w
        return self.cache[key]


def backtest(panel: Panel) -> pd.DataFrame:
    rules = {
        "No satellites (BTC/ETH/SOL only)": NO_SATS,
        "LINK/XRP/INJ (live, hindsight)": LIVE_V3,
        "Hot 30 days, monthly": MonthlyRule(panel, "hot 30d"),
        "Hot 90 days, monthly": MonthlyRule(panel, "hot 90d"),
        "Strong vs BTC and in a bull trend, monthly": MonthlyRule(
            panel, "strong vs BTC"
        ),
    }
    rows = []
    for s in start_dates():
        for name, w in rules.items():
            m = summarize(simulate(panel, Policy(name, w), s))
            m["start"] = s.date().isoformat()
            rows.append(m)
    return pd.DataFrame(rows)


def main() -> None:
    OUT.mkdir(exist_ok=True)
    panel = build_panel(list(SYMBOLS))
    df = backtest(panel)
    df.to_csv(OUT / "trending_sweep.csv", index=False)
    base = df[df.policy.str.startswith("No satellites")].set_index("start")
    summary = []
    for name, g in df.groupby("policy", sort=False):
        g = g.set_index("start")
        gap = g.xirr - base.xirr
        summary.append(
            {
                "rule": name,
                "median_return": round(g.xirr.median(), 4),
                "low_return": round(g.xirr.quantile(0.1), 4),
                "median_drawdown": round(g.nav_max_dd.median(), 4),
                "worst_drawdown": round(g.nav_max_dd.min(), 4),
                "beats_no_satellites": None
                if name.startswith("No")
                else round(float((gap > 0).mean()), 3),
                "edge_vs_no_satellites": None
                if name.startswith("No")
                else round(float(gap.median()), 4),
                "trades_per_year": round(float(g.trades_per_year.median()), 0),
            }
        )
    out = pd.DataFrame(summary)
    out.to_csv(OUT / "trending_summary.csv", index=False)
    print(out.to_string(index=False))


if __name__ == "__main__":
    main()
