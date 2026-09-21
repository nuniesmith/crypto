"""Clock-time swing/break study: 15m / 1h / 4h, VWAP+EMA as filter, vs buy-hold.

Follow-up to the 1m scalp wipeout. Holds are in minutes of wall clock, not bars.
"""

from __future__ import annotations

import json
from dataclasses import asdict
from datetime import datetime, timezone
from pathlib import Path

import numpy as np
import pandas as pd

from crypto.data.store import DATA_DIR, coverage_report, load_ohlcv, normalize_ohlcv, resample_ohlcv
from crypto.features.core import StaticFeatures, compute_static
from crypto.opt.metrics import Metrics, compute, pretty
from crypto.opt.study import _buy_hold, _metrics_dict, _slice_sf
from crypto.sim.engine import RiskPolicy, simulate
from crypto.sim.fees import FeeModel
from crypto.strategies.ema_cross import ema_cross
from crypto.strategies.range_break import range_breakout
from crypto.strategies.structure import range_filtered, structure_filtered
from crypto.strategies.trendline_break import trendline_break
from crypto.strategies.vwap_mr import vwap_mean_reversion

PAIRS = ["XBTUSD", "ETHUSD", "SOLUSD"]
INTERVALS = [15, 60, 240]
# 2h, 1d, 7d — scalp-ish / swing / “let ATR and flip decide”
HOLD_MINUTES = [120, 1440, 10080]
STRATS = [
    "vwap_mr",
    "ema_cross",
    "trendline_break",
    "range_break",
    "structure_filtered",
    "range_filtered",
]


def _hold_bars(interval: int, hold_min: int) -> int:
    return max(1, int(round(hold_min / interval)))


def _signals(name: str, sf: StaticFeatures) -> np.ndarray:
    if name == "vwap_mr":
        return vwap_mean_reversion(sf)
    if name == "ema_cross":
        return ema_cross(sf)
    if name == "trendline_break":
        return trendline_break(sf)
    if name == "range_break":
        return range_breakout(sf)
    if name == "structure_filtered":
        return structure_filtered(sf)
    if name == "range_filtered":
        return range_filtered(sf)
    raise KeyError(name)


def _run(sf: StaticFeatures, strategy: str, fee_tier: int, hold_bars: int, maker: bool = True) -> Metrics:
    sig = _signals(strategy, sf)
    fee = FeeModel.from_tier(fee_tier)
    pol = RiskPolicy(notional_usd=1_000.0, max_hold_bars=hold_bars)
    return compute(simulate(sf, sig, fee, pol, maker_entry=maker, maker_exit=False))


def _load_pair(pair: str, days: int, interval: int, data_dir: Path | None = None) -> pd.DataFrame:
    if data_dir is not None:
        path = None
        for iv in (1, 15, 60):
            cand = data_dir / f"{pair}_{iv}m.parquet"
            if cand.exists():
                path = cand
                break
        if path is None:
            raise FileNotFoundError(f"no parquet for {pair} in {data_dir}")
        df = normalize_ohlcv(pd.read_parquet(path))
        if days is not None:
            cutoff = df.index.max() - pd.Timedelta(days=days)
            df = df[df.index >= cutoff]
    else:
        df = load_ohlcv(pair, days=days, interval=1)
    if interval > 1:
        df = resample_ohlcv(df, interval)
    return df


def _excess(m: Metrics, bh_pnl: float) -> float:
    return round(m.total_pnl - bh_pnl, 2)


