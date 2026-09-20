"""Baseline research report — one strategy, fixed params, fee-aware."""

from __future__ import annotations

from crypto.data.store import load_ohlcv
from crypto.features.core import compute_static
from crypto.opt.metrics import compute, pretty
from crypto.sim.engine import RiskPolicy, simulate
from crypto.sim.fees import FeeModel
from crypto.strategies import ema_cross, vwap_mean_reversion


def run_research(
    pair: str = "XBTUSD",
    days: int = 14,
    fee_tier: int = 1,
    strategy: str = "vwap_mr",
) -> None:
    print(f"=== research {pair}  {days}d  strategy={strategy}  fee-tier={fee_tier} ===")
    df = load_ohlcv(pair, days=days)
    sf = compute_static(df)
    fee = FeeModel.from_tier(fee_tier)
    policy = RiskPolicy(notional_usd=1_000.0, max_hold_bars=20)

    if strategy == "vwap_mr":
        signals = vwap_mean_reversion(sf, z_entry=1.0)
    else:
        signals = ema_cross(sf)

    # two fee scenarios: optimistic maker entry / pessimistic all-taker
    for label, me, mx in [
        ("maker-entry / taker-exit", True, False),
        ("all-taker (worst)", False, False),
    ]:
        trades = simulate(sf, signals, fee, policy, maker_entry=me, maker_exit=mx)
        m = compute(trades)
        print(f"  {label:28s}  {pretty(m)}")
