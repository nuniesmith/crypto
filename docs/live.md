# Live runbook — oryx

What is running: **crypto-bot LIVE** on **oryx**, user systemd, Kraken spot, 1h SOL/ETH.

## Process

| | |
|---|---|
| Unit | `~/.config/systemd/user/crypto-bot-live.service` (copy of `scripts/crypto-bot-live.service`) |
| Exec | `~/github/crypto/scripts/run-bot.sh live` |
| Binary | `~/github/crypto/bot/target/release/crypto-bot live --confirm I_UNDERSTAND_REAL_MONEY --loop` |
| Working dir | `~/github/crypto` |
| Env | `~/github/crypto/.env` (`KRAKEN_API_KEY`, `KRAKEN_API_SECRET`, `DISCORD_WEBHOOK_URL`) |
| State | `data/paper/state.json` (gitignored) |
| Journal | `data/paper/journal.jsonl` |

Paper unit `crypto-bot-paper.service` must stay **disabled**. Two loops on the same `state.json` double-enter.

Use `systemctl --user`, not `sudo systemctl`. The binary is not on `PATH`.

## Deploy after a git pull

```bash
cd ~/github/crypto
git pull
cargo build --release --manifest-path bot/Cargo.toml
systemctl --user daemon-reload
systemctl --user disable --now crypto-bot-paper
systemctl --user restart crypto-bot-live
journalctl --user -u crypto-bot-live -n 40 --no-pager
```

A restart reloads the binary and sends a Discord **startup** snapshot. It does **not** flatten Kraken. BTC is never sold to match a flat book. ETH/SOL are only sold on a strategy **exit** (or a short signal that maps to selling inventory).

## Commands

```bash
./scripts/run-bot.sh paper     # paper loop (do not run on oryx while live is up)
./scripts/run-bot.sh live      # real Kraken limits (needs keys + confirm)
./scripts/run-bot.sh status    # Kraken account (if mode=live) + $1k books
./scripts/run-bot.sh report    # Discord snapshot now
./scripts/run-bot.sh build
```

`crypto-bot: command not found` means you used PATH. Use the script or `./bot/target/release/crypto-bot`.

## Discord

Live body order:

1. **Kraken account** — `Balance` marked to USD (ZUSD, SOL, XETH, XXBT, …). Dust under $0.01 dropped.
2. **strategy books** — internal $1k each, labeled *not Kraken cash*.

Cadence: startup on process start; daily from 15:00 UTC; weekly Monday; monthly on the 1st. Balance is fetched only when a report is actually sent.

## How a cycle works

1. Load `state.json` and Kraken `Balance`.
2. Pull closed 1h OHLC for SOLUSD, ETHUSD, and XBTUSD mark.
3. For each signal book, `step_book` on a **new** closed bar only: time stop (24h), 1.5×ATR stop, flip, or enter.
4. Map that to a **wallet-capped** live action:
   - `buy_hold` (`sol_bh`): clamp qty to wallet SOL, **no order**.
   - long enter + inventory ≥ min: **adopt** (no buy).
   - long enter + no inventory: buy with the **trade sleeve's** cash, if it clears Kraken min (ETH 0.001 / SOL 0.06). That cash is capped both by the sleeve's own headroom and by what is left once the hold sleeve's dollars are reserved, so a signal can never eat the BTC reserve.
   - exit / short signal: **sell the ETH or SOL pile**, never more than the wallet.
   - never open a spot short, never trade BTC on a 1h signal.
5. On a new 1h bar, at most once per UTC day, rebalance BTC **in either
   direction** when it is more than ±10 points off 70% of the hold sleeve.
   Strategy orders are sized first, housekeeping second. The daily slot is
   only consumed when an order is actually accepted — a rejected one used to
   mark the day done and leave the account out of band for another 24h.
6. Save state. Log only on new bar or trade.

## Safety

- Live will not start without the exact confirm string.
- Do not run paper systemd and live together.
- Live orders are capped to the Kraken wallet. Paper-scale $1k books are trackers only.
- BTC HODL is not dumped on restart. ETH/SOL are not dumped just because a book is flat.
- **Unfilled limit orders are cancelled after 10 minutes.** Kraken limits never expire and `place_order` takes no `expiretm`, so an order priced at the last trade could sit forever holding USD that `Balance` still reports as spendable — the next cycle would then size against money already reserved. Only txids this bot recorded are cancelled; orders placed by hand in the Kraken UI are left alone.
- A rebalance needs a **complete** set of marks. `fetch_marks` returns an empty vec on error, so a ticker outage leaves prices at 0.0, which understates the account and every target — the policy refuses rather than sizing a real order off that.
- Signal books stay flat until a 1h close prints an entry. Quiet logs for hours are normal.
- zsh treats a leading `#` as a command if you paste comments. Use bash or drop the comment lines.

## Not live

BTC, XRP, FET, TRUMP, screenshot memes, forex. Research said do not add them: [research.md](research.md).
