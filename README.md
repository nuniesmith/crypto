# crypto — Kraken bot (BTC hold + daily ETH/SOL regime) + fee-aware research

What is **running now** (oryx, `systemd --user`): **LIVE Kraken**, an hourly loop: a BTC+USD hold sleeve, and ETH/SOL on a daily regime rule (since 2026-10-04; the 1h SOL/ETH books are now simulation-only).

This repo is not a 1-minute scalp. Walk-forwards after Kraken fees killed 1m/15m. The live sleeves came from a 1h/4h structure-break study versus buy-and-hold.

| Layer | Role |
|---|---|
| `bot/` | Rust paper/live stepper (`exchange-apiws` + `indicators-ta` + `rustrade-framework`) |
| `src/crypto/` | Python fetch, simulator, Optuna, walk-forward / direction studies |
| `scripts/` | `run-bot.sh` + systemd user units |
| `data/` | Parquet cache + study reports (gitignored) |

## What is live

Host: **oryx**. Unit: `crypto-bot-live.service` (user systemd, survives logout). Paper unit stays **off** — both processes share `data/paper/state.json`.

Command actually executed:

```text
./bot/target/release/crypto-bot live --confirm I_UNDERSTAND_REAL_MONEY --loop
```

via `./scripts/run-bot.sh live`.

**Live wallet (real Kraken balances, not $1k books).** Two sleeves over one
account, both sized as a share of the account TOTAL:

**Since 2026-10-04 the ETH/SOL sleeve is a daily regime rule, not the 1h books.** Each coin gets half the sleeve (10% of the account). It holds all of it in a bull and keeps a never-sold 50% core in a bear. A bear starts when the daily close falls more than 5% below its 200-day average, a bull when it rises more than 5% above; inside that buffer the state holds. A coin trades only when its regime changes (about three times a year), never on the drift in between. Chosen from [`src/crypto/opt/regime.py`](src/crypto/opt/regime.py)'s study of standard rules at tier-1 fees: since 2018 (ETH) and 2021 (SOL) it returned 6.2× and 24.7× against buy-and-hold's 3.1× and 8.6×, mostly by stepping aside in the 2018 and 2022 crashes, and it lagged holding in a crash-free stretch (ETH 2022–26: 2.1× vs 2.7×). The 1h books still step as simulations so their record stays comparable, but they place no orders. Rule: `bot/src/regime.rs`; sizing: `alloc::regime_rebalance`.

(Before that, the sleeve was cut from 50% to 20% on 2026-09-21: the 1h books lost to buy-and-hold on the holdout by $325 (SOL) and $280 (ETH), capturing roughly 22% of the bull move — see [docs/research.md](docs/research.md). The sleeve stays at 20%, the operator's call.)

| Sleeve | Share | Policy |
|---|---|---|
| **Hold** — BTC + USD | **80%** | BTC **70% of the sleeve** (56% of the account), cash the rest. Rebalanced **both ways** outside ±10 points, at most once per UTC day. No 1h signals on BTC. |
| **Trade** — ETH + SOL | **20%** | 10% per coin. **Daily regime rule** (`regime.rs`): 100% of the coin's share in a bull, the 50% core in a bear (200-day average ±5%). Sized once per UTC day, only when a coin's regime changes. The 1h books (`eth_1h_sf`, `sol_1h_tl`) and `sol_bh` are simulation-only. |

Raising the sleeve should follow a run where it actually clears the gate — beats buy-and-hold **and** survives walk-forward — not a good week. `Policy::HOLD_ONLY` freezes ETH/SOL entirely and stays tested, so switching to it is one constant, not a rewrite.

Every target is derived from the account total, so **a deposit needs no
bookkeeping**: new USD raises the total, both sleeves' targets rise with it,
and the next rebalance buys BTC back to target while the rest becomes trading
capital. Kraken mins (verified against `/0/public/AssetPairs`): XBT 0.00005,
ETH 0.001, SOL 0.06, cost min $0.50.

Why the hold is a share of the *account* and not just "BTC vs USD": funding
ETH/SOL from `USD − 30% of (BTC+USD)` is exactly **zero** once BTC+USD sits at
70/30. The old one-directional rebalancer only ever worked because BTC was
underweight; making it two-sided without this change would have driven BTC to
target and then never bought another coin.

## Pending deploy: one account, BTC/ETH/SOL/cash targets

**Not yet live.** This is branch `feat/trend-all-coins` — reviewed, tested,
pushed, but not merged or deployed as of 2026-10-05. Until it deploys,
everything above ("What is live") is what oryx actually runs. This section
describes what replaces it.

The operator retired the two-sleeve split (a frozen BTC hold plus a small
ETH/SOL trading sleeve) for **one account with target weights of its TOTAL
value: BTC 50%, ETH 25%, SOL 15%, cash 10%** at the bull floor. The same
daily trend rule (`bot/src/regime.rs`: 200-day average ±5%) now sizes **all
three coins**, not just ETH and SOL against a BTC pile that never traded —
`regime::PAIRS` grew from `[ETHUSD, SOLUSD]` to `[XBTUSD, ETHUSD, SOLUSD]`.
A coin's effective target is its base weight × `regime::exposure` (all of it
in a bull, the 50% core in a bear), and cash is simply whatever that leaves —
10% when every coin is bull, up to 55% when all three are bear.