def run_direction(
    *,
    days: int = 365,
    holdout_days: int = 60,
    fee_tier: int = 3,
    folds: int = 6,
    pairs: list[str] | None = None,
) -> Path:
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_dir = DATA_DIR / "studies"
    out_dir.mkdir(parents=True, exist_ok=True)
    report: dict = {
        "stamp": stamp,
        "kind": "direction",
        "days": days,
        "holdout_days": holdout_days,
        "fee_tier": fee_tier,
        "maker_entry": True,
        "intervals": INTERVALS,
        "hold_minutes": HOLD_MINUTES,
        "note": (
            "Clock-time holds. VWAP+EMA are a filter on structure/range breaks, "
            "not a 1m entry. Compared to buy-and-hold on the same window. "
            "1m Binance USDT series; Kraken native H1 replay is a separate block."
        ),
        "pairs": list(pairs or PAIRS),
        "binance": {},
        "kraken_native": {},
        "recommendation": {},
    }
    pairs = list(pairs or PAIRS)

    print(
        f"=== direction study  {', '.join(pairs)}  {days}d holdout={holdout_days}d  "
        f"maker tier {fee_tier} ==="
    )
    report["binance"] = _block(
        label="binance_year",
        pairs=pairs,
        days=days,
        holdout_days=holdout_days,
        fee_tier=fee_tier,
        folds=folds,
        data_dir=None,
        intervals=INTERVALS,
    )
    kraken_dir = DATA_DIR / "kraken_native"
    native_pairs = [
        p for p in pairs if (kraken_dir / f"{p}_1m.parquet").exists()
    ]
    if native_pairs:
        print("\n=== Kraken-native H1 2026 replay ===")
        # ~181 calendar days in the native cache; 45d holdout ≈ last quarter of H1
        report["kraken_native"] = _block(
            label="kraken_h1",
            pairs=native_pairs,
            days=200,
            holdout_days=45,
            fee_tier=fee_tier,
            folds=4,
            data_dir=kraken_dir,
            intervals=[60, 240],
        )
    else:
        print("No data/kraken_native for these pairs — skipping venue replay")

    rec = _recommend(report)
    report["recommendation"] = rec
    json_path = out_dir / f"{stamp}_direction.json"
    md_path = out_dir / f"{stamp}_direction.md"
    json_path.write_text(json.dumps(report, indent=2, default=str), encoding="utf-8")
    md_path.write_text(_markdown(report), encoding="utf-8")
    print(f"\nWrote {md_path}")
    print("\n=== recommendation ===")
    print(rec.get("headline", ""))
    for b in rec.get("bullets", []):
        print(f"  - {b}")
    return md_path


