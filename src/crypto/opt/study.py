"""In-depth walk-forward + locked-holdout study.

Purpose: decide which 1m scalp family (if any) is worth taking further,
after Kraken fees, on ≥1 year of 1-minute bars.

Phases
  0. Coverage + buy-and-hold baseline
  1. Default-param grid: strategy × pair × fee-tier × maker/taker  (IS window)
  2. Expanding walk-forward on the IS window (6 folds)
  3. Optuna on the IS window for the top candidates (holdout never seen)
  4. Locked holdout evaluation of defaults + tuned params
"""

from __future__ import annotations

import json
from dataclasses import asdict
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
import optuna
import pandas as pd

from crypto.data.store import DATA_DIR, coverage_report, load_ohlcv
from crypto.features.core import StaticFeatures, compute_static
from crypto.opt.metrics import Metrics, compute, pretty
from crypto.sim.engine import RiskPolicy, simulate
from crypto.sim.fees import FeeModel
from crypto.strategies import STRATEGIES
from crypto.strategies.ema_pullback import ema_pullback
from crypto.strategies.range_break import range_breakout
from crypto.strategies.trendline_break import trendline_break
from crypto.strategies.vwap_mr import vwap_mean_reversion

optuna.logging.set_verbosity(optuna.logging.WARNING)

PAIRS_DEFAULT = ["XBTUSD", "ETHUSD", "SOLUSD"]
STRATS_DEFAULT = list(STRATEGIES.keys())
FEE_TIERS = [1, 3, 6]


def _slice_sf(sf: StaticFeatures, start: int, end: int) -> StaticFeatures:
    sl = slice(start, end)
    return StaticFeatures(
        n=end - start,
        time=sf.time[sl],
        open=sf.open[sl],
        high=sf.high[sl],
        low=sf.low[sl],
        close=sf.close[sl],
        volume=sf.volume[sl],
        vwap=sf.vwap[sl],
        ema9=sf.ema9[sl],
        ema21=sf.ema21[sl],
        atr14=sf.atr14[sl],
        ret1=sf.ret1[sl],
        range_pct=sf.range_pct[sl],
    )


def _signals(name: str, sf: StaticFeatures, params: dict | None = None) -> np.ndarray:
    p = params or {}
    if name == "vwap_mr":
        return vwap_mean_reversion(
            sf,
            z_entry=float(p.get("z_entry", 1.2)),
            ema_slope_bars=int(p.get("ema_slope_bars", 3)),
        )
    if name == "range_break":
        return range_breakout(
            sf,
            lookback=int(p.get("lookback", 15)),
            vol_mult=float(p.get("vol_mult", 1.2)),
        )
    if name == "ema_pullback":
        return ema_pullback(sf, touch_atr=float(p.get("touch_atr", 0.25)))
    if name == "trendline_break":
        return trendline_break(
            sf,
            pivot_lookback=int(p.get("pivot_lookback", 8)),
            vol_mult=float(p.get("vol_mult", 1.3)),
        )
    return STRATEGIES[name](sf)


def _run(
    sf: StaticFeatures,
    strategy: str,
    fee_tier: int,
    maker_entry: bool,
    params: dict | None = None,
    max_hold_bars: int = 20,
) -> Metrics:
    sig = _signals(strategy, sf, params)
    fee = FeeModel.from_tier(fee_tier)
    pol = RiskPolicy(notional_usd=1_000.0, max_hold_bars=int(max_hold_bars))
    trades = simulate(sf, sig, fee, pol, maker_entry=maker_entry, maker_exit=False)
    return compute(trades)


def _metrics_dict(m: Metrics) -> dict:
    d = asdict(m)
    for k, v in list(d.items()):
        if isinstance(v, float):
            d[k] = round(v, 6)
    return d


