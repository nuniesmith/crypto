# Live runbook — oryx

What is running: **crypto-bot LIVE** on **oryx**, in Docker, Kraken spot, the
six-coin trend policy (see the [README](../README.md)).

## Process

| | |
|---|---|
| Container | `crypto-bot-live`, compose service `crypto-bot` in `nuniesmith/oryx` (`/home/actions/oryx/docker-compose.yml`) |
| Image | `ghcr.io/nuniesmith/crypto:latest` (the deploy pulls, never builds) |
| Command | `crypto-bot webui --port 8090 --autostart`: the web UI starts the loop (`live --confirm I_UNDERSTAND_REAL_MONEY --loop`) as its child and restarts it if it dies |
| Env | `/home/actions/oryx/.env`, written by oryx's CI/CD (`KRAKEN_API_KEY`, `KRAKEN_API_SECRET`, the Discord webhook) |
| State | `/home/actions/oryx/data/crypto-bot/paper/state.json` and `journal.jsonl` (`CRYPTO_BOT_ROOT=/app`, `./data/crypto-bot` mounted at `/app/data`) |
| Health | `curl -fsS http://127.0.0.1:8090/api/status`: 503 while a wanted loop is down or quiet |

The systemd units in `scripts/` are retired (the container replaced them on
2026-10-14). Never run one beside the container: two loops on one account
trade against each other and clash on the API nonce.

## Deploy

Merge to `main`. [`docker-publish.yml`](../.github/workflows/docker-publish.yml)
builds and pushes the image, then starts `ci-cd.yml` in `nuniesmith/oryx`, which
pulls it and recreates the container. That needs the `ORYX_DEPLOY_TOKEN`
secret; without it the job fails rather than reporting a deploy that never
happened. A restart sends a Discord startup snapshot and flattens nothing.

## Looking at it

```bash
curl -s http://127.0.0.1:8090/api/status | jq .          # or the web UI: crypto.7gram.xyz / oryx:8090
docker logs --tail 50 crypto-bot-live
docker inspect -f '{{.State.Health.Status}}' crypto-bot-live
docker exec crypto-bot-live crypto-bot forward --actual   # the forward test: public candles only
```

**Not** `crypto-bot status`, `report` or `live --dry-run` while the loop
runs: each opens its own private-API session on the live key, and Kraken
rejects one of the two callers (`EAPI:Invalid nonce`). The web UI exists so
nothing else has to ask Kraken: it reads the balance snapshot the loop writes.

For a dry run, stop the loop first (Stop in the web UI), and point
`CRYPTO_BOT_ROOT` at a copy of the data unless you mean to write the live
state: a binary built anywhere else reads and writes the checkout it was
compiled in.

## Safety

- Live and rebalance will not start without the exact confirm string.
- Orders are post-only limits with a client order id and a 10-minute
  Kraken-side expiry, so nothing rests on the book holding USD the next tick
  thinks is spendable. Only orders this bot recorded are ever cancelled; a
  hand-placed order is left alone, and the hourly foreign-trade check reports
  its fills on Discord.
- **Spendable USD is not the USD balance.** Kraken's `Balance` still counts
  money an open buy order has claimed. Every open buy, whoever placed it,
  counts against spendable USD before anything is sized (measured 2026-09-21:
  `ZUSD balance=120.41 hold_trade=37.38`).
- A rebalance needs a complete set of marks. A ticker outage leaves prices at
  0, which would understate the account and every target, so the policy
  refuses rather than sizing a real order from that.
- Order minimums are read live from `/0/public/AssetPairs`, not constants.
- `Policy::FROZEN` (`bot/src/alloc.rs`) is the tested kill switch:
  monitoring and reports continue, nothing is placed.
- zsh treats a pasted `#` comment line as a command. Use bash, or drop the
  comment lines.

## Not live

Anything outside the six coins. Adding coins lost in the basket study
(point-in-time picks cost 2.4–3.7% a year), and monthly trending-coin picks
lost too: [basket-study-2026-10-16.md](basket-study-2026-10-16.md),
[janus-and-crates-2026-10-17.md](janus-and-crates-2026-10-17.md).
