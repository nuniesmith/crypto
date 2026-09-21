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

Three **internal $1k books** (strategy trackers, not Kraken cash):

| Book | Pair | Rule | Hold |
|---|---|---|---|
| `sol_1h_tl` | SOLUSD | `trendline_break` | 24 × 1h |
| `eth_1h_sf` | ETHUSD | `structure_filtered` (VWAP+EMA gate) | 24 × 1h |
| `sol_bh` | SOLUSD | buy-and-hold benchmark | until flattened |

Loop wakes every 60s and **only acts on a new closed 1h bar**. Live path places Kraken **limit** orders. Fees in the books: Kraken Pro tier-3 maker 0.22% + 1 bp slip (taker 0.38% if used).

Discord (`DISCORD_WEBHOOK_URL`): startup, daily ~15:00 UTC, weekly Monday, monthly 1st. Live reports fetch `POST /0/private/Balance` and print the **real Kraken account first**. The $1k books are labeled as internal trackers.

Not in the live bot: BTC, XRP, FET, TRUMP, memes, forex. See [docs/research.md](docs/research.md).

Known gap: the books are the source of truth for position. `sol_bh` long size is the internal $1k mark, not necessarily the SOL sitting on Kraken.

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
