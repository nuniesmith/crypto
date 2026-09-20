"""Local parquet + simple status helpers."""

from __future__ import annotations

import os
from pathlib import Path

import pandas as pd

OHLCV_COLS = ["open", "high", "low", "close", "vwap", "volume", "count"]


def _default_data_dir() -> Path:
    env = os.environ.get("CRYPTO_DATA_DIR")
    if env:
        return Path(env).expanduser().resolve()
    # src/crypto/data/store.py → repo root
    return Path(__file__).resolve().parents[3] / "data"


DATA_DIR = _default_data_dir()


def pair_path(pair: str, interval: int = 1) -> Path:
    return DATA_DIR / f"{pair}_{interval}m.parquet"


def normalize_ohlcv(df: pd.DataFrame) -> pd.DataFrame:
    """UTC DatetimeIndex + the columns the simulator expects."""
    out = df.copy()
    if not isinstance(out.index, pd.DatetimeIndex):
        if "time" in out.columns:
            out["time"] = pd.to_datetime(out["time"], utc=True, errors="coerce")
            out = out.set_index("time")
        else:
            out.index = pd.to_datetime(out.index, utc=True, errors="coerce")
    if out.index.tz is None:
        out.index = out.index.tz_localize("UTC")
    else:
        out.index = out.index.tz_convert("UTC")
    out = out[~out.index.isna()].sort_index()
    out = out[~out.index.duplicated(keep="last")]
    if "count" not in out.columns and "trades" in out.columns:
        out["count"] = out["trades"]
    if "vwap" not in out.columns:
        vol = out["volume"].replace(0, pd.NA) if "volume" in out.columns else pd.NA
        typical = (out["high"] + out["low"] + out["close"]) / 3.0
        out["vwap"] = typical
        if "volume" in out.columns:
            out.loc[out["volume"] <= 0, "vwap"] = typical
    for c in ["open", "high", "low", "close", "vwap", "volume"]:
        if c in out.columns:
            out[c] = pd.to_numeric(out[c], errors="coerce")
    if "count" in out.columns:
        out["count"] = pd.to_numeric(out["count"], errors="coerce").fillna(0).astype(int)
    else:
        out["count"] = 0
    keep = [c for c in OHLCV_COLS if c in out.columns]
    extra = [c for c in out.columns if c not in keep]
    out = out[keep + extra]
    return out.dropna(subset=["open", "high", "low", "close"])


def save_ohlcv(df: pd.DataFrame, pair: str, interval: int = 1) -> Path:
    path = pair_path(pair, interval)
    path.parent.mkdir(parents=True, exist_ok=True)
    df = normalize_ohlcv(df)
    if path.exists():
        old = normalize_ohlcv(pd.read_parquet(path))
        df = pd.concat([old, df])
        df = df[~df.index.duplicated(keep="last")].sort_index()
    df.to_parquet(path)
    return path


def resample_ohlcv(df: pd.DataFrame, minutes: int) -> pd.DataFrame:
    """Downsample 1-minute OHLCV to a coarser bar."""
    if minutes <= 1:
        return df
    ohlc = df.resample(f"{minutes}min").agg(
        {
            "open": "first",
            "high": "max",
            "low": "min",
            "close": "last",
            "volume": "sum",
            "count": "sum",
        }
    )
    if "vwap" in df.columns and "volume" in df.columns:
        pv = (df["vwap"] * df["volume"]).resample(f"{minutes}min").sum()
        vol = df["volume"].resample(f"{minutes}min").sum().replace(0, pd.NA)
        ohlc["vwap"] = (pv / vol).astype(float)
    ohlc = ohlc.dropna(subset=["open", "high", "low", "close"])
    return normalize_ohlcv(ohlc)


def load_ohlcv(pair: str, days: int | None = None, interval: int = 1) -> pd.DataFrame:
    path = pair_path(pair, interval)
    if not path.exists():
        raise FileNotFoundError(
            f"No cache for {pair}. Run: crypto fetch-history --pair {pair} --days {days or 365}"
        )
    df = normalize_ohlcv(pd.read_parquet(path))
    if days is not None:
        cutoff = df.index.max() - pd.Timedelta(days=days)
        df = df[df.index >= cutoff]
    return df


def coverage_report(df: pd.DataFrame, interval_minutes: int = 1) -> dict:
    if df.empty:
        return {"bars": 0, "span_days": 0.0, "expected": 0, "missing": 0, "coverage_pct": 0.0}
    delta = pd.Timedelta(minutes=interval_minutes)
    span = df.index.max() - df.index.min()
    expected = int(span / delta) + 1
    missing = max(expected - len(df), 0)
    return {
        "bars": int(len(df)),
        "span_days": round(span.total_seconds() / 86400, 2),
        "expected": expected,
        "missing": missing,
        "coverage_pct": round(100.0 * len(df) / expected, 2) if expected else 0.0,
        "from": str(df.index.min()),
        "to": str(df.index.max()),
    }


def status() -> None:
    if not DATA_DIR.exists():
        print("No data/ directory yet. Run `crypto fetch-history` first.")
        return
    files = sorted(DATA_DIR.glob("*m.parquet"))
    if not files:
        print("data/ is empty.")
        return
    print(f"{'pair':12} {'bars':>10} {'days':>8} {'cov%':>7} {'from':>22} {'to':>22}")
    for f in files:
        df = normalize_ohlcv(pd.read_parquet(f))
        iv = 1
        stem = f.stem
        if stem.endswith("m"):
            try:
                iv = int(stem.rsplit("_", 1)[-1][:-1])
            except ValueError:
                iv = 1
        rep = coverage_report(df, iv)
        print(
            f"{f.stem:12} {rep['bars']:10,} {rep['span_days']:8.1f} "
            f"{rep['coverage_pct']:6.1f}% {rep['from']:>22} {rep['to']:>22}"
        )
