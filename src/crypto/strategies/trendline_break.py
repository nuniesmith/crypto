"""Simple swing trendline break proxy.

Builds a crude ascending/descending trendline from the last two swing pivots
(lookback highs/lows) and fires when price closes through that line with
volume confirmation.

This is intentionally lightweight — full geometric trendline fitting belongs
in a later research pass; here we just want a fast, fee-aware signal that
captures “break of structure” scalps common on crypto 1 m charts.
"""

from __future__ import annotations

import numpy as np

from crypto.features.core import StaticFeatures
from crypto.strategies.range_break import pd_rolling_mean


def trendline_break(
    sf: StaticFeatures,
    *,
    pivot_lookback: int = 8,
    vol_mult: float = 1.3,
) -> np.ndarray:
    n = sf.n
    sig = np.zeros(n, dtype=np.int8)
    if n < pivot_lookback * 3:
        return sig

    high, low, close, vol = sf.high, sf.low, sf.close, sf.volume
    vol_ma = pd_rolling_mean(vol, pivot_lookback * 2)

    is_swing_high = np.zeros(n, dtype=bool)
    is_swing_low = np.zeros(n, dtype=bool)
    lb = pivot_lookback
    for i in range(lb, n - lb):
        if high[i] == high[i - lb : i + lb + 1].max():
            is_swing_high[i] = True
        if low[i] == low[i - lb : i + lb + 1].min():
            is_swing_low[i] = True

    last_sh: list[tuple[int, float]] = []
    last_sl: list[tuple[int, float]] = []

    for i in range(n):
        if is_swing_high[i]:
            last_sh.append((i, high[i]))
            if len(last_sh) > 3:
                last_sh.pop(0)
        if is_swing_low[i]:
            last_sl.append((i, low[i]))
            if len(last_sl) > 3:
                last_sl.pop(0)

        if i < lb * 2:
            continue
        vol_ok = vol[i] >= vol_mult * vol_ma[i]

        if len(last_sh) >= 2 and vol_ok:
            (i1, p1), (i2, p2) = last_sh[-2], last_sh[-1]
            if i2 > i1 and i > i2:
                slope = (p2 - p1) / (i2 - i1)
                line = p2 + slope * (i - i2)
                if close[i] > line and close[i - 1] <= line:
                    sig[i] = 1

        if len(last_sl) >= 2 and vol_ok:
            (i1, p1), (i2, p2) = last_sl[-2], last_sl[-1]
            if i2 > i1 and i > i2:
                slope = (p2 - p1) / (i2 - i1)
                line = p2 + slope * (i - i2)
                if close[i] < line and close[i - 1] >= line:
                    sig[i] = -1

    return sig
