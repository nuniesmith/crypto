"""Long-horizon regime study: hold ETH/SOL, and sell portions in bear phases.

The live 1h ETH/SOL sleeve lost to buy-and-hold: it captured roughly a fifth of
the bull moves and paid Kraken tier-1 fees on every swing (docs/research.md).
This asks a different question: hold the coin, and step the exposure down in
portions when a slow trend signal turns bearish, back up when it turns bullish.
Holding for weeks to months means a handful of trades a year, so fees stop
dominating.

Only standard, untuned signals are tested, each at its textbook parameter. The
point is whether the *family* helps on both coins and in both halves of the
history, not to find the best-fitting number. Choosing a winner by searching a
grid on the same data would just re-learn this repo's earlier lesson.

Execution model: the signal is read on day t's close and traded at day t+1's
open (no lookahead). A change of exposure trades only the difference, and pays
`fee` on the notional traded. Strategies and buy-and-hold start on the same day:
the first day every signal has its full history (the 200-day average needs 200
days).

Run: `python -m crypto.opt.regime` (needs data/{ETH,SOL}USD_1440m.parquet, from
`fetch_binance_vision(pair, days=..., interval=1440)`).
"""

from __future__ import annotations

import sys
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import pandas as pd

DATA = Path(__file__).resolve().parents[3] / "data"

# Kraken tier 1 (the account's real tier; see the fee-tier note in the repo):
# maker 0.40%, taker 0.80%, plus 0.10% for slippage on a daily-open fill.
FEES = {"maker": 0.0040 + 0.0010, "taker": 0.0080 + 0.0010}


def load(pair: str) -> pd.DataFrame:
    df = pd.read_parquet(DATA / f"{pair}_1440m.parquet")[["open", "close"]].astype(float)
    # The archive skips a few days (exchange maintenance); carry the last close
    # forward so a missing day is "no information", not a hole.
    full = pd.date_range(df.index.min(), df.index.max(), freq="D", tz="UTC")
    return df.reindex(full).ffill()


# ─── exposure rules: each maps daily closes → target exposure in [0, 1] ──────


def sma(s: pd.Series, n: int) -> pd.Series:
    return s.rolling(n, min_periods=n).mean()


def above_sma200(close: pd.Series) -> pd.Series:
    m = sma(close, 200)
    return (close > m).astype(float).where(m.notna())


def sma200_band(close: pd.Series, band: float = 0.05) -> pd.Series:
    """In above SMA200×(1+band), out below SMA200×(1−band), else unchanged:
    a buffer so a price hugging its average doesn't flip every few days."""
    m = sma(close, 200)
    out = pd.Series(np.nan, index=close.index)
    state = np.nan
    for i, (c, a) in enumerate(zip(close.to_numpy(), m.to_numpy())):
        if np.isnan(a):
            continue
        if np.isnan(state):
            state = 1.0 if c > a else 0.0
        elif c > a * (1 + band):
            state = 1.0
        elif c < a * (1 - band):
            state = 0.0
        out.iloc[i] = state
    return out


def cross_50_200(close: pd.Series) -> pd.Series:
    fast, slow = sma(close, 50), sma(close, 200)
    return (fast > slow).astype(float).where(slow.notna())


def weekly_20w(close: pd.Series) -> pd.Series:
    """Weekly close above its 20-week average. Read at each week's close
    (Sunday) and held for the following week."""
    weekly = close.resample("W-SUN").last()
    sig = (weekly > sma(weekly, 20)).astype(float).where(sma(weekly, 20).notna())
    return sig.reindex(close.index, method="ffill")


def tiered(close: pd.Series) -> pd.Series:
    """Three steps instead of a switch: 100% when price is above its 200-day
    average AND the 50-day is above the 200-day, 62.5% with one of the two,
    25% with neither (a core that is never sold)."""
    m50, m200 = sma(close, 50), sma(close, 200)
    score = (close > m200).astype(float) + (m50 > m200).astype(float)
    return (0.25 + 0.375 * score).where(m200.notna())


