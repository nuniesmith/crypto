"""Swing tranches study: hold each coin, and trade a few slices of it on big swings.

The operator's idea (2026-10-05): keep a HODL core of BTC, ETH and SOL, and trade
3 x 10% of each coin's allocation on long-timeframe swings, selling a slice near
the top of a Bollinger band and buying it back near the bottom, holding for weeks
to months so Kraken's tier-1 fees stop mattering. Sentiment (a Fear & Greed
index) was suggested as another input.

That is a COUNTER-trend rule: it sells strength and buys weakness. The ETH/SOL
sleeve running live since 2026-10-04 is the opposite, a TREND rule (regime.py's
"core50+sma200±5%": it sells half in a bear and buys back in a bull). So this
study puts both families side by side, on all three coins, against holding.

Rules, each at its textbook parameter (no grid search; see regime.py's header):
  hold            the coin's whole allocation, never traded
  live trend      the live rule: 50% core + 200-day average ±5% (regime.py)
  bb20 daily      70% core + three 10% slices; sell one at a daily close above
                  the upper Bollinger band (20, 2σ), buy one back at a close
                  below the lower band. One slice per excursion outside a band.
  bb20 weekly     the same on weekly closes (read Sunday, held all week)
  fng extremes    70% core + three 10% slices; sell one on each entry into
                  Extreme Greed (index > 75), buy one on each entry into Extreme
                  Fear (< 25), alternative.me's own bands. History from 2018-02.
  hold 85%        a fixed 85% (the slices' midpoint), never traded: separates
                  "timing" from "simply holding less"

Execution and fees are regime.py's: read at day t's close, traded at day t+1's
open, tier-1 maker 0.40% plus 0.10% slippage on each traded notional.

Run: `python -m crypto.opt.swing` (needs data/{BTC,ETH,SOL}USD_1440m.parquet and
data/fng_alternative_me.json from https://api.alternative.me/fng/?limit=0).
"""

from __future__ import annotations

import json
import sys

import numpy as np
import pandas as pd

from crypto.opt.regime import DATA, FEES, load, simulate, sma, sma200_band, with_core

CORE = 0.70
STEP = 0.10


def _bands(
    close: pd.Series, n: int = 20, k: float = 2.0
) -> tuple[pd.Series, pd.Series]:
    mid = sma(close, n)
    sd = close.rolling(n, min_periods=n).std(ddof=0)
    return mid + k * sd, mid - k * sd


def _slices(sell: pd.Series, buy: pd.Series, valid: pd.Series) -> pd.Series:
    """Exposure in [CORE, 1] from per-day sell/buy conditions, starting fully
    held. A condition acts once per excursion: it re-arms only after a day on
    which it is false, so a month spent above the upper band sells one slice,
    not three."""
    out = pd.Series(np.nan, index=sell.index)
    expo, sell_armed, buy_armed = 1.0, True, True
    for i, (s, b, v) in enumerate(
        zip(sell.to_numpy(), buy.to_numpy(), valid.to_numpy())
    ):
        if not v:
            continue
        if s:
            if sell_armed and expo > CORE + 1e-9:
                expo -= STEP
            sell_armed = False
        else:
            sell_armed = True
        if b:
            if buy_armed and expo < 1.0 - 1e-9:
                expo += STEP
            buy_armed = False
        else:
            buy_armed = True
        out.iloc[i] = round(expo, 4)
    return out


def bb_daily(close: pd.Series) -> pd.Series:
    upper, lower = _bands(close)
    return _slices(close > upper, close < lower, upper.notna())


def bb_weekly(close: pd.Series, n: int = 20, k: float = 2.0) -> pd.Series:
    weekly = close.resample("W-SUN").last()
    upper, lower = _bands(weekly, n, k)
    sig = _slices(weekly > upper, weekly < lower, upper.notna())
    return sig.reindex(close.index, method="ffill")


def load_fng() -> pd.Series:
    rows = json.loads((DATA / "fng_alternative_me.json").read_text())["data"]
    s = pd.Series(
        {
            pd.Timestamp(int(r["timestamp"]), unit="s", tz="UTC"): float(r["value"])
            for r in rows
        }
    ).sort_index()
    return s


def fng_extremes(close: pd.Series) -> pd.Series:
    # A day alternative.me skipped is "no reading", so the state carries over.
    fng = load_fng().reindex(close.index).ffill()
    valid = fng.notna()
    return _slices(fng > 75, fng < 25, valid)


