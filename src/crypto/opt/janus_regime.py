"""Would Janus's regime detectors have managed the basket better than the 200-day rule?

Janus's regime bridge serves `janus-regime`, which is indicators-ta's detector
stack (indicator-based ADX/Bollinger/ATR/EMA, an HMM, and their ensemble);
live it runs on minute klines. tools/regime-replay feeds the same detectors
DAILY and WEEKLY candles, the timescales the bot's 200-day rule works on, and
this runs the live policy (basket.py's engine, unchanged) with each detector
in place of the 200-day rule. Nothing is tuned: the detectors run at their
default settings, and each mapping from regime to bull/bear is fixed up front.

Mappings from a detector's label to the bot's bull/bear:
  hysteresis  change only on an explicit bull or bear call, else keep the
              last state (how the 200-day band behaves)
  strict      bull only on a bull call; anything else is bear (half target)
  defensive   the 200-day rule, but also bear whenever the detector says bear

Until a detector has warmed up (and, for hysteresis, made its first call) a
coin follows the 200-day rule, so no variant sits in cash for want of a read.
A weekly bar closes on Sunday (UTC); its label governs the following 7 days.

Run: `python -m crypto.opt.janus_regime` (needs basket_data.py's parquet
files and `cargo build --release` in tools/regime-replay).
"""

from __future__ import annotations

import io
import subprocess
from pathlib import Path

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
from crypto.opt.basket_study import start_dates

REPLAY = (
    Path(__file__).resolve().parents[3]
    / "tools"
    / "regime-replay"
    / "target"
    / "release"
    / "regime-replay"
)
OUT = DATA / "basket_results"
DETECTORS = ("ensemble", "indicator", "hmm")
MAPPINGS = ("hysteresis", "strict", "defensive")
TIMESCALES = ("daily", "weekly")
SINCE = "2021-06-01"  # the first start date: label statistics cover what was traded


def candles(pair: str, timescale: str) -> pd.DataFrame:
    df = pd.read_parquet(DATA / f"{pair}_1440m.parquet")[
        ["high", "low", "close"]
    ].astype(float)
    if timescale == "weekly":
        # Monday..Sunday bars, each stamped with its Sunday
        df = (
            df.resample("W-SUN", closed="right", label="right")
            .agg({"high": "max", "low": "min", "close": "last"})
            .dropna()
        )
    return df


def replay(pair: str, timescale: str = "daily") -> pd.DataFrame:
    """indicators-ta's regimes for `pair`, one row per bar of `timescale`."""
    df = candles(pair, timescale)
    csv = "date,high,low,close\n" + "\n".join(
        f"{d.date().isoformat()},{r.high},{r.low},{r.close}" for d, r in df.iterrows()
    )
    res = subprocess.run(
        [str(REPLAY)], input=csv, capture_output=True, text=True, check=True
    )
    out = pd.read_csv(io.StringIO(res.stdout))
    out.index = pd.to_datetime(out["date"]).dt.tz_localize("UTC")
    return out.drop(columns="date")


def on_days(labels: pd.DataFrame, days: pd.DatetimeIndex) -> pd.DataFrame:
    """Bar labels as known after each day's close (a week's from its Sunday on).

    A still-forming last week is stamped with a future Sunday, so no day sees it."""
    return labels.reindex(days, method="ffill")


def to_state(labels: pd.Series, mapping: str, sma_state: pd.Series) -> pd.Series:
    """A detector's labels as the bot's 1.0 (bull) / 0.0 (bear) / NaN (unknown).

    Until the detector has warmed up, and for hysteresis until its first bull
    or bear call, the coin follows the 200-day rule (`sma_state`)."""
    base = sma_state.reindex(labels.index)
    out = pd.Series(np.nan, index=labels.index)
    state = np.nan
    for d, lab in labels.items():
        b = base[d]
        if pd.isna(lab) or lab == "warmup":
            out[d] = b
            continue
        if mapping == "hysteresis":
            if lab == "bull":
                state = 1.0
            elif lab == "bear":
                state = 0.0
            out[d] = b if np.isnan(state) else state
        elif mapping == "strict":
            out[d] = 1.0 if lab == "bull" else 0.0
        elif mapping == "defensive":
            out[d] = b if np.isnan(b) else (0.0 if lab == "bear" else b)
        else:
            raise ValueError(mapping)
    return out


