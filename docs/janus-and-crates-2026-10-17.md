# Janus, the crates and trending coins (2026-10-17)

> The Python this cites (`src/crypto/`) left `main` on 2026-10-17 and runs unchanged from the tag
> `python-research-final`: see [research-code.md](research-code.md).

Research only; the bot is untouched. Three questions:

1. Should Janus manage the basket, perhaps with new longer-term detectors? Or should the long-term rule
   stay static in the bot, with Janus kept for short-term actions?
2. How should the bot use the published crates: exchange-apiws, indicators-ta, jflow-core and the
   rustrade family?
3. Are there trending or strong coins worth adding?

The engine, deposits ($250, then $50 a week) and start dates are the
[basket study](basket-study-2026-10-16.md)'s: every month from June 2021 to October 2025, each run to
2026-10-15.

## Verdict

1. **Keep the long-term rule static, in the bot.** Janus's regime detectors replaced the 200-day rule on
   daily and on weekly candles, in 18 variants. 17 did worse. The one that edged it, the weekly HMM as an
   extra bear filter at +0.7% a year, acted in 15 short episodes, helped in 9 and hurt in 6, and was the
   best of 18 tries: luck explains it as well as skill does. Longer-term detectors in Janus would
   duplicate what the 200-day rule already does, less well.
2. **Use Janus for short-term actions only in shadow.** Live, Janus has called every pair "neutral" on
   every logged day: 44 pair-days without one clear call, while 71 of 81 large Kraken coins sit in a
   200-day uptrend. Its minute-scale detectors measure intraday mean reversion, not the trend.
   Shorter-term trading on this account has already failed twice at Kraken's tier-1 fees. In the
   2026-10-05 swing study, daily swings lost to plain holding on every coin, weekly swings were too
   noisy to trust, and the trend rule beat both. In the earlier 1-hour research, no default strategy
   made money after fees. Every order the bot places is already post-only, so there is no fee left to
   save either.
3. **Don't add trending coins.** Re-picking three satellites every month by 30-day return, 90-day return
   or strength against BTC lost to holding no satellites at 91–100% of start dates, by 3.7–9.1% a year,
   on a coin list that flatters them. Today's leaders are listed below for reference, not as a buy list.
4. **The crates' best use is keeping the bot alive and honest.** The most valuable change found is not a
   signal: if the trading loop dies, nothing restarts it, the healthcheck still passes and nobody is
   told. Second is an alert on trades the bot didn't place, the failure that hit the 2026-10-14 deposit.

## 1. Janus's detectors in place of the 200-day rule

`tools/regime-replay` feeds candles to indicators-ta 0.3's detectors at their default settings: the
indicator detector (ADX, Bollinger, ATR, EMAs), the HMM, and their ensemble, which is what Janus's regime
bridge serves. `src/crypto/opt/janus_regime.py` maps each label to the bot's bull or bear in three ways
fixed up front, then runs the unchanged engine with the live weights:

- **hysteresis:** change only on an explicit bull or bear call, otherwise keep the last state, as the
  200-day band does.
- **strict:** bull only on a bull call; anything else is bear (half target).
- **defensive:** the 200-day rule, plus bear whenever the detector says bear.

Weekly bars run Monday to Sunday. A week's label is known at Sunday's close (UTC) and governs the next
seven days. Until a detector has warmed up, a coin follows the 200-day rule, so no variant sits in cash
for want of a reading. "Detector warm" is the share of coin-days the detector was in charge: the
indicator detector's 200-bar average needs almost four years of weekly bars. Nothing was tuned.

