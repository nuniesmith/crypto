#!/usr/bin/env bash
# Start the Kraken bot from this repo (no PATH setup needed).
#
#   ./scripts/run-bot.sh              # paper loop (default)
#   ./scripts/run-bot.sh paper
#   ./scripts/run-bot.sh live         # real Kraken orders (needs API keys)
#   ./scripts/run-bot.sh status
#   ./scripts/run-bot.sh report       # Discord snapshot now
#   ./scripts/run-bot.sh build
#
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/bot/target/release/crypto-bot"
MODE="${1:-paper}"

load_env() {
  if [ -f "$ROOT/.env" ]; then
    set -a
    # shellcheck disable=SC1091
    . "$ROOT/.env"
    set +a
  fi
}

need_bin() {
  if [ ! -x "$BIN" ]; then
    echo "building crypto-bot (release)…"
    cargo build --release --manifest-path "$ROOT/bot/Cargo.toml"
  fi
}

case "$MODE" in
  -h|--help|help)
    sed -n '2,12p' "$0"
    exit 0
    ;;
  build)
    cargo build --release --manifest-path "$ROOT/bot/Cargo.toml"
    echo "ok: $BIN"
    exit 0
    ;;
  paper|"")
    load_env
    need_bin
    cd "$ROOT"
    echo "starting PAPER loop  ($BIN paper --loop)"
    exec "$BIN" paper --loop
    ;;
  live)
    load_env
    need_bin
    if [ -z "${KRAKEN_API_KEY:-}" ] || [ -z "${KRAKEN_API_SECRET:-}" ]; then
      echo "live needs KRAKEN_API_KEY and KRAKEN_API_SECRET in $ROOT/.env" >&2
      exit 1
    fi
    cd "$ROOT"
    echo "starting LIVE loop  ($BIN live --confirm I_UNDERSTAND_REAL_MONEY --loop)"
    echo "this places real Kraken orders. Ctrl-C to stop."
    exec "$BIN" live --confirm I_UNDERSTAND_REAL_MONEY --loop
    ;;
  status)
    load_env
    need_bin
    cd "$ROOT"
    exec "$BIN" status
    ;;
  report)
    load_env
    need_bin
    cd "$ROOT"
    exec "$BIN" report
    ;;
  *)
    echo "unknown mode: $MODE  (paper|live|status|report|build)" >&2
    exit 2
    ;;
esac
