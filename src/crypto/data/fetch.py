"""Kraken public OHLC downloader (no API key required).

Kraken returns at most 720 candles per call. We page with the `since`
parameter and write incrementally to parquet so an interrupted run can resume.
"""

from __future__ import annotations

import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pandas as pd
import requests

from .store import DATA_DIR, pair_path

KRAKEN_OHLC = "https://api.kraken.com/0/public/OHLC"
# Official pair aliases used by the public API
PAIR_MAP = {
    "XBTUSD": "XBTUSD",
    "BTCUSD": "XBTUSD",
    "ETHUSD": "ETHUSD",
    "SOLUSD": "SOLUSD",
    "XXBTZUSD": "XBTUSD",
}


def _normalize_pair(pair: str) -> str:
    p = pair.upper().replace("/", "").replace("-", "")
    return PAIR_MAP.get(p, p)


def _kraken_ohlc(pair: str, interval: int, since: int | None = None) -> tuple[list, int]:
    params: dict = {"pair": pair, "interval": interval}
    if since is not None:
        params["since"] = since
    r = requests.get(KRAKEN_OHLC, params=params, timeout=30)
    r.raise_for_status()
    body = r.json()
    if body.get("error"):
        raise RuntimeError(f"Kraken error: {body['error']}")
    result = body["result"]
    # key is the internal name (e.g. XXBTZUSD)
    candles = next(v for k, v in result.items() if k != "last")
    last = int(result["last"])
    return candles, last


def fetch_pair(
    pair: str,
    days: int = 7,
    interval: int = 1,
    sleep: float = 1.1,
) -> pd.DataFrame:
    """Download `days` of OHLCV and merge with any existing cache."""
    pair = _normalize_pair(pair)
    path = pair_path(pair, interval)
    path.parent.mkdir(parents=True, exist_ok=True)

    end = datetime.now(timezone.utc)
    start = end - timedelta(days=days)
    since = int(start.timestamp())

    existing: pd.DataFrame | None = None
    if path.exists():
        existing = pd.read_parquet(path)
        if not existing.empty:
            # resume from last closed candle
            last_ts = int(existing.index[-1].timestamp())
            since = max(since, last_ts)

    rows: list[list] = []
    while True:
        candles, last = _kraken_ohlc(pair, interval, since)
        if not candles:
            break
        rows.extend(candles)
        # stop when we have caught up or the page did not advance
        if last <= since or len(candles) < 2:
            break
        since = last
        time.sleep(sleep)  # polite rate-limit

    if not rows and existing is not None:
        print(f"{pair}: cache already covers requested window")
        return existing

    cols = ["time", "open", "high", "low", "close", "vwap", "volume", "count"]
    df = pd.DataFrame(rows, columns=cols)
    df["time"] = pd.to_datetime(df["time"], unit="s", utc=True)
    for c in ["open", "high", "low", "close", "vwap", "volume"]:
        df[c] = df[c].astype(float)
    df["count"] = df["count"].astype(int)
    df = df.set_index("time").sort_index()
    # drop the still-forming candle if present
    if len(df) and df.index[-1] > end - timedelta(minutes=interval):
        df = df.iloc[:-1]

    if existing is not None and not existing.empty:
        df = pd.concat([existing, df])
        df = df[~df.index.duplicated(keep="last")].sort_index()

    # keep only the requested look-back
    cutoff = end - timedelta(days=days)
    df = df[df.index >= cutoff]

    df.to_parquet(path)
    print(f"{pair}: {len(df):,} bars → {path}")
    return df


def fetch_pairs(pairs: list[str], days: int = 7, interval: int = 1) -> None:
    for p in pairs:
        try:
            fetch_pair(p, days=days, interval=interval)
        except Exception as exc:
            print(f"FAILED {p}: {exc}")
