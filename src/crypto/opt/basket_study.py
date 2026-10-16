"""Experiments for the long-term basket study, run through basket.py's engine.

1. Start-date robustness: the live policy and its benchmarks from every month
   since June 2021 to today, with the same weekly deposit.
2. Satellites chosen without hindsight: LINK/XRP/INJ were picked on
   2026-10-09 from a screen over the same history used to validate them.
   These rules choose three satellites from what was knowable at the time,
   re-chosen each January:
     none        BTC/ETH/SOL only, at the same 93% invested
     fixed       LINK/XRP/INJ throughout (the live, hindsight pick)
     volume      top 3 by trailing 180-day dollar volume
     trend       top 3 by trailing 2-year return under the trend rule (the
                 screen the live picks came from, made point-in-time)
     random      3 at random from the top 20 by volume (many draws)
   The candidate pool is every coin in basket_data.py except BTC/ETH/SOL,
   including LUNA, FTT and EOS, which a real-time rule could have picked.
3. Per-coin profiles: hold vs the trend rule, drawdowns, correlation to BTC.
4. Swap test: the live basket with INJ replaced by each candidate.

Outputs CSVs under data/basket_results/ for the report.
Run: `python -m crypto.opt.basket_study`.
"""

from __future__ import annotations

import json
import sys

import numpy as np
import pandas as pd

from crypto.opt.basket import (
    BTC_ONLY,
    DATA,
    LIVE_V3,
    V2,
    Panel,
    Policy,
    build_panel,
    max_drawdown,
    simulate,
    summarize,
)
from crypto.opt.basket_data import SYMBOLS

OUT = DATA / "basket_results"
MAJORS = ("BTCUSD", "ETHUSD", "SOLUSD")
SAT_W = 0.05
MAJOR_W = {"BTCUSD": 0.44, "ETHUSD": 0.22, "SOLUSD": 0.12}
NO_SATS = {p: w / sum(MAJOR_W.values()) * 0.93 for p, w in MAJOR_W.items()}
WINDOW_START = "2021-06-01"
END = None  # through the last closed day


def benchmarks() -> list[Policy]:
    return [
        Policy("live v3 (trend)", LIVE_V3),
        Policy("same weights, no trend", LIVE_V3, trend=False),
        Policy("BTC/ETH/SOL only (trend)", NO_SATS),
        Policy("v2 50/25/15 + 10% cash (trend)", V2),
        Policy("BTC only, no trend", BTC_ONLY, trend=False),
        Policy("BTC only (trend)", BTC_ONLY),
    ]


# ─── 1. start-date robustness ─────────────────────────────────────────────────


def start_dates(
    first: str = WINDOW_START, last: str = "2025-10-01"
) -> list[pd.Timestamp]:
    return list(pd.date_range(first, last, freq="MS", tz="UTC"))


def robustness(panel: Panel, policies: list[Policy]) -> pd.DataFrame:
    rows = []
    for s in start_dates():
        for pol in policies:
            r = simulate(panel, pol, s, END)
            m = summarize(r)
            m["start"] = s.date().isoformat()
            rows.append(m)
    return pd.DataFrame(rows)


# ─── 2. satellites chosen point-in-time ───────────────────────────────────────


def trend_nav(panel: Panel) -> np.ndarray:
    """Each coin's own trend-rule growth (100% bull / 50% bear), day by day,
    from its listing; NaN before. Used only to RANK coins, so no fees."""
    n, k = panel.close.shape
    nav = np.full((n, k), np.nan)
    for j in range(k):
        level = np.nan
        for i in range(n):
            if not panel.listed[i, j]:
                continue
            if np.isnan(level):
                level = 1.0
            elif i > 0 and panel.listed[i - 1, j]:
                ret = panel.close[i, j] / panel.close[i - 1, j] - 1
                b = panel.bull[i, j]
                expo = 1.0 if np.isnan(b) else (1.0 if b == 1.0 else 0.5)
                level *= 1 + expo * ret
            nav[i, j] = level
    return nav


class SatelliteRule:
    """Base weights per day: the majors fixed, three satellites re-chosen each
    January from what was known that day."""

    def __init__(
        self, panel: Panel, pick, name: str, majors: dict[str, float] | None = None
    ):
        self.panel = panel
        self.pick = pick  # (day index) -> list of 3 pairs
        self.name = name
        self.majors = majors or MAJOR_W
        self.cache: dict[int, dict[str, float]] = {}
        self.choices: list[tuple[str, list[str]]] = []

    def __call__(self, i: int) -> dict[str, float]:
        day = self.panel.index[i]
        key = day.year
        if key not in self.cache:
            sats = self.pick(i)
            self.choices.append((day.date().isoformat(), sats))
            w = dict(self.majors)
            for p in sats:
                w[p] = SAT_W
            # fewer than three eligible: their share stays in cash
            self.cache[key] = w
        return self.cache[key]


