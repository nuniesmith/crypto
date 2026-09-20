"""Baseline research report — one strategy, fixed params, fee-aware."""

from __future__ import annotations

from crypto.data.store import load_ohlcv
from crypto.features.core import compute_static
from crypto.opt.metrics import compute, pretty
from crypto.sim.engine import RiskPolicy, simulate
from crypto.sim.fees import FeeModel
from crypto.strategies import STRATEGIES


def run_research(
    pair: str = "XBTUSD",
    days: int = 14,
    fee_tier: int = 1,
    strategy: str = "vwap_mr",
) -> None:
    if strategy not in STRATEGIES:
        raise SystemExit(f"Unknown strategy {strategy!r}. Choose from: {list(STRATEGIES)}")

    print(f"=== research {pair}  {days}d  strategy={strategy}  fee-tier={fee_tier} ===")
    df = load_ohlcv(pair, days=days)
    print(f"  bars={len(df):,}  {df.index.min()} → {df.index.max()}")
    sf = compute_static(df)
    fee = FeeModel.from_tier(fee_tier)
    policy = RiskPolicy(notional_usd=1_000.0, max_hold_bars=20)

    signals = STRATEGIES[strategy](sf)

    for label, me, mx in [
        ("maker-entry / taker-exit", True, False),
        ("all-taker (worst)", False, False),
    ]:
        trades = simulate(sf, signals, fee, policy, maker_entry=me, maker_exit=mx)
        m = compute(trades)
        print(f"  {label:28s}  {pretty(m)}")