def _buy_hold(sf: StaticFeatures, notional: float = 1_000.0, fee_tier: int = 3) -> dict:
    if sf.n < 2:
        return {"pnl": 0.0, "ret_pct": 0.0}
    px0, px1 = float(sf.close[0]), float(sf.close[-1])
    size = notional / px0
    fee = FeeModel.from_tier(fee_tier)
    costs = fee.cost(notional, is_maker=False) + fee.cost(size * px1, is_maker=False)
    pnl = size * (px1 - px0) - costs
    return {
        "pnl": round(pnl, 2),
        "ret_pct": round(100.0 * (px1 / px0 - 1.0), 3),
        "start": float(px0),
        "end": float(px1),
        "fees": round(costs, 2),
    }


def _suggest(trial: optuna.Trial, strategy: str) -> dict:
    p: dict = {
        "maker_entry": trial.suggest_categorical("maker_entry", [True, False]),
        "max_hold_bars": trial.suggest_int("max_hold_bars", 5, 45),
    }
    if strategy == "vwap_mr":
        p["z_entry"] = trial.suggest_float("z_entry", 0.6, 2.5)
        p["ema_slope_bars"] = trial.suggest_int("ema_slope_bars", 2, 8)
    elif strategy == "range_break":
        p["lookback"] = trial.suggest_int("lookback", 8, 30)
        p["vol_mult"] = trial.suggest_float("vol_mult", 0.8, 2.5)
    elif strategy == "ema_pullback":
        p["touch_atr"] = trial.suggest_float("touch_atr", 0.1, 0.6)
    elif strategy == "trendline_break":
        p["pivot_lookback"] = trial.suggest_int("pivot_lookback", 5, 15)
        p["vol_mult"] = trial.suggest_float("vol_mult", 1.0, 2.5)
    return p


def _objective_factory(sf: StaticFeatures, strategy: str, fee_tier: int):
    def objective(trial: optuna.Trial) -> float:
        p = _suggest(trial, strategy)
        m = _run(
            sf,
            strategy,
            fee_tier,
            bool(p["maker_entry"]),
            p,
            max_hold_bars=int(p["max_hold_bars"]),
        )
        if m.n_trades < 20:
            return -1e6
        # Prefer positive expectancy after fees; penalise huge DD.
        return m.total_pnl - 0.25 * m.max_dd

    return objective


