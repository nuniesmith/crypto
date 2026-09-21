# Research conclusions (as of 2026-09-20)

These decisions are why the live bot is **1h SOL + ETH only**. Raw reports land in `data/studies/` on the machine that ran them (gitignored).

Gate used everywhere: **Kraken Pro tier 3, maker entry, net of fees, versus buy-and-hold**, plus expanding walk-forward. A green holdout that loses to BH or fails WF is not an add-to-live.

## Timeframes

| Horizon | Result |
|---|---|
| 1m VWAP fade / scalp | Dead after fees. Do not paper. |
| 15m | Still fee-drag. Almost no +EV cells that beat BH. |
| 1h / 4h structure break | Can print green on a bounce. Almost never beats BH *and* survives WF. |
| Daily SMA200 / slow trend on BTC | Cash in the bear, cash on the bounce. Not an edge. |

VWAP + EMA9/21 are a **filter** on structure/range breaks, not a 1m entry. Holds are **clock time** (2h / 1d / 7d), not bar counts.

## Why these live sleeves

From the 1h walk-forward + direction grid on BTC/ETH/SOL (`20260920T134848Z_study`, `20260920T135527Z_direction`):

- Lead: **SOLUSD 1h `trendline_break`**, 24h hold, maker. WF median was the only cell with 3/5 +EV folds.
- Second: **ETHUSD 1h `structure_filtered`** (VWAP+EMA gate). Same hold.
- **SOLUSD buy-and-hold** as the $1k benchmark book so Discord always shows “did the signal beat sitting in SOL?”

1m/15m families (`vwap_mr`, `ema_pullback`, tight range breaks) were abandoned.

## BTC (`XBTUSD`)

Re-ran the 15m/1h/4h grid (`20260920T194344Z_direction`) and a daily long-only screen (`20260920T194851Z_btc_slow`).

- Best 1h cell (trendline, 7d hold) was +EV on the bounce and **still lost to BH**.
- Same sleeves as live (1h, 24h hold): tiny green holdout, **0–1/5** WF.
- Daily SMA20 cut bear DD and kept most of the bounce; WF still 2/5.
- SMA200 “won” the bear by not trading, then missed the recovery.

**Do not add BTC 1h signals.** BTC on Kraken is a 70/30 HODL mix vs USD (±10% band, at most one rebalance per day). ETH/SOL stay as small piles the 1h books may trade; they are not sold just to sit in USD. Live order size is the wallet, not $1k.

## Other Kraken USD names (screenshot screen)

20 pairs with a year of 15m Binance Vision, same grid (`20260920T202418Z_direction`). Skipped forex. No year of history: FARTCOIN, MEW, PONKE, FWOG, USELESS.

The 60d holdout was a bounce: PUMP +112%, NEAR +85%, AVAX +54% **buy-and-hold**. The 1h/24h live sleeve lost to BH on almost every name.

| Candidate | Why not live |
|---|---|
| XRP 1h trendline | Beat BH on the bounce; **1/5** WF; lost the bear. |
| TRUMP 1h structure | Only 1h spec green IS *and* beat BH; 568K vol; political gap risk. |
| FET 4h range-filter | Best excess vs BH; not the 1h engine; thin trade counts on WF. |
| VIRTUAL, WIF, BONK, PEPE, … | Lose to BH and/or too thin for $1k books. |

**Do not add XRP, FET, TRUMP, or memes.** Green 24h candles in the Kraken app are the bounce from holding, not from these signals.

## How to re-run

```bash
.venv/bin/crypto fetch-history --pair XBTUSD ETHUSD SOLUSD --days 400
.venv/bin/crypto fetch-history --pair XRPUSD AVAXUSD --days 400 --interval 15
.venv/bin/crypto direction --pair XBTUSD ETHUSD SOLUSD --days 365 --holdout-days 60 --fee-tier 3
.venv/bin/crypto study --pair SOLUSD --interval 60 --days 365 --holdout-days 60 --folds 6 --trials 80
```

`direction --pair` filters the grid. `fetch-history --interval 15` is enough for 15m/1h/4h resample. Kraken public OHLC is only ~720 bars (~12h at 1m, ~30d at 1h); year-scale data is Binance Vision USDT mapped to Kraken USD names. Rank on that tape; do not treat dollar PnL as Kraken-USD truth.
