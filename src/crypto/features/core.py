"""Static features computed once per dataset (VWAP, EMA, ATR, session flags).

Mirrors the futures pattern: everything that does not depend on tunable
parameters lives here so Optuna trials stay cheap.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
import pandas as pd


@dataclass
class StaticFeatures:
    n: int
    time: np.ndarray          # datetime64[ns]
    open: np.ndarray
    high: np.ndarray
    low: np.ndarray
    close: np.ndarray
    volume: np.ndarray
    # indicators
    vwap: np.ndarray          # session-anchored (UTC day)
    ema9: np.ndarray
    ema21: np.ndarray
    atr14: np.ndarray
    # helpers
    ret1: np.ndarray          # close-to-close return
    range_pct: np.ndarray     # (high-low)/close


def _session_vwap(df: pd.DataFrame) -> np.ndarray:
    """UTC-day session VWAP (crypto is 24/7; daily reset is a clean default)."""
    typical = (df["high"] + df["low"] + df["close"]) / 3.0
    pv = typical * df["volume"]
    day = df.index.floor("D")
    cum_pv = pv.groupby(day).cumsum()
    cum_vol = df["volume"].groupby(day).cumsum().replace(0, np.nan)
    return (cum_pv / cum_vol).ffill().to_numpy()


def _ema(series: pd.Series, span: int) -> np.ndarray:
    return series.ewm(span=span, adjust=False).mean().to_numpy()


def _atr(df: pd.DataFrame, length: int = 14) -> np.ndarray:
    prev_close = df["close"].shift(1)
    tr = pd.concat(
        [
            df["high"] - df["low"],
            (df["high"] - prev_close).abs(),
            (df["low"] - prev_close).abs(),
        ],
        axis=1,
    ).max(axis=1)
    return tr.ewm(alpha=1 / length, adjust=False).mean().to_numpy()


def compute_static(df: pd.DataFrame) -> StaticFeatures:
    """Build StaticFeatures from an OHLCV DataFrame (UTC index required)."""
    if df.empty:
        raise ValueError("empty dataframe")
    df = df.copy()
    n = len(df)
    vwap = _session_vwap(df)
    ema9 = _ema(df["close"], 9)
    ema21 = _ema(df["close"], 21)
    atr14 = _atr(df, 14)
    ret1 = df["close"].pct_change().fillna(0).to_numpy()
    range_pct = ((df["high"] - df["low"]) / df["close"]).to_numpy()

    return StaticFeatures(
        n=n,
        time=df.index.to_numpy(),
        open=df["open"].to_numpy(),
        high=df["high"].to_numpy(),
        low=df["low"].to_numpy(),
        close=df["close"].to_numpy(),
        volume=df["volume"].to_numpy(),
        vwap=vwap,
        ema9=ema9,
        ema21=ema21,
        atr14=atr14,
        ret1=ret1,
        range_pct=range_pct,
    )
