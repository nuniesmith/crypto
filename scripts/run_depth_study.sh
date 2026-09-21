#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
PY="$ROOT/.venv/bin/python"
export PYTHONUNBUFFERED=1
echo "[$(date -Is)] fetch-history 400d XBTUSD ETHUSD SOLUSD (1m Vision)"
"$PY" -m crypto fetch-history --pair XBTUSD ETHUSD SOLUSD --days 400
echo "[$(date -Is)] status"
"$PY" -m crypto status
echo "[$(date -Is)] 1h study 365d holdout 60 folds 6 trials 80 (1m scalps are dead; this is the live-sleeve check)"
"$PY" -m crypto study --pair XBTUSD ETHUSD SOLUSD --days 365 --holdout-days 60 --folds 6 --trials 80 --fee-tier 3 --interval 60
echo "[$(date -Is)] DONE"
ls -lt "$ROOT/data/studies" | head -5
