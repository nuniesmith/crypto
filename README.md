# crypto — 1-minute Kraken spot scalper (BTC / ETH / SOL)

Fee-aware research + live-ready skeleton for high-frequency spot scalping on Kraken.
Architecture deliberately mirrors the futures walk-forward optimizer so we can reuse
the same study discipline, risk-policy separation, and Optuna workflow.

## Why fees dominate

Kraken Pro spot (July 2026+ schedule):

| Tier | 30-day vol or AoP | Maker | Taker |
|------|-------------------|-------|-------|
| 1    | $0                | 0.40 %| 0.80 %|
| 3    | $10 k / $20 k     | 0.22 %| 0.38 %|
| 6    | $100 k / $200 k   | 0.12 %| 0.25 %|
| 12   | $10 M / $10 M     | 0.00 %| 0.10 %|

A 1-minute scalp that captures 0.15 % of price movement is **net negative** at
retail tiers if both legs are taker. The back-tester therefore:

* charges realistic maker/taker percentages (configurable per leg),
* supports “maker-first” entry (limit at signal price, timeout → cancel or market),
* requires positive expectancy **after** fees before a study is accepted.

## Core signal (your TradingView script)

`pine/vwap_ema9.pine` — session VWAP + EMA(9) with optional smoothing / Bollinger.
Strategies below treat VWAP as the mean and EMA9 as short-term momentum / filter.

## Quick start

```bash
python3 -m venv .venv
.venv/bin/pip install -e ".[dev]"
cp .env.example .env          # only needed for live keys

# Fetch ~30 days of 1-minute bars (public API, no key)
.venv/bin/crypto fetch --pair XBTUSD ETHUSD SOLUSD --days 30

# Run a baseline mean-reversion study
.venv/bin/crypto research --pair XBTUSD --days 14

# Optuna tune (fee-aware)
.venv/bin/crypto optimize --pair XBTUSD --trials 200 --fee-tier 3
```

Data lands in `data/` (parquet + sqlite cache). Studies are resume-able.

## Package layout

```
src/crypto/
  data/          # Kraken public OHLC + local cache
  features/      # VWAP, EMA, ATR, session flags (static + dynamic)
  strategies/    # VWAP mean-reversion, EMA cross, breakout scalps
  sim/           # bar-by-bar simulator with maker/taker fees & slippage
  opt/           # Optuna search, walk-forward, metrics
pine/            # TradingView indicators (your VWAP+EMA9 + future alerts)
```

## Re-used ideas from the futures repo

* Static features computed once, dynamic features per trial.
* Risk policy is **not** searchable (account size, max risk %, daily halt).
* Locked hold-out + baseline comparison before export.
* Study schema versioning so parameter-space changes invalidate old trials.
* Commission / slippage applied on every fill; PnL is always net.

## Next steps (in rough priority)

1. Solidify 1 m data pipeline + gap handling.
2. Implement 2–3 fee-robust scalp rules and unit-test the simulator.
3. Walk-forward Optuna with realistic fee tiers.
4. Export best params → Pine alerts + thin Python live runner (ccxt or raw REST).
5. Paper-trade on Kraken, then size up only after live expectancy matches back-test.

## Disclaimer

This is research code. Crypto markets are 24/7, highly competitive, and fee
drag kills most high-frequency retail strategies. Never risk capital you cannot
afford to lose. Past performance (even after fees) is not indicative of future
results.
