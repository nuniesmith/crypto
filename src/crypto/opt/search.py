"""Optuna search over strategy hyper-parameters (fee-aware objective)."""

from __future__ import annotations

import optuna

from crypto.data.store import load_ohlcv
from crypto.features.core import compute_static
from crypto.opt.metrics import compute
from crypto.sim.engine import RiskPolicy, simulate
from crypto.sim.fees import FeeModel
from crypto.strategies.vwap_mr import vwap_mean_reversion


def run_optimize(
    pair: str = "XBTUSD",
    days: int = 30,
    n_trials: int = 100,
    fee_tier: int = 3,
    strategy: str = "vwap_mr",
) -> None:
    print(f"=== optimize {pair}  {days}d  trials={n_trials}  fee-tier={fee_tier} ===")
    df = load_ohlcv(pair, days=days)
    sf = compute_static(df)
    fee = FeeModel.from_tier(fee_tier)
    policy = RiskPolicy(notional_usd=1_000.0)

    def objective(trial: optuna.Trial) -> float:
        z = trial.suggest_float("z_entry", 0.6, 2.5)
        slope_bars = trial.suggest_int("ema_slope_bars", 2, 8)
        max_hold = trial.suggest_int("max_hold_bars", 5, 45)
        maker_entry = trial.suggest_categorical("maker_entry", [True, False])

        signals = vwap_mean_reversion(sf, z_entry=z, ema_slope_bars=slope_bars)
        pol = RiskPolicy(notional_usd=1_000.0, max_hold_bars=max_hold)
        trades = simulate(
            sf, signals, fee, pol, maker_entry=maker_entry, maker_exit=False
        )
        m = compute(trades)
        # objective: total PnL, but heavily penalise low trade count or bad PF
        if m.n_trades < 20:
            return -1e6
        if m.profit_factor < 1.0:
            return m.total_pnl - 500  # soft penalty
        return m.total_pnl

    study = optuna.create_study(direction="maximize", study_name=f"{pair}-vwap_mr")
    study.optimize(objective, n_trials=n_trials, show_progress_bar=False)

    print("Best trial:")
    print(f"  value (net PnL) = {study.best_value:.2f}")
    for k, v in study.best_params.items():
        print(f"  {k}: {v}")

    # re-run best for full metrics
    bp = study.best_params
    signals = vwap_mean_reversion(
        sf, z_entry=bp["z_entry"], ema_slope_bars=bp["ema_slope_bars"]
    )
    pol = RiskPolicy(notional_usd=1_000.0, max_hold_bars=bp["max_hold_bars"])
    trades = simulate(
        sf,
        signals,
        fee,
        pol,
        maker_entry=bp["maker_entry"],
        maker_exit=False,
    )
    from crypto.opt.metrics import pretty

    print("  ", pretty(compute(trades)))
