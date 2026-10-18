# crypto — a long-term Kraken bot in Rust

One Kraken spot account, held for the long term: six coins and cash at fixed
target weights, each coin halved while it is in a bear market and restored when
the bull returns. It trades a handful of times a year: on a regime flip, to
invest a deposit, or when told to. Everything here is Rust; the Python research
that chose the policy is archived ([docs/research-code.md](docs/research-code.md)).

## What runs

| | |
|---|---|
| Where | Docker container `crypto-bot-live` on **oryx**, from the compose file in `nuniesmith/oryx` (`/home/actions/oryx`) |
| Image | `ghcr.io/nuniesmith/crypto:latest`, built from `bot/` by [`docker-publish.yml`](.github/workflows/docker-publish.yml) |
| Command | `crypto-bot webui --port 8090 --autostart`: the web UI, which starts and supervises the trading loop (`crypto-bot live --loop`) |
| Web UI | port 8090 (proxied as crypto.7gram.xyz): dashboard, start/stop, forecast calculator; `/api/status` is the container healthcheck |
| State | `/home/actions/oryx/data/crypto-bot/paper/state.json` (`CRYPTO_BOT_ROOT=/app` in the container) |
| Deploy | merge to `main` → the workflow builds and pushes the image → it starts oryx's `ci-cd.yml` (`ORYX_DEPLOY_TOKEN`), which pulls and restarts the container |

## The policy (`bot/src/alloc.rs`, `bot/src/regime.rs`)

| BTC | ETH | SOL | LINK | XRP | INJ | Cash |
|---|---|---|---|---|---|---|
| 44% | 22% | 12% | 5% | 5% | 5% | 7% |

Those are the bull-market targets, as shares of the account's **total** value.

- **Trend rule.** A coin turns bear when its daily close falls more than 5%
  below its 200-day average, and bull when it rises more than 5% above. Inside
  that band the regime holds. In a bear a coin targets half its weight; cash
  takes the rest (7% with every coin bull, up to 53.5% with all six bear).
- **No drift rebalancing.** A coin's holding changes only when its own regime
  flips, when a deposit is invested, on a move onto a new policy, or on the
  `rebalance` command. The research found that rebalancing on drift added about
  30 trades a year for no extra return.
- **Deposits** are read from Kraken's Ledgers and invested across the coins by
  their current targets. A USDC/USDT balance at or above Kraken's $5 minimum is
  swept to USD. Withdrawals are recorded, never traded on. Because every target
  is a share of the total, a deposit needs no bookkeeping.
- **Orders** are post-only limits at the touch, each with a client order id and
  a 10-minute Kraken-side expiry, at most one open per pair. An unfilled order
  is re-priced on the next hourly work tick.
- **Kill switch.** `Policy::FROZEN` keeps monitoring and reporting and places
  nothing.

Why these coins and this rule: [docs/basket-study-2026-10-16.md](docs/basket-study-2026-10-16.md)
(keep the trend rule, add no coins) and
[docs/janus-and-crates-2026-10-17.md](docs/janus-and-crates-2026-10-17.md)
(Janus's detectors lose to the 200-day rule, so the long-term rule stays static).

## Supervision

- The web UI owns the loop. A loop that exits is reported to Discord and
  restarted with backoff (30 s doubling to 30 min). `/api/status` answers 503,
  and the container turns unhealthy, while a wanted loop is down or has
  stopped writing its balance snapshot.
- Every hour the loop checks Kraken's closed orders for trades it did not
  place, and alerts on Discord.
- **One process calls Kraken's private API: the trading loop.** The web UI
  reads the loop's balance snapshot instead, because a second caller on the
  same key causes `EAPI:Invalid nonce`. For the same reason, do not run
  `status`, `report` or `live --dry-run` with the live key while the loop runs.
- Discord reports: on startup, daily at 15:00 UTC, weekly on Monday, monthly
  on the 1st.

Runbook: [docs/live.md](docs/live.md).

## Commands

```text
crypto-bot webui [--port 8090] [--autostart]     dashboard + start/stop + forecast (what the container runs)
crypto-bot live --confirm I_UNDERSTAND_REAL_MONEY --loop
crypto-bot live --dry-run                        real wallet read, no orders (private API: not beside a running loop)
crypto-bot rebalance --dry-run | --confirm I_UNDERSTAND_REAL_MONEY
crypto-bot status | report                       account summary / Discord snapshot now (private API too)
crypto-bot paper [--loop]                        paper bookkeeping only
```

Live and rebalance refuse to run without the exact confirm string.

## Layout

```
bot/                     the bot (Cargo crate `crypto-bot`)
  src/alloc.rs           the policy: base weights, effective targets, order sizing
  src/regime.rs          the 200-day ±5% trend rule
  src/main.rs            CLI, the hourly work tick, deposits, reports
  src/live.rs            Kraken gateway (orders, balances, ledgers, closed orders)
  src/ledger.rs          live P&L from settled fills
  src/paper.rs           state.json: schema, migrations, snapshots
  src/webui.rs, webui/   web UI, watchdog, /api/status
  src/foreign.rs         alert on trades the bot did not place
  src/discord.rs         reports and alerts
  src/minimums.rs        Kraken order minimums
tools/regime-replay/     replays indicators-ta's regime detectors over daily candles
docs/                    runbook, study write-ups, the research-code archive
scripts/                 run-bot.sh for a local build; the retired systemd units
```

Crates (nuniesmith): [`exchange-apiws`](https://github.com/nuniesmith/exchange-apiws)
(Kraken REST, pinned by git rev), `indicators-ta` 0.3, `rustrade-framework` 0.5.2.

## Research

| Doc | Question |
|---|---|
| [research.md](docs/research.md) | 1m–4h strategies after Kraken fees (2026-09-20): none beat holding |
| [coin-analysis-2026-09-23.md](docs/coin-analysis-2026-09-23.md) | which coins to consider |
| [basket-study-2026-10-16.md](docs/basket-study-2026-10-16.md) | the six-coin policy with weekly deposits; the frozen forward test |
| [janus-and-crates-2026-10-17.md](docs/janus-and-crates-2026-10-17.md) | Janus's regimes, trending coins, what the crates need |
| [research-code.md](docs/research-code.md) | where the Python went, and how to run the forward test |

## Disclaimer

A small live bot plus research notes. Markets run 24/7, and fees kill most
short-horizon retail strategies. Never risk money you cannot afford to lose.
Past net-of-fee results are not a forecast.