def eligible(panel: Panel, i: int, min_history: int, exclude=MAJORS) -> list[int]:
    out = []
    for j, p in enumerate(panel.pairs):
        if p in exclude or not panel.listed[i, j]:
            continue
        first = int(np.argmax(panel.listed[:, j]))
        if i - first >= min_history:
            out.append(j)
    return out


def by_volume(
    panel: Panel,
    n: int = 3,
    lookback: int = 180,
    min_history: int = 365,
    exclude=MAJORS,
):
    def pick(i: int) -> list[str]:
        js = eligible(panel, i, min_history, exclude)
        vol = {j: panel.volume[max(0, i - lookback) : i, j].mean() for j in js}
        ranked = sorted(js, key=lambda j: -vol[j])
        return [panel.pairs[j] for j in ranked[:n]]

    return pick


def by_trend(panel: Panel, nav: np.ndarray, n: int = 3, lookback: int = 730):
    def pick(i: int) -> list[str]:
        js = eligible(panel, i, lookback + 1)
        growth = {j: nav[i - 1, j] / nav[i - 1 - lookback, j] for j in js}
        ranked = sorted(js, key=lambda j: -growth[j])
        return [panel.pairs[j] for j in ranked[:n]]

    return pick


def by_random(
    panel: Panel,
    rng: np.random.Generator,
    pool: int = 20,
    n: int = 3,
    min_history: int = 365,
    exclude=MAJORS,
):
    top = by_volume(panel, n=pool, min_history=min_history, exclude=exclude)

    def pick(i: int) -> list[str]:
        cands = top(i)
        if len(cands) <= n:
            return cands
        return list(rng.choice(cands, size=n, replace=False))

    return pick


def satellite_rules(
    panel: Panel, draws: int = 200
) -> tuple[pd.DataFrame, pd.DataFrame, dict]:
    nav = trend_nav(panel)
    named = {
        "none (BTC/ETH/SOL at 93%)": NO_SATS,
        "fixed LINK/XRP/INJ (live)": LIVE_V3,
        "top-3 by volume": SatelliteRule(panel, by_volume(panel), "volume"),
        "top-3 by 2y trend return": SatelliteRule(panel, by_trend(panel, nav), "trend"),
    }
    rows, choices = [], {}
    starts = start_dates()
    for name, w in named.items():
        for s in starts:
            # a rule re-chooses each January; a fresh rule object per start
            # would re-pick the same coins, so one object serves every start
            r = simulate(panel, Policy(name, w), s, END)
            m = summarize(r)
            m["start"] = s.date().isoformat()
            rows.append(m)
        if isinstance(w, SatelliteRule):
            choices[name] = sorted({(d[:4], tuple(c)) for d, c in w.choices})
    full = pd.DataFrame(rows)

    rng = np.random.default_rng(20261016)
    rand_rows = []
    for d in range(draws):
        rule = SatelliteRule(panel, by_random(panel, rng), f"random {d}")
        r = simulate(panel, Policy("random 3 of top 20", rule), WINDOW_START, END)
        m = summarize(r)
        m["draw"] = d
        m["picks"] = ";".join(
            f"{y}:{'/'.join(c)}"
            for y, c in sorted({(x[:4], tuple(c)) for x, c in rule.choices})
        )
        rand_rows.append(m)
    return full, pd.DataFrame(rand_rows), choices


# ─── 3. per-coin profiles ─────────────────────────────────────────────────────


