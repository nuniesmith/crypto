"""Bar-by-bar simulator with maker/taker fees.

Designed for 1-minute scalps: positions are flat overnight (or after max hold),
size is fixed notional or risk-based, and every fill pays the configured fee.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from crypto.features.core import StaticFeatures
from crypto.sim.fees import FeeModel


@dataclass
class Trade:
    entry_idx: int
    exit_idx: int
    side: int               # +1 long, -1 short
    entry: float
    exit: float
    size: float             # base-currency quantity
    pnl: float              # net USD after fees
    fees: float
    r_multiple: float
    exit_reason: str
    is_maker_entry: bool
    is_maker_exit: bool


@dataclass
class RiskPolicy:
    notional_usd: float = 1_000.0   # fixed size for scalps
    max_hold_bars: int = 30         # 30 minutes default
    daily_loss_halt_pct: float = 0.02


def simulate(
    sf: StaticFeatures,
    signals: np.ndarray,            # +1 / -1 / 0 per bar (entry signal on close)
    fee: FeeModel,
    policy: RiskPolicy | None = None,
    *,
    maker_entry: bool = True,
    maker_exit: bool = False,
) -> list[Trade]:
    """Replay signals. Entry on next bar open (or limit fill approximation).

    `signals[i] != 0` means “enter on bar i close / bar i+1 open”.
    We never pyramid; one position at a time.
    """
    if policy is None:
        policy = RiskPolicy()
    n = sf.n
    trades: list[Trade] = []
    pos_side = 0
    entry_px = 0.0
    entry_idx = 0
    size = 0.0
    fees_paid = 0.0

    for i in range(1, n):
        # ---- manage open position ------------------------------------
        if pos_side != 0:
            held = i - entry_idx
            # stop / target placeholders can be added later; for now time exit
            # and signal flip
            exit_now = False
            reason = ""
            if held >= policy.max_hold_bars:
                exit_now = True
                reason = "TIME"
            elif signals[i] == -pos_side:
                exit_now = True
                reason = "FLIP"
            # simple ATR stop (1.5× ATR from entry)
            atr = sf.atr14[entry_idx]
            if atr > 0:
                if pos_side > 0 and sf.low[i] < entry_px - 1.5 * atr:
                    exit_now = True
                    reason = "STOP"
                elif pos_side < 0 and sf.high[i] > entry_px + 1.5 * atr:
                    exit_now = True
                    reason = "STOP"

            if exit_now:
                # exit fill: market → adverse slippage already in FeeModel
                fill = sf.close[i]
                notional = size * fill
                fee_exit = fee.cost(notional, is_maker=maker_exit)
                gross = pos_side * size * (fill - entry_px)
                net = gross - fees_paid - fee_exit
                risk = abs(entry_px * 0.005) * size or 1e-9  # 0.5 % nominal risk
                trades.append(
                    Trade(
                        entry_idx=entry_idx,
                        exit_idx=i,
                        side=pos_side,
                        entry=entry_px,
                        exit=fill,
                        size=size,
                        pnl=net,
                        fees=fees_paid + fee_exit,
                        r_multiple=net / risk,
                        exit_reason=reason,
                        is_maker_entry=maker_entry,
                        is_maker_exit=maker_exit,
                    )
                )
                pos_side = 0
                fees_paid = 0.0
                continue

        # ---- new entry -----------------------------------------------
        if pos_side == 0 and signals[i] != 0 and i + 1 < n:
            side = int(signals[i])
            # approximate limit fill at signal bar close; if we miss, skip
            fill = sf.close[i]
            size = policy.notional_usd / fill
            notional = size * fill
            fee_entry = fee.cost(notional, is_maker=maker_entry)
            pos_side = side
            entry_px = fill
            entry_idx = i
            fees_paid = fee_entry

    return trades