def fixed(level: float):
    def f(close: pd.Series) -> pd.Series:
        return pd.Series(level, index=close.index)

    return f


def hybrid(close: pd.Series) -> pd.Series:
    """50% never sold, 20% on the live trend rule, 30% as weekly-band slices:
    the trend half steps aside in crashes, the slices trade the ranges."""
    trend = sma200_band(close)
    slices = (bb_weekly(close) - CORE) / (1 - CORE)  # 0..1 = how many of the 3 are held
    return 0.5 + 0.2 * trend + 0.3 * slices


RULES = {
    "hold": None,
    "live trend": with_core(sma200_band, 0.5),
    "bb20 daily": bb_daily,
    "bb20 weekly": bb_weekly,
    "fng extremes": fng_extremes,
    "hold 85%": fixed(0.85),
    "hybrid 50/20/30": hybrid,
}


def study(pair: str, fee_name: str = "maker") -> pd.DataFrame:
    df = load(pair)
    fee = FEES[fee_name]
    targets = {
        name: (None if rule is None else rule(df["close"]))
        for name, rule in RULES.items()
    }
    first = max(t.first_valid_index() for t in targets.values() if t is not None)
    end = df.index[-1]
    mid = first + (end - first) / 2
    rows = []
    for period, (a, b) in {
        "full": (first, end),
        "1st half": (first, mid),
        "2nd half": (mid, end),
    }.items():
        years = (b - a).days / 365.25
        for name, tgt in targets.items():
            r = simulate(df, tgt, fee, a, b)
            rows.append(
                {
                    "pair": pair,
                    "period": f"{period} {a.date()}→{b.date()}",
                    "rule": name,
                    "×": round(r.multiple, 2),
                    "CAGR%": round(r.cagr * 100, 1),
                    "maxDD%": round(r.max_dd * 100, 1),
                    "calmar": round(r.calmar, 2),
                    "trades/yr": round(r.trades / years, 1),
                    "fees%": round(r.fees_pct, 1),
                    "exposure": round(r.avg_exposure, 2),
                }
            )
    return pd.DataFrame(rows)


def robustness(fee_name: str = "maker") -> pd.DataFrame:
    """The weekly band's neighbours, each against "hold 85%" in each half of each
    coin's history. If only (20, 2) beat it, the result would be luck; this
    reports every neighbour rather than picking the best."""
    fee = FEES[fee_name]
    rows = []
    for pair in ("BTCUSD", "ETHUSD", "SOLUSD"):
        df = load(pair)
        close = df["close"]
        first = sma(close, 200).first_valid_index()
        end = df.index[-1]
        mid = first + (end - first) / 2
        for n in (13, 20, 26):
            for k in (1.5, 2.0, 2.5):
                tgt = bb_weekly(close, n, k)
                for half, (a, b) in {"1st": (first, mid), "2nd": (mid, end)}.items():
                    r = simulate(df, tgt, fee, a, b).multiple
                    base = simulate(df, fixed(0.85)(close), fee, a, b).multiple
                    rows.append(
                        {
                            "pair": pair,
                            "n": n,
                            "k": k,
                            "half": half,
                            "edge%": round((r / base - 1) * 100, 1),
                        }
                    )
    return pd.DataFrame(rows)


def main(argv: list[str] | None = None) -> None:
    args = argv or sys.argv[1:]
    fee_name = next((a for a in args if a in FEES), "maker")
    pd.set_option("display.width", 200)
    if "robust" in args:
        out = robustness(fee_name)
        table = out.pivot_table(
            index=["n", "k"], columns=["pair", "half"], values="edge%"
        )
        print(
            "weekly band slices vs hold 85%, return difference in % (positive = slices won)"
        )
        print(table.to_string())
        beat = (out["edge%"] > 0).mean() * 100
        print(f"\nneighbours beating hold 85%: {beat:.0f}% of {len(out)} coin-halves")
        return
    pd.set_option("display.max_rows", 200)
    for pair in ("BTCUSD", "ETHUSD", "SOLUSD"):
        out = study(pair, fee_name)
        print(f"\n=== {pair}  (fees: {fee_name} {FEES[fee_name] * 100:.2f}% per side)")
        for period, g in out.groupby("period", sort=False):
            print(f"\n{period}")
            print(g.drop(columns=["pair", "period"]).to_string(index=False))


if __name__ == "__main__":
    main()