def _block(
    *,
    label: str,
    pairs: list[str],
    days: int,
    holdout_days: int,
    fee_tier: int,
    folds: int,
    data_dir: Path | None,
    intervals: list[int],
) -> dict:
    out: dict = {"label": label, "coverage": {}, "buy_hold": {}, "grid": [], "walk_forward": [], "summary": []}
    loaded: dict[tuple[str, int], tuple[StaticFeatures, int, int]] = {}

    for pair in pairs:
        try:
            df1 = _load_pair(pair, days=days, interval=1, data_dir=data_dir)
        except FileNotFoundError as e:
            print(f"  skip {pair}: {e}")
            continue
        for interval in intervals:
            df = resample_ohlcv(df1, interval) if interval > 1 else df1
            cov = coverage_report(df, interval)
            out["coverage"][f"{pair}_{interval}m"] = cov
            print(f"  {label} {pair} {interval}m  {cov['bars']:,} bars  {cov['span_days']:.1f}d  {cov['from']} → {cov['to']}")
            sf = compute_static(df)
            times = pd.DatetimeIndex(sf.time)
            hold_cut = times.max() - pd.Timedelta(days=holdout_days)
            hold_i = int(np.searchsorted(times.values, np.datetime64(hold_cut.to_datetime64())))
            hold_i = max(min(hold_i, sf.n - 30), int(sf.n * 0.55))
            loaded[(pair, interval)] = (sf, hold_i, sf.n)
            bh_is = _buy_hold(_slice_sf(sf, 0, hold_i), fee_tier=fee_tier)
            bh_oos = _buy_hold(_slice_sf(sf, hold_i, sf.n), fee_tier=fee_tier)
            out["buy_hold"][f"{pair}_{interval}m"] = {"is": bh_is, "holdout": bh_oos}

    grid = []
    for (pair, interval), (sf, hold_i, n) in loaded.items():
        is_sf = _slice_sf(sf, 0, hold_i)
        oos_sf = _slice_sf(sf, hold_i, n)
        bh_is = out["buy_hold"][f"{pair}_{interval}m"]["is"]["pnl"]
        bh_oos = out["buy_hold"][f"{pair}_{interval}m"]["holdout"]["pnl"]
        for strat in STRATS:
            for hold_min in HOLD_MINUTES:
                hb = _hold_bars(interval, hold_min)
                m_is = _run(is_sf, strat, fee_tier, hb)
                m_oos = _run(oos_sf, strat, fee_tier, hb)
                row = {
                    "pair": pair,
                    "interval": interval,
                    "strategy": strat,
                    "hold_minutes": hold_min,
                    "hold_bars": hb,
                    "is": _metrics_dict(m_is),
                    "holdout": _metrics_dict(m_oos),
                    "is_excess_vs_bh": _excess(m_is, bh_is),
                    "holdout_excess_vs_bh": _excess(m_oos, bh_oos),
                }
                grid.append(row)
                print(
                    f"  {pair:7} {interval:3}m {strat:20} hold={hold_min:5}m  "
                    f"IS {pretty(m_is)}  | OOS {pretty(m_oos)}  "
                    f"exBH {row['holdout_excess_vs_bh']:+.0f}"
                )
    out["grid"] = grid

    # Walk-forward on 60m/240m, 1d hold, the two break families + filtered
    wf_strats = ["trendline_break", "structure_filtered", "ema_cross"]
    wf_holds = [1440]
    wf_iv = [i for i in intervals if i in (60, 240)]
    wf_rows = []
    for pair in pairs:
        for interval in wf_iv:
            if (pair, interval) not in loaded:
                continue
            sf, hold_i, _ = loaded[(pair, interval)]
            edges = np.linspace(0, hold_i, folds + 1, dtype=int)
            min_test = max(20, 200 // max(interval // 15, 1))
            for strat in wf_strats:
                for hold_min in wf_holds:
                    hb = _hold_bars(interval, hold_min)
                    pnls = []
                    for k in range(1, folds):
                        tr_end, te_end = int(edges[k]), int(edges[k + 1])
                        if te_end - tr_end < min_test:
                            continue
                        m = _run(_slice_sf(sf, tr_end, te_end), strat, fee_tier, hb)
                        pnls.append(m.total_pnl)
                        wf_rows.append({
                            "pair": pair,
                            "interval": interval,
                            "strategy": strat,
                            "hold_minutes": hold_min,
                            "fold": k,
                            **_metrics_dict(m),
                        })
                    if pnls:
                        print(
                            f"    WF {pair} {interval}m {strat} 1d-hold  "
                            f"med ${np.median(pnls):.1f}  +folds {(np.array(pnls)>0).sum()}/{len(pnls)}"
                        )
    out["walk_forward"] = wf_rows

    # Rank by holdout excess vs BH, requiring some trades
    ranked = [
        r for r in grid
        if r["holdout"]["n_trades"] >= 8
    ]
    ranked.sort(key=lambda r: (r["holdout_excess_vs_bh"], r["holdout"]["profit_factor"]), reverse=True)
    out["summary"] = ranked[:15]
    return out


def _ok_holdout(r: dict) -> bool:
    h = r["holdout"]
    return (
        h["n_trades"] >= 12
        and h["total_pnl"] > 0
        and h["profit_factor"] > 1.2
        and r["holdout_excess_vs_bh"] > 0
        and r["is"]["total_pnl"] > -abs(r["is"].get("total_fees", 1e9))  # not a total wipe in the bear
    )


def _recommend(report: dict) -> dict:
    b = report.get("binance") or {}
    knat = report.get("kraken_native") or {}
    grid = b.get("grid") or []
    wf = b.get("walk_forward") or []
    bullets = []
    survivors = [r for r in grid if _ok_holdout(r)]

    # 1m already failed; this study starts at 15m
    n_15_pos = sum(1 for r in grid if r["interval"] == 15 and r["holdout"]["total_pnl"] > 0)
    n_60_pos = sum(1 for r in grid if r["interval"] == 60 and r["holdout"]["total_pnl"] > 0)
    n_240_pos = sum(1 for r in grid if r["interval"] == 240 and r["holdout"]["total_pnl"] > 0)
    bullets.append(
        f"Holdout +EV count (maker, tier {report['fee_tier']}): "
        f"15m {n_15_pos} / 1h {n_60_pos} / 4h {n_240_pos} of the grid cells."
    )

    if survivors:
        best = max(survivors, key=lambda r: r["holdout_excess_vs_bh"])
        bullets.append(
            f"Beat buy-and-hold on holdout with PF>1.2 and ≥12 trades: "
            f"{best['strategy']} {best['pair']} {best['interval']}m hold={best['hold_minutes']}m "
            f"PnL ${best['holdout']['total_pnl']:.0f} vs BH excess {best['holdout_excess_vs_bh']:+.0f}."
        )
        # bear window
        if best["is"]["total_pnl"] <= 0:
            bullets.append(
                f"Same spec lost ${best['is']['total_pnl']:.0f} on the in-sample bear — "
                f"it is a bounce-catcher until proven on a down window."
            )
            direction = "cautious_swing"
            headline = (
                f"Park 1m scalps. The only spec that beat fees *and* buy-and-hold on the "
                f"recent bounce is {best['interval']}m {best['strategy']} on {best['pair']} "
                f"with a ~{best['hold_minutes']}m hold. Do not size it until it also survives a bear fold."
            )
        else:
            direction = "pursue_swing"
            headline = (
                f"Move the book to {best['interval']}m {best['strategy']} "
                f"({best['pair']}), maker-only, ~{best['hold_minutes']//60}h hold. "
                f"VWAP+EMA as filter, not 1m entries."
            )
    else:
        # anything +EV on holdout even if it lost to BH?
        pos = [r for r in grid if r["holdout"]["total_pnl"] > 0 and r["holdout"]["n_trades"] >= 8]
        if pos:
            best = max(pos, key=lambda r: r["holdout"]["profit_factor"])
            bullets.append(
                f"Best holdout PF among +EV cells: {best['strategy']} {best['pair']} "
                f"{best['interval']}m hold={best['hold_minutes']}m PF {best['holdout']['profit_factor']:.2f} "
                f"but excess vs buy-hold is {best['holdout_excess_vs_bh']:+.0f} — BH still wins."
            )
            direction = "bh_wins"
            headline = (
                "1m is dead; 15m is still fee-drag. A few 1h/4h breakout specs print green "
                "on the bounce but lose to buy-and-hold. Next edge is *not trading*, or a "
                "much slower trend system that has to beat BH on purpose."
            )
        else:
            direction = "abandon_short_horizon"
            headline = (
                "After clock-time holds and a VWAP+EMA filter, nothing at 15m/1h/4h is stably "
                "+EV after Kraken fees on holdout. Stop building entries on these bars."
            )
            bullets.append("Zero holdout cells with ≥8 trades were profitable after fees.")

    # WF
    if wf:
        keys = {}
        for r in wf:
            key = (r["pair"], r["interval"], r["strategy"])
            keys.setdefault(key, []).append(r["total_pnl"])
        scored = [
            {"pair": a, "interval": b, "strategy": c, "median": float(np.median(v)),
             "pos": int(sum(1 for x in v if x > 0)), "n": len(v)}
            for (a, b, c), v in keys.items()
        ]
        scored.sort(key=lambda x: x["median"], reverse=True)
        if scored:
            s = scored[0]
            bullets.append(
                f"Walk-forward leader (1d hold): {s['strategy']} {s['pair']} {s['interval']}m "
                f"median ${s['median']:.0f}, +EV {s['pos']}/{s['n']} folds."
            )

    kg = (knat.get("grid") or [])
    if kg:
        kpos = [r for r in kg if r["holdout"]["total_pnl"] > 0 and r["holdout"]["n_trades"] >= 8]
        bullets.append(
            f"Kraken-native H1 2026: {len(kpos)} holdout cells +EV after fees "
            f"(USD book, not USDT). Use this as venue confirmation, not a new hunt."
        )
        if survivors:
            # did the same spec work on Kraken?
            best = max(survivors, key=lambda r: r["holdout_excess_vs_bh"])
            match = [
                r for r in kg
                if r["pair"] == best["pair"]
                and r["strategy"] == best["strategy"]
                and r["interval"] == best["interval"]
                and r["hold_minutes"] == best["hold_minutes"]
            ]
            if match:
                m = match[0]
                bullets.append(
                    f"Same spec on Kraken H1 holdout: PnL ${m['holdout']['total_pnl']:.0f} "
                    f"PF {m['holdout']['profit_factor']:.2f} n={m['holdout']['n_trades']} "
                    f"excess vs BH {m['holdout_excess_vs_bh']:+.0f}."
                )

    bullets.append(
        "Do not paper a 1m VWAP fade. If anything is built next, it is a 1h/4h structure "
        "break with VWAP+EMA9/21 as a gate, maker limits, and an explicit beat-buy-and-hold test."
    )
    return {"headline": headline, "direction": direction, "bullets": bullets, "survivors": survivors[:8]}


def _markdown(report: dict) -> str:
    rec = report.get("recommendation") or {}
    lines = [
        f"# Direction study {report['stamp']}",
        "",
        rec.get("headline", ""),
        "",
        report.get("note", ""),
        "",
        "## Recommendation",
        "",
    ]
    for b in rec.get("bullets") or []:
        lines.append(f"- {b}")

    def dump_block(name: str, block: dict) -> None:
        if not block:
            return
        lines.extend(["", f"## {name}", "", "### Buy & hold", ""])
        seen = set()
        for key, bh in (block.get("buy_hold") or {}).items():
            pair = key.split("_")[0]
            if pair in seen:
                continue
            seen.add(pair)
            lines.append(
                f"- **{pair}** IS ${bh['is']['pnl']:.0f} ({bh['is']['ret_pct']:.1f}%) · "
                f"holdout ${bh['holdout']['pnl']:.0f} ({bh['holdout']['ret_pct']:.1f}%)"
            )
        lines.extend([
            "",
            "### Holdout leaders (min 8 trades)",
            "",
            "| pair | tf | strategy | hold | n | win | PnL | PF | vs BH | IS PnL |",
            "|------|----|----------|------|--:|----:|----:|---:|------:|-------:|",
        ])
        rows = [r for r in (block.get("grid") or []) if r["holdout"]["n_trades"] >= 8]
        rows.sort(key=lambda r: r["holdout"]["total_pnl"], reverse=True)
        for r in rows[:18]:
            h, i = r["holdout"], r["is"]
            lines.append(
                f"| {r['pair']} | {r['interval']}m | {r['strategy']} | {r['hold_minutes']}m | "
                f"{h['n_trades']} | {h['win_rate']*100:.0f}% | {h['total_pnl']:.1f} | "
                f"{h['profit_factor']:.2f} | {r['holdout_excess_vs_bh']:+.0f} | {i['total_pnl']:.1f} |"
            )

    dump_block("Binance year (USDT 1m → resampled)", report.get("binance") or {})
    dump_block("Kraken native H1 2026 (USD)", report.get("kraken_native") or {})
    lines.append("")
    return "\n".join(lines)
