# Research code (Python, archived)

The live bot has always been Rust (`bot/`). The research behind it was Python
under `src/crypto/`: 5,210 lines that nothing in the bot, its Docker image or
its deploy ever ran. On 2026-10-17 it left `main` so the repo is Rust-only. It
is not lost. The tag **`python-research-final`** is the last commit that has
it, and every command in the study docs runs from there unchanged.

```bash
git clone --branch python-research-final https://github.com/nuniesmith/crypto ~/github/crypto-research
cd ~/github/crypto-research
python3 -m venv .venv && .venv/bin/pip install -e .
```

Use a fresh clone, not the live checkout (`~/github/crypto`): never switch
branches there.

## The forward test (still running)

`crypto.opt.forward` froze the basket study's portfolios on 2026-10-16 and
scores them only on Kraken daily candles from that day on, so it is the one
piece of this code with a future. A tag suits it: the module says nothing in it
may be tuned after the fact, and a tagged commit cannot be edited. It needs no
key (Kraken's public OHLC, no nonce) and reads the live state read-only:

```bash
.venv/bin/python -m crypto.opt.forward --actual /home/actions/oryx/data/crypto-bot/paper/state.json
```

Kraken returns the last 720 daily candles, so it can score about 18 months of
forward data before it needs another source.

## What each part was

| Code | What it did | Where the result lives |
|---|---|---|
| `__main__.py`, `data/`, `features/`, `sim/`, `strategies/`, `opt/study.py`, `opt/search.py`, `opt/direction.py`, `opt/research.py`, `opt/metrics.py` | The 1m–4h strategy research: Binance Vision history, a fee-aware replay, Optuna walk-forwards, trendline/structure/range/VWAP/EMA signals | [research.md](research.md). Verdict: at Kraken's tier-1 fees, 0 of 7 default combos were profitable even in-sample, and the 1h books lost to holding on the holdout |
| `opt/regime.py`, `opt/swing.py`, `opt/portfolio.py` | Daily trend rules (200-day average ±5%), swing slices on Bollinger and Fear & Greed, and drift rebalancing versus trading only on a regime flip | `bot/src/regime.rs` and `bot/src/alloc.rs` docs: the trend rule won, drift rebalancing added about 30 trades a year for nothing |
| `opt/basket.py`, `opt/basket_data.py`, `opt/basket_study.py`, `opt/forward.py` | A day-by-day replay of the live policy over many coins, deposits included, plus the frozen forward test | [basket-study-2026-10-16.md](basket-study-2026-10-16.md) |
| `opt/janus_regime.py`, `opt/trending.py`, `opt/watchlist.py` | Janus's regime detectors on daily candles, monthly trending-coin picks, a CoinGecko watchlist | [janus-and-crates-2026-10-17.md](janus-and-crates-2026-10-17.md) |

## Research in Rust from here

`basket.py` was a Python copy of the policy, so a study could disagree with
the bot it was judging. The Rust replacement should replay the bot's own
`alloc` and `regime` code over daily candles, with a deposit schedule. That
starts with a port of the forward test, checked against the tagged Python on
the same candles before the Python one is retired.
[janus-and-crates-2026-10-17.md](janus-and-crates-2026-10-17.md) lists what
`rustrade-backtest` and `indicators-ta` would need for that.
