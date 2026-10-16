"""Long-term basket study: the live policy with weekly deposits, and which
coins (if any) belong in it.

The live bot (policy v3, bot/src/alloc.rs + regime.rs) holds BTC 44 / ETH 22 /
SOL 12 / LINK 5 / XRP 5 / INJ 5, cash the rest (7% when everything is bull).
Each coin follows the daily 200-day-average rule with a ±5% band; a coin in a
bear holds half its target (the core). It trades a coin only when that coin's
regime flips (closing the gap to its new target, either side), and invests
each deposit BUY-ONLY into whatever the bigger total makes underweight. There
is no drift rebalancing: a coin that runs up is never trimmed in a bull.

`simulate` replays exactly that, a day at a time, for any set of base
weights, with a weekly deposit:

  - day d's decisions use closes up to d-1 (the bot reads the regime at 00:05
    UTC from the last CLOSED daily candle) and fill at day d's open;
  - every coin trade pays Kraken tier-1 maker 0.40% plus 0.10% slippage, and
    each deposit pays 0.2% to turn USDC into USD, as the live account does;
  - an order under $5 is not placed (Kraken's minimums are ~$4-5 per coin);
  - buys are capped by the cash on hand, in the bot's pair order, and a flip
    that cash cannot fully fund is retried on later days, as the bot does.

Benchmarks run through the same engine: the same weights with no trend rule
(the bot minus regime), BTC alone, and v2 (BTC/ETH/SOL before the satellites).

Run: `python -m crypto.opt.basket` (needs basket_data.py's parquet files).
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
import pandas as pd

DATA = Path(__file__).resolve().parents[3] / "data"

FEE = 0.0040 + 0.0010  # tier-1 maker + slippage, on every coin trade
CONVERSION = 0.002  # USDC -> USD on each deposit (the live account's real cost)
MIN_ORDER = 5.0  # USD; Kraken's ordermin x price is $4-5.5 on every pair
SMA_DAYS = 200
BAND = 0.05
CORE = 0.50
FEE_RESERVE = 0.99  # a buy spends at most 99% of free cash, as the bot does

# Coins whose Binance ticker outlived them: price is cut at the collapse and
# held at its last print, so the holding is worth what it really became.
DEAD_AFTER = {"LUNAUSD": "2022-05-13"}

LIVE_V3 = {
    "BTCUSD": 0.44,
    "ETHUSD": 0.22,
    "SOLUSD": 0.12,
    "LINKUSD": 0.05,
    "XRPUSD": 0.05,
    "INJUSD": 0.05,
}
V2 = {"BTCUSD": 0.50, "ETHUSD": 0.25, "SOLUSD": 0.15}
BTC_ONLY = {"BTCUSD": 1.0}


# ─── data ─────────────────────────────────────────────────────────────────────


def load(pair: str) -> pd.DataFrame:
    df = pd.read_parquet(DATA / f"{pair}_1440m.parquet")
    df = df[["open", "close", "quote_volume"]].astype(float)
    if pair in DEAD_AFTER:
        df = df.loc[: DEAD_AFTER[pair]]
    return df


@dataclass
class Panel:
    """Aligned daily opens/closes/volumes and trend regimes for many coins."""

    index: pd.DatetimeIndex
    pairs: list[str]
    open: np.ndarray  # [day, coin], NaN before listing
    close: np.ndarray
    volume: np.ndarray  # USDT traded that day
    bull: np.ndarray  # regime decided for day d from closes to d-1; NaN unknown
    listed: np.ndarray  # bool: has a price that day


def sma200_band(close: np.ndarray) -> np.ndarray:
    """regime.py's rule (identical to bot/src/regime.rs): bull above SMA200
    x(1+band), bear below x(1-band), unchanged between; first read is
    close > SMA. NaN until 200 closes exist."""
    out = np.full(close.shape, np.nan)
    s = pd.Series(close)
    m = s.rolling(SMA_DAYS, min_periods=SMA_DAYS).mean().to_numpy()
    state = np.nan
    for i in range(len(close)):
        c, a = close[i], m[i]
        if np.isnan(a) or np.isnan(c):
            continue
        if np.isnan(state):
            state = 1.0 if c > a else 0.0
        elif c > a * (1 + BAND):
            state = 1.0
        elif c < a * (1 - BAND):
            state = 0.0
        out[i] = state
    return out


def build_panel(pairs: list[str]) -> Panel:
    frames = {p: load(p) for p in pairs}
    start = min(f.index.min() for f in frames.values())
    end = max(f.index.max() for f in frames.values())
    idx = pd.date_range(start, end, freq="D", tz="UTC")
    n, k = len(idx), len(pairs)
    o = np.full((n, k), np.nan)
    c = np.full((n, k), np.nan)
    v = np.zeros((n, k))
    listed = np.zeros((n, k), dtype=bool)
    for j, p in enumerate(pairs):
        f = frames[p].reindex(idx)
        first, last = frames[p].index.min(), frames[p].index.max()
        inside = (idx >= first) & (idx <= last)
        # a missing day inside a coin's life is "no news": carry the close
        f.loc[inside] = f.loc[inside].ffill()
        if last < end:
            # The data stops before the panel does. A dead coin (DEAD_AFTER)
            # is worth its last print forever; a coin that is alive but left
            # Binance (XMR delisted 2024-02, EOS renamed to A 2025-05, TON
            # 2026-06) is held at its last price rather than counted as $0.
            # Either way it is never traded again (`listed` stays False).
            after = idx > last
            f.loc[after, ["open", "close"]] = frames[p]["close"].iloc[-1]
        o[:, j] = f["open"].to_numpy()
        c[:, j] = f["close"].to_numpy()
        v[:, j] = f["quote_volume"].fillna(0.0).to_numpy()
        listed[:, j] = inside
    bull = np.full((n, k), np.nan)
    for j in range(k):
        reg = np.full(n, np.nan)
        live = listed[:, j]
        if live.any():
            reg_live = sma200_band(np.where(live, c[:, j], np.nan)[live])
            reg[live] = reg_live
        # decided at 00:05 UTC on day d from the candle that closed at d-1
        bull[1:, j] = reg[:-1]
    return Panel(idx, pairs, o, c, v, bull, listed)


def utc(x: str | pd.Timestamp) -> pd.Timestamp:
    t = pd.Timestamp(x)
    return t.tz_localize("UTC") if t.tzinfo is None else t.tz_convert("UTC")


# ─── the policy ───────────────────────────────────────────────────────────────

WeightsFn = Callable[[int], dict[str, float]]


@dataclass
class Policy:
    name: str
    weights: dict[str, float] | WeightsFn  # base (bull) weight per pair
    trend: bool = True  # False = the same bot with every coin always "bull"
    core: float = CORE


@dataclass
class Result:
    name: str
    index: pd.DatetimeIndex
    value: np.ndarray
    contributed: np.ndarray
    nav: np.ndarray  # time-weighted unit value, 1.0 at the start
    flows: list[tuple[pd.Timestamp, float]]
    trades: int
    fees: float
    cash_share: np.ndarray
    holdings: pd.DataFrame = field(default_factory=pd.DataFrame)
    per_coin: pd.DataFrame = field(default_factory=pd.DataFrame)


def simulate(
    panel: Panel,
    policy: Policy,
    start: str | pd.Timestamp,
    end: str | pd.Timestamp | None = None,
    initial: float = 250.0,
    weekly: float = 50.0,
    fee: float = FEE,
    schedule: dict[pd.Timestamp, float] | None = None,
) -> Result:
    """Replay `policy` from `start`. Deposits are `initial` on day one and
    `weekly` every 7th day after, unless `schedule` (UTC day -> USD) gives
    the real deposit stream, e.g. the live account's own."""
    fee_rate = fee
    idx = panel.index
    i0 = int(idx.searchsorted(utc(start)))
    i1 = len(idx) if end is None else int(idx.searchsorted(utc(end), "right"))
    k = len(panel.pairs)
    col = {p: j for j, p in enumerate(panel.pairs)}

    units = np.zeros(k)
    bought = np.zeros(k)  # USD spent buying each coin (fees included)
    sold = np.zeros(k)  # USD received selling it (after fees)
    cash = 0.0
    applied = [None] * k  # regime last acted on per coin (None = never)
    deposit_pending = False
    trades = 0
    fees = 0.0
    nav_units = 0.0

    days = i1 - i0
    value = np.zeros(days)
    contributed = np.zeros(days)
    nav = np.zeros(days)
    cash_share = np.zeros(days)
    flows: list[tuple[pd.Timestamp, float]] = []
    held_rows = []
    total_in = 0.0

    def base_weights(i: int) -> dict[str, float]:
        w = policy.weights(i) if callable(policy.weights) else policy.weights
        return w

    prev_bases: dict[str, float] = {}
    for t, i in enumerate(range(i0, i1)):
        px_open = panel.open[i]
        # ── 1. deposit (weekly, starting day 0 with the initial amount) ──
        if schedule is not None:
            amount = float(schedule.get(idx[i], 0.0))
        else:
            amount = initial if t == 0 else (weekly if t % 7 == 0 else 0.0)
        if amount > 0:
            # issue fund units at the pre-deposit value (time-weighted NAV)
            pre = cash + np.nansum(units * np.nan_to_num(px_open))
            unit_value = pre / nav_units if nav_units > 0 else 1.0
            nav_units += amount / unit_value
            cash += amount * (1 - CONVERSION)
            total_in += amount
            flows.append((idx[i], amount))
            deposit_pending = True

        bases = base_weights(i)
        # a coin leaving the basket: sell it all; a coin joining: a gap to close
        for p in prev_bases:
            if p not in bases:
                j = col[p]
                if units[j] > 0 and panel.listed[i, j] and not np.isnan(px_open[j]):
                    proceeds = units[j] * px_open[j]
                    if proceeds >= MIN_ORDER:
                        fee = proceeds * fee_rate
                        cash += proceeds - fee
                        sold[j] += proceeds - fee
                        fees += fee
                        trades += 1
                        units[j] = 0.0
                applied[j] = None
        for p in bases:
            if p not in prev_bases:
                applied[col[p]] = None
        prev_bases = dict(bases)

        total = cash + np.nansum(units * np.nan_to_num(px_open))
        spendable = cash

        def target_of(j: int, bull: float, bases=bases, total=total) -> float:
            w = bases.get(panel.pairs[j], 0.0)
            if policy.trend and bull == 0.0:
                w *= policy.core
            return w * total

        # ── 2. flips (and coins never applied): close the gap, either side ──
        order = [col[p] for p in bases]
        for j in order:
            if not panel.listed[i, j] or np.isnan(px_open[j]):
                continue
            b = panel.bull[i, j] if policy.trend else 1.0
            if np.isnan(b):
                continue  # regime not read yet: the bot leaves it alone
            if applied[j] == b:
                continue
            price = px_open[j]
            delta = target_of(j, b) - units[j] * price
            if delta > 0:
                spend = min(delta, max(spendable, 0.0) * FEE_RESERVE)
                if spend < MIN_ORDER:
                    if delta < MIN_ORDER:
                        applied[j] = b  # already within a minimum order
                    continue  # under-funded: retry another day
                fee = spend * fee_rate
                units[j] += (spend - fee) / price
                bought[j] += spend
                cash -= spend
                spendable -= spend
                fees += fee
                trades += 1
                if delta - spend < MIN_ORDER:
                    applied[j] = b
            else:
                q = min(-delta / price, units[j])
                if q * price < MIN_ORDER:
                    applied[j] = b
                    continue
                proceeds = q * price
                fee = proceeds * fee_rate
                units[j] -= q
                cash += proceeds - fee
                sold[j] += proceeds - fee
                fees += fee
                trades += 1
                applied[j] = b

        # ── 3. deposit top-ups: BUY-ONLY into applied coins under target ──
        if deposit_pending:
            wanted_any = False
            for j in order:
                if not panel.listed[i, j] or np.isnan(px_open[j]):
                    continue
                b = panel.bull[i, j] if policy.trend else 1.0
                if np.isnan(b) or applied[j] != b:
                    continue
                price = px_open[j]
                delta = target_of(j, b) - units[j] * price
                if delta <= 0:
                    continue
                spend = min(delta, max(spendable, 0.0) * FEE_RESERVE)
                if spend < MIN_ORDER:
                    continue
                fee = spend * fee_rate
                units[j] += (spend - fee) / price
                bought[j] += spend
                cash -= spend
                spendable -= spend
                fees += fee
                trades += 1
                wanted_any = True
            if not wanted_any:
                deposit_pending = False

        # ── 4. mark to the close ──
        px_close = np.nan_to_num(panel.close[i])
        v = cash + float(np.sum(units * px_close))
        value[t] = v
        contributed[t] = total_in
        nav[t] = v / nav_units if nav_units > 0 else 1.0
        cash_share[t] = cash / v if v > 0 else 1.0
        if t % 7 == 0 or i == i1 - 1:
            held_rows.append((idx[i], *(units * px_close), cash))

    final_px = np.nan_to_num(panel.close[i1 - 1])
    per_coin = pd.DataFrame(
        {"bought": bought, "sold": sold, "final": units * final_px},
        index=panel.pairs,
    )
    per_coin["pnl"] = per_coin["final"] + per_coin["sold"] - per_coin["bought"]
    per_coin = per_coin[(per_coin["bought"] > 0) | (per_coin["final"] > 0)]
    holdings = pd.DataFrame(
        [r[1:] for r in held_rows],
        index=pd.DatetimeIndex([r[0] for r in held_rows]),
        columns=[*panel.pairs, "cash"],
    )
    return Result(
        policy.name,
        idx[i0:i1],
        value,
        contributed,
        nav,
        flows,
        trades,
        fees,
        cash_share,
        holdings,
        per_coin,
    )


