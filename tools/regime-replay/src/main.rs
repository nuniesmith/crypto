//! Replays indicators-ta's three regime detectors over one coin's candles.
//!
//! Janus's regime bridge serves `janus-regime`, which is indicators-ta's
//! detector stack: indicator-based (ADX, Bollinger, ATR, EMA), a hidden
//! Markov model, and an ensemble of the two. Live, Janus feeds it minute
//! klines. This feeds it DAILY candles instead, the timescale the crypto
//! bot's 200-day rule works on, so basket.py (tag `python-research-final`) could
//! test whether it would have managed the account better.
//!
//! stdin:  CSV `date,high,low,close` (header line, oldest first)
//! stdout: CSV `date,ensemble,ensemble_conf,indicator,indicator_conf,hmm,hmm_conf`
//!         Each regime is one of: bull, bear, mean_reverting, volatile,
//!         uncertain, or `warmup` before the detector is ready. A row
//!         describes the regime AFTER that day's close.

use std::io::{self, BufRead, Write};

use indicators::{
    EnsembleRegimeDetector, HMMRegimeDetector, MarketRegime, RegimeDetector, TrendDirection,
};

fn label(r: MarketRegime) -> &'static str {
    match r {
        MarketRegime::Trending(TrendDirection::Bullish) => "bull",
        MarketRegime::Trending(TrendDirection::Bearish) => "bear",
        MarketRegime::MeanReverting => "mean_reverting",
        MarketRegime::Volatile => "volatile",
        MarketRegime::Uncertain => "uncertain",
    }
}

fn main() {
    let mut ensemble = EnsembleRegimeDetector::default_config();
    let mut indicator = RegimeDetector::default_config();
    let mut hmm = HMMRegimeDetector::default_config();
    let stdin = io::stdin();
    let mut out = io::BufWriter::new(io::stdout().lock());
    writeln!(out, "date,ensemble,ensemble_conf,indicator,indicator_conf,hmm,hmm_conf").unwrap();
    for (n, line) in stdin.lock().lines().enumerate() {
        let line = line.expect("stdin");
        if n == 0 {
            continue; // header
        }
        let f: Vec<&str> = line.split(',').collect();
        let (date, high, low, close) = (
            f[0],
            f[1].parse::<f64>().expect("high"),
            f[2].parse::<f64>().expect("low"),
            f[3].parse::<f64>().expect("close"),
        );
        let e = ensemble.update(high, low, close);
        let i = indicator.update(high, low, close);
        let h = hmm.update_ohlc(high, low, close);
        let el = if ensemble.is_ready() { label(e.regime) } else { "warmup" };
        let il = if indicator.is_ready() { label(i.regime) } else { "warmup" };
        let hl = if hmm.is_ready() { label(h.regime) } else { "warmup" };
        writeln!(
            out,
            "{date},{el},{:.3},{il},{:.3},{hl},{:.3}",
            e.confidence, i.confidence, h.confidence
        )
        .unwrap();
    }
}
