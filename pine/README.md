# TradingView

`vwap_ema9.pine` — session VWAP + EMA9 (optional smoothing / Bollinger).

This is a **visual / filter**, not the live entry. Live entries are 1h `trendline_break` (SOL) and `structure_filtered` (ETH, VWAP+EMA gate). See [docs/research.md](../docs/research.md).

1m VWAP mean-reversion was tested and is net-negative after Kraken fees. Do not alert-trade it.
