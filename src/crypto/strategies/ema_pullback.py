"""EMA trend + pullback scalp.

1. Trend filter: EMA9 > EMA21 (long) or EMA9 < EMA21 (short).
2. Price pulls back to touch / pierce EMA9 then closes back in trend direction.
3. Optional: only take trades on the VWAP side of the trend.

Higher win-rate than pure cross; fewer trades; still fee-sensitive.
"""

from __future__ import annotations

import numpy as np

from crypto.features.core import StaticFeatures


def ema_pullback(
    sf: StaticFeatures,
    *,
    touch_atr: float = 0.25,
    require_vwap_side: bool = True,
) -> np.ndarray:
    n = sf.n
    sig = np.zeros(n, dtype=np.int8)
    ema9, ema21, atr, close, low, high, vwap = (
        sf.ema9,
        sf.ema21,
        sf.atr14,
        sf.close,
        sf.low,
        sf.high,
        sf.vwap,
    )

    for i in range(25, n):
        if atr[i] <= 0 or np.isnan(ema9[i]):
            continue
        uptrend = ema9[i] > ema21[i]
        dntrend = ema9[i] < ema21[i]
        touched = abs(close[i] - ema9[i]) <= touch_atr * atr[i] or (
            low[i] <= ema9[i] <= high[i]
        )

        if uptrend and touched and close[i] > ema9[i]:
            if not require_vwap_side or close[i] >= vwap[i]:
                sig[i] = 1
        elif dntrend and touched and close[i] < ema9[i]:
            if not require_vwap_side or close[i] <= vwap[i]:
                sig[i] = -1
    return sig
