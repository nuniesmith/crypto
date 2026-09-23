# Research conclusions (as of 2026-09-20)

These decisions are why the live bot is **1h SOL + ETH only**. Raw reports land in `data/studies/` on the machine that ran them (gitignored).

Gate used everywhere: **Kraken Pro tier 3, maker entry, net of fees, versus buy-and-hold**, plus expanding walk-forward. A green holdout that loses to BH or fails WF is not an add-to-live.

## 2026-09-21 re-run: the live sleeves do NOT clear this gate

Re-ran `direction --pair SOLUSD ETHUSD XBTUSD --days 365 --holdout-days 60
--fee-tier 3` (`20260921T115031Z_direction`) to answer one question the rest
of this document never states: **do the two sleeves holding real money beat
buy-and-hold?**

They do not.

| sleeve | n | win | holdout PnL | PF | **vs BH** | WF (1d hold) |
|---|--:|--:|--:|--:|--:|---|
| SOLUSD 60m `trendline_break` | 18 | 17% | +91.4 | 1.40 | **−325** | med +$118, 3/5 |
| ETHUSD 60m `structure_filtered` | 12 | 8% | +77.9 | 1.43 | **−280** | med +$23, 3/5 |

Buy-and-hold over the same holdout: **SOL +$416 (+42.6%)**, ETH +$358 (+36.8%).

**Zero of the 18 cells in the holdout-leaders table beat buy-and-hold.** The
closest is XBTUSD 60m trendline at −62 — and this document already refuses
BTC on exactly that basis ("still lost to BH"). The rule was applied to BTC
and not to the sleeves that went live.

The gate at the top of this file says: *a green holdout that loses to BH or
fails WF is not an add-to-live.* Both live sleeves lose to BH. They pass the
WF half (3/5 folds, positive median) and fail the BH half. The gate is an
AND.

### What can and cannot be claimed for them

- **The 60-day holdout was a bounce.** A long-only signal that sits in cash
  much of the time is expected to lose to BH in a bull leg, so this window
  flatters holding. That is a real caveat — and it is also exactly why the
  gate exists: you do not get to pick the window after seeing it.
- **IS PnL is not evidence.** SOL trendline shows IS +365.8 against a BH of
  −680, which looks like a strategy that protects in a bear. In-sample is
  where Optuna fitted the parameters; quoting it as edge is the mistake this
  whole file was written to avoid. Adding IS to holdout to claim the sleeve
  beats BH over the full 400 days is the same mistake wearing a hat.
- **Walk-forward is the honest multi-window test**, and it is weakly
  positive: 3/5 folds, median +$118 (SOL) and +$23 (ETH). Weak is not
  nothing, but 3/5 is close to a coin flip on fold count.
- **The samples are tiny.** 18 and 12 holdout trades. ETH wins **8%** of
  them — one winner in twelve — so its entire +$77.9 rests on a single
  trade. That is a lottery ticket, not a distribution.

The tool's own recommendation line, unchanged from the run: *"Next edge is
not trading, or a much slower trend system that has to beat BH on purpose."*

### Open decision

Either take the two sleeves off and let the wallet be a BTC/USD hold plus
whatever ETH/SOL is already there, or keep them knowingly as a bear-regime
hedge that is expected to underperform in a bounce — but not on the basis
that this document says they passed, because it does not.

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

- Lead: **SOLUSD 1h `trendline_break`**, 24h hold, maker. WF median was the only cell with 3/5 +EV folds. **It loses to buy-and-hold on the holdout by $325 — see the 2026-09-21 re-run above.**
- Second: **ETHUSD 1h `structure_filtered`** (VWAP+EMA gate). Same hold. **Also loses to BH, by $280.**
- **SOLUSD buy-and-hold** as the $1k benchmark book so Discord always shows “did the signal beat sitting in SOL?”

1m/15m families (`vwap_mr`, `ema_pullback`, tight range breaks) were abandoned.

## BTC (`XBTUSD`)

Re-ran the 15m/1h/4h grid (`20260920T194344Z_direction`) and a daily long-only screen (`20260920T194851Z_btc_slow`).

