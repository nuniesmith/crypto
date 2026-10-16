"""Forward test: the basket study's portfolios, from 2026-10-16 onward only.

Everything in basket_study.py was measured on history that was known when the
study was written (data through 2026-10-15). This module is the part that was
NOT: the same engine, the same rules, frozen here before any of the forward
data existed, run only on daily candles from FORWARD_START on. Nothing in it
may be tuned after the fact; if a rule changes, start a new forward test with
a new start date instead of editing this one.

Prices come from Kraken's public OHLC (the venue the bot trades on; no key, no
nonce). Kraken returns the last 720 daily candles, which is enough history for
the 200-day regime as of the start, and covers about 18 months of forward data.

Each portfolio starts with $250 on FORWARD_START and adds $50 every 7 days,
the same deposit stream the study used. `--actual PATH` also reads the live
bot's state.json (read-only) and compares its real daily account value with
the simulated live policy over the same period.

Run: `python -m crypto.opt.forward [--actual /home/actions/oryx/data/crypto-bot/paper/state.json]`
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np
import pandas as pd
import requests

from crypto.opt.basket import (
    BTC_ONLY,
    LIVE_V3,
    Panel,
    Policy,
    simulate,
    sma200_band,
    summarize,
)

FORWARD_START = "2026-10-16"  # the first day the basket study never saw
INITIAL = 250.0
WEEKLY = 50.0

KRAKEN_PAIRS = {
    "BTCUSD": "XBTUSD",
    "ETHUSD": "ETHUSD",
    "SOLUSD": "SOLUSD",
    "LINKUSD": "LINKUSD",
    "XRPUSD": "XRPUSD",
    "INJUSD": "INJUSD",
}
MAJORS_93 = {
    "BTCUSD": 0.44 / 0.78 * 0.93,
    "ETHUSD": 0.22 / 0.78 * 0.93,
    "SOLUSD": 0.12 / 0.78 * 0.93,
}

# Frozen 2026-10-16. Do not edit: a changed rule is a new forward test.
PORTFOLIOS = [
    Policy("live v3 (trend)", LIVE_V3),
    Policy("BTC/ETH/SOL only (trend)", MAJORS_93),
    Policy("BTC only (trend)", BTC_ONLY),
    Policy("same weights, no trend", LIVE_V3, trend=False),
    Policy("BTC only, no trend", BTC_ONLY, trend=False),
]


def kraken_daily(pair: str) -> pd.DataFrame:
    r = requests.get(
        "https://api.kraken.com/0/public/OHLC",
        params={"pair": pair, "interval": 1440},
        timeout=30,
    )
    r.raise_for_status()
    body = r.json()
    if body.get("error"):
        raise RuntimeError(f"Kraken OHLC {pair}: {body['error']}")
    rows = next(v for k, v in body["result"].items() if k != "last")
    df = pd.DataFrame(
        rows,
        columns=["time", "open", "high", "low", "close", "vwap", "volume", "count"],
    )
    df.index = pd.to_datetime(df["time"], unit="s", utc=True)
    df = df[["open", "close", "vwap", "volume"]].astype(float)
    df["quote_volume"] = df["vwap"] * df["volume"]
    today = pd.Timestamp.now(tz="UTC").normalize()
    return df[df.index < today]  # today's candle is still forming


def kraken_panel() -> Panel:
    frames = {ours: kraken_daily(k) for ours, k in KRAKEN_PAIRS.items()}
    start = max(f.index.min() for f in frames.values())
    end = min(f.index.max() for f in frames.values())
    idx = pd.date_range(start, end, freq="D", tz="UTC")
    pairs = list(frames)
    n, k = len(idx), len(pairs)
    o = np.full((n, k), np.nan)
    c = np.full((n, k), np.nan)
    v = np.zeros((n, k))
    for j, p in enumerate(pairs):
        f = frames[p].reindex(idx).ffill()
        o[:, j] = f["open"].to_numpy()
        c[:, j] = f["close"].to_numpy()
        v[:, j] = f["quote_volume"].fillna(0.0).to_numpy()
    bull = np.full((n, k), np.nan)
    for j in range(k):
        bull[1:, j] = sma200_band(c[:, j])[:-1]
    return Panel(idx, pairs, o, c, v, bull, np.ones((n, k), dtype=bool))


def actual_account(state_path: Path, start: pd.Timestamp) -> pd.DataFrame | None:
    """The live account's daily value and deposits since `start`, read-only."""
    try:
        s = json.loads(state_path.read_text())
    except (OSError, ValueError) as e:
        print(f"(actual account not read: {e})")
        return None
    hist = pd.DataFrame(s.get("history", []))
    if hist.empty:
        return None
    hist.index = pd.to_datetime(hist["date"]).dt.tz_localize("UTC")
    hist = hist.loc[start:, ["total_usd"]]
    flows = pd.DataFrame(s.get("flows", []))
    if not flows.empty:
        flows["day"] = pd.to_datetime(flows["ts"], unit="s", utc=True).dt.normalize()
        sign = np.where(flows["kind"] == "deposit", 1.0, -1.0)
        flows["usd"] = sign * flows["usd_value"]
        daily = flows.groupby("day")["usd"].sum()
        hist["deposits"] = daily.reindex(hist.index).fillna(0.0)
    else:
        hist["deposits"] = 0.0
    return hist


