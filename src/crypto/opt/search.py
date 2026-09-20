"""Optuna search over strategy hyper-parameters (fee-aware objective)."""

from __future__ import annotations

import optuna

from crypto.data.store import load_ohlcv
from crypto.features.core import compute_static
from crypto.opt.metrics import compute, pretty
from crypto.sim.engine import RiskPolicy, simulate
from crypto.sim.fees import FeeModel
from crypto.strategies import STRATEGIES
from crypto.strategies.vwap_mr import vwap_mean_reversion
from crypto.strategies.range_break import range_breakout
from crypto.strategies.ema_pullback import ema_pullback
from crypto.strategies.trendline_break import trendline_break


def run_optimize(
    pair: str = "XBTUSD",
    days: int = 30,
    n_trials: int = 100,
    fee_tier: int = 3,
    strategy: str = "vwap_mr",
) -> None:
    if strategy not in STRATEGIES:
        raise SystemExit(f"Unknown strategy {strategy!r}. Choose from: {list(STRATEGIES)}")

    print(f"=== optimize {pair}  {days}d  trials={n_trials}  fee-tier={fee_tier}  strategy={strategy} ===")
    df = load_ohlcv(pair, days=days)
    print(f"  bars={len(df):,}  {df.index.min()} → {df.index.max()}")
    sf = compute_static(df)
    fee = FeeModel.from_tier(fee_tier)

    def objective(trial: optuna.Trial) -> float:
        maker_entry = trial.suggest_categorical("maker_entry", [True, False])
        max_hold = trial.suggest_int("max_hold_bars", 5, 45)

        if strategy == "vwap_mr":
            z = trial.suggest_float("z_entry", 0.6, 2.5)
            slope_bars = trial.suggest_int("ema_slope_bars", 2, 8)
            signals = vwap_mean_reversion(sf, z_entry=z, ema_slope_bars=slope_bars)
        elif strategy == "range_break":
            lookback = trial.suggest_int("lookback", 8, 30)
            vol_mult = trial.suggest_float("vol_mult", 0.8, 2.5)
            signals = range_breakout(sf, lookback=lookback, vol_mult=vol_mult)
        elif strategy == "ema_pullback":
            touch = trial.suggest_float("touch_atr", 0.1, 0.6)
            signals = ema_pullback(sf, touch_atr=touch)
        elif strategy == "trendline_break":
            piv = trial.suggest_int("pivot_lookback", 5, 15)
            vol_mult = trial.suggest_float("vol_mult", 1.0, 2.5)
            signals = trendline_break(sf, pivot_lookback=piv, vol_mult=vol_mult)
        else:
            signals = STRATEGIES[strategy](sf)

        pol = RiskPolicy(notional_usd=1_000.0, max_hold_bars=max_hold)
        trades = simulate(sf, signals, fee, pol, maker_entry=maker_entry, maker_exit=False)
        m = compute(trades)
        if m.n_trades < 15:
            return -1e6
        if m.profit_factor < 1.0:
            return m.total_pnl - 500
        return m.total_pnl

    study = optuna.create_study(direction="maximize", study_name=f"{pair}-{strategy}")
    study.optimize(objective, n_trials=n_trials, show_progress_bar=False)

    print("Best trial:")
    print(f"  value (net PnL) = {study.best_value:.2f}")
    for k, v in study.best_params.items():
        print(f"  {k}: {v}")

    bp = study.best_params
    max_hold = bp.get("max_hold_bars", 20)
    maker_entry = bp.get("maker_entry", True)

    if strategy == "vwap_mr":
        signals = vwap_mean_reversion(
            sf, z_entry=bp["z_entry"], ema_slope_bars=bp["ema_slope_bars"]
        )
    elif strategy == "range_break":
        signals = range_breakout(sf, lookback=bp["lookback"], vol_mult=bp["vol_mult"])
    elif strategy == "ema_pullback":
        signals = ema_pullback(sf, touch_atr=bp["touch_atr"])
    elif strategy == "trendline_break":
        signals = trendline_break(
            sf, pivot_lookback=bp["pivot_lookback"], vol_mult=bp["vol_mult"]
        )
    else:
        signals = STRATEGIES[strategy](sf)

    pol = RiskPolicy(notional_usd=1_000.0, max_hold_bars=max_hold)
    trades = simulate(sf, signals, fee, pol, maker_entry=maker_entry, maker_exit=False)
    print("  ", pretty(compute(trades)))
