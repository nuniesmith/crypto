"""Import Kraken official OHLCVT CSV / ZIP archives into our parquet cache.

Kraken CSV format (no header):
  timestamp, open, high, low, close, volume, trades

Typical paths inside the ZIP:
  XBTUSD_1.csv, ETHUSD_1.csv, SOLUSD_1.csv  (1-minute)
"""

from __future__ import annotations

from pathlib import Path

import zipfile

import pandas as pd

from .store import save_ohlcv

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
    df = df.rename(columns={"trades": "count"})
    out = save_ohlcv(df, pair, interval)
    full = pd.read_parquet(out)
    print(f"{pair}: {len(full):,} bars  {full.index.min()} → {full.index.max()}  → {out}")
    return full


def import_zip(zip_path: Path, interval: int = 1, pairs: list[str] | None = None) -> None:
    """Extract matching 1m CSVs from a Kraken OHLCVT zip and merge into parquet."""
    zip_path = Path(zip_path)
    wanted = {_normalize_pair(p) for p in pairs} if pairs else None
    suffix = f"_{interval}.csv"
    with zipfile.ZipFile(zip_path) as zf:
        names = [n for n in zf.namelist() if n.lower().endswith(suffix)]
        if not names:
            names = [n for n in zf.namelist() if n.lower().endswith(".csv")]
        for name in names:
            stem = Path(name).stem
            pair = _normalize_pair(stem)
            if wanted and pair not in wanted:
                continue
            if wanted is None and pair not in PAIR_ALIASES and pair not in ("XBTUSD", "ETHUSD", "SOLUSD"):
                continue
            print(f"  {zip_path.name}: {name} → {pair}")
            with zf.open(name) as fh:
                df = pd.read_csv(
                    fh,
                    header=None,
                    names=["time", "open", "high", "low", "close", "volume", "trades"],
                )
            df["time"] = pd.to_datetime(df["time"], unit="s", utc=True)
            for c in ["open", "high", "low", "close", "volume"]:
                df[c] = pd.to_numeric(df[c], errors="coerce")
            df["count"] = pd.to_numeric(df["trades"], errors="coerce").fillna(0).astype(int)
            df = df.dropna(subset=["open", "high", "low", "close"]).set_index("time")
            df = df[~df.index.duplicated(keep="last")]
            out = save_ohlcv(df[["open", "high", "low", "close", "volume", "count"]], pair, interval)
            full = pd.read_parquet(out)
            print(f"    {pair}: {len(full):,} bars  {full.index.min()} → {full.index.max()}")


def import_dir(directory: Path, interval: int = 1, pairs: list[str] | None = None) -> None:
    directory = Path(directory)
    zips = sorted(directory.glob("*.zip")) + sorted(directory.glob("Kraken_OHLCVT*.zip"))
    for z in dict.fromkeys(zips):
        try:
            import_zip(z, interval=interval, pairs=pairs)
        except Exception as exc:
            print(f"FAILED {z}: {exc}")
    pattern = f"*_{interval}.csv"
    files = sorted(directory.rglob(pattern))
    if not files:
        files = sorted(directory.rglob("*.csv"))
    if not files and not zips:
        print(f"No CSV/ZIP files found under {directory}")
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
