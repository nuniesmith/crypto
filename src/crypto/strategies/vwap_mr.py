"""VWAP mean-reversion scalp.

Enter long when price is sufficiently below session VWAP and EMA9 is turning up;
short when above VWAP and EMA9 turning down. Designed for high trade count
with tight holds so fee drag is the main risk.
"""

from __future__ import annotations

import numpy as np

from crypto.features.core import StaticFeatures


def vwap_mean_reversion(
    sf: StaticFeatures,
    *,
    z_entry: float = 1.2,          # distance from VWAP in ATR units
    ema_slope_bars: int = 3,       # look-back for EMA9 slope
    min_atr: float = 0.0,          # filter dead markets
) -> np.ndarray:
    """Return +1 / -1 / 0 signal array (length = sf.n)."""
    n = sf.n
    sig = np.zeros(n, dtype=np.int8)
    atr = sf.atr14
    vwap = sf.vwap
    ema = sf.ema9
    close = sf.close

    for i in range(max(ema_slope_bars + 1, 20), n):
        if atr[i] < min_atr or np.isnan(vwap[i]) or atr[i] == 0:
            continue
        dist = (close[i] - vwap[i]) / atr[i]
        slope = ema[i] - ema[i - ema_slope_bars]

        if dist < -z_entry and slope > 0:
            sig[i] = 1
        elif dist > z_entry and slope < 0:
            sig[i] = -1
    return sig
