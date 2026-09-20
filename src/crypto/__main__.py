"""CLI entry-point: crypto fetch | research | optimize | status."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="crypto",
        description="1-minute Kraken spot scalper — fetch, research, optimize",
    )
    sub = parser.add_subparsers(dest="cmd", required=True)

    # ---- fetch ----------------------------------------------------------
    p_fetch = sub.add_parser("fetch", help="Download 1-minute OHLCV from Kraken")
    p_fetch.add_argument(
        "--pair",
        nargs="+",
        default=["XBTUSD"],
        help="Kraken pair codes, e.g. XBTUSD ETHUSD SOLUSD",
    )
    p_fetch.add_argument("--days", type=int, default=7, help="Look-back days")
    p_fetch.add_argument("--interval", type=int, default=1, help="Minutes (1,5,15,...)")

    # ---- research -------------------------------------------------------
    p_res = sub.add_parser("research", help="Baseline strategy report")
    p_res.add_argument("--pair", default="XBTUSD")
    p_res.add_argument("--days", type=int, default=14)
    p_res.add_argument(
        "--fee-tier",
        type=int,
        default=1,
        help="Kraken fee tier 1–12 (affects maker/taker %)",
    )
    p_res.add_argument("--strategy", default="vwap_mr", choices=["vwap_mr", "ema_cross"])

    # ---- optimize -------------------------------------------------------
    p_opt = sub.add_parser("optimize", help="Optuna fee-aware search")
    p_opt.add_argument("--pair", default="XBTUSD")
    p_opt.add_argument("--days", type=int, default=30)
    p_opt.add_argument("--trials", type=int, default=100)
    p_opt.add_argument("--fee-tier", type=int, default=3)
    p_opt.add_argument("--strategy", default="vwap_mr")

    # ---- status ---------------------------------------------------------
    sub.add_parser("status", help="Show cached data & latest studies")

    args = parser.parse_args(argv)

    if args.cmd == "fetch":
        from crypto.data.fetch import fetch_pairs

        fetch_pairs(args.pair, days=args.days, interval=args.interval)
        return 0

    if args.cmd == "research":
        from crypto.opt.research import run_research

        run_research(
            pair=args.pair,
            days=args.days,
            fee_tier=args.fee_tier,
            strategy=args.strategy,
        )
        return 0

    if args.cmd == "optimize":
        from crypto.opt.search import run_optimize

        run_optimize(
            pair=args.pair,
            days=args.days,
            n_trials=args.trials,
            fee_tier=args.fee_tier,
            strategy=args.strategy,
        )
        return 0

    if args.cmd == "status":
        from crypto.data.store import status

        status()
        return 0

    return 1


if __name__ == "__main__":
    sys.exit(main())