def panel_with(panel: Panel, states: dict[str, pd.Series]) -> Panel:
    """`panel` with its regimes replaced; decided for day d from data to d-1."""
    bull = np.full(panel.bull.shape, np.nan)
    for j, p in enumerate(panel.pairs):
        s = states[p].reindex(panel.index)
        # mask days the coin wasn't listed, then shift one day (no lookahead)
        vals = np.where(panel.listed[:, j], s.to_numpy(), np.nan)
        bull[1:, j] = vals[:-1]
    return Panel(
        panel.index,
        panel.pairs,
        panel.open,
        panel.close,
        panel.volume,
        bull,
        panel.listed,
    )


def sma_states(panel: Panel) -> dict[str, pd.Series]:
    """The 200-day rule's state AFTER each day's close (undoing the panel's shift)."""
    out = {}
    for j, p in enumerate(panel.pairs):
        after = np.full(len(panel.index), np.nan)
        after[:-1] = panel.bull[1:, j]
        out[p] = pd.Series(after, index=panel.index)
    return out


def label_shares(regimes: dict[tuple[str, str], pd.DataFrame]) -> pd.DataFrame:
    """How each detector spends its time, per coin and timescale, since SINCE."""
    rows = []
    for (p, ts), r in regimes.items():
        w = r.loc[SINCE:]
        years = (w.index[-1] - w.index[0]).days / 365.25
        for det in DETECTORS:
            row = {"coin": p.replace("USD", ""), "timescale": ts, "detector": det}
            row.update(
                {
                    k: round(float(v), 3)
                    for k, v in w[det].value_counts(normalize=True).items()
                }
            )
            changes = (w[det] != w[det].shift()).iloc[1:].sum()
            row["label_changes_per_year"] = round(changes / years, 1)
            rows.append(row)
    return pd.DataFrame(rows).fillna(0)


def main() -> None:
    OUT.mkdir(exist_ok=True)
    pairs = sorted(LIVE_V3)
    base = build_panel(pairs)
    smas = sma_states(base)
    regimes = {(p, ts): replay(p, ts) for p in pairs for ts in TIMESCALES}
    label_shares(regimes).to_csv(OUT / "janus_label_shares.csv", index=False)

    variants: dict[str, Panel] = {"200-day rule (live)": base}
    in_charge: dict[str, float] = {}
    for ts in TIMESCALES:
        for det in DETECTORS:
            daily = {p: on_days(regimes[p, ts], base.index)[det] for p in pairs}
            warm = pd.concat([s.loc[SINCE:] for s in daily.values()])
            share = float((warm.notna() & (warm != "warmup")).mean())
            for mapping in MAPPINGS:
                name = f"{det} {ts} / {mapping}"
                states = {p: to_state(daily[p], mapping, smas[p]) for p in pairs}
                variants[name] = panel_with(base, states)
                in_charge[name] = round(share, 3)

    rows = []
    for s in start_dates():
        for name, panel in variants.items():
            m = summarize(simulate(panel, Policy(name, LIVE_V3), s))
            m["start"] = s.date().isoformat()
            rows.append(m)
    df = pd.DataFrame(rows)
    df.to_csv(OUT / "janus_regime_sweep.csv", index=False)

    live = df[df.policy == "200-day rule (live)"].set_index("start")
    summary = []
    for name, g in df.groupby("policy", sort=False):
        g = g.set_index("start")
        gap = g.xirr - live.xirr
        summary.append(
            {
                "regime_signal": name,
                "detector_warm_share": in_charge.get(name),
                "median_return": round(g.xirr.median(), 4),
                "low_return": round(g.xirr.quantile(0.1), 4),
                "median_drawdown": round(g.nav_max_dd.median(), 4),
                "worst_drawdown": round(g.nav_max_dd.min(), 4),
                "beats_200d": None
                if name.startswith("200")
                else round(float((gap > 0).mean()), 3),
                "edge_vs_200d": None
                if name.startswith("200")
                else round(float(gap.median()), 4),
                "trades_per_year": round(float(g.trades_per_year.median()), 0),
            }
        )
    out = pd.DataFrame(summary)
    out.to_csv(OUT / "janus_regime_summary.csv", index=False)
    print(out.to_string(index=False))


if __name__ == "__main__":
    main()
