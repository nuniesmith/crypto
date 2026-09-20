"""Import Kraken official OHLCVT CSV / ZIP archives into our parquet cache.

Kraken CSV format (no header):
  timestamp, open, high, low, close, volume, trades

Typical paths inside the ZIP:
  XBTUSD_1.csv, ETHUSD_1.csv, SOLUSD_1.csv  (1-minute)
"""

from __future__ import annotations

from pathlib import Path

import pandas as pd

from .store import DATA_DIR, pair_path

PAIR_ALIASES = {
    "XBTUSD": "XBTUSD",
    "XXBTZUSD": "XBTUSD",
    "BTCUSD": "XBTUSD",
    "ETHUSD": "ETHUSD",
    "XETHZUSD": "ETHUSD",
    "SOLUSD": "SOLUSD",
}


def _normalize_pair(name: str) -> str:
    n = name.upper().replace("/", "").replace("-", "").replace("_1", "").replace(".CSV", "")
    for suf in ("_1", "_5", "_15", "_60", "_1440"):
        if n.endswith(suf):
            n = n[: -len(suf)]
    return PAIR_ALIASES.get(n, n)


def import_csv(csv_path: Path, pair: str | None = None, interval: int = 1) -> pd.DataFrame:
    csv_path = Path(csv_path)
    if pair is None:
        pair = _normalize_pair(csv_path.stem)
    else:
        pair = _normalize_pair(pair)

    df = pd.read_csv(
        csv_path,
        header=None,
        names=["time", "open", "high", "low", "close", "volume", "trades"],
    )
    df["time"] = pd.to_datetime(df["time"], unit="s", utc=True)
    for c in ["open", "high", "low", "close", "volume"]:
        df[c] = pd.to_numeric(df[c], errors="coerce")
    df["trades"] = pd.to_numeric(df["trades"], errors="coerce").fillna(0).astype(int)
    df = df.dropna(subset=["open", "high", "low", "close"])
    df = df.set_index("time").sort_index()
    df = df[~df.index.duplicated(keep="last")]

    out = pair_path(pair, interval)
    out.parent.mkdir(parents=True, exist_ok=True)
    df.to_parquet(out)
    print(f"{pair}: {len(df):,} bars  {df.index.min()} → {df.index.max()}  → {out}")
    return df


def import_dir(directory: Path, interval: int = 1, pairs: list[str] | None = None) -> None:
    directory = Path(directory)
    pattern = f"*_{interval}.csv"
    files = sorted(directory.rglob(pattern))
    if not files:
        files = sorted(directory.rglob("*.csv"))
    if not files:
        print(f"No CSV files found under {directory}")
        return

    wanted = None
    if pairs:
        wanted = {_normalize_pair(p) for p in pairs}

    for f in files:
        p = _normalize_pair(f.stem)
        if wanted and p not in wanted:
            continue
        try:
            import_csv(f, pair=p, interval=interval)
        except Exception as exc:
            print(f"FAILED {f}: {exc}")
