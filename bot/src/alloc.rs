//! Live wallet policy.
//!
//! BTC is a HODL mix vs USD (70/30, rebalance only outside ±10%).
//! ETH/SOL 1h books may only trade the coins already on Kraken (plus leftover
//! USD above the 30% cash floor). No $1k notionals. buy_hold is mark-only.
//! Spot: no short opens — a short signal sells inventory.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wallet {
    pub usd: f64,
    pub btc: f64,
    pub eth: f64,
    pub sol: f64,
}

pub const BTC_TARGET: f64 = 0.70;
pub const BTC_BAND: f64 = 0.10;
pub const COST_MIN_USD: f64 = 0.50;
pub const MIN_BTC: f64 = 0.00005;
pub const MIN_ETH: f64 = 0.001;
pub const MIN_SOL: f64 = 0.06;

impl Wallet {
    pub fn from_balances(balances: &[(String, f64)]) -> Self {
        let mut w = Self::default();
        for (code, amt) in balances {
            if !amt.is_finite() || *amt <= 0.0 {
                continue;
            }
            let base = code.split(['.', '-']).next().unwrap_or(code);
            match base {
                "ZUSD" | "USD" => w.usd += *amt,
                "XXBT" | "XBT" | "BTC" => w.btc += *amt,
                "XETH" | "ETH" => w.eth += *amt,
                "SOL" => w.sol += *amt,
                _ => {}
            }
        }
        w
    }

    pub fn coin(&self, pair: &str) -> f64 {
        match pair {
            "ETHUSD" => self.eth,
            "SOLUSD" => self.sol,
            "XBTUSD" => self.btc,
            _ => 0.0,
        }
    }

    pub fn min_qty(pair: &str) -> f64 {
        match pair {
            "ETHUSD" => MIN_ETH,
            "SOLUSD" => MIN_SOL,
            "XBTUSD" => MIN_BTC,
            _ => f64::MAX,
        }
    }
}

pub fn floor_qty(qty: f64) -> f64 {
    if !qty.is_finite() || qty <= 0.0 {
        return 0.0;
    }
    (qty * 1e8).floor() / 1e8
}

pub fn limit_price(pair: &str, px: f64) -> String {
    match pair {
        "XBTUSD" => format!("{px:.1}"),
        "ETHUSD" | "SOLUSD" => format!("{px:.2}"),
        _ => format!("{px:.4}"),
    }
}

pub fn core_usd(w: &Wallet, btc_px: f64) -> f64 {
    w.btc * btc_px.max(0.0) + w.usd
}

pub fn btc_weight(w: &Wallet, btc_px: f64) -> f64 {
    let core = core_usd(w, btc_px);
    if core <= 0.0 {
        return 0.0;
    }
    (w.btc * btc_px) / core
}

/// USD spendable on ETH/SOL without taking BTC+USD cash below 30%.
pub fn surplus_usd(w: &Wallet, btc_px: f64) -> f64 {
    let core = core_usd(w, btc_px);
    let floor = (1.0 - BTC_TARGET) * core;
    (w.usd - floor).max(0.0)
}

#[derive(Clone, Debug)]
pub struct Rebalance {
    pub pair: &'static str,
    pub side: i8,
    pub qty: f64,
    pub price: f64,
}

