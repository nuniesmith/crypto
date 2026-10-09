//! The trend rule run on every coin since 2026-10-05: hold each coin, and
//! keep only half of it while the coin is in a bear regime.
//!
//! Through 2026-10-04 this ran ETH and SOL only, inside a BTC-hold-plus-
//! trade-sleeve policy. The operator then asked for ONE account targeting
//! BTC/ETH/SOL/cash directly (see `alloc.rs`), with the same trend rule
//! sizing all three coins rather than two of them against a frozen BTC
//! pile — so `PAIRS` below grew to include `XBTUSD`.
//!
//! `src/crypto/opt/regime.py` compared standard, untuned rules on Binance daily
//! data (ETH from 2018, SOL from 2021) at Kraken tier-1 fees, over the whole
//! history and each half separately. The one chosen was the most consistent,
//! not the best-fitting:
//!
//! * **Core 50%**: half of each coin's sleeve is never sold.
//! * **The other half follows the 200-day average with a ±5% buffer.** It is
//!   sold when the daily close falls more than 5% below the average, bought
//!   back when it rises more than 5% above, and left alone in between, so a
//!   price hugging its average does not flip-flop.
//!
//! Over the full history it returned 6.2× (ETH) and 24.7× (SOL), against
//! buy-and-hold's 3.1× and 8.6×, with shallower drawdowns. It lagged holding
//! in a crash-free stretch (ETH 2022–26: 2.1× vs 2.7×), because the big gains
//! came from stepping aside in 2018 and 2022. It averaged about three trades a
//! year per coin, which is why fees stop mattering.
//!
//! The state is a function of CLOSED daily candles only: the day still forming
//! is never read.

/// Days in the average the regime is read against.
pub const SMA_DAYS: usize = 200;
/// The buffer either side of the average: crossing it is what flips the state.
pub const BAND: f64 = 0.05;
/// The share of each coin's sleeve held in a bear regime, the core that is
/// never sold.
pub const CORE: f64 = 0.50;
/// The coins this rule runs: every coin the account targets.
///
/// **Grew from `["ETHUSD", "SOLUSD"]` to all three on 2026-10-05** — see
/// `alloc.rs` for the account-level policy this feeds.
pub const PAIRS: [&str; 6] = ["XBTUSD", "ETHUSD", "SOLUSD", "LINKUSD", "XRPUSD", "INJUSD"];

/// One day's reading for a coin.
///
/// `Serialize`/`Deserialize` so `State::regime_reading` (`paper.rs`) can
/// persist the last reading for Discord's regime section — the daily read
/// and the daily/weekly/monthly report run on different schedules, so the
/// report cannot assume it is reading fresh off a just-completed read.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Reading {
    /// The last closed daily close.
    pub close: f64,
    /// The 200-day average it was compared with.
    pub sma: f64,
    /// Bull (true) or bear (false), after the ±5% buffer.
    pub bull: bool,
}

impl Reading {
    /// How far the close sits from its average, e.g. 0.278 for 27.8% above.
    pub fn distance(&self) -> f64 {
        self.close / self.sma - 1.0
    }
}

/// The regime on the LAST of `closes`: closed daily closes, oldest first.
///
/// Walks the whole series rather than reading the last day alone, because
/// inside the buffer the state is whatever it was last set to. Starting from
/// the first day the average exists, the state is the plain above/below test,
/// then it only changes on a close beyond the buffer. With Kraken's 720 daily
/// candles that leaves 520 days of walk, far more than it takes to settle
/// into the same state a longer history would give. It is the same machine
/// as `sma200_band` in `src/crypto/opt/regime.py`, so the study and the bot
/// cannot disagree.
///
/// `None` until there are `SMA_DAYS` closes.
pub fn evaluate(closes: &[f64]) -> Option<Reading> {
    if closes.len() < SMA_DAYS {
        return None;
    }
    let n = SMA_DAYS as f64;
    let mut sum: f64 = closes[..SMA_DAYS].iter().sum();
    let mut bull: Option<bool> = None;
    let mut last = None;
    for i in (SMA_DAYS - 1)..closes.len() {
        if i >= SMA_DAYS {
            sum += closes[i] - closes[i - SMA_DAYS];
        }
        let sma = sum / n;
        let close = closes[i];
        let state = match bull {
            None => close > sma,
            Some(_) if close > sma * (1.0 + BAND) => true,
            Some(_) if close < sma * (1.0 - BAND) => false,
            Some(s) => s,
        };
        bull = Some(state);
        last = Some(Reading {
            close,
            sma,
            bull: state,
        });
    }
    last
}

/// The share of a coin's sleeve to hold in this regime.
pub fn exposure(bull: bool) -> f64 {
    if bull {
        1.0
    } else {
        CORE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(n: usize, px: f64) -> Vec<f64> {
        vec![px; n]
    }

    #[test]
    fn needs_two_hundred_closed_days() {
        assert_eq!(evaluate(&flat(SMA_DAYS - 1, 100.0)), None);
        assert!(evaluate(&flat(SMA_DAYS, 100.0)).is_some());
    }

    #[test]
    fn starts_from_the_plain_comparison_with_the_average() {
        // On the first day the average exists there is no previous state for
        // the buffer to hold, so it is simply above or below.
        let r = evaluate(&flat(SMA_DAYS, 100.0)).unwrap();
        assert!(!r.bull, "a close equal to its average is not above it");
        let mut up = flat(SMA_DAYS - 1, 100.0);
        up.push(101.0);
        assert!(evaluate(&up).unwrap().bull);
    }

    #[test]
    fn flips_only_beyond_the_buffer_and_holds_inside_it() {
        let mut c = flat(SMA_DAYS, 100.0); // bear on day one (100 is not > 100)
        c.push(104.0); // above the average, but inside +5%
        assert!(
            !evaluate(&c).unwrap().bull,
            "inside the buffer keeps the bear state"
        );
        c.push(106.0); // beyond +5% of an average still ~100
        assert!(evaluate(&c).unwrap().bull, "beyond +5% flips to bull");
        c.push(97.0); // below the average, inside −5%
        assert!(
            evaluate(&c).unwrap().bull,
            "inside the buffer keeps the bull state"
        );
        c.push(94.0); // beyond −5%
        assert!(!evaluate(&c).unwrap().bull, "beyond −5% flips to bear");
    }

    #[test]
    fn reads_the_average_of_the_last_two_hundred_days_only() {
        // A long-gone high must not still be propping up the average.
        let mut c = flat(SMA_DAYS, 1_000.0);
        c.extend(flat(SMA_DAYS, 100.0));
        let r = evaluate(&c).unwrap();
        assert!((r.sma - 100.0).abs() < 1e-6, "sma={}", r.sma);
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // CORE is a const; this is a regression guard, not dead logic.
    fn the_core_is_kept_in_a_bear() {
        assert_eq!(exposure(true), 1.0);
        assert_eq!(exposure(false), CORE);
        assert!(CORE > 0.0, "a bear never sells the whole coin");
    }
}