def coin_profiles(panel: Panel, since: str = WINDOW_START) -> pd.DataFrame:
    i0 = int(panel.index.searchsorted(pd.Timestamp(since, tz="UTC")))
    btc = panel.pairs.index("BTCUSD")
    btc_ret = np.diff(np.log(panel.close[i0:, btc]))
    rows = []
    for j, p in enumerate(panel.pairs):
        listed = panel.listed[:, j]
        # each coin from the later of the window start and the day its
        # regime is first known (200 closes after listing)
        reg_known = np.where(~np.isnan(panel.bull[:, j]))[0]
        if len(reg_known) == 0:
            continue
        s = max(i0, int(reg_known[0]))
        if s >= len(panel.index) - 30:
            continue
        start = panel.index[s]
        sub = Panel(
            panel.index,
            [p],
            panel.open[:, [j]],
            panel.close[:, [j]],
            panel.volume[:, [j]],
            panel.bull[:, [j]],
            panel.listed[:, [j]],
        )
        hold = simulate(
            sub,
            Policy("hold", {p: 1.0}, trend=False),
            start,
            END,
            initial=1e6,
            weekly=0,
        )
        tr = simulate(sub, Policy("trend", {p: 1.0}), start, END, initial=1e6, weekly=0)
        years = (panel.index[-1] - start).days / 365.25
        c = panel.close[s:, j]
        ret = np.diff(np.log(c))
        b = btc_ret[s - i0 :]
        m = min(len(ret), len(b))
        corr = float(np.corrcoef(ret[:m], b[:m])[0, 1]) if m > 30 else float("nan")
        beta = (
            float(np.cov(ret[:m], b[:m])[0, 1] / np.var(b[:m]))
            if m > 30
            else float("nan")
        )
        bull_share = float(np.nanmean(panel.bull[s:, j]))
        alive = bool(listed[-1])
        rows.append(
            {
                "coin": p.replace("USD", ""),
                "from": start.date().isoformat(),
                "years": round(years, 2),
                "hold_x": round(hold.value[-1] / 1e6, 2),
                "trend_x": round(tr.value[-1] / 1e6, 2),
                "hold_cagr": round((hold.value[-1] / 1e6) ** (1 / years) - 1, 3),
                "trend_cagr": round((tr.value[-1] / 1e6) ** (1 / years) - 1, 3),
                "hold_dd": round(max_drawdown(hold.value), 3),
                "trend_dd": round(max_drawdown(tr.value), 3),
                "trend_trades_per_year": round(tr.trades / years, 1),
                "corr_btc": round(corr, 2),
                "beta_btc": round(beta, 2),
                "bull_share": round(bull_share, 2),
                "still_listed": alive,
                "full_window": start.date().isoformat()
                == pd.Timestamp(since).date().isoformat(),
            }
        )
    return pd.DataFrame(rows).sort_values("trend_cagr", ascending=False)


# ─── 4. swap test: INJ replaced by each candidate ─────────────────────────────


def swaps(panel: Panel, candidates: list[str]) -> pd.DataFrame:
    rows = []
    starts = start_dates()
    base = {
        s: summarize(simulate(panel, Policy("live", LIVE_V3), s, END)) for s in starts
    }
    for c in candidates:
        w = {p: v for p, v in LIVE_V3.items() if p != "INJUSD"}
        w[c] = 0.05
        wins, gaps = 0, []
        for s in starts:
            m = summarize(simulate(panel, Policy(f"INJ->{c}", w), s, END))
            gap = m["xirr"] - base[s]["xirr"]
            gaps.append(gap)
            wins += gap > 0
        full = summarize(simulate(panel, Policy(f"INJ->{c}", w), WINDOW_START, END))
        rows.append(
            {
                "swap_in": c.replace("USD", ""),
                "full_window_xirr": full["xirr"],
                "full_window_multiple": full["multiple"],
                "full_window_nav_dd": full["nav_max_dd"],
                "beats_live_share": round(wins / len(starts), 2),
                "median_xirr_gap": round(float(np.median(gaps)), 4),
                "worst_xirr_gap": round(float(np.min(gaps)), 4),
            }
        )
    live = base[pd.Timestamp(WINDOW_START, tz="UTC")]
    rows.append(
        {
            "swap_in": "INJ (live)",
            "full_window_xirr": live["xirr"],
            "full_window_multiple": live["multiple"],
            "full_window_nav_dd": live["nav_max_dd"],
            "beats_live_share": float("nan"),
            "median_xirr_gap": 0.0,
            "worst_xirr_gap": 0.0,
        }
    )
    return pd.DataFrame(rows).sort_values("median_xirr_gap", ascending=False)


# ─── 5. a longer window: BTC/ETH core, every other coin a candidate ────────────

LONG_START = "2019-01-01"
LONG_MAJORS_ONLY = {"BTCUSD": 0.62, "ETHUSD": 0.31}
LONG_MAJORS = {"BTCUSD": 0.52, "ETHUSD": 0.26}  # + 3 x 5% satellites = 93%


