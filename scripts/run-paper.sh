#!/usr/bin/env bash
# 24/7 paper loop. On the home server: copy the repo, set .env, then
#   systemd --user enable --now crypto-bot-paper
# or just:
#   DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/... ./scripts/run-paper.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/bot/target/release/crypto-bot"
if [ ! -x "$BIN" ]; then
  cargo build --release --manifest-path "$ROOT/bot/Cargo.toml"
fi
# load .env without printing it
if [ -f "$ROOT/.env" ]; then
  set -a
  # shellcheck disable=SC1091
  . "$ROOT/.env"
  set +a
fi
cd "$ROOT"
exec "$BIN" paper --loop