def with_core(rule, core: float):
    """Keep `core` always held; the signal moves only the rest."""

    def f(close: pd.Series) -> pd.Series:
        return core + (1 - core) * rule(close)

    return f


RULES = {
    "hold": None,  # buy and hold, the bar to beat
    "sma200": above_sma200,
    "sma200±5%": sma200_band,
    "cross50/200": cross_50_200,
    "20w": weekly_20w,
    "core50+sma200±5%": with_core(sma200_band, 0.5),
    "core50+20w": with_core(weekly_20w, 0.5),
    "tiered25/62/100": tiered,
}


# ─── simulation ──────────────────────────────────────────────────────────────


@dataclass
class Result:
    multiple: float
    cagr: float
    max_dd: float
    trades: int
    fees_pct: float
    avg_exposure: float

    @property
    def calmar(self) -> float:
        return self.cagr / self.max_dd if self.max_dd > 0 else float("nan")


def simulate(df: pd.DataFrame, target: pd.Series | None, fee: float, start, end) -> Result:
    d = df.loc[start:end]
    tgt = pd.Series(1.0, index=d.index) if target is None else target.loc[start:end]
    cash, units = 1.0, 0.0
    exposure = 0.0
    equity_curve = []
    trades = 0
    fees = 0.0
    opens, closes, targets = d["open"].to_numpy(), d["close"].to_numpy(), tgt.to_numpy()
    for i in range(len(d)):
        # Trade at today's open toward YESTERDAY's close-read target (the
        # first day buys in to the first target at its open).
        want = targets[i - 1] if i > 0 else targets[0]
        price = opens[i]
        equity = cash + units * price
        if abs(want - exposure) > 1e-9:
            notional = abs(want - exposure) * equity
            cost = notional * fee
            new_units = (want * (equity - cost)) / price
            cash = equity - cost - new_units * price
            units = new_units
            exposure = want
            fees += cost
            trades += 1
        equity_curve.append(cash + units * closes[i])
    eq = np.array(equity_curve)
    years = (d.index[-1] - d.index[0]).days / 365.25
    peak = np.maximum.accumulate(eq)
    max_dd = float(np.max(1 - eq / peak))
    return Result(
        multiple=float(eq[-1]),
        cagr=float(eq[-1] ** (1 / years) - 1) if years > 0 else float("nan"),
        max_dd=max_dd,
        trades=trades - 1 if target is None else trades,  # hold's first buy isn't a "trade"
        fees_pct=fees * 100,
        avg_exposure=float(np.mean(targets)),
    )


def study(pair: str, fee_name: str = "maker") -> pd.DataFrame:
    df = load(pair)
    fee = FEES[fee_name]
    targets = {name: (None if rule is None else rule(df["close"])) for name, rule in RULES.items()}
    first = max(t.first_valid_index() for t in targets.values() if t is not None)
    end = df.index[-1]
    mid = first + (end - first) / 2
    rows = []
    for period, (a, b) in {
        "full": (first, end),
        "1st half": (first, mid),
        "2nd half": (mid, end),
    }.items():
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
                    "trades": r.trades,
                    "fees%": round(r.fees_pct, 1),
                    "exposure": round(r.avg_exposure, 2),
                }
            )
    return pd.DataFrame(rows)


def main(argv: list[str] | None = None) -> None:
    fee_name = (argv or sys.argv[1:] or ["maker"])[0]
    pd.set_option("display.width", 200)
    pd.set_option("display.max_rows", 200)
    for pair in ("ETHUSD", "SOLUSD"):
        out = study(pair, fee_name)
        print(f"\n=== {pair}  (fees: {fee_name} {FEES[fee_name] * 100:.2f}% per side)")
        for period, g in out.groupby("period", sort=False):
            print(f"\n{period}")
            print(g.drop(columns=["pair", "period"]).to_string(index=False))


if __name__ == "__main__":
    main()