def long_window(panel: Panel, draws: int = 200) -> tuple[pd.DataFrame, pd.DataFrame]:
    excl = ("BTCUSD", "ETHUSD")
    starts = list(pd.date_range(LONG_START, "2025-10-01", freq="QS", tz="UTC"))
    vol_rule = SatelliteRule(
        panel, by_volume(panel, min_history=200, exclude=excl), "volume", LONG_MAJORS
    )
    policies = [
        Policy("BTC/ETH only (trend)", LONG_MAJORS_ONLY),
        Policy("BTC/ETH + top-3 by volume (trend)", vol_rule),
        Policy("BTC only (trend)", BTC_ONLY),
        Policy("BTC/ETH only, no trend", LONG_MAJORS_ONLY, trend=False),
    ]
    rows = []
    for s in starts:
        for pol in policies:
            m = summarize(simulate(panel, pol, s, END))
            m["start"] = s.date().isoformat()
            rows.append(m)
    rng = np.random.default_rng(20190101)
    rand = []
    for d in range(draws):
        rule = SatelliteRule(
            panel,
            by_random(panel, rng, pool=10, min_history=200, exclude=excl),
            f"random {d}",
            LONG_MAJORS,
        )
        m = summarize(
            simulate(
                panel, Policy("BTC/ETH + random 3 of top 10", rule), LONG_START, END
            )
        )
        m["draw"] = d
        m["picks"] = ";".join(
            f"{y}:{'/'.join(c)}"
            for y, c in sorted({(x[:4], tuple(c)) for x, c in rule.choices})
        )
        rand.append(m)
    return pd.DataFrame(rows), pd.DataFrame(rand)


# ─── 6. sensitivity: deposit size and fees ────────────────────────────────────


def sensitivity(panel: Panel) -> pd.DataFrame:
    rows = []
    for weekly in (25.0, 50.0, 100.0, 250.0):
        for name, w, trend in (
            ("live v3 (trend)", LIVE_V3, True),
            ("BTC only (trend)", BTC_ONLY, True),
        ):
            m = summarize(
                simulate(
                    panel,
                    Policy(name, w, trend=trend),
                    WINDOW_START,
                    END,
                    weekly=weekly,
                )
            )
            m["weekly"] = weekly
            m["fee"] = "tier-1 maker"
            rows.append(m)
    for label, fee in (
        ("maker 0.25% (higher tier)", 0.0025 + 0.001),
        ("taker 0.80% (fills missed)", 0.0080 + 0.001),
    ):
        m = summarize(
            simulate(
                panel, Policy("live v3 (trend)", LIVE_V3), WINDOW_START, END, fee=fee
            )
        )
        m["weekly"] = 50.0
        m["fee"] = label
        rows.append(m)
    return pd.DataFrame(rows)


def main(steps: list[str]) -> None:
    OUT.mkdir(exist_ok=True)
    all_pairs = list(SYMBOLS)
    panel = build_panel(all_pairs)
    print(
        f"panel {panel.index[0].date()} .. {panel.index[-1].date()}, {len(all_pairs)} coins"
    )
    if "robust" in steps:
        df = robustness(panel, benchmarks())
        df.to_csv(OUT / "robustness.csv", index=False)
        print("robustness:", len(df), "rows")
    if "sats" in steps:
        full, rand, choices = satellite_rules(panel)
        full.to_csv(OUT / "satellite_rules.csv", index=False)
        rand.to_csv(OUT / "satellite_random.csv", index=False)
        (OUT / "satellite_choices.json").write_text(json.dumps(choices, indent=1))
        print("satellite rules:", len(full), "rows,", len(rand), "random draws")
    if "coins" in steps:
        df = coin_profiles(panel)
        df.to_csv(OUT / "coin_profiles.csv", index=False)
        print(df.to_string(index=False))
    if "long" in steps:
        df, rand = long_window(panel)
        df.to_csv(OUT / "long_window.csv", index=False)
        rand.to_csv(OUT / "long_random.csv", index=False)
        print("long window:", len(df), "rows,", len(rand), "random draws")
    if "sens" in steps:
        df = sensitivity(panel)
        df.to_csv(OUT / "sensitivity.csv", index=False)
        print(
            df[
                [
                    "policy",
                    "weekly",
                    "fee",
                    "xirr",
                    "multiple",
                    "nav_max_dd",
                    "trades_per_year",
                    "fees_pct_paid",
                ]
            ].to_string(index=False)
        )
    if "swaps" in steps:
        cands = [
            p
            for p in all_pairs
            if p not in LIVE_V3 and p not in ("LUNAUSD", "FTTUSD", "EOSUSD", "XMRUSD")
        ]
        # only coins with a regime by the window start can run every start date
        i0 = int(panel.index.searchsorted(pd.Timestamp(WINDOW_START, tz="UTC")))
        cands = [p for p in cands if not np.isnan(panel.bull[i0, panel.pairs.index(p)])]
        df = swaps(panel, cands)
        df.to_csv(OUT / "swaps.csv", index=False)
        print(df.to_string(index=False))


if __name__ == "__main__":
    main(sys.argv[1:] or ["robust", "sats", "coins", "swaps", "long", "sens"])
