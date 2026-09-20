"""Fast EMA9 vs slower EMA21 cross, filtered by VWAP side.

Classic momentum scalp; fewer trades than pure mean-reversion but often
cleaner after fees.
"""

from __future__ import annotations

import numpy as np

from crypto.features.core import StaticFeatures


def ema_cross(
    sf: StaticFeatures,
    *,
    require_vwap_side: bool = True,
) -> np.ndarray:
    n = sf.n
    sig = np.zeros(n, dtype=np.int8)
    ema9, ema21, vwap, close = sf.ema9, sf.ema21, sf.vwap, sf.close

    for i in range(22, n):
        cross_up = ema9[i - 1] <= ema21[i - 1] and ema9[i] > ema21[i]
        cross_dn = ema9[i - 1] >= ema21[i - 1] and ema9[i] < ema21[i]
        if cross_up:
            if not require_vwap_side or close[i] > vwap[i]:
                sig[i] = 1
        elif cross_dn:
            if not require_vwap_side or close[i] < vwap[i]:
                sig[i] = -1
    return sig