| Signal | Detector warm | Median return | Low (p10) | Median drawdown | Worst drawdown | Beats 200-day rule | Edge | Trades/yr |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| 200-day rule (live) | — | 16.2% | 5.1% | -44.2% | -63.9% | — | — | 153 |
| ensemble daily / hysteresis | 100% | 14.5% | 2.9% | -45.0% | -68.6% | 0% | -2.8% | 202 |
| ensemble daily / strict | 100% | 2.9% | -7.3% | -41.6% | -61.4% | 0% | -14.4% | 419 |
| ensemble daily / defensive | 100% | 16.4% | 5.1% | -44.2% | -62.0% | 45% | -0.2% | 220 |
| indicator daily / hysteresis | 100% | 11.8% | 0.8% | -46.4% | -64.2% | 0% | -5.0% | 199 |
| indicator daily / strict | 100% | 10.9% | 0.8% | -46.4% | -62.4% | 0% | -5.6% | 208 |
| indicator daily / defensive | 100% | 15.6% | 4.1% | -43.0% | -62.9% | 0% | -1.7% | 174 |
| hmm daily / hysteresis | 100% | 4.3% | -7.3% | -59.0% | -76.5% | 0% | -10.7% | 115 |
| hmm daily / strict | 100% | 4.3% | -4.5% | -40.9% | -60.1% | 0% | -13.4% | 186 |
| hmm daily / defensive | 100% | 10.2% | 0.3% | -44.5% | -62.1% | 0% | -5.9% | 191 |
| ensemble weekly / hysteresis | 98% | 13.4% | -0.1% | -45.4% | -66.6% | 0% | -2.5% | 137 |
| ensemble weekly / strict | 98% | 16.9% | 2.0% | -39.3% | -62.0% | 28% | -1.7% | 208 |
| ensemble weekly / defensive | 98% | 14.8% | 2.7% | -43.7% | -63.6% | 0% | -2.3% | 154 |
| indicator weekly / hysteresis | 74% | 6.4% | -13.5% | -56.0% | -73.2% | 0% | -9.7% | 134 |
| indicator weekly / strict | 74% | 6.8% | -13.5% | -56.0% | -73.2% | 0% | -9.4% | 133 |
| indicator weekly / defensive | 74% | 12.7% | -1.5% | -43.7% | -63.6% | 0% | -3.5% | 149 |
| hmm weekly / hysteresis | 92% | 11.5% | 1.1% | -52.3% | -71.1% | 0% | -4.8% | 109 |
| hmm weekly / strict | 92% | 12.9% | 6.1% | -38.2% | -58.6% | 38% | -4.3% | 110 |
| hmm weekly / defensive | 92% | 16.8% | 5.1% | -44.2% | -63.9% | 76% | +0.7% | 154 |

"Beats" is the share of the 53 start dates where the variant's money-weighted return was higher; "edge"
is the median difference. Some variants have a higher median return but still lose most head-to-head
comparisons, because the medians come from different start dates.

**Why they lose.** The detectors change their minds far more often than the trend does, and every change
buys or sells half of a coin's position at 0.5% in fees and slippage:

| Detector | Timescale | Label changes a year | What it says most |
|---|---|--:|---|
| Ensemble | daily | 48–99 | "uncertain", 52–79% of days |
| Indicator | daily | 7–14 | bear 45–64% of days, bull 34–55% |
| HMM | daily | 13–59 | "volatile", 62–96% of days |
| Ensemble | weekly | 12–18 | bear 26–49% of weeks, bull 28–38% |
| Indicator | weekly | 1–3 | still warming up for up to 59% of weeks |
| HMM | weekly | 2–9 | "volatile", 71–96% of weeks |
| 200-day rule | daily | 2.4–6.3 | the trend |

**The one variant that edged the live rule** (weekly HMM, defensive) differs from it only on days the HMM
says bear while the 200-day rule says bull. That happened in 15 episodes, all on ETH (11) and SOL (4),
each lasting 1–25 days. The coin then fell in 9 and rose in 6. All 53 start dates share most of their
path, so those same few episodes decide nearly every start; that is how 15 events can "win" 76% of
starts. Nine or more out of fifteen is what a fair coin gives about 30% of the time.

## 2. What Janus says live

The fks `janus-bridge` (Milestone 1, its rule predeclared on 2026-10-12) samples Janus every five
minutes. It logs a daily call per pair when at least 60% of the day's samples sit on one side, and it
never touches the bot.

| Logged days | Pair-days | Clear calls | "Neutral" all day | Highest bullish share in a day | Highest bearish share |
|---|--:|--:|--:|--:|--:|
| 2026-10-13 (partial) to 2026-10-16 | 44 | 0 | 44 | 17% | 2% |

Janus isn't stuck. Its label changed during 22 of the 44 pair-days, and its confidence ranged from 0.37
to 0.86. Its amygdala called the market low-volatility and mean-reverting 83% of the time. At the minute
scale this market mostly mean-reverts, and the bridge's mapping turns that into "neutral". Over the same
days the bot's 200-day rule had all six basket coins in a bull.

Two caveats:

- The bot's flags are empty in those records. The bridge got the corrected state path only when its
  container was recreated on 2026-10-17. Its parser matches the live state file (`XBTUSD` to
  `BTC/USDT`, and so on), so the record due at 00:05 UTC on 2026-10-18 should be the first to carry them.
- The bridge scores agreement with the 200-day rule, not profit. With no clear calls, it has nothing to
  score yet.

## 3. Trending coins

`src/crypto/opt/trending.py` re-picks three satellites, 5% each beside BTC 44 / ETH 22 / SOL 12, on the
first day of every month. It picks from the 20 most liquid coins at the time (trailing 90-day dollar
volume, at least 180 days listed), using only data up to the day before. A dropped coin is sold and a new
one bought, and the trend rule applies to satellites as to the rest. The 40 candidates are today's large
coins plus LUNA, FTT and EOS, a list that flatters any rule that buys winners.

