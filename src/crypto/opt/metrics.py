"""Summary stats from a list of Trade objects."""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from crypto.sim.engine import Trade


@dataclass
class Metrics:
    n_trades: int
    win_rate: float
    avg_pnl: float
    total_pnl: float
    avg_r: float
    profit_factor: float
    max_dd: float
    expectancy: float
    avg_hold_bars: float
    total_fees: float


def compute(trades: list[Trade]) -> Metrics:
    if not trades:
        return Metrics(0, 0, 0, 0, 0, 0, 0, 0, 0, 0)

    pnls = np.array([t.pnl for t in trades])
    wins = pnls[pnls > 0]
    losses = pnls[pnls <= 0]
    total = float(pnls.sum())
    gross_win = float(wins.sum()) if len(wins) else 0.0
    gross_loss = float(-losses.sum()) if len(losses) else 1e-9
    pf = gross_win / gross_loss if gross_loss else float("inf")

    # simple equity curve max drawdown
    equity = np.cumsum(pnls)
    peak = np.maximum.accumulate(equity)
    dd = peak - equity
    max_dd = float(dd.max()) if len(dd) else 0.0

    holds = [t.exit_idx - t.entry_idx for t in trades]
    fees = sum(t.fees for t in trades)

    return Metrics(
        n_trades=len(trades),
        win_rate=float((pnls > 0).mean()),
        avg_pnl=float(pnls.mean()),
        total_pnl=total,
        avg_r=float(np.mean([t.r_multiple for t in trades])),
        profit_factor=pf,
        max_dd=max_dd,
        expectancy=float(pnls.mean()),
        avg_hold_bars=float(np.mean(holds)),
        total_fees=fees,
    )


def pretty(m: Metrics) -> str:
    return (
        f"trades={m.n_trades:4d}  win={m.win_rate:5.1%}  "
        f"PnL=${m.total_pnl:8.2f}  avg=${m.avg_pnl:6.3f}  "
        f"PF={m.profit_factor:5.2f}  DD=${m.max_dd:7.2f}  "
        f"fees=${m.total_fees:7.2f}  hold={m.avg_hold_bars:4.1f}b"
    )
