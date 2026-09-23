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
    "XRPUSD": "XRPUSDT",
    "AVAXUSD": "AVAXUSDT",
    "NEARUSD": "NEARUSDT",
    "TAOUSD": "TAOUSDT",
    "XDGUSD": "DOGEUSDT",
    "DOGEUSD": "DOGEUSDT",
    "ADAUSD": "ADAUSDT",
    "LINKUSD": "LINKUSDT",
    "PEPEUSD": "PEPEUSDT",
    "DOTUSD": "DOTUSDT",
    "RENDERUSD": "RENDERUSDT",
    "FETUSD": "FETUSDT",
    "OPUSD": "OPUSDT",
    "WIFUSD": "WIFUSDT",
    "BONKUSD": "BONKUSDT",
    "SHIBUSD": "SHIBUSDT",
    "FLOKIUSD": "FLOKIUSDT",
    "TRUMPUSD": "TRUMPUSDT",
    "VIRTUALUSD": "VIRTUALUSDT",
    "TURBOUSD": "TURBOUSDT",
    "PUMPUSD": "PUMPUSDT",
    # --- added 2026-09-23 to screen beyond the original 20 -------------------
    # Liquid Kraken USD names with >=1y of Binance USDT history. The first
    # screen covered exactly the names already in this table, so widening the
    # universe starts here rather than with a CLI flag.
    "ATOMUSD": "ATOMUSDT",
    "LTCUSD": "LTCUSDT",
    "UNIUSD": "UNIUSDT",
    "AAVEUSD": "AAVEUSDT",
    "INJUSD": "INJUSDT",
    "TIAUSD": "TIAUSDT",
    "SUIUSD": "SUIUSDT",
    "APTUSD": "APTUSDT",
    "ARBUSD": "ARBUSDT",
    "FILUSD": "FILUSDT",
    "XLMUSD": "XLMUSDT",
    "HBARUSD": "HBARUSDT",
    "ALGOUSD": "ALGOUSDT",
    "ICPUSD": "ICPUSDT",
    "GRTUSD": "GRTUSDT",
    "IMXUSD": "IMXUSDT",
    "CRVUSD": "CRVUSDT",
    "LDOUSD": "LDOUSDT",
    "ENAUSD": "ENAUSDT",
    "ONDOUSD": "ONDOUSDT",
    "JUPUSD": "JUPUSDT",
    "SEIUSD": "SEIUSDT",
    "STXUSD": "STXUSDT",
    "TRXUSD": "TRXUSDT",
    "ETCUSD": "ETCUSDT",
    "BCHUSD": "BCHUSDT",
    "VETUSD": "VETUSDT",
    "RUNEUSD": "RUNEUSDT",
}
# Local name that wins when several keys map to the same Binance symbol.
# Kraken spells these two differently from everyone else and the parquet cache
# is keyed on the Kraken name.
_CANONICAL_LOCAL = {"BTCUSDT": "XBTUSD", "DOGEUSDT": "XDGUSD"}


def _invert(mapping: dict[str, str]) -> dict[str, str]:
    """Binance symbol -> local pair name.

    Derived rather than hand-written. The two tables used to be maintained
    separately, so adding a coin meant editing both and forgetting one failed
    quietly -- the fetch would succeed and the parquet would land under a name
    nothing else looked for.
    """
    out: dict[str, str] = {}
    for local, binance in mapping.items():
        if binance in _CANONICAL_LOCAL:
            out[binance] = _CANONICAL_LOCAL[binance]
        elif local.endswith("USDT"):
            # A passthrough key like "ETHUSDT": "ETHUSDT" is the Binance name,
            # not a local one; the local spelling is the USD form.
            continue
        else:
            out.setdefault(binance, local)
    return out


LOCAL_PAIR = _invert(BINANCE_PAIR)

VISION_INTERVAL = {1: "1m", 5: "5m", 15: "15m", 60: "1h", 240: "4h"}

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
    qv = pd.to_numeric(df["quote_volume"], errors="coerce")
    base = pd.to_numeric(df["volume"], errors="coerce").replace(0, float("nan"))
    typical = (df["high"] + df["low"] + df["close"]) / 3.0
    df["vwap"] = (qv / base).fillna(typical)
    return df.set_index("time")[["open", "high", "low", "close", "vwap", "volume", "count"]]


def _read_cached_zip(path: Path) -> pd.DataFrame:
    return _klines_from_zip(path.read_bytes())


def fetch_binance_1m(pair: str, days: int = 400, interval: int = 1) -> pd.DataFrame:
    return fetch_binance_vision(pair, days=days, interval=interval)


def fetch_binance_vision(pair: str, days: int = 400, interval: int = 1) -> pd.DataFrame:
    if interval not in VISION_INTERVAL:
        raise ValueError(f"Binance Vision interval must be one of {sorted(VISION_INTERVAL)}")
    iv = VISION_INTERVAL[interval]
    symbol = _binance_symbol(pair)
    local = LOCAL_PAIR.get(symbol, pair.upper())
    end = datetime.now(timezone.utc).date()
    start = end - timedelta(days=days)
    raw_dir = DATA_DIR / "raw" / "binance" / symbol / iv
    frames: list[pd.DataFrame] = []

    months = _month_range(start, end)
    for ym in months:
        year, month = (int(x) for x in ym.split("-"))
        is_current = (year, month) == (end.year, end.month)
        if not is_current:
            url = f"{BINANCE_VISION}/monthly/klines/{symbol}/{iv}/{symbol}-{iv}-{ym}.zip"
            dest = raw_dir / f"{symbol}-{iv}-{ym}.zip"
            print(f"  {symbol} {iv} monthly {ym} …", flush=True)
            if not _download(url, dest):
                print("    skip (not published)")
                continue
            frames.append(_read_cached_zip(dest))
            continue
        d = date(year, month, 1)
        n_daily = 0
        while d <= end:
            ds = d.isoformat()
            url = f"{BINANCE_VISION}/daily/klines/{symbol}/{iv}/{symbol}-{iv}-{ds}.zip"
            dest = raw_dir / f"{symbol}-{iv}-{ds}.zip"
            ok = _download(url, dest)
            if ok:
                frames.append(_read_cached_zip(dest))
                n_daily += 1
            d += timedelta(days=1)
        print(f"  {symbol} {iv} daily {ym}: {n_daily} day files", flush=True)

    if not frames:
        raise RuntimeError(f"No Binance {iv} files downloaded for {symbol}")

    df = normalize_ohlcv(pd.concat(frames))
    df = df[df.index >= pd.Timestamp(start, tz="UTC")]
    from .store import pair_path

    path = pair_path(local, interval)
    path.parent.mkdir(parents=True, exist_ok=True)
    df.to_parquet(path)
    rep = coverage_report(df, interval)
    print(
        f"{local}: {rep['bars']:,} bars  {rep['span_days']:.1f}d  "
        f"cov={rep['coverage_pct']}%  {rep['from']} → {rep['to']}  → {path}"
    )
    return df


def fetch_history(pairs: list[str], days: int = 400, interval: int = 1) -> None:
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    print(f"Fetching ≥{days}d of {interval}m bars via Binance Vision → {DATA_DIR}")
    for p in pairs:
        try:
            fetch_binance_vision(p, days=days, interval=interval)
        except Exception as exc:
            print(f"FAILED {p}: {exc}")
