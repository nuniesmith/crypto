# Edge research notes — 1 m Kraken spot

## Fee reality check

At tier 1 (retail) a round-trip taker costs ~1.6 %. Even a clean 0.3 % move is
net negative. Practical requirements for a viable scalp:

1. **Maker entries** (limit orders resting on the book) whenever possible.
2. Volume or assets-on-platform high enough to reach at least tier 5–6.
3. Average winner ≥ 2–3× average loser after fees, or very high win-rate with
   tight risk.
4. High trade count only if expectancy stays positive; otherwise fewer, cleaner
   trades win.

## Strategy families worth testing

| Family              | Idea                                      | Trade count | Fee sensitivity |
|---------------------|-------------------------------------------|-------------|-----------------|
| VWAP mean-reversion | Fade ±1–1.5 ATR from session VWAP + EMA9 slope | High        | Very high       |
| EMA9/21 cross       | Momentum, filtered by VWAP side           | Medium      | High            |
| Micro-breakout      | 5–15 bar range break + volume spike       | Medium      | High            |
| Order-flow proxy    | Large candle + rejection at VWAP          | Low–medium  | Medium          |

## What we stole from the futures repo

- Static vs dynamic feature split.
- Risk policy outside the search space.
- Net-of-cost PnL as the only objective that matters.
- Study schema / resume discipline (to be tightened later).
- Pine parity goal: whatever the Python simulator does, a TradingView alert
  version should be able to emit the same signals.

## Next concrete experiments

1. Fetch 30–60 days of 1 m BTC, ETH, SOL.
2. Run `research` on both strategies at fee tiers 1, 3, 6.
3. Optuna on VWAP-MR with maker_entry as a categorical.
4. Add a simple volume-confirmation gate and re-test.
5. Only after positive hold-out expectancy → paper live via REST/ccxt.