| Satellites | Median return | Low (p10) | Median drawdown | Worst drawdown | Beats no satellites | Edge | Trades/yr |
|---|--:|--:|--:|--:|--:|--:|--:|
| No satellites (BTC/ETH/SOL only) | 14.4% | 3.2% | -43.2% | -63.1% | — | — | 94 |
| LINK/XRP/INJ (live, hindsight) | 16.2% | 5.1% | -44.2% | -63.9% | 100% | +2.4% | 153 |
| Hot 30 days, monthly | 12.3% | -0.1% | -47.5% | -66.0% | 9% | -3.7% | 232 |
| Hot 90 days, monthly | 6.7% | -4.2% | -48.0% | -67.3% | 0% | -8.8% | 207 |
| Strong vs BTC and in a bull trend, monthly | 5.5% | -4.3% | -49.0% | -67.2% | 0% | -9.1% | 198 |

Monthly momentum did worse than the basket study's yearly picks, which lost 2.4–3.7% a year, and it
churned more than twice as much as holding no satellites.

### What the screens see today (information only)

`src/crypto/opt/watchlist.py` reads CoinGecko's trending searches and its 100 largest coins, leaving out
stablecoins and wrapped or staked copies. It prices each coin on Kraken's own daily closes, here through
2026-10-16. 71 of the 81 Kraken-listed coins with 200 days of history are in a 200-day bull, so "in an
uptrend" separates almost nothing right now.

The live basket:

| Coin | 200-day trend | Above its 200-day average | 30 days | 90 days | vs BTC, 90 days | Kraken volume a day |
|---|---|--:|--:|--:|--:|--:|
| BTC | bull | +13% | +7% | +26% | 0% | $213.5M |
| ETH | bull | +13% | 0% | +30% | +4% | $87.3M |
| SOL | bull | +19% | +6% | +38% | +12% | $47.4M |
| LINK | bull | +26% | +12% | +48% | +22% | $8.3M |
| XRP | bull | +2% | +1% | +20% | -6% | $62.9M |
| INJ | bull | +70% | +61% | +69% | +43% | $2.8M |

Trending on CoinGecko and tradeable against USD on Kraken. DACITY, QTC, PRL and DNA were also trending
but have no Kraken USD pair.

| Trending rank | Coin | Market-cap rank | 200-day trend | Above its 200-day average | 30 days | 90 days | Kraken volume a day |
|--:|---|--:|---|--:|--:|--:|--:|
| 2 | TIA | 92 | bull | +80% | +101% | +90% | $1.3M |
| 3 | PENGU | 118 | bull | -1% | +9% | +23% | $2.7M |
| 4 | INJ | 84 | bull | +70% | +61% | +69% | $2.8M |
| 5 | SOL | 7 | bull | +19% | +6% | +38% | $47.4M |
| 6 | SUI | 28 | bull | +18% | +43% | +37% | $18.0M |
| 7 | STRK | 110 | bull | +103% | +165% | +153% | $3.1M |
| 8 | NEAR | 19 | bull | +123% | +93% | +162% | $28.8M |
| 11 | PUMP | 49 | bull | +71% | +25% | +173% | $6.9M |
| 13 | EIGEN | 152 | bull | +22% | +31% | +9% | $0.4M |
| 14 | ZK | 197 | bull | +36% | +86% | +76% | $0.1M |
| 15 | BTC | 1 | bull | +13% | +7% | +26% | $213.5M |

The strongest liquid coins outside the basket: in a 200-day bull, at least $1M a day on Kraken, ranked by
90-day return against BTC.

| Coin | Market-cap rank | Above its 200-day average | 30 days | 90 days | vs BTC, 90 days | Kraken volume a day |
|---|--:|--:|--:|--:|--:|--:|
| AKE | 98 | +281% | +49% | +1420% | +1395% | $1.9M |
| QNT | 38 | +133% | +228% | +214% | +188% | $15.9M |
| DRV | 125 | +155% | +59% | +208% | +182% | $1.6M |
| PUMP | 49 | +71% | +25% | +173% | +147% | $6.9M |
| RAY | 116 | +100% | +33% | +173% | +147% | $1.3M |
| NEAR | 19 | +123% | +93% | +162% | +136% | $28.8M |
| ZRO | 90 | +65% | +106% | +160% | +134% | $2.8M |
| STRK | 110 | +103% | +165% | +153% | +127% | $3.1M |
| ENA | 41 | +64% | +34% | +146% | +120% | $5.3M |
| ARB | 67 | +63% | +14% | +117% | +91% | $3.3M |