def run_study(
    pairs: list[str] | None = None,
    days: int = 365,
    holdout_days: int = 60,
    folds: int = 6,
    trials: int = 80,
    fee_tier_tune: int = 3,
    interval: int = 1,
) -> Path:
    pairs = pairs or PAIRS_DEFAULT
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_dir = DATA_DIR / "studies"
    out_dir.mkdir(parents=True, exist_ok=True)
    report: dict = {
        "stamp": stamp,
        "days": days,
        "holdout_days": holdout_days,
        "folds": folds,
        "trials": trials,
        "fee_tier_tune": fee_tier_tune,
        "interval": interval,
        "notional_usd": 1000.0,
        "source_note": (
            "1m bars from Binance Vision (BTCUSDT/ETHUSDT/SOLUSDT) mapped to "
            "XBTUSD/ETHUSD/SOLUSD. Fees are Kraken Pro spot tiers. Venue basis "
            "is USDT vs Kraken USD — use rankings, not dollar PnL, as the decision."
        ),
        "coverage": {},
        "buy_hold": {},
        "grid": [],
        "walk_forward": [],
        "optuna": [],
        "holdout": [],
        "recommendation": {},
    }

    loaded: dict[str, tuple[pd.DataFrame, StaticFeatures, int, int]] = {}
    print(f"=== study  {interval}m  days={days} holdout={holdout_days}d  folds={folds} trials={trials} ===")
    for pair in pairs:
        df = load_ohlcv(pair, days=days, interval=1)
        if interval > 1:
            from crypto.data.store import resample_ohlcv

            df = resample_ohlcv(df, interval)
        cov = coverage_report(df, 1)
        report["coverage"][pair] = cov
        print(f"  {pair}: {cov['bars']:,} bars  {cov['span_days']:.1f}d  cov={cov['coverage_pct']}%  {cov['from']} → {cov['to']}")
        sf = compute_static(df)
        times = pd.DatetimeIndex(sf.time)
        hold_cut = times.max() - pd.Timedelta(days=holdout_days)
        hold_i = int(np.searchsorted(times.values, np.datetime64(hold_cut.to_datetime64())))
        hold_i = max(min(hold_i, sf.n - 100), int(sf.n * 0.6))
        loaded[pair] = (df, sf, hold_i, sf.n)
        bh_is = _buy_hold(_slice_sf(sf, 0, hold_i), fee_tier=fee_tier_tune)
        bh_oos = _buy_hold(_slice_sf(sf, hold_i, sf.n), fee_tier=fee_tier_tune)
        report["buy_hold"][pair] = {"is": bh_is, "holdout": bh_oos}
        print(f"    buy&hold IS ${bh_is['pnl']:.0f} ({bh_is['ret_pct']:.1f}%)  holdout ${bh_oos['pnl']:.0f} ({bh_oos['ret_pct']:.1f}%)")

    # ----- Phase 1: default grid on IS --------------------------------
    print("\n--- phase 1: default-param grid (in-sample) ---")
    grid_rows = []
    for pair in pairs:
        _, sf, hold_i, _ = loaded[pair]
        is_sf = _slice_sf(sf, 0, hold_i)
        for strat in STRATS_DEFAULT:
            for tier in FEE_TIERS:
                for maker in (True, False):
                    m = _run(is_sf, strat, tier, maker)
                    row = {
                        "pair": pair,
                        "strategy": strat,
                        "fee_tier": tier,
                        "maker_entry": maker,
                        "window": "IS",
                        **_metrics_dict(m),
                    }
                    grid_rows.append(row)
                    print(f"  {pair:7} {strat:16} t{tier} {'M' if maker else 'T'}  {pretty(m)}")
    report["grid"] = grid_rows

    def _rank_key(r: dict) -> tuple:
        return (r["total_pnl"], r["profit_factor"], r["n_trades"])

    # Focus WF on fee-tier used for live-ish research + maker-entry (the only
    # realistic scalp path). Still keep the full grid in the JSON.
    wf_candidates = [
        r for r in grid_rows
        if r["fee_tier"] == fee_tier_tune and r["maker_entry"] is True
    ]

    # ----- Phase 2: expanding walk-forward ----------------------------
    print(f"\n--- phase 2: expanding walk-forward ({folds} folds, tier {fee_tier_tune} maker) ---")
    wf_rows = []
    for pair in pairs:
        _, sf, hold_i, _ = loaded[pair]
        edges = np.linspace(0, hold_i, folds + 1, dtype=int)
        min_train = max(int(edges[1]), max(500, 20_000 // max(interval, 1)))
        min_test = max(200, 5_000 // max(interval, 1))
        for strat in STRATS_DEFAULT:
            fold_pnls = []
            fold_pfs = []
            fold_n = []
            for k in range(1, folds):
                tr_end = int(edges[k])
                te_end = int(edges[k + 1])
                if tr_end < min_train or te_end - tr_end < min_test:
                    continue
                test_sf = _slice_sf(sf, tr_end, te_end)
                m = _run(test_sf, strat, fee_tier_tune, True)
                fold_pnls.append(m.total_pnl)
                fold_pfs.append(m.profit_factor if np.isfinite(m.profit_factor) else 0.0)
                fold_n.append(m.n_trades)
                wf_rows.append({
                    "pair": pair,
                    "strategy": strat,
                    "fold": k,
                    "train_end": tr_end,
                    "test_end": te_end,
                    **_metrics_dict(m),
                })
                print(f"  {pair:7} {strat:16} fold {k}/{folds-1}  {pretty(m)}")
            if fold_pnls:
                print(
                    f"    >> {pair} {strat}: median OOS PnL ${np.median(fold_pnls):.2f}  "
                    f"mean PF {np.mean(fold_pfs):.2f}  folds+={(np.array(fold_pnls)>0).sum()}/{len(fold_pnls)}"
                )
    report["walk_forward"] = wf_rows

    # Score candidates by median OOS PnL (maker, tune tier)
    wf_summary = []
    for pair in pairs:
        for strat in STRATS_DEFAULT:
            xs = [r for r in wf_rows if r["pair"] == pair and r["strategy"] == strat]
            if not xs:
                continue
            pnls = [r["total_pnl"] for r in xs]
            wf_summary.append({
                "pair": pair,
                "strategy": strat,
                "median_oos_pnl": float(np.median(pnls)),
                "mean_oos_pnl": float(np.mean(pnls)),
                "pos_folds": int(sum(1 for p in pnls if p > 0)),
                "n_folds": len(pnls),
                "mean_pf": float(np.mean([r["profit_factor"] for r in xs])),
                "mean_trades": float(np.mean([r["n_trades"] for r in xs])),
            })
    wf_summary.sort(key=lambda r: r["median_oos_pnl"], reverse=True)
    report["walk_forward_summary"] = wf_summary
    top = wf_summary[: min(4, len(wf_summary))]
    print("\n  top walk-forward (median OOS PnL):")
    for r in top:
        print(
            f"    {r['pair']:7} {r['strategy']:16}  med ${r['median_oos_pnl']:.2f}  "
            f"+folds {r['pos_folds']}/{r['n_folds']}  PF {r['mean_pf']:.2f}"
        )

    # ----- Phase 3: Optuna on IS, never touches holdout ---------------
    print(f"\n--- phase 3: Optuna ({trials} trials) on IS, top candidates ---")
    opt_rows = []
    tune_targets = top[:3] if top else [{"pair": p, "strategy": "vwap_mr"} for p in pairs]
    # Always include VWAP-MR on BTC when that series is in the run — original thesis.
    if "XBTUSD" in loaded and not any(
        t["pair"] == "XBTUSD" and t["strategy"] == "vwap_mr" for t in tune_targets
    ):
        tune_targets.append({"pair": "XBTUSD", "strategy": "vwap_mr"})
    tune_targets = [t for t in tune_targets if t["pair"] in loaded]

    for t in tune_targets:
        pair, strat = t["pair"], t["strategy"]
        _, sf, hold_i, _ = loaded[pair]
        is_sf = _slice_sf(sf, 0, hold_i)
        study = optuna.create_study(direction="maximize", study_name=f"{pair}-{strat}")
        study.optimize(_objective_factory(is_sf, strat, fee_tier_tune), n_trials=trials, show_progress_bar=False)
        bp = dict(study.best_params)
        m_is = _run(
            is_sf,
            strat,
            fee_tier_tune,
            bool(bp.get("maker_entry", True)),
            bp,
            max_hold_bars=int(bp.get("max_hold_bars", 20)),
        )
        row = {
            "pair": pair,
            "strategy": strat,
            "best_value": study.best_value,
            "best_params": bp,
            "is": _metrics_dict(m_is),
        }
        opt_rows.append(row)
        print(f"  {pair} {strat} best IS {pretty(m_is)}")
        print(f"    params {bp}")
    report["optuna"] = opt_rows

    # ----- Phase 4: locked holdout ------------------------------------
    print("\n--- phase 4: locked holdout (never used in search) ---")
    hold_rows = []
    # defaults + tuned
    seen = set()
    evals: list[tuple[str, str, dict | None, str]] = []
    for strat in STRATS_DEFAULT:
        for pair in pairs:
            evals.append((pair, strat, None, "default"))
    for t in opt_rows:
        evals.append((t["pair"], t["strategy"], t["best_params"], "tuned"))

    for pair, strat, params, tag in evals:
        key = (pair, strat, tag)
        if key in seen:
            continue
        seen.add(key)
        _, sf, hold_i, n = loaded[pair]
        oos = _slice_sf(sf, hold_i, n)
        maker = True if params is None else bool(params.get("maker_entry", True))
        hold = 20 if params is None else int(params.get("max_hold_bars", 20))
        m = _run(oos, strat, fee_tier_tune, maker, params, max_hold_bars=hold)
        row = {
            "pair": pair,
            "strategy": strat,
            "tag": tag,
            "params": params or {},
            **_metrics_dict(m),
        }
        hold_rows.append(row)
        print(f"  {pair:7} {strat:16} {tag:8}  {pretty(m)}")
    report["holdout"] = hold_rows

    rec = _recommend(report)
    report["recommendation"] = rec
    json_path = out_dir / f"{stamp}_study.json"
    md_path = out_dir / f"{stamp}_study.md"
    json_path.write_text(json.dumps(report, indent=2, default=str), encoding="utf-8")
    md_path.write_text(_markdown(report), encoding="utf-8")
    print(f"\nWrote {json_path}")
    print(f"Wrote {md_path}")
    print("\n=== recommendation ===")
    print(rec.get("headline", ""))
    for line in rec.get("bullets", []):
        print(f"  - {line}")
    return md_path


def _recommend(report: dict) -> dict:
    hold = report.get("holdout") or []
    wf = report.get("walk_forward_summary") or []
    tuned = [r for r in hold if r.get("tag") == "tuned"]
    defaults = [r for r in hold if r.get("tag") == "default"]

    def _ok(r: dict) -> bool:
        return r["n_trades"] >= 15 and r["total_pnl"] > 0 and r["profit_factor"] > 1.05

    tuned_ok = [r for r in tuned if _ok(r)]
    def_ok = [r for r in defaults if _ok(r)]
    wf_ok = [r for r in wf if r["median_oos_pnl"] > 0 and r["pos_folds"] >= max(2, r["n_folds"] // 2)]

    bullets = []
    if not wf and not hold:
        return {"headline": "No results — data missing.", "direction": "blocked", "bullets": []}

    # Fee reality
    grid = report.get("grid") or []
    taker = [r for r in grid if r["maker_entry"] is False and r["fee_tier"] == 1]
    maker6 = [r for r in grid if r["maker_entry"] is True and r["fee_tier"] == 6]
    if taker:
        n_pos = sum(1 for r in taker if r["total_pnl"] > 0)
        bullets.append(
            f"Retail taker (tier 1) is a dead end: {n_pos}/{len(taker)} default combos were +EV in-sample."
        )
    if maker6:
        n_pos = sum(1 for r in maker6 if r["total_pnl"] > 0)
        bullets.append(
            f"Maker + tier 6 (0.12%/0.25%): {n_pos}/{len(maker6)} default combos +EV in-sample — fee tier is the first-order variable."
        )

    if wf:
        best_wf = wf[0]
        bullets.append(
            f"Walk-forward median OOS leader: {best_wf['strategy']} on {best_wf['pair']} "
            f"(${best_wf['median_oos_pnl']:.2f}/fold, +EV in {best_wf['pos_folds']}/{best_wf['n_folds']} folds)."
        )

    if tuned_ok:
        best = max(tuned_ok, key=lambda r: r["total_pnl"])
        bullets.append(
            f"Tuned params survived holdout: {best['strategy']} {best['pair']} "
            f"PnL ${best['total_pnl']:.2f} PF {best['profit_factor']:.2f} n={best['n_trades']}."
        )
        direction = "pursue"
        headline = (
            f"Pursue {best['strategy']} on {best['pair']} as a maker-only scalp, "
            f"but only if you can actually rest limits and sit at a better fee tier."
        )
    elif def_ok:
        best = max(def_ok, key=lambda r: r["total_pnl"])
        bullets.append(
            f"Untuned default survived holdout: {best['strategy']} {best['pair']} "
            f"PnL ${best['total_pnl']:.2f} PF {best['profit_factor']:.2f}."
        )
        direction = "cautious"
        headline = (
            f"{best['strategy']} is the only family that wasn't immediately destroyed "
            f"on holdout. Treat it as a research lead, not a live system."
        )
    elif wf_ok:
        best = wf_ok[0]
        bullets.append("Nothing survived the locked holdout with PF>1.05 after fees.")
        direction = "higher_timeframe"
        headline = (
            "1-minute scalps are not paying their Kraken fees on this sample. "
            "Move the same VWAP+EMA9 idea to 5m/15m, or drop mean-reversion scalps."
        )
    else:
        direction = "abandon_1m_scalp"
        headline = (
            "No 1-minute family showed stable +EV after fees on walk-forward or holdout. "
            "Do not paper-trade these scalps yet. Next: higher timeframe, or a completely "
            "different edge (inventory / maker rebate at high tier, not prediction)."
        )
        bullets.append("Walk-forward medians and holdout PnL are ≤ 0 after costs for every default family.")

    bullets.append(
        "Do not mix this with live size until holdout PF>1.2, ≥30 trades, and maker fill-rate is measured on Kraken, not assumed."
    )
    return {
        "headline": headline,
        "direction": direction,
        "bullets": bullets,
        "tuned_ok": [{k: r[k] for k in ("pair", "strategy", "total_pnl", "profit_factor", "n_trades", "win_rate")} for r in tuned_ok],
        "wf_ok": wf_ok[:5],
    }


def _markdown(report: dict) -> str:
    rec = report.get("recommendation") or {}
    lines = [
        f"# 1m scalp study {report['stamp']}",
        "",
        rec.get("headline", ""),
        "",
        f"- Window: {report['days']}d, holdout {report['holdout_days']}d, {report['folds']} WF folds, {report['trials']} Optuna trials.",
        f"- Size: ${report['notional_usd']:.0f} notional / trade. Fee model: Kraken Pro spot.",
        f"- {report['source_note']}",
        "",
        "## Coverage",
        "",
        "| pair | bars | days | cov% | from | to |",
        "|------|------:|-----:|-----:|------|-----|",
    ]
    for pair, cov in (report.get("coverage") or {}).items():
        lines.append(
            f"| {pair} | {cov['bars']:,} | {cov['span_days']:.1f} | {cov['coverage_pct']:.1f} | {cov['from']} | {cov['to']} |"
        )
    lines += ["", "## Buy & hold (same $1k, 2× taker)", ""]
    for pair, bh in (report.get("buy_hold") or {}).items():
        lines.append(
            f"- **{pair}** IS ${bh['is']['pnl']:.0f} ({bh['is']['ret_pct']:.1f}%) · "
            f"holdout ${bh['holdout']['pnl']:.0f} ({bh['holdout']['ret_pct']:.1f}%)"
        )
    lines += ["", "## Walk-forward summary (maker, tune tier)", "",
              "| pair | strategy | median OOS $ | +folds | mean PF | trades/fold |",
              "|------|----------|-------------:|-------:|--------:|------------:|"]
    for r in report.get("walk_forward_summary") or []:
        lines.append(
            f"| {r['pair']} | {r['strategy']} | {r['median_oos_pnl']:.2f} | "
            f"{r['pos_folds']}/{r['n_folds']} | {r['mean_pf']:.2f} | {r['mean_trades']:.0f} |"
        )
    lines += ["", "## Locked holdout", "",
              "| pair | strategy | tag | trades | win | PnL | PF | DD |",
              "|------|----------|-----|-------:|----:|----:|---:|---:|"]
    for r in sorted(report.get("holdout") or [], key=lambda x: x["total_pnl"], reverse=True)[:20]:
        lines.append(
            f"| {r['pair']} | {r['strategy']} | {r['tag']} | {r['n_trades']} | "
            f"{r['win_rate']*100:.1f}% | {r['total_pnl']:.2f} | {r['profit_factor']:.2f} | {r['max_dd']:.2f} |"
        )
    lines += ["", "## Recommendation", ""]
    for b in rec.get("bullets") or []:
        lines.append(f"- {b}")
    lines.append("")
    return "\n".join(lines)