# ─── metrics ──────────────────────────────────────────────────────────────────


def xirr(
    flows: list[tuple[pd.Timestamp, float]], end: pd.Timestamp, final: float
) -> float:
    """Money-weighted annual return: the rate at which the deposits grow into
    `final`. Solved by bisection (deposits are outflows, the end value an
    inflow)."""
    t = np.array([(end - d).days / 365.25 for d, _ in flows])
    a = np.array([amt for _, amt in flows])

    def fv(r: float) -> float:
        return float(np.sum(a * (1 + r) ** t)) - final

    lo, hi = -0.99, 10.0
    if fv(lo) > 0 or fv(hi) < 0:
        return float("nan")
    for _ in range(200):
        mid = (lo + hi) / 2
        if fv(mid) > 0:
            hi = mid
        else:
            lo = mid
    return (lo + hi) / 2


def max_drawdown(series: np.ndarray) -> float:
    peak = np.maximum.accumulate(series)
    return float(np.min(series / peak - 1))


def summarize(r: Result) -> dict[str, float | str | int]:
    years = (r.index[-1] - r.index[0]).days / 365.25
    final = float(r.value[-1])
    paid = float(r.contributed[-1])
    nav = r.nav
    daily = np.diff(np.log(np.maximum(nav, 1e-12)))
    vol = float(np.std(daily) * np.sqrt(365)) if len(daily) > 1 else float("nan")
    cagr = float(nav[-1] ** (1 / years) - 1) if years > 0 else float("nan")
    # how far the account fell below what had been paid in, at worst
    underwater = float(np.min(r.value / np.maximum(r.contributed, 1e-9) - 1))
    return {
        "policy": r.name,
        "years": round(years, 2),
        "paid_in": round(paid, 0),
        "final": round(final, 0),
        "multiple": round(final / paid, 3),
        "xirr": round(xirr(r.flows, r.index[-1], final), 4),
        "nav_cagr": round(cagr, 4),
        "nav_max_dd": round(max_drawdown(nav), 4),
        "vol": round(vol, 4),
        "worst_vs_paid": round(underwater, 4),
        "trades_per_year": round(r.trades / years, 1) if years > 0 else 0,
        "fees_pct_paid": round(r.fees / paid, 4),
        "avg_cash": round(float(np.mean(r.cash_share)), 3),
    }


if __name__ == "__main__":
    pairs = sorted(set(LIVE_V3) | set(V2))
    panel = build_panel(pairs)
    start = "2021-06-01"
    no_sats = {p: w / sum(V2.values()) * 0.93 for p, w in V2.items()}
    rows = [
        summarize(simulate(panel, Policy("v3 live (trend)", LIVE_V3), start)),
        summarize(
            simulate(panel, Policy("v3 weights, no trend", LIVE_V3, trend=False), start)
        ),
        summarize(simulate(panel, Policy("v2 BTC/ETH/SOL (trend)", V2), start)),
        summarize(
            simulate(panel, Policy("BTC/ETH/SOL at 93% (trend)", no_sats), start)
        ),
        summarize(
            simulate(panel, Policy("BTC only, no trend", BTC_ONLY, trend=False), start)
        ),
        summarize(simulate(panel, Policy("BTC only (trend)", BTC_ONLY), start)),
    ]
    print(pd.DataFrame(rows).to_string(index=False))