def main(argv: list[str]) -> None:
    panel = kraken_panel()
    start = pd.Timestamp(FORWARD_START, tz="UTC")
    if "--start" in argv:  # mechanics checks only: not the pre-registered test
        start = pd.Timestamp(argv[argv.index("--start") + 1], tz="UTC")
        print(
            f"NOTE: --start {start.date()} overrides the pre-registered {FORWARD_START}; not a forward result"
        )
    last = panel.index[-1]
    if last < start:
        print(
            f"Forward test starts {FORWARD_START}; Kraken's last closed day is {last.date()}. Nothing to score yet."
        )
        return
    days = (last - start).days + 1
    print(
        f"Forward test {start.date()} .. {last.date()} ({days} days), ${INITIAL:.0f} + ${WEEKLY:.0f}/week"
    )
    rows = []
    for pol in PORTFOLIOS:
        r = simulate(panel, pol, start, None, initial=INITIAL, weekly=WEEKLY)
        m = summarize(r)
        rows.append(
            {
                "portfolio": pol.name,
                "paid_in": round(float(r.contributed[-1]), 2),
                "value": round(float(r.value[-1]), 2),
                "gain": round(float(r.value[-1] / r.contributed[-1] - 1), 4),
                "nav_return": round(float(r.nav[-1] - 1), 4),
                "nav_max_dd": m["nav_max_dd"],
                "trades": r.trades,
            }
        )
    print(pd.DataFrame(rows).to_string(index=False))
    regimes = {
        p.replace("USD", ""): ("bull" if panel.bull[-1, j] == 1.0 else "bear")
        for j, p in enumerate(panel.pairs)
    }
    print("regimes for the next day:", regimes)

    if "--actual" in argv:
        path = Path(argv[argv.index("--actual") + 1])
        acct = actual_account(path, start)
        if acct is not None:
            acct = acct.loc[: panel.index[-1]]  # only days with a closed candle
        if acct is not None and len(acct) > 1:
            # The live policy replayed with the account's REAL starting value
            # and deposits: what's left between the two is execution (fills,
            # timing, fees), not the deposit stream.
            schedule = {acct.index[0]: float(acct["total_usd"].iloc[0])}
            for day, dep in acct["deposits"].iloc[1:].items():
                if dep:
                    schedule[day] = schedule.get(day, 0.0) + float(dep)
            r = simulate(
                panel, PORTFOLIOS[0], acct.index[0], acct.index[-1], schedule=schedule
            )
            paid = sum(schedule.values())
            real = float(acct["total_usd"].iloc[-1])
            model = float(r.value[-1])
            print(
                f"actual account {acct.index[0].date()} .. {acct.index[-1].date()}: paid in ${paid:.2f}, "
                f"real ${real:.2f} ({real / paid - 1:+.2%}), model ${model:.2f} ({model / paid - 1:+.2%}), "
                f"real minus model ${real - model:+.2f}"
            )


if __name__ == "__main__":
    main(sys.argv[1:])
