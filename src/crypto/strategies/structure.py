"""Structure break filtered by session VWAP side + EMA9/21 trend.

VWAP+EMA9 are not the entry. They decide whether a break is allowed.
"""

from __future__ import annotations

import numpy as np

from crypto.features.core import StaticFeatures
from crypto.strategies.range_break import range_breakout
from crypto.strategies.trendline_break import trendline_break


def _apply_filter(sf: StaticFeatures, raw: np.ndarray, *, require_vwap: bool, require_ema: bool) -> np.ndarray:
    sig = raw.copy()
    if not require_vwap and not require_ema:
        return sig
    for i in range(sf.n):
        s = int(sig[i])
        if s == 0:
            continue
        vwap_ok = True
        ema_ok = True
        if require_vwap:
            vwap_ok = (sf.close[i] > sf.vwap[i]) if s > 0 else (sf.close[i] < sf.vwap[i])
        if require_ema:
            ema_ok = (sf.ema9[i] > sf.ema21[i]) if s > 0 else (sf.ema9[i] < sf.ema21[i])
        if not (vwap_ok and ema_ok):
            sig[i] = 0
    return sig


def structure_filtered(
    sf: StaticFeatures,
    *,
    pivot_lookback: int = 8,
    vol_mult: float = 1.3,
    require_vwap: bool = True,
    require_ema: bool = True,
) -> np.ndarray:
    raw = trendline_break(sf, pivot_lookback=pivot_lookback, vol_mult=vol_mult)
    return _apply_filter(sf, raw, require_vwap=require_vwap, require_ema=require_ema)


def range_filtered(
    sf: StaticFeatures,
    *,
    lookback: int = 15,
    vol_mult: float = 1.2,
    require_vwap: bool = True,
    require_ema: bool = True,
) -> np.ndarray:
    raw = range_breakout(sf, lookback=lookback, vol_mult=vol_mult)
    return _apply_filter(sf, raw, require_vwap=require_vwap, require_ema=require_ema)