The top of that list shows what a momentum screen buys: coins that have already run, one of them by
1,420% in 90 days.

## 4. The crates

| Crate | What the bot uses now | Recommendation |
|---|---|---|
| exchange-apiws 0.11.0 | All Kraken access, pinned to git commit `50874dc` (#78: post-only and other order options, filtered ledgers, trade volume), which is newer than 0.11.0. | Publish 0.12.0 (the crates.io publish workflow landed in #80) and depend on `exchange-apiws = "0.12"`. It already has `get_closed_orders` and `get_trades_history`, which the foreign-trade alert below needs. WebSockets aren't worth it for an hourly loop. |
| rustrade-framework, rustrade-core 0.5.2 | `live.rs` implements rustrade's `ExchangeClient` for Kraken. `brains.rs`, `kraken_src.rs` and `paper_ex.rs` are a parallel framework that their own doc comments mark as not wired into the live path; `features.rs` and `signal.rs` serve only them. | Delete the dead modules, which also drops indicators-ta from the bot. Keep the `ExchangeClient` implementation. |
| rustrade-supervisor 0.5.2 | Not used. The web UI starts the trading loop as a child process and never restarts it. `/api/status` answers 200 with `running: false`, so the Docker healthcheck passes over a dead loop. | Restart the child with backoff, answer 503 while it isn't running, and post to Discord. The supervisor restarts with backoff out of the box, but only in-process tasks, so adopting it means moving the loop into the web UI's process. Do the small fix first. |
| rustrade-risk 0.5.2 | Not used. The only guard is the `FROZEN` policy, a tested switch that stops all orders. | Skip the loss breakers: a daily-loss or drawdown halt would freeze the trend rule mid-crash, which is why the old spot bot's halt was disabled. Do add a pre-trade sanity cap on how much one order or one day may buy or sell, as a share of the account, modelled on `PortfolioRisk`'s pre-trade gate. |
| rustrade-backtest 0.5.2 | Not used. Research runs on `basket.py`, a Python copy of the policy. | It replays several symbols against shared cash with maker/taker fees, but has no deposits. A deposit schedule, shaped like its `FundingModel`, would let the bot's own policy code drive the backtest. Low priority while the forward test's replay of the real account checks that the copy matches. |
| indicators-ta 0.3.0 | Only the dead paper-book signals (EMA, ATR, VWAP). The 200-day rule is hand-written in `regime.rs` and copied in `basket.py`. | Add the 200-day ±5% band as an indicator, and use it in the bot, in Janus and in research: one definition instead of three. Its regime detectors are tuned for intraday bars; at daily and weekly scale they mostly say "uncertain" or "volatile". |
| jflow-core 0.1.0 | Not used. It holds Janus's internal types: signals, config, metrics and channels. | Nothing for the bot. It matters only if the bot consumed Janus in-process, which this report argues against. |

## 5. Next steps, in order of value

1. **Make a dead trading loop visible and self-healing** (bot). Restart the child with backoff, return
   503 from `/api/status` while the loop is down so Docker marks the container unhealthy, and post to
   Discord on every unexpected exit. A silent stop leaves deposits in cash and misses flips.
2. **Alert on trades the bot didn't place** (bot). Each tick, compare Kraken's closed orders with the
   txids the bot recorded, then post to Discord and stop ordering on any stranger. This would have caught
   the FKS copy trading the 2026-10-14 deposit. It has to run inside the bot, the account's only
   private-API caller.
3. **Publish exchange-apiws 0.12.0 and drop the git pin** (crates).
4. **Delete the dead rustrade scaffolding** (bot).
5. **Add a per-order and per-day sanity cap** (bot).
6. **Put one 200-day band in indicators-ta** (crates). If Janus should know the long-term trend, it can
   serve this band, and the bridge can flag days when Janus's copy and the bot's disagree: a free check
   on both data feeds.
7. **Leave Janus in shadow** (Janus). Revisit once the bridge has 90 days of records that include clear
   calls.

## Reproduce

```sh
(cd tools/regime-replay && cargo build --release)
PYTHONPATH=src python -m crypto.opt.janus_regime   # about 90 s
PYTHONPATH=src python -m crypto.opt.trending       # about 30 s
PYTHONPATH=src python -m crypto.opt.watchlist      # about 2 min, live public data
```

Results land in `data/basket_results/`, which is not committed: `janus_regime_summary.csv`,
`janus_regime_sweep.csv`, `janus_label_shares.csv`, `trending_summary.csv`, `trending_sweep.csv` and
`watchlist_<day>.csv`. The candles come from `python -m crypto.opt.basket_data`.
