//! EMA / ATR / session VWAP from `indicators-ta` (nuniesmith).

use indicators::atr;
use indicators::ema;
use indicators::indicator::Indicator;
use indicators::types::Candle as TaCandle;
use indicators::volume::Vwap;

#[derive(Clone, Debug)]
pub struct Bar {
    /// Unix seconds (Kraken OHLC).
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

#[derive(Clone, Debug)]
pub struct Features {
    pub vwap: Vec<f64>,
    pub ema9: Vec<f64>,
    pub ema21: Vec<f64>,
    pub atr14: Vec<f64>,
}

fn nan_vec(n: usize) -> Vec<f64> {
    vec![f64::NAN; n]
}

/// UTC-day session VWAP via `indicators_ta::volume::Vwap` on each day's slice.
fn session_vwap(candles: &[TaCandle]) -> Vec<f64> {
    let n = candles.len();
    let mut out = nan_vec(n);
    if n == 0 {
        return out;
    }
    let vwap = Vwap::cumulative();
    let mut i = 0;
    while i < n {
        let day = candles[i].time / 86_400_000;
        let mut j = i + 1;
        while j < n && candles[j].time / 86_400_000 == day {
            j += 1;
        }
        if let Ok(cols) = vwap.calculate(&candles[i..j]) {
            if let Some(vals) = cols.get("VWAP") {
                out[i..j].copy_from_slice(vals);
            }
        }
        i = j;
    }
    out
}

pub fn compute(bars: &[Bar]) -> Features {
    let n = bars.len();
    if n == 0 {
        return Features {
            vwap: vec![],
            ema9: vec![],
            ema21: vec![],
            atr14: vec![],
        };
    }
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let highs: Vec<f64> = bars.iter().map(|b| b.high).collect();
    let lows: Vec<f64> = bars.iter().map(|b| b.low).collect();
    let ema9 = ema(&closes, 9).unwrap_or_else(|_| nan_vec(n));
    let ema21 = ema(&closes, 21).unwrap_or_else(|_| nan_vec(n));
    let atr14 = atr(&highs, &lows, &closes, 14).unwrap_or_else(|_| nan_vec(n));
    let ta: Vec<TaCandle> = bars
        .iter()
        .map(|b| TaCandle {
            time: b.time * 1000,
            open: b.open,
            high: b.high,
            low: b.low,
            close: b.close,
            volume: b.volume,
        })
        .collect();
    Features {
        vwap: session_vwap(&ta),
        ema9,
        ema21,
        atr14,
    }
}
