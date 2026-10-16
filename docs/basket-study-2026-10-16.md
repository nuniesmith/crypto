# Long-term basket study (2026-10-16)

Research only; the bot is untouched. Question: how does the live policy (v3: BTC 44 / ETH 22 / SOL 12 /
LINK, XRP, INJ 5 each, the daily 200-day ±5% trend rule, trades only on flips and deposits) do with a
weekly deposit, and should any coins be added?

## Verdict

1. **Keep the trend rule.** With $250 + $50 a week, it beat the same coins without it at
   89% of monthly start dates since June 2021
   (median +3.8% a year) and cut the worst
   drawdown from -79.9% to
   -63.9%.
2. **Don't add coins.** Satellites picked by any rule you could have followed at the time lowered returns
   against BTC/ETH/SOL alone. Random alt picks beat BTC/ETH/SOL alone in
   4% of 200 draws; in the longer 2019–2026 window,
   10% of 200 draws beat BTC/ETH alone.
3. **LINK/XRP/INJ look good only in hindsight.** They were picked on 2026-10-09 from a screen over the same
   history used to validate them. Against random picks they score in the top
   1% — what choosing with hindsight produces.
4. **BTC alone with the trend rule matched the live basket** in the main window with a smaller drawdown and a
   third of the trades, and beat BTC/ETH at most start dates since 2019. ETH has lagged BTC since 2021.

## Start-date sweep (main window, every month June 2021 – October 2025, each run to 2026-10-15)

| Policy | Median return | Low (p10) | High (p90) | Median drawdown | Worst drawdown | Live wins | Live edge | Trades/yr |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| Live policy (trend rule) | 16.2% | 5.1% | 29.5% | -44.2% | -63.9% | — | — | 153 |
| Same coins, no trend rule | 15.3% | -1.7% | 27.9% | -60.1% | -79.9% | 89% | +3.8% | 105 |
| BTC/ETH/SOL only (trend rule) | 14.4% | 3.2% | 25.4% | -43.2% | -63.1% | 100% | +2.4% | 94 |
| Old v2: 50/25/15, 10% cash (trend rule) | 14.2% | 3.2% | 25.1% | -42.5% | -62.8% | 100% | +2.5% | 95 |
| BTC only, no trend rule | 17.1% | -4.7% | 30.0% | -53.1% | -77.1% | 62% | +1.4% | 52 |
| BTC only (trend rule) | 19.0% | 3.3% | 29.5% | -37.7% | -60.7% | 47% | -0.2% | 48 |

## Satellites chosen without hindsight (re-chosen each January)

| Rule | Median return | Whole window | Beats no satellites | Edge |
|---|--:|--:|--:|--:|
| No satellites (BTC/ETH/SOL only) | 14.4% | 23.6% | — | — |
| LINK/XRP/INJ, picked with hindsight | 16.2% | 27.5% | 100% | +2.4% |
| Top 3 by trading volume | 12.7% | 21.1% | 0% | -2.4% |
| Top 3 by the Oct 9 screen, run in real time | 12.4% | 18.0% | 21% | -3.7% |

Yearly picks: **Top 3 by trading volume**: 2021: DOGE, BNB, XRP; 2022: BNB, ADA, XRP; 2023: XRP, BNB, DOGE; 2024: XRP, BNB, DOGE; 2025: DOGE, XRP, PEPE; 2026: XRP, BNB, DOGE; **Top 3 by the Oct 9 screen, run in real time**: 2021: POL, LINK, ADA; 2022: DOGE, POL, ADA; 2023: POL, DOGE, BNB; 2024: INJ, FET, TRX; 2025: INJ, RENDER, FET; 2026: ZEC, TRX, XRP.

## Longer window, BTC/ETH core (January 2019 – October 2026, quarterly starts)

| Policy | Whole window | Median by start | Median drawdown | Beats BTC/ETH | Edge |
|---|--:|--:|--:|--:|--:|
| BTC/ETH only (trend rule) | 36.4% | 18.3% | -41.8% | — | — |
| BTC/ETH + top 3 by volume (trend rule) | 30.6% | 17.1% | -43.4% | 0% | -1.5% |
| BTC only (trend rule) | 37.9% | 26.5% | -39.1% | 93% | +4.0% |
| BTC/ETH only, no trend rule | 36.1% | 17.3% | -61.1% | 7% | -1.7% |

## Where the live policy's backtest profit came from

| Coin | Bought | Sold | Held now | Profit | On bought |
|---|--:|--:|--:|--:|--:|
| BTC | $25,849 | $18,817 | $13,515 | $6,483 | 25.1% |
| SOL | $11,234 | $11,038 | $3,719 | $3,523 | 31.4% |
| XRP | $4,900 | $5,852 | $1,466 | $2,417 | 49.3% |
| INJ | $3,003 | $2,815 | $2,463 | $2,276 | 75.8% |
| ETH | $19,681 | $13,900 | $6,487 | $707 | 3.6% |
| LINK | $5,075 | $3,583 | $1,554 | $61 | 1.2% |

## Each coin alone, main window (lump sum; trend = the live rule)

