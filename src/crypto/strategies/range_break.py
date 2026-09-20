"""Micro range-breakout scalp.

Look back `lookback` bars, take the high/low of that window (excluding the
current bar). Enter long on a close above the window high, short below the
window low. Optional volume confirmation.

Classic crypto scalp structure that works on 1 m when volatility expands.
"""

from __future__ import annotations

import numpy as np

from crypto.features.core import StaticFeatures


def range_breakout(
    sf: StaticFeatures,
    *,
    lookback: int = 15,
    vol_mult: float = 1.2,
    require_vol: bool = True,
) -> np.ndarray:
    n = sf.n
    sig = np.zeros(n, dtype=np.int8)
    if lookback < 3 or n < lookback + 5:
        return sig

    high, low, close, vol = sf.high, sf.low, sf.close, sf.volume
    vol_ma = pd_rolling_mean(vol, lookback)

    for i in range(lookback + 1, n):
        window_high = high[i - lookback : i].max()
        window_low = low[i - lookback : i].min()
        vol_ok = (not require_vol) or (vol[i] >= vol_mult * vol_ma[i])

        if close[i] > window_high and vol_ok:
            sig[i] = 1
        elif close[i] < window_low and vol_ok:
            sig[i] = -1
    return sig


def pd_rolling_mean(arr: np.ndarray, window: int) -> np.ndarray:
    """Simple rolling mean without pulling pandas into the hot loop."""
    out = np.full_like(arr, np.nan, dtype=float)
    csum = np.cumsum(np.insert(arr.astype(float), 0, 0.0))
    out[window - 1 :] = (csum[window:] - csum[:-window]) / window
    first = np.nanmean(arr[:window]) if window <= len(arr) else 0.0
    out[: window - 1] = first
    return out