**No drift rebalancing.** [`src/crypto/opt/portfolio.py`](src/crypto/opt/portfolio.py)
simulated this account against a 5/25-rebalanced version of the same targets
and a buy-and-hold of the same split: the trend rule trading only on a flip
beat both, and 5/25 rebalancing added roughly 30 trades a year trimming
ordinary drift for no extra return.
So a coin's holding is touched only by: its own regime flipping, the
one-time move onto this policy, investing a deposit, or the operator's
`crypto-bot rebalance` command — never by price drift between those events.

Every order is a **post-only limit at the touch** (buy at the bid, sell at
the ask), with a client order id and a Kraken-side expiry, and there is at
most **one open order per pair** at a time — a rejected or unfilled order is
simply re-priced and re-placed the next hour.

Deposits are detected from Kraken's Ledgers (paged past its 50-per-call
limit, deduplicated by ledger id): a USD/USDC/USDT deposit adds to a
persisted `deposit_backlog_usd`, invested across the three coins by their
current effective weights as cash allows; a BTC/ETH/SOL deposit counts for
performance accounting only (it is already invested). Any USDC/USDT balance
at or above Kraken's $5 ordermin is swept to USD the same way. Withdrawals
are recorded, never traded on.

The **work tick** (once per UTC hour, plus once on startup) replaces the
1h-bar trigger entirely: `state.books` (the three $1,000 simulation books)
is left on disk as history and is never stepped or read from again — see
`bot/src/paper.rs`'s module docs. Order settlement still runs every 60s wake.

`Policy::FROZEN` (`bot/src/alloc.rs`) is the tested kill switch: monitoring
and reporting continue, nothing is ever placed. One constant to flip, not a
rewrite — same idea as the old `Policy::HOLD_ONLY` it replaces.

Discord reports (same daily/weekly/monthly schedule) show each asset's
weight against its effective target, each coin's regime with its distance
from the 200-day average and the price that would flip it, pending work
(flips not yet applied, the deposit backlog, open orders), and deposits for
the period. The old $1,000-book simulation section is gone from the report.

`state.json` gains `policy_version`, `regime_bull`, `regime_reading`,
`deposit_backlog_usd`, `flows`, `last_ledger_time`, `net_deposits_usd`,
`history`, `last_history_day` and `last_work_hour` — all `#[serde(default)]`,
so the file already on oryx keeps loading. On first load, `policy_version <
2` triggers a one-time move: every coin is marked pending its effective
target (today's account is roughly BTC 56% / ETH 10% / SOL 10% / USD 24%,
so this sells some BTC and buys ETH and SOL over the following ticks), and
the (now unified) ledger adopts whatever is already held so a later
regime-flip sell has a real basis to compute P&L against.

## Two sets of books, deliberately

The bot keeps **two** accounts that must never be read as one number.

