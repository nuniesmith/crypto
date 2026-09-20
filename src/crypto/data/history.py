"""Year-scale 1-minute history.

Kraken's public OHLC API only returns 720 one-minute candles (~12 hours).
This module downloads Binance Vision monthly/daily 1m klines (free, continuous)
and writes them into the same parquet cache the backtester already uses.

Kraken official OHLCVT ZIPs (when you have them under data/raw/) can be merged
with `crypto import-ohlcvt`.
"""

from __future__ import annotations

import io
import zipfile
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

import pandas as pd
import requests

from .store import DATA_DIR, coverage_report, normalize_ohlcv

BINANCE_VISION = "https://data.binance.vision/data/spot"
BINANCE_PAIR = {
    "XBTUSD": "BTCUSDT",
    "BTCUSD": "BTCUSDT",
    "BTCUSDT": "BTCUSDT",
    "ETHUSD": "ETHUSDT",
    "ETHUSDT": "ETHUSDT",
    "SOLUSD": "SOLUSDT",
    "SOLUSDT": "SOLUSDT",
}
LOCAL_PAIR = {
    "BTCUSDT": "XBTUSD",
    "ETHUSDT": "ETHUSD",
    "SOLUSDT": "SOLUSD",
}

_SESSION = requests.Session()
_SESSION.headers.update({"User-Agent": "crypto-scalper/0.1 (research; binance-vision)"})


def _binance_symbol(pair: str) -> str:
    p = pair.upper().replace("/", "").replace("-", "")
    if p not in BINANCE_PAIR:
        raise ValueError(f"No Binance 1m mapping for {pair!r}. Known: {sorted(set(BINANCE_PAIR))}")
    return BINANCE_PAIR[p]


def _month_range(start: date, end: date) -> list[str]:
    months = []
    y, m = start.year, start.month
    while (y, m) <= (end.year, end.month):
        months.append(f"{y:04d}-{m:02d}")
        m += 1
        if m == 13:
            y, m = y + 1, 1
    return months


def _download(url: str, dest: Path) -> bool:
    dest.parent.mkdir(parents=True, exist_ok=True)
    try:
        head = _SESSION.head(url, timeout=30, allow_redirects=True)
        if head.status_code == 404:
            return False
        head.raise_for_status()
        size = int(head.headers.get("Content-Length") or 0)
        if dest.exists() and size and dest.stat().st_size == size:
            return True
    except requests.HTTPError as exc:
        if exc.response is not None and exc.response.status_code == 404:
            return False
        raise
    with _SESSION.get(url, timeout=120, stream=True) as r:
        if r.status_code == 404:
            return False
        r.raise_for_status()
        tmp = dest.with_suffix(dest.suffix + ".part")
        with tmp.open("wb") as f:
            for chunk in r.iter_content(1 << 16):
                if chunk:
                    f.write(chunk)
        tmp.replace(dest)
    return True


def _epoch_unit(values: pd.Series) -> str:
    v = pd.to_numeric(values, errors="coerce").dropna()
    if v.empty:
        return "ms"
    x = float(v.iloc[0])
    if x > 1e16:
        return "ns"
    if x > 1e14:
        return "us"
    if x > 1e11:
        return "ms"
    return "s"


def _klines_from_zip(raw: bytes) -> pd.DataFrame:
    with zipfile.ZipFile(io.BytesIO(raw)) as zf:
        names = [n for n in zf.namelist() if n.endswith(".csv")]
        if not names:
            return pd.DataFrame()
        with zf.open(names[0]) as fh:
            df = pd.read_csv(fh, header=None)
    # Binance Vision kline CSV (no header):
    # open_time, open, high, low, close, volume, close_time, quote_volume,
    # count, taker_buy_base, taker_buy_quote, ignore
    df = df.iloc[:, :9]
    df.columns = [
        "open_time",
        "open",
        "high",
        "low",
        "close",
        "volume",
        "close_time",
        "quote_volume",
        "count",
    ]
    df["time"] = pd.to_datetime(df["open_time"], unit=_epoch_unit(df["open_time"]), utc=True)
    for c in ["open", "high", "low", "close", "volume", "quote_volume"]:
        df[c] = pd.to_numeric(df[c], errors="coerce")
    df["count"] = pd.to_numeric(df["count"], errors="coerce").fillna(0).astype(int)
    vol = df["volume"].replace(0, pd.NA)
    df["vwap"] = (df["quote_volume"] / vol).astype(float)
    typical = (df["high"] + df["low"] + df["close"]) / 3.0
    df["vwap"] = df["vwap"].fillna(typical)
    return df.set_index("time")[["open", "high", "low", "close", "vwap", "volume", "count"]]


def _read_cached_zip(path: Path) -> pd.DataFrame:
    return _klines_from_zip(path.read_bytes())


def fetch_binance_1m(pair: str, days: int = 400, interval: int = 1) -> pd.DataFrame:
    if interval != 1:
        raise ValueError("Binance Vision helper currently implements 1-minute only")
    symbol = _binance_symbol(pair)
    local = LOCAL_PAIR.get(symbol, pair.upper())
    end = datetime.now(timezone.utc).date()
    start = end - timedelta(days=days)
    raw_dir = DATA_DIR / "raw" / "binance" / symbol / "1m"
    frames: list[pd.DataFrame] = []

    months = _month_range(start, end)
    for ym in months:
        year, month = (int(x) for x in ym.split("-"))
        is_current = (year, month) == (end.year, end.month)
        if not is_current:
            url = f"{BINANCE_VISION}/monthly/klines/{symbol}/1m/{symbol}-1m-{ym}.zip"
            dest = raw_dir / f"{symbol}-1m-{ym}.zip"
            print(f"  {symbol} monthly {ym} …", flush=True)
            if not _download(url, dest):
                print(f"    skip (not published)")
                continue
            frames.append(_read_cached_zip(dest))
            continue
        # Current month is only published as daily zips.
        d = date(year, month, 1)
        n_daily = 0
        while d <= end:
            ds = d.isoformat()
            url = f"{BINANCE_VISION}/daily/klines/{symbol}/1m/{symbol}-1m-{ds}.zip"
            dest = raw_dir / f"{symbol}-1m-{ds}.zip"
            ok = _download(url, dest)
            if ok:
                frames.append(_read_cached_zip(dest))
                n_daily += 1
            d += timedelta(days=1)
        print(f"  {symbol} daily {ym}: {n_daily} day files", flush=True)

    if not frames:
        raise RuntimeError(f"No Binance 1m files downloaded for {symbol}")

    df = normalize_ohlcv(pd.concat(frames))
    df = df[df.index >= pd.Timestamp(start, tz="UTC")]
    from .store import pair_path

    # Replace the cache (do not merge leftover Kraken 12h API bars into USDT 1m).
    path = pair_path(local, 1)
    path.parent.mkdir(parents=True, exist_ok=True)
    df.to_parquet(path)
    full = df
    rep = coverage_report(full, 1)
    print(
        f"{local}: {rep['bars']:,} bars  {rep['span_days']:.1f}d  "
        f"cov={rep['coverage_pct']}%  {rep['from']} → {rep['to']}  → {path}"
    )
    return full


def fetch_history(pairs: list[str], days: int = 400, interval: int = 1) -> None:
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    print(f"Fetching ≥{days}d of {interval}m bars via Binance Vision → {DATA_DIR}")
    for p in pairs:
        try:
            fetch_binance_1m(p, days=days, interval=interval)
        except Exception as exc:
            print(f"FAILED {p}: {exc}")
