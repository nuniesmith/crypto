//! 1h structure-break signals. Swings are confirmed with a right-hand window
//! so the live bot does not use future bars (the research loop did).

use crate::features::{Bar, Features};

pub fn rolling_mean(vol: &[f64], window: usize) -> Vec<f64> {
    let n = vol.len();
    let mut out = vec![0.0; n];
    if n == 0 || window == 0 {
        return out;
    }
    let mut csum = 0.0;
    let mut prefix = vec![0.0; n + 1];
    for i in 0..n {
        csum += vol[i];
        prefix[i + 1] = csum;
    }
    for i in 0..n {
        if i + 1 >= window {
            out[i] = (prefix[i + 1] - prefix[i + 1 - window]) / window as f64;
        } else {
            out[i] = prefix[i + 1] / (i + 1) as f64;
        }
    }
    out
}

/// +1 long, -1 short, 0 flat. `i` is the last *confirmed* closed bar.
pub fn trendline_break(bars: &[Bar], feat: &Features, pivot_lb: usize, vol_mult: f64) -> Vec<i8> {
    let n = bars.len();
    let mut sig = vec![0i8; n];
    if n < pivot_lb * 3 {
        return sig;
    }
    let vol: Vec<f64> = bars.iter().map(|b| b.volume).collect();
    let vol_ma = rolling_mean(&vol, pivot_lb * 2);

    let mut is_sh = vec![false; n];
    let mut is_sl = vec![false; n];
    let lb = pivot_lb;
    // Confirmed pivots only: need `lb` bars on the right.
    if n > lb * 2 {
        for i in lb..n - lb {
            let mut hi = f64::NEG_INFINITY;
            let mut lo = f64::INFINITY;
            for b in &bars[i - lb..=i + lb] {
                hi = hi.max(b.high);
                lo = lo.min(b.low);
            }
            if (bars[i].high - hi).abs() < 1e-12 {
                is_sh[i] = true;
            }
            if (bars[i].low - lo).abs() < 1e-12 {
                is_sl[i] = true;
            }
        }
    }

    let mut last_sh: Vec<(usize, f64)> = Vec::new();
    let mut last_sl: Vec<(usize, f64)> = Vec::new();
    for i in 0..n {
        if is_sh[i] {
            last_sh.push((i, bars[i].high));
            if last_sh.len() > 3 {
                last_sh.remove(0);
            }
        }
        if is_sl[i] {
            last_sl.push((i, bars[i].low));
            if last_sl.len() > 3 {
                last_sl.remove(0);
            }
        }
        if i < lb * 2 {
            continue;
        }
        let vol_ok = bars[i].volume >= vol_mult * vol_ma[i];
        if last_sh.len() >= 2 && vol_ok {
            let (i1, p1) = last_sh[last_sh.len() - 2];
            let (i2, p2) = last_sh[last_sh.len() - 1];
            if i2 > i1 && i > i2 {
                let slope = (p2 - p1) / (i2 - i1) as f64;
                let line = p2 + slope * (i - i2) as f64;
                if bars[i].close > line && bars[i - 1].close <= line {
                    sig[i] = 1;
                }
            }
        }
        if last_sl.len() >= 2 && vol_ok {
            let (i1, p1) = last_sl[last_sl.len() - 2];
            let (i2, p2) = last_sl[last_sl.len() - 1];
            if i2 > i1 && i > i2 {
                let slope = (p2 - p1) / (i2 - i1) as f64;
                let line = p2 + slope * (i - i2) as f64;
                if bars[i].close < line && bars[i - 1].close >= line {
                    sig[i] = -1;
                }
            }
        }
    }
    let _ = feat;
    sig
}

pub fn structure_filtered(
    bars: &[Bar],
    feat: &Features,
    pivot_lb: usize,
    vol_mult: f64,
) -> Vec<i8> {
    let mut sig = trendline_break(bars, feat, pivot_lb, vol_mult);
    for i in 0..sig.len() {
        let s = sig[i];
        if s == 0 {
            continue;
        }
        let vwap_ok = if s > 0 {
            bars[i].close > feat.vwap[i]
        } else {
            bars[i].close < feat.vwap[i]
        };
        let ema_ok = if s > 0 {
            feat.ema9[i] > feat.ema21[i]
        } else {
            feat.ema9[i] < feat.ema21[i]
        };
        if !(vwap_ok && ema_ok) {
            sig[i] = 0;
        }
    }
    sig
}

#[cfg(test)]
mod tests {
    use crate::features::{compute, Bar};

    fn ramp(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let p = 100.0 + i as f64;
                Bar {
                    time: 1_700_000_000 + (i as i64) * 3600,
                    open: p,
                    high: p + 1.0,
                    low: p - 0.5,
                    close: p + 0.2,
                    volume: 10.0 + (i % 7) as f64,
                }
            })
            .collect()
    }

    #[test]
    fn ema_rises_on_uptrend() {
        let bars = ramp(40);
        let f = compute(&bars);
        assert!(f.ema9[39] > f.ema9[10]);
        assert!(f.ema9[39] > f.ema21[39]);
    }
}
