"""CLI entry-point: crypto fetch | fetch-history | import-ohlcvt | research | optimize | status."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="crypto",
        description="Kraken research CLI — history, fee-aware study/direction, paper bot launcher",
    )
    sub = parser.add_subparsers(dest="cmd", required=True)

    p_fetch = sub.add_parser("fetch", help="Download recent 1-minute OHLCV from Kraken API (~12 h only)")
    p_fetch.add_argument("--pair", nargs="+", default=["XBTUSD"], help="Kraken pair codes")
    p_fetch.add_argument("--days", type=int, default=7, help="Look-back days")
    p_fetch.add_argument("--interval", type=int, default=1, help="Minutes")

    p_hist = sub.add_parser(
        "fetch-history",
        help="Download ≥1 year of OHLCV via Binance Vision (Kraken public 1m is ~12h)",
    )
    p_hist.add_argument("--pair", nargs="+", default=["XBTUSD", "ETHUSD", "SOLUSD"])
    p_hist.add_argument("--days", type=int, default=400, help="Look-back days (default 400 ≈ 13 months)")
    p_hist.add_argument("--interval", type=int, default=1, help="Bar minutes (1, 15, 60, 240)")

    p_imp = sub.add_parser("import-ohlcvt", help="Import Kraken official OHLCVT CSV into data/*.parquet")
    p_imp.add_argument("path", help="Directory or single CSV with Kraken OHLCVT files")
    p_imp.add_argument("--interval", type=int, default=1)
    p_imp.add_argument("--pair", nargs="*", default=None, help="Optional filter e.g. XBTUSD ETHUSD")

    p_res = sub.add_parser("research", help="Baseline strategy report")
    p_res.add_argument("--pair", default="XBTUSD")
    p_res.add_argument("--days", type=int, default=14)
    p_res.add_argument("--fee-tier", type=int, default=1, help="Kraken fee tier 1-12 (affects maker/taker %%)")
    p_res.add_argument("--strategy", default="vwap_mr",
                       choices=["vwap_mr", "ema_cross", "range_break", "ema_pullback", "trendline_break"])

    p_opt = sub.add_parser("optimize", help="Optuna fee-aware search")
    p_opt.add_argument("--pair", default="XBTUSD")
    p_opt.add_argument("--days", type=int, default=30)
    p_opt.add_argument("--trials", type=int, default=100)
    p_opt.add_argument("--fee-tier", type=int, default=3)
    p_opt.add_argument("--strategy", default="vwap_mr",
                       choices=["vwap_mr", "ema_cross", "range_break", "ema_pullback", "trendline_break"])

    p_study = sub.add_parser(
        "study",
        help="In-depth walk-forward + holdout + Optuna across BTC/ETH/SOL (needs fetch-history first)",
    )
    p_study.add_argument("--pair", nargs="+", default=["XBTUSD", "ETHUSD", "SOLUSD"])
    p_study.add_argument("--days", type=int, default=365)
    p_study.add_argument("--holdout-days", type=int, default=60)
    p_study.add_argument("--folds", type=int, default=6)
    p_study.add_argument("--trials", type=int, default=80)
    p_study.add_argument("--fee-tier", type=int, default=3)
    p_study.add_argument("--interval", type=int, default=1, help="Bar minutes (1, 5, 15, 60)")

    p_dir = sub.add_parser(
        "direction",
        help="Clock-time 15m/1h/4h study: VWAP+EMA filter, vs buy-hold, Kraken-native replay",
    )
    p_dir.add_argument("--pair", nargs="+", default=["XBTUSD", "ETHUSD", "SOLUSD"])
    p_dir.add_argument("--days", type=int, default=365)
    p_dir.add_argument("--holdout-days", type=int, default=60)
    p_dir.add_argument("--folds", type=int, default=6)
    p_dir.add_argument("--fee-tier", type=int, default=3)

    p_paper = sub.add_parser("paper", help="Run the Rust Kraken paper bot (exchange-apiws)")
    p_paper.add_argument("--loop", action="store_true", help="Stay up; act on each new closed 1h bar")
    p_paper.add_argument("--status", action="store_true", help="Print paper books only")
    p_paper.add_argument("--build", action="store_true", help="Force cargo build --release first")

    sub.add_parser("status", help="Show cached data")

    args = parser.parse_args(argv)

    if args.cmd == "fetch":
        from crypto.data.fetch import fetch_pairs
        fetch_pairs(args.pair, days=args.days, interval=args.interval)
        return 0

    if args.cmd == "fetch-history":
        from crypto.data.history import fetch_history
        fetch_history(args.pair, days=args.days, interval=args.interval)
        return 0

    if args.cmd == "import-ohlcvt":
        from crypto.data.import_ohlcvt import import_csv, import_dir, import_zip
        p = Path(args.path)
        if p.is_file() and p.suffix.lower() == ".zip":
            import_zip(p, interval=args.interval, pairs=args.pair)
        elif p.is_file():
            import_csv(p, interval=args.interval)
        else:
            import_dir(p, interval=args.interval, pairs=args.pair)
        return 0

    if args.cmd == "research":
        from crypto.opt.research import run_research
        run_research(pair=args.pair, days=args.days, fee_tier=args.fee_tier, strategy=args.strategy)
        return 0

    if args.cmd == "optimize":
        from crypto.opt.search import run_optimize
        run_optimize(pair=args.pair, days=args.days, n_trials=args.trials,
                     fee_tier=args.fee_tier, strategy=args.strategy)
        return 0

    if args.cmd == "study":
        from crypto.opt.study import run_study
        run_study(
            pairs=args.pair,
            days=args.days,
            holdout_days=args.holdout_days,
            folds=args.folds,
            trials=args.trials,
            fee_tier_tune=args.fee_tier,
            interval=args.interval,
        )
        return 0

    if args.cmd == "direction":
        from crypto.opt.direction import run_direction
        run_direction(
            pairs=args.pair,
            days=args.days,
            holdout_days=args.holdout_days,
            folds=args.folds,
            fee_tier=args.fee_tier,
        )
        return 0

    if args.cmd == "paper":
        return _run_paper_bot(looping=args.loop, status_only=args.status, force_build=args.build)

    if args.cmd == "status":
        from crypto.data.store import status
        status()
        return 0

    return 1


def _run_paper_bot(*, looping: bool, status_only: bool, force_build: bool) -> int:
    import subprocess

    root = Path(__file__).resolve().parents[2]
    manifest = root / "bot" / "Cargo.toml"
    bin_path = root / "bot" / "target" / "release" / "crypto-bot"
    if force_build or not bin_path.exists():
        print("building crypto-bot (release, exchange-apiws / Kraken)…")
        r = subprocess.run(
            ["cargo", "build", "--release", "--manifest-path", str(manifest)],
            cwd=root / "bot",
        )
        if r.returncode != 0:
            return r.returncode
    argv = [str(bin_path)]
    if status_only:
        argv.append("status")
    else:
        argv.append("paper")
        if looping:
            argv.append("--loop")
    return subprocess.call(argv)


if __name__ == "__main__":
    sys.exit(main())
