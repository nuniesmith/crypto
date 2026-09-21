# crypto — Kraken 1h SOL/ETH bot + fee-aware research

What is **running now** (oryx, `systemd --user`): **LIVE Kraken**, 1-hour bars, SOL and ETH only.

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

**The ETH/SOL sleeve was cut from 50% to 20% on 2026-09-21.** It lost to buy-and-hold on the holdout by $325 (SOL) and $280 (ETH), capturing roughly 22% of the bull move, and zero of the 18 cells in that run beat BH — see [docs/research.md](docs/research.md). It is a **bet size on a signal that failed this repo's own gate**, not an allocation to a proven edge. 20% is tuition: enough that live results mean something, little enough that another 22%-capture rally costs a few percent of the account.

| Sleeve | Share | Policy |
|---|---|---|
| **Hold** — BTC + USD | **80%** | BTC **70% of the sleeve** (56% of the account), cash the rest. Rebalanced **both ways** outside ±10 points, at most once per UTC day. No 1h signals on BTC. |
| **Trade** — ETH + SOL | **20%** | `eth_1h_sf` and `sol_1h_tl` spend from this sleeve only. A new long **adopts** existing inventory rather than buying. A flat book does not dump the pile. `sol_bh` is **mark-only**. |

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

Loop wakes every 60s and **only acts on a new closed 1h bar**. Live path places Kraken **limit** orders, wallet-capped. Spot: no short opens. Fees in the paper-scale books: tier-3 maker 0.22% + 1 bp slip.

Discord (`DISCORD_WEBHOOK_URL`): startup, daily ~15:00 UTC, weekly Monday, monthly 1st. Live reports fetch `POST /0/private/Balance` first, then the policy line, then paper-scale books.

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
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/...
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