- Best 1h cell (trendline, 7d hold) was +EV on the bounce and **still lost to BH**.
- Same sleeves as live (1h, 24h hold): tiny green holdout, **0–1/5** WF.
- Daily SMA20 cut bear DD and kept most of the bounce; WF still 2/5.
- SMA200 “won” the bear by not trading, then missed the recovery.

**Do not add BTC 1h signals.** BTC is HODL. The 70/30 mix **only sells BTC** if it is above 80% of BTC+USD — it will not buy BTC with ETH/SOL proceeds. ETH/SOL 1h books trade the wallet pile (or buy with leftover USD above the 30% cash floor). Live size is the wallet, not $1k. A paper $1k enter that never fills must not dump coins on exit.

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

## 2026-09-23: 28 more coins, same verdict

Screened 28 Kraken USD names the earlier grid could not reach: ATOM, LTC, UNI,
AAVE, INJ, TIA, SUI, APT, ARB, FIL, XLM, HBAR, ALGO, ICP, GRT, IMX, CRV, LDO,
ENA, ONDO, JUP, SEI, STX, TRX, ETC, BCH, VET, RUNE
(`20260923T004306Z_direction`, 365d / 60d holdout / fee tier 3).

**Nothing clears the bar. Do not add any of them.**

First, why this run was even possible: the previous "screen more coins" attempt
failed on 12 of 18 names with "No Binance 1m mapping". That was not a network
problem -- `BINANCE_PAIR` is a hand-written table and its entries reduced to
exactly the 23 coins already screened. The tool could only re-test its own past
conclusions. The table is now wider and `LOCAL_PAIR` is derived from it.

### The window shape dominates everything

IS was a brutal bear -- every one of the 28 is between -52% and -87% buy-and-hold.
The 60d holdout was a violent bounce: ARB +162%, UNI +139%, ENA +140%, STX +108%,
VET +95%. Against a bounce that steep, anything that sits in cash part of the
time loses to buy-and-hold almost by construction. Every single entry in the
holdout-leaders table posts a big PnL and a NEGATIVE excess (-154 to -814).

### RUNE: the walk-forward leader that fails the holdout

`structure_filtered RUNEUSD 60m` tops the walk-forward table -- 4/5 folds +EV,
median $92, 78 trades -- and it is the same engine already live on ETH, which
makes it the most tempting name in the run.

| window | trades | PnL | PF | vs BH |
|---|---:|---:|---:|---:|
| walk-forward | 78 | median +92 | - | - |
| in-sample | 96 | +314 | - | +978 |
| **holdout** | **17** | **-212** | **0.29** | **-734** |

The folds and the holdout disagree, and the holdout is the one nothing was
fitted to. A PF of 0.29 is not a marginal miss. **This is the trap the whole
holdout exists to catch**, and it caught it.

### ATOM is the only name that beat buy-and-hold in BOTH windows

`range_break ATOMUSD 240m hold=1440m`:

| window | trades | PnL | PF | vs BH |
|---|---:|---:|---:|---:|
| in-sample (bear) | 78 | -323 | - | **+359** |
| holdout (bounce) | 17 | +354 | 4.60 | **+86** |

It lost money in the bear but lost far less than holding, and beat holding on
the bounce. That is the only spec in 28 coins to manage both. Still not
tradeable as it stands: 4h is not the live 1h engine, 17 holdout trades is thin,
and +86 excess is small against that noise. It is the one name worth a dedicated
study rather than a line in a screen.

### Ignore the profit factors on 1-3 trade cells

Several grid rows report PF values like -2.9e14. Those are divide-by-zero
artifacts on cells with three trades and no losers. Any ranking that sorts on
PF without a trade-count floor will surface them first.

### Standing conclusion, now over 48 coins

Two screens, 48 distinct names, one engine. Nothing has beaten buy-and-hold on
an untouched holdout with a believable trade count. The live sleeves remain
ETH/SOL 1h at 20%, and that allocation is tuition, not a proven edge.