pub fn btc_rebalance(w: &Wallet, btc_px: f64) -> Option<Rebalance> {
    if btc_px <= 0.0 {
        return None;
    }
    let core = core_usd(w, btc_px);
    if core < 1.0 {
        return None;
    }
    let weight = btc_weight(w, btc_px);
    // Only trim BTC when overweight. Never auto-buy BTC with ETH/SOL/USD proceeds.
    if weight <= BTC_TARGET + BTC_BAND {
        return None;
    }
    let delta_usd = w.btc * btc_px - BTC_TARGET * core;
    if delta_usd < COST_MIN_USD {
        return None;
    }
    let q = floor_qty((delta_usd / btc_px).min(w.btc));
    if q < MIN_BTC {
        return None;
    }
    Some(Rebalance {
        pair: "XBTUSD",
        side: -1,
        qty: q,
        price: btc_px,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum LiveAction {
    None,
    Skip(&'static str),
    Adopt { pair: String, qty: f64 },
    Buy { pair: String, qty: f64, price: f64 },
    Sell { pair: String, qty: f64, price: f64 },
}

/// Map a paper-book open/close onto a wallet-capped spot order.
/// `live_qty` is inventory this book already attached on Kraken (0 = paper-only).
pub fn signal_action(
    strategy: &str,
    pair: &str,
    opened: bool,
    closed: bool,
    pos_side: Option<i8>,
    mark: f64,
    w: &Wallet,
    btc_px: f64,
    live_qty: f64,
) -> LiveAction {
    if strategy == "buy_hold" {
        return LiveAction::Skip("buy_hold is mark-only");
    }
    if pair == "XBTUSD" {
        return LiveAction::Skip("BTC is HODL-only");
    }
    if mark <= 0.0 {
        return LiveAction::Skip("no mark");
    }
    let min_q = Wallet::min_qty(pair);
    if min_q.is_infinite() {
        return LiveAction::Skip("unknown pair");
    }
    let have = floor_qty(w.coin(pair));

    if closed {
        let q = floor_qty(live_qty.min(have));
        if q >= min_q {
            return LiveAction::Sell {
                pair: pair.to_string(),
                qty: q,
                price: mark,
            };
        }
        if !opened {
            return LiveAction::Skip("exit with no live inventory — not dumping wallet");
        }
    }

    if opened {
        let side = pos_side.unwrap_or(0);
        if side < 0 {
            return LiveAction::Skip("no spot short — not selling a flat wallet");
        }
        if side > 0 {
            if have >= min_q {
                return LiveAction::Adopt {
                    pair: pair.to_string(),
                    qty: have,
                };
            }
            let spend = surplus_usd(w, btc_px);
            let q = floor_qty(spend / mark);
            if q < min_q || spend < COST_MIN_USD {
                return LiveAction::Skip("long signal but surplus USD below min order");
            }
            return LiveAction::Buy {
                pair: pair.to_string(),
                qty: q,
                price: mark,
            };
        }
    }
    LiveAction::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet() -> Wallet {
        Wallet {
            usd: 52.99,
            btc: 0.00110007,
            eth: 0.00978356,
            sol: 0.16914939,
        }
    }

    #[test]
    fn parses_kraken_codes() {
        let w = Wallet::from_balances(&[
            ("ZUSD".into(), 52.99),
            ("XXBT".into(), 0.0011),
            ("XETH".into(), 0.009),
            ("SOL".into(), 0.16),
            ("SOL.S".into(), 0.01),
        ]);
        assert!((w.usd - 52.99).abs() < 1e-9);
        assert!((w.btc - 0.0011).abs() < 1e-12);
        assert!((w.eth - 0.009).abs() < 1e-12);
        assert!((w.sol - 0.17).abs() < 1e-12);
    }

    #[test]
    fn current_account_inside_btc_band() {
        let w = wallet();
        let px = 81_300.0;
        let wt = btc_weight(&w, px);
        assert!(wt > 0.60 && wt < 0.80, "weight={wt}");
        assert!(btc_rebalance(&w, px).is_none());
        let extra = surplus_usd(&w, px);
        assert!(extra > 5.0 && extra < 20.0, "surplus={extra}");
    }

    #[test]
    fn rebalance_sells_when_btc_heavy() {
        let w = Wallet {
            usd: 5.0,
            btc: 0.002,
            eth: 0.0,
            sol: 0.0,
        };
        let px = 80_000.0;
        let r = btc_rebalance(&w, px).expect("sell");
        assert_eq!(r.side, -1);
        assert_eq!(r.pair, "XBTUSD");
        assert!(r.qty >= MIN_BTC);
    }

    #[test]
    fn buy_hold_and_btc_never_signal_trade() {
        let w = wallet();
        assert!(matches!(
            signal_action("buy_hold", "SOLUSD", true, false, Some(1), 110.0, &w, 80_000.0, 0.0),
            LiveAction::Skip(_)
        ));
        assert!(matches!(
            signal_action("trendline_break", "XBTUSD", true, false, Some(1), 80_000.0, &w, 80_000.0, 0.0),
            LiveAction::Skip(_)
        ));
    }

    #[test]
    fn long_adopts_existing_sol_pile() {
        let w = wallet();
        match signal_action(
            "trendline_break",
            "SOLUSD",
            true,
            false,
            Some(1),
            111.0,
            &w,
            81_300.0,
            0.0,
        ) {
            LiveAction::Adopt { qty, .. } => assert!((qty - w.sol).abs() < 1e-8),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn exit_sells_sol_pile_not_paper_qty() {
        let w = wallet();
        match signal_action(
            "trendline_break",
            "SOLUSD",
            false,
            true,
            None,
            111.0,
            &w,
            81_300.0,
            w.sol,
        ) {
            LiveAction::Sell { qty, .. } => assert!((qty - w.sol).abs() < 1e-8),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn paper_exit_does_not_dump_wallet() {
        let w = wallet();
        assert!(matches!(
            signal_action(
                "trendline_break",
                "SOLUSD",
                false,
                true,
                None,
                111.0,
                &w,
                81_300.0,
                0.0,
            ),
            LiveAction::Skip(_)
        ));
    }

    #[test]
    fn short_on_flat_does_not_dump_eth() {
        let w = wallet();
        assert!(matches!(
            signal_action(
                "structure_filtered",
                "ETHUSD",
                true,
                false,
                Some(-1),
                2600.0,
                &w,
                81_300.0,
                0.0,
            ),
            LiveAction::Skip(_)
        ));
    }

    #[test]
    fn usd_heavy_does_not_auto_buy_btc() {
        let w = Wallet {
            usd: 97.0,
            btc: 0.00110007,
            eth: 0.0,
            sol: 0.0,
        };
        assert!(btc_rebalance(&w, 81_300.0).is_none());
    }

    #[test]
    fn no_spot_short_without_inventory() {
        let w = Wallet {
            usd: 50.0,
            btc: 0.001,
            eth: 0.0,
            sol: 0.0,
        };
        assert!(matches!(
            signal_action("structure_filtered", "ETHUSD", true, false, Some(-1), 2600.0, &w, 80_000.0, 0.0),
            LiveAction::Skip(_)
        ));
    }

    #[test]
    fn buy_uses_surplus_not_one_k() {
        let w = Wallet {
            usd: 52.99,
            btc: 0.00110007,
            eth: 0.0,
            sol: 0.0,
        };
        match signal_action(
            "structure_filtered",
            "ETHUSD",
            true,
            false,
            Some(1),
            2600.0,
            &w,
            81_300.0,
            0.0,
        ) {
            LiveAction::Buy { qty, .. } => {
                assert!(qty >= MIN_ETH);
                assert!(qty * 2600.0 < 30.0, "qty={qty}");
            }
            LiveAction::Skip(_) => {} // surplus might miss ETH min depending on px
            other => panic!("{other:?}"),
        }
    }
}
