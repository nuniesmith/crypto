//! The last check before a coin order reaches Kraken: a sanity cap on size,
//! not a trading rule.
//!
//! The policy decides what to trade; this only refuses an order no correct
//! policy would ever send, so a bug (a wrong weight, quantity or price, or a
//! loop that buys and sells the same coin) costs a bounded amount instead of
//! the account. A refused order is not placed. The gap behind it is retried
//! every tick, and in live mode Discord hears about it once per UTC day.
//!
//! There is deliberately no loss breaker here: a daily-loss or drawdown halt
//! would freeze the trend rule in the middle of a crash, the moment its bear
//! sells matter most.

use std::fmt;

use crate::{alloc, paper, regime};

/// No single BUY above this share of the account's total value. A
/// legitimate buy never exceeds a coin's base weight (BTC's 44%, bought from
/// nothing), so a bigger one is a bug.
///
/// Sells are not capped per order: with no drift rebalancing a coin can grow
/// into most of the account, and its bear flip must then sell it down to its
/// core in one order.
pub const MAX_BUY_SHARE: f64 = 0.60;

/// No more coin trading per UTC day, filled plus still open, than this many
/// times the account's total value. A legitimate day stays at or under 1x,
/// even with every coin flipping and a deposit invested together. A loop
/// that buys and sells the same coin passes 2x within a few round trips,
/// having cost about 0.8% of the account in fees rather than an unbounded
/// bleed.
pub const MAX_DAY_TURNOVER: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Refusal {
    /// The account's value is zero or unknown, so no size can be judged.
    NoAccountValue,
    BuyTooBig {
        notional: f64,
        total: f64,
    },
    DayFull {
        traded: f64,
        notional: f64,
        total: f64,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAccountValue => write!(f, "the account's value is zero or unknown"),
            Self::BuyTooBig { notional, total } => write!(
                f,
                "a ${notional:.2} buy is {:.0}% of the ${total:.2} account (cap {:.0}%)",
                notional / total * 100.0,
                MAX_BUY_SHARE * 100.0
            ),
            Self::DayFull {
                traded,
                notional,
                total,
            } => write!(
                f,
                "${traded:.2} already traded today plus ${notional:.2} would pass {MAX_DAY_TURNOVER}x \
                 the ${total:.2} account"
            ),
        }
    }
}

/// Whether one coin order may go to Kraken. `side` is +1 for a buy and -1
/// for a sell, `notional` its USD size, `total` the account's value, and
/// `traded_today` what [`traded_today`] returns.
pub fn check(side: i8, notional: f64, total: f64, traded_today: f64) -> Result<(), Refusal> {
    if !(total.is_finite() && total > 0.0) {
        return Err(Refusal::NoAccountValue);
    }
    if side > 0 && notional > MAX_BUY_SHARE * total {
        return Err(Refusal::BuyTooBig { notional, total });
    }
    if traded_today + notional > MAX_DAY_TURNOVER * total {
        return Err(Refusal::DayFull {
            traded: traded_today,
            notional,
            total,
        });
    }
    Ok(())
}

/// Coin trading so far this UTC day: today's fills, plus today's orders
/// still open valued at `m`. An order is one or the other, never both: it
/// leaves `pending_orders` when its fill is recorded. Selling USDC or USDT
/// for USD is not market risk and does not count.
pub fn traded_today(state: &paper::State, m: alloc::Marks, now: i64) -> f64 {
    let day_start = now - now.rem_euclid(86_400);
    let coin = |pair: &str| regime::PAIRS.contains(&pair);
    let filled: f64 = state
        .live
        .fills
        .iter()
        .filter(|f| f.ts >= day_start && coin(&f.pair))
        .map(|f| f.cost.abs())
        .sum();
    let open: f64 = state
        .pending_orders
        .iter()
        .filter(|o| o.placed_at >= day_start && coin(&o.pair))
        .map(|o| o.qty.abs() * m.of(&o.pair))
        .sum();
    filled + open
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{Fill, Sleeve};

    #[test]
    fn a_buy_bigger_than_any_weight_is_refused_but_a_big_sell_is_not() {
        assert_eq!(
            check(1, 440.0, 1000.0, 0.0),
            Ok(()),
            "BTC's whole 44% weight"
        );
        assert_eq!(
            check(1, 601.0, 1000.0, 0.0),
            Err(Refusal::BuyTooBig {
                notional: 601.0,
                total: 1000.0
            })
        );
        // BTC grown to 80% of the account, sold down to its 22% core.
        assert_eq!(check(-1, 580.0, 1000.0, 0.0), Ok(()));
    }

    #[test]
    fn the_day_is_full_past_twice_the_account() {
        assert_eq!(check(-1, 400.0, 1000.0, 1500.0), Ok(()));
        assert!(matches!(
            check(-1, 600.0, 1000.0, 1500.0),
            Err(Refusal::DayFull { .. })
        ));
    }

    #[test]
    fn an_account_with_no_value_trades_nothing() {
        assert_eq!(check(1, 10.0, 0.0, 0.0), Err(Refusal::NoAccountValue));
        assert_eq!(check(-1, 10.0, f64::NAN, 0.0), Err(Refusal::NoAccountValue));
    }

    fn fill(ts: i64, pair: &str, cost: f64) -> Fill {
        Fill {
            txid: format!("T{ts}{pair}"),
            ts,
            pair: pair.into(),
            side: 1,
            sleeve: Sleeve::Hold,
            book: None,
            qty: 0.0,
            cost,
            fee: 0.0,
        }
    }

    #[test]
    fn today_counts_coin_fills_and_open_coin_orders_only() {
        let day = 1_792_195_200; // 2026-10-17 00:00 UTC
        let now = day + 15 * 3600;
        let mut state = paper::State::default_paper();
        state.live.fills = vec![
            fill(day - 60, "XBTUSD", 900.0), // yesterday
            fill(day + 3600, "ETHUSD", 120.0),
            fill(day + 7200, "USDCUSD", 500.0), // a stablecoin sale
        ];
        state.pending_orders = vec![
            paper::PendingOrder {
                txid: "O1".into(),
                pair: "SOLUSD".into(),
                side: -1,
                placed_at: now - 60,
                book: None,
                qty: 0.5,
            },
            paper::PendingOrder {
                txid: "O2".into(),
                pair: "XBTUSD".into(),
                side: 1,
                placed_at: day - 60, // placed yesterday, still open
                book: None,
                qty: 0.01,
            },
        ];
        let m = alloc::Marks {
            sol: 120.0,
            btc: 85_000.0,
            ..alloc::Marks::default()
        };
        assert!((traded_today(&state, m, now) - (120.0 + 0.5 * 120.0)).abs() < 1e-9);
    }
}
