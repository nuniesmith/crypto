"""Daily candles for the long-term basket study (basket.py).

Binance's public market-data mirror (data-api.binance.vision): no key, 1,000
daily candles per request, so a coin's whole history is a handful of calls.
USDT-quoted pairs stand in for USD; quote volume (USDT traded per day) is kept
because the study ranks coins by liquidity at each point in time.

Saved to data/<PAIR>_1440m.parquet with the same layout regime.py's `load`
reads (open/high/low/close/volume, UTC daily index), plus `quote_volume`.

Run: `python -m crypto.opt.basket_data` (all coins) or with symbols as args.
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

import pandas as pd
import requests

DATA = Path(__file__).resolve().parents[3] / "data"
API = "https://data-api.binance.vision/api/v3/klines"

# Our pair name -> Binance symbols, oldest first. A coin renamed on Binance
# (MATIC -> POL) is stitched from both so its history is continuous.
SYMBOLS: dict[str, list[str]] = {
    # the live basket
    "BTCUSD": ["BTCUSDT"],
    "ETHUSD": ["ETHUSDT"],
    "SOLUSD": ["SOLUSDT"],
    "LINKUSD": ["LINKUSDT"],
    "XRPUSD": ["XRPUSDT"],
    "INJUSD": ["INJUSDT"],
    # candidates: large caps Kraken lists against USD
    "BNBUSD": ["BNBUSDT"],
    "TRXUSD": ["TRXUSDT"],
    "DOGEUSD": ["DOGEUSDT"],
    "ADAUSD": ["ADAUSDT"],
    "AVAXUSD": ["AVAXUSDT"],
    "DOTUSD": ["DOTUSDT"],
    "LTCUSD": ["LTCUSDT"],
    "BCHUSD": ["BCHUSDT"],
    "XLMUSD": ["XLMUSDT"],
    "HBARUSD": ["HBARUSDT"],
    "NEARUSD": ["NEARUSDT"],
    "UNIUSD": ["UNIUSDT"],
    "AAVEUSD": ["AAVEUSDT"],
    "ZECUSD": ["ZECUSDT"],
    "XMRUSD": ["XMRUSDT"],
    "ATOMUSD": ["ATOMUSDT"],
    "ICPUSD": ["ICPUSDT"],
    "FILUSD": ["FILUSDT"],
    "ETCUSD": ["ETCUSDT"],
    "POLUSD": ["MATICUSDT", "POLUSDT"],
    "SHIBUSD": ["SHIBUSDT"],
    "ALGOUSD": ["ALGOUSDT"],
    "APTUSD": ["APTUSDT"],
    "ARBUSD": ["ARBUSDT"],
    "OPUSD": ["OPUSDT"],
    "SUIUSD": ["SUIUSDT"],
    "TAOUSD": ["TAOUSDT"],
    "TONUSD": ["TONUSDT"],
    "ONDOUSD": ["ONDOUSDT"],
    "ENAUSD": ["ENAUSDT"],
    "PEPEUSD": ["PEPEUSDT"],
    "WLDUSD": ["WLDUSDT"],
    "RENDERUSD": ["RNDRUSDT", "RENDERUSDT"],
    "FETUSD": ["FETUSDT"],
    # coins that collapsed or were delisted, so a systematic "add the
    # biggest coins" rule meets them the way it would have in real time
    "LUNAUSD": ["LUNAUSDT"],
    "FTTUSD": ["FTTUSDT"],
    "EOSUSD": ["EOSUSDT"],
}

START_MS = int(pd.Timestamp("2017-07-01", tz="UTC").timestamp() * 1000)
COLUMNS = [
    "open_time",
    "open",
    "high",
    "low",
    "close",
    "volume",
    "close_time",
    "quote_volume",
    "trades",
    "taker_base",
    "taker_quote",
    "ignore",
]


def fetch_symbol(symbol: str) -> pd.DataFrame:
    """Every daily candle Binance has for `symbol` (empty if it has none)."""
    rows: list[list] = []
    start = START_MS
    while True:
        r = requests.get(
            API,
            params={
                "symbol": symbol,
                "interval": "1d",
                "startTime": start,
                "limit": 1000,
            },
            timeout=30,
        )
        if r.status_code == 400:  # unknown or retired symbol
            break
        r.raise_for_status()
        batch = r.json()
        if not batch:
            break
        rows.extend(batch)
        if len(batch) < 1000:
            break
        start = batch[-1][0] + 86_400_000
        time.sleep(0.2)
    if not rows:
        return pd.DataFrame()
    df = pd.DataFrame(rows, columns=COLUMNS)
    df.index = pd.to_datetime(df["open_time"], unit="ms", utc=True).dt.normalize()
    df = df[["open", "high", "low", "close", "volume", "quote_volume"]].astype(float)
    # Today's candle is still forming: a close that is not a close.
    today = pd.Timestamp.now(tz="UTC").normalize()
    return df[df.index < today]


def fetch(pair: str) -> pd.DataFrame:
    parts = [fetch_symbol(s) for s in SYMBOLS[pair]]
    parts = [p for p in parts if not p.empty]
    if not parts:
        return pd.DataFrame()
    df = pd.concat(parts)
    return df[~df.index.duplicated(keep="last")].sort_index()


def main(pairs: list[str]) -> None:
    DATA.mkdir(exist_ok=True)
    for pair in pairs:
        df = fetch(pair)
        if df.empty:
            print(f"{pair:10} no data")
            continue
        df.to_parquet(DATA / f"{pair}_1440m.parquet")
        print(
            f"{pair:10} {df.index[0].date()} .. {df.index[-1].date()}  {len(df):5} days",
            flush=True,
        )


if __name__ == "__main__":
    main(sys.argv[1:] or list(SYMBOLS))
