"""Whole-account study: BTC/ETH/SOL/USD targets, the trend rule, and rebalancing.

swing.py and regime.py study one coin at a time against its own cash. The bot
the operator chose on 2026-10-05 runs one account: targets of 50% BTC, 25% ETH,
15% SOL and 10% USD, the live trend rule on every coin (all of a coin's target
in a bull, a 50% core in a bear; regime.py's 200-day average ±5%), and
rebalancing back to those targets so deposits get invested and drift gets
trimmed. This simulates that account, so rebalancing between coins and cash is
measured rather than assumed.

The rebalancing band is the textbook "5/25" rule (Swedroe): an asset is out of
band when its weight is more than 5 points, or more than 25% of its own target,
away from that target, whichever is smaller. When any asset is out of band the
whole account goes back to its targets. Nothing here is tuned.

Policies compared, all starting from the same 50/25/15/10 split:
  hold mix         bought once, never traded again
  rebalanced       5/25 rebalancing, no trend rule
  trend on flips   the trend rule, trading only when a coin's regime flips
  trend + 5/25     the trend rule and 5/25 rebalancing: what the bot will run

Execution and fees as in regime.py: decided on day t's close, traded at day
t+1's open, tier-1 maker 0.40% plus 0.10% slippage on every traded dollar.

Run: `python -m crypto.opt.portfolio` (needs data/{BTC,ETH,SOL}USD_1440m.parquet).
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
import pandas as pd

from crypto.opt.regime import FEES, load, sma200_band

COINS = ("BTCUSD", "ETHUSD", "SOLUSD")
BASE = np.array([0.50, 0.25, 0.15])  # cash is the remaining 0.10
CORE = 0.50


def band(target: np.ndarray) -> np.ndarray:
    """The 5/25 threshold for each weight: 5 points or 25% of the target."""
    return np.minimum(0.05, 0.25 * target)


@dataclass
class Run:
    multiple: float
    cagr: float
    max_dd: float
    trades_per_year: float
    fees_pct: float
    yearly: pd.Series


def simulate(name: str, fee: float) -> Run:
    frames = {c: load(c) for c in COINS}
    regimes = {c: sma200_band(frames[c]["close"]) for c in COINS}
    start = max(r.first_valid_index() for r in regimes.values())
    end = min(f.index[-1] for f in frames.values())
    idx = frames["BTCUSD"].loc[start:end].index
    opens = np.column_stack([frames[c]["open"].reindex(idx).to_numpy() for c in COINS])
    closes = np.column_stack(
        [frames[c]["close"].reindex(idx).to_numpy() for c in COINS]
    )
    bull = np.column_stack([regimes[c].reindex(idx).to_numpy() for c in COINS])

    trend = name in ("trend on flips", "trend + 5/25")
    rebalance = name in ("rebalanced", "trend + 5/25")

    def targets(i: int) -> np.ndarray:
        """Coin target weights after day i's close (cash is 1 - their sum)."""
        if not trend:
            return BASE.copy()
        return BASE * np.where(bull[i] == 1.0, 1.0, CORE)

    units = np.zeros(3)
    cash = 1.0
    fees = 0.0
    trades = 0
    equity = []
    want_prev = None  # targets read at the previous close
    # What the last trade aimed at, so a flip can be told from no change.
    targets_at_last_trade = targets(0)
    for i in range(len(idx)):
        px = opens[i]
        value = cash + float(units @ px)
        if i == 0:
            want = targets(0)
            act = True
        else:
            want = want_prev
            weights = units * px / value
            cash_w = 1.0 - weights.sum()
            want_cash = 1.0 - want.sum()
            if rebalance:
                out = np.abs(weights - want) > band(want)
                out_cash = abs(cash_w - want_cash) > band(np.array([want_cash]))[0]
                act = bool(out.any() or out_cash)
            elif trend:
                act = bool(np.any(want != targets_at_last_trade))
            else:
                act = False
        if act:
            goal = want * value
            if trend and not rebalance and i > 0:
                # Like the live rule: only a coin whose regime flipped trades,
                # against cash; every other coin is left where it drifted.
                flipped = want != targets_at_last_trade
                goal = np.where(flipped, goal, units * px)
            delta = goal - units * px
            cost = float(np.abs(delta).sum()) * fee
            # The coins land exactly on their goals and cash carries the fee.
            units = goal / px
            cash = value - float(units @ px) - cost
            fees += cost
            trades += int(np.count_nonzero(np.abs(delta) > 1e-9)) if i > 0 else 0
            targets_at_last_trade = want.copy()
        equity.append(cash + float(units @ closes[i]))
        want_prev = targets(i)

    eq = pd.Series(equity, index=idx)
    years = (idx[-1] - idx[0]).days / 365.25
    peak = eq.cummax()
    yearly = eq.resample("YE").last().pct_change()
    yearly.iloc[0] = eq.resample("YE").last().iloc[0] / eq.iloc[0] - 1
    return Run(
        multiple=float(eq.iloc[-1]),
        cagr=float(eq.iloc[-1] ** (1 / years) - 1),
        max_dd=float((1 - eq / peak).max()),
        trades_per_year=trades / years,
        fees_pct=fees * 100,
        yearly=yearly,
    )


def main() -> None:
    fee = FEES["maker"]
    names = ("hold mix", "rebalanced", "trend on flips", "trend + 5/25")
    runs = {n: simulate(n, fee) for n in names}
    first = load("SOLUSD")
    start = sma200_band(first["close"]).first_valid_index()
    print(
        f"50/25/15/10 account from {start.date()} (fees: maker {fee * 100:.2f}% per side)\n"
    )
    rows = [
        {
            "policy": n,
            "×": round(r.multiple, 2),
            "CAGR%": round(r.cagr * 100, 1),
            "maxDD%": round(r.max_dd * 100, 1),
            "trades/yr": round(r.trades_per_year, 1),
            "fees%": round(r.fees_pct, 1),
        }
        for n, r in runs.items()
    ]
    print(pd.DataFrame(rows).to_string(index=False))
    print("\nCalendar-year returns, %:")
    table = pd.DataFrame({n: (r.yearly * 100).round(1) for n, r in runs.items()})
    table.index = table.index.year
    print(table.to_string())


if __name__ == "__main__":
    main()