| | `state.json` → `books` | `state.json` → `live` |
|---|---|---|
| What | $1,000-per-book **simulation** | **real Kraken fills** (`vol_exec`/`cost`/`fee`) |
| Answers | does this signal have an edge at a size worth trading? | what did the 20% sleeve actually make or lose, net of real fees? |
| Fees | modelled tier-3 maker 0.22% + 1bp slip | whatever Kraken charged |
| Written by | `paper.rs` only | `ledger.rs`, from settled orders only |

These used to be one set of numbers, and the result described neither. On
2026-09-22 `eth_1h_sf` went long ETH at 2696.09 and out at 2748.29 — ETH up
1.9%, signal correct — and the book reported **−$0.11**, because
`LiveAction::Buy` had overwritten the simulated `qty` with the 0.001 ETH the
wallet could afford while the entry fee stayed charged on $1,000 of notional.
The simulation's answer was **+$14.71**; Kraken's was about **+4 cents** on a
$2.70 position after ~1.4 cents of real fee.

The live ledger starts clean from a stated date (`live.since`) and book
trades before `paper_clean_since` are left exactly as recorded — they mixed
the two and cannot honestly be restated as either.

Loop wakes every 60s and **only acts on a new closed 1h bar**. Live path places Kraken **limit** orders, wallet-capped. Spot: no short opens. Fees in the paper-scale books: tier-3 maker 0.22% + 1 bp slip.

Discord (`CRYPTO_DISCORD_WEBHOOK_URL`): startup, daily ~15:00 UTC, weekly Monday, monthly 1st. Live reports fetch `POST /0/private/Balance` first, then the policy line, then the **live sleeve's real P&L**, and only then the simulation books.

Not in the live bot: XRP, FET, TRUMP, memes, forex. See [docs/research.md](docs/research.md).

## Operate (oryx)

User units — no `sudo systemctl`. Binary is not on `PATH`.

```bash
cd ~/github/crypto
git pull
cargo build --release --manifest-path bot/Cargo.toml
systemctl --user daemon-reload
systemctl --user disable --now crypto-bot-paper   # never run next to live
systemctl --user enable --now crypto-bot-live
systemctl --user status crypto-bot-live
journalctl --user -u crypto-bot-live -f
./scripts/run-bot.sh status
./scripts/run-bot.sh report
```

`.env` (not committed):

```
KRAKEN_API_KEY=...
KRAKEN_API_SECRET=...
CRYPTO_DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/...
```

Live refuses to start without `--confirm I_UNDERSTAND_REAL_MONEY` (the launcher embeds that). Full runbook: [docs/live.md](docs/live.md).

## Research CLI

```bash
python3 -m venv .venv
.venv/bin/pip install -e ".[dev]"

# ≥1y history (Binance Vision). Kraken 1m public OHLC is ~12h.
.venv/bin/crypto fetch-history --pair XBTUSD ETHUSD SOLUSD --days 400
.venv/bin/crypto fetch-history --pair XRPUSD AVAXUSD --days 400 --interval 15

.venv/bin/crypto direction --pair XBTUSD ETHUSD SOLUSD
.venv/bin/crypto study --pair SOLUSD --interval 60 --days 365 --holdout-days 60
.venv/bin/crypto status
```

Studies write `data/studies/` (gitignored). Conclusions that decided the live books: [docs/research.md](docs/research.md).

## Layout

```
bot/src/          Rust bot (paper stepper is the running loop; live places limits)
src/crypto/       Python research
  data/           Kraken OHLC + Binance Vision history
  features/       VWAP, EMA, ATR
  strategies/     trendline, structure, range, VWAP-MR, EMA
  sim/            fee-aware replay
  opt/            study / direction / Optuna
scripts/          run-bot.sh, systemd user units
docs/             live runbook + research conclusions
pine/             VWAP+EMA9 as a *filter*, not a 1m entry
```

Crates (crates.io / nuniesmith): `exchange-apiws` 0.11 (Kraken), `indicators-ta` 0.3, `rustrade-framework` 0.5.2.

## Disclaimer

Research plus a small live Kraken bot. Markets are 24/7; fees kill most short-horizon retail strategies. Never risk money you cannot afford to lose. Past net-of-fee results are not a forecast.