| Coin | Group | Since | Hold × | Trend × | Hold drawdown | Trend drawdown | Corr BTC |
|---|--:|--:|--:|--:|--:|--:|--:|
| SOL | In your basket | 2021-06-01 | 3.20 | 9.16 | -96% | -80% | 0.70 |
| RENDER | Candidate | 2022-06-15 | 5.08 | 4.96 | -91% | -84% | 0.64 |
| SUI | Candidate | 2023-11-19 | 1.88 | 2.72 | -88% | -74% | 0.62 |
| ZEC | Candidate | 2021-06-01 | 7.51 | 5.45 | -94% | -88% | 0.55 |
| PEPE | Candidate | 2023-11-21 | 3.60 | 2.36 | -91% | -86% | 0.68 |
| TRX | Candidate | 2021-06-01 | 4.30 | 2.92 | -60% | -59% | 0.48 |
| BTC | In your basket | 2021-06-01 | 2.19 | 2.65 | -77% | -56% | 1.00 |
| FET | Candidate | 2021-06-01 | 0.63 | 2.06 | -96% | -92% | 0.58 |
| INJ | In your basket | 2021-06-01 | 0.90 | 1.95 | -95% | -91% | 0.61 |
| NEAR | Candidate | 2021-06-01 | 1.56 | 1.28 | -95% | -95% | 0.61 |
| ETH | In your basket | 2021-06-01 | 0.90 | 1.20 | -79% | -57% | 0.84 |
| AVAX | Candidate | 2021-06-01 | 0.55 | 1.17 | -96% | -86% | 0.68 |
| BNB | Candidate | 2021-06-01 | 2.05 | 1.17 | -70% | -69% | 0.73 |
| XRP | In your basket | 2021-06-01 | 1.27 | 0.74 | -78% | -78% | 0.66 |
| HBAR | Candidate | 2021-06-01 | 0.36 | 0.73 | -93% | -77% | 0.60 |
| ENA | Candidate | 2024-10-19 | 0.49 | 0.86 | -94% | -83% | 0.58 |
| BCH | Candidate | 2021-06-01 | 0.39 | 0.64 | -89% | -70% | 0.69 |
| XLM | Candidate | 2021-06-01 | 0.45 | 0.57 | -83% | -74% | 0.62 |
| ADA | Candidate | 2021-06-01 | 0.13 | 0.48 | -95% | -76% | 0.72 |
| LTC | Candidate | 2021-06-01 | 0.32 | 0.42 | -85% | -71% | 0.71 |
| AAVE | Candidate | 2021-06-01 | 0.41 | 0.39 | -88% | -83% | 0.69 |
| LINK | In your basket | 2021-06-01 | 0.39 | 0.37 | -85% | -83% | 0.72 |
| OP | Candidate | 2022-12-18 | 0.14 | 0.48 | -98% | -92% | 0.58 |
| ALGO | Candidate | 2021-06-01 | 0.13 | 0.32 | -97% | -87% | 0.65 |
| ICP | Candidate | 2021-11-27 | 0.08 | 0.32 | -95% | -86% | 0.59 |
| ARB | Candidate | 2023-10-09 | 0.23 | 0.46 | -97% | -89% | 0.61 |
| UNI | Candidate | 2021-06-01 | 0.26 | 0.25 | -92% | -89% | 0.66 |
| ONDO | Candidate | 2025-10-28 | 0.62 | 0.77 | -69% | -48% | 0.61 |
| SHIB | Candidate | 2021-11-26 | 0.12 | 0.21 | -91% | -81% | 0.67 |
| DOGE | Candidate | 2021-06-01 | 0.25 | 0.18 | -88% | -90% | 0.72 |
| APT | Candidate | 2023-05-07 | 0.09 | 0.32 | -97% | -87% | 0.63 |
| FIL | Candidate | 2021-06-01 | 0.02 | 0.16 | -99% | -92% | 0.63 |
| POL | Candidate | 2021-06-01 | 0.05 | 0.14 | -98% | -92% | 0.69 |
| ETC | Candidate | 2021-06-01 | 0.11 | 0.14 | -92% | -87% | 0.72 |
| WLD | Candidate | 2024-02-09 | 0.21 | 0.36 | -98% | -95% | 0.54 |
| DOT | Candidate | 2021-06-01 | 0.05 | 0.13 | -99% | -93% | 0.70 |
| ATOM | Candidate | 2021-06-01 | 0.12 | 0.12 | -97% | -96% | 0.62 |
| TAO | Candidate | 2024-10-28 | 0.54 | 0.24 | -80% | -85% | 0.66 |

## Method

- `basket.py` replays the bot day by day: regime from closes to d-1 (read 00:05 UTC), fills at day d's open,
  tier-1 maker 0.40% + 0.10% slippage, 0.2% USDC→USD per deposit, $5 minimum order, buys capped by cash in the
  bot's pair order, flips retried when under-funded, deposits buy-only into applied coins under target.
  Cross-check: single-coin hold/trend multiples reproduce `regime.py`/`swing.py` (BTC 7.46/9.81 vs 7.51/9.88).
- `basket_study.py` runs every experiment; `basket_data.py` fetches Binance daily candles (public mirror) for the
  6 live coins, ~30 candidates and three that collapsed or left (LUNA cut at 2022-05-13 and held at its last
  print; XMR/EOS/TON held at their last Binance price after their data ends).
- Limits: candidates were chosen in 2026 (survivorship makes alts look better, so the case against adding them
  is if anything understated); about 1.5 cycles of BTC-dominance history; Binance USDT prices for Kraken USD.

## Forward test

`forward.py` freezes these portfolios on 2026-10-16 and scores them on Kraken daily candles from that day only:
`python -m crypto.opt.forward [--actual /home/actions/oryx/data/crypto-bot/paper/state.json]`. `--actual`
replays the live policy with the real account's starting value and deposits; on 2026-10-09..15 the real
account ended $1.16 below the model on $333.82 paid in. Don't edit its rules: a changed rule is a new
forward test with a new start date.

Run everything: `python -m crypto.opt.basket_data && python -m crypto.opt.basket_study`.
