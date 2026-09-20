#!/usr/bin/env bash
# Back-compat wrapper. Prefer: ./scripts/run-bot.sh paper
exec "$(cd "$(dirname "$0")" && pwd)/run-bot.sh" paper "$@"
