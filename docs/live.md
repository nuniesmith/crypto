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

A restart reloads the binary and sends a Discord **startup** snapshot. It does **not** flatten Kraken. Existing books in `state.json` keep their positions; buy-hold will not re-enter if already long.

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

1. Load `state.json`.
2. Pull closed 1h OHLC for SOLUSD and ETHUSD (drop the still-forming bar).
3. For each book, `step_book` on a **new** closed bar only: time stop (24h), 1.5×ATR stop, flip, or enter.
4. If live and a book **opened** this bar, place a Kraken limit at the book entry (qty/price from the $1k notional).
5. Save state. Log only on new bar or trade.

`get_position` on the exchange adapter still returns flat. The **books** are the position source. Restarting does not query Kraken for fills.

## Safety

- Live will not start without the exact confirm string.
- Do not run paper systemd and live together.
- `sol_bh` size is `$1000 / entry`. That is not the SOL balance on the account unless a fill actually happened at that size.
- Signal books stay flat until a 1h close prints an entry. Quiet logs for hours are normal.
- zsh treats a leading `#` as a command if you paste comments. Use bash or drop the comment lines.

## Not live

BTC, XRP, FET, TRUMP, screenshot memes, forex. Research said do not add them: [research.md](research.md).
