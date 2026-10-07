//! rustrade Brains: 1h structure-break (SOL) and VWAP+EMA-gated break (ETH).
#![allow(dead_code)]

use std::sync::Mutex;

use async_trait::async_trait;
use chrono::Utc;
use rustrade::{
    Brain, Candle, Decision, MarketDataEvent, Position, Price, Result, SizeHint, Symbol,
};

use crate::features::{compute, Bar};
use crate::signal::{structure_filtered, trendline_break};

const HOLD_SECS: i64 = 24 * 3600;
const PIVOT_LB: usize = 8;
const VOL_MULT: f64 = 1.3;
const ATR_STOP: f64 = 1.5;
const NOTIONAL: f64 = 1_000.0;

struct Buf {
    candles: Vec<Candle>,
}

pub struct StructureBrain {
    name: &'static str,
    symbol: Symbol,
    filtered: bool,
    state: Mutex<Buf>,
}

impl StructureBrain {
    pub fn sol_trendline() -> Self {
        Self {
            name: "sol_1h_tl",
            symbol: Symbol::from("SOLUSD"),
            filtered: false,
            state: Mutex::new(Buf {
                candles: Vec::new(),
            }),
        }
    }

    pub fn eth_filtered() -> Self {
        Self {
            name: "eth_1h_sf",
            symbol: Symbol::from("ETHUSD"),
            filtered: true,
            state: Mutex::new(Buf {
                candles: Vec::new(),
            }),
        }
    }
}

fn to_bars(candles: &[Candle]) -> Vec<Bar> {
    candles
        .iter()
        .map(|c| Bar {
            time: c.time / 1000,
            open: c.open,
            high: c.high,
            low: c.low,
            close: c.close,
            volume: c.volume,
        })
        .collect()
}

#[async_trait]
impl Brain for StructureBrain {
    fn name(&self) -> &str {
        self.name
    }

    fn owned_symbols(&self) -> Option<Vec<Symbol>> {
        Some(vec![self.symbol.clone()])
    }

    async fn on_event(&self, event: &MarketDataEvent, position: &Position) -> Result<Decision> {
        let MarketDataEvent::Candle { symbol, candle, .. } = event else {
            return Ok(Decision::hold());
        };
        if symbol != &self.symbol {
            return Ok(Decision::hold());
        }

        let mut st = self.state.lock().expect("brain mutex");
        if st.candles.last().map(|c| c.time) == Some(candle.time) {
            return Ok(Decision::hold());
        }
        st.candles.push(*candle);
        if st.candles.len() > 800 {
            let drop = st.candles.len() - 720;
            st.candles.drain(0..drop);
        }
        if st.candles.len() < 50 {
            return Ok(Decision::hold());
        }

        // Ignore historical catch-up from the candle poller (only trade the live bar).
        let now_ms = Utc::now().timestamp_millis();
        if candle.time < now_ms - 2 * 3600 * 1000 {
            return Ok(Decision::hold());
        }

        let bars = to_bars(&st.candles);
        let feat = compute(&bars);
        let i = bars.len() - 1;
        let sig = if self.filtered {
            structure_filtered(&bars, &feat, PIVOT_LB, VOL_MULT)
        } else {
            trendline_break(&bars, &feat, PIVOT_LB, VOL_MULT)
        };
        let s = sig[i];
        let px = bars[i].close;
        let atr = feat.atr14[i];

        if !position.is_flat() {
            let entry = position.entry_price.unwrap_or(px);
            let entry_ts = st
                .candles
                .iter()
                .rev()
                .find(|c| (c.close - entry).abs() < 1e-6)
                .map(|c| c.time / 1000)
                .unwrap_or(bars[i].time);
            let held = bars[i].time - entry_ts;
            let long = position.is_long();
            let stop_hit = atr.is_finite()
                && atr > 0.0
                && ((long && bars[i].low < entry - ATR_STOP * atr)
                    || (!long && bars[i].high > entry + ATR_STOP * atr));
            let flip = (long && s < 0) || (!long && s > 0);
            if held >= HOLD_SECS || stop_hit || flip {
                return Ok(Decision::close().with_metadata(serde_json::json!({
                    "reason": if stop_hit { "STOP" } else if flip { "FLIP" } else { "TIME" },
                })));
            }
            return Ok(Decision::hold());
        }

        if s == 0 || !atr.is_finite() {
            return Ok(Decision::hold());
        }
        let stop = if s > 0 {
            px - ATR_STOP * atr
        } else {
            px + ATR_STOP * atr
        };
        let d = if s > 0 {
            Decision::buy(0.7)
        } else {
            Decision::sell(0.7)
        };
        Ok(d.with_size_hint(SizeHint::NotionalUsd(NOTIONAL))
            .with_limit_price(Price(px))
            .with_stop(Price(stop))
            .with_metadata(serde_json::json!({"book": self.name, "bar": bars[i].time})))
    }
}
