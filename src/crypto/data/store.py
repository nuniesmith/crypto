"""Local parquet + simple status helpers."""

from __future__ import annotations

from pathlib import Path

import pandas as pd

DATA_DIR = Path("data")


def pair_path(pair: str, interval: int = 1) -> Path:
    return DATA_DIR / f"{pair}_{interval}m.parquet"


def load_ohlcv(pair: str, days: int | None = None, interval: int = 1) -> pd.DataFrame:
    path = pair_path(pair, interval)
    if not path.exists():
        raise FileNotFoundError(
            f"No cache for {pair}. Run: crypto fetch --pair {pair} --days {days or 7}"
        )
    df = pd.read_parquet(path)
    if days is not None:
        cutoff = df.index.max() - pd.Timedelta(days=days)
        df = df[df.index >= cutoff]
    return df


def status() -> None:
    if not DATA_DIR.exists():
        print("No data/ directory yet. Run `crypto fetch` first.")
        return
    files = sorted(DATA_DIR.glob("*m.parquet"))
    if not files:
        print("data/ is empty.")
        return
    print(f"{'pair':12} {'bars':>10} {'from':>22} {'to':>22}")
    for f in files:
        df = pd.read_parquet(f)
        print(
            f"{f.stem:12} {len(df):10,} "
            f"{str(df.index.min()):>22} {str(df.index.max()):>22}"
        )
