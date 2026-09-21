//! Live wallet policy: two sleeves over one Kraken account.
//!
//! * **Hold** — BTC + USD, `CORE_SHARE` of the account, mixed `BTC_TARGET`
//!   BTC to cash. Rebalanced in BOTH directions when BTC drifts outside the
//!   band, so a deposit is absorbed rather than left sitting.
//! * **Trade** — ETH + SOL + the cash behind them, the rest of the account.
//!   The 1h books spend from this sleeve only; buy_hold is mark-only, and
//!   spot never opens a short (a short signal sells inventory).
//!
//! The two sleeves share one physical USD balance, so every number below is
//! derived from the account TOTAL rather than from a running cash ledger.
//! That is what makes a deposit self-correcting: new USD raises the total,
//! which raises both sleeves' targets, and the next rebalance moves real
//! coins to match. No deposit has to be recorded anywhere.
//!
//! ## Why the hold is a share of the account and not just "BTC vs USD"
//!
//! The first version targeted 70/30 across BTC+USD alone and funded ETH/SOL
//! from `USD − 30% of (BTC+USD)`. At the target that expression is exactly
//! zero: a 70/30 account has no spare dollar by construction. It only ever
//! bought ETH or SOL because BTC sat UNDERWEIGHT (48% against a stated 70%),
//! and it could never correct that, because the rebalance only sold. Making
//! that rebalance two-sided without this change would have driven BTC to
//! target and starved the 1h strategy of capital permanently — the account
//! would have looked healthy while quietly ceasing to trade.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wallet {
    /// TOTAL USD as Kraken's `Balance` reports it — money reserved by an
    /// open order is still counted here, because it is still yours and the
    /// account is still worth it.
    pub usd: f64,
    pub btc: f64,
    pub eth: f64,
    pub sol: f64,
    /// USD an open buy order has already claimed. Not spendable twice.
    ///
    /// Kraken's `Balance` does NOT subtract this; only `BalanceEx`'s
    /// `hold_trade` and the open-order book show it. Measured on the live
    /// account on 2026-09-21: `ZUSD balance=120.41 hold_trade=37.38`, so a
    /// policy reading `Balance` alone believed it had $37 more to spend
    /// than existed, and would have sized an order Kraken then rejected.
    pub usd_held: f64,
}

impl Wallet {
    /// USD that can actually be committed to a new order right now.
    ///
    /// Everything that VALUES the account uses `usd` (held money still
    /// counts); everything that SPENDS uses this.
    pub fn usd_available(&self) -> f64 {
        (self.usd - self.usd_held).max(0.0)
    }
}

/// Which wallet policy is in force.
///
/// A parameter rather than a compile-time constant so BOTH configurations
/// stay tested. Turning the sleeves off would otherwise delete the only
/// coverage of adopt-inventory, exit-sells-the-pile, deposit absorption and
/// trade-sleeve funding — and re-arming later would be a leap into code
/// nothing had exercised in months.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Policy {
    /// Whether the ETH/SOL 1h books may touch the wallet at all.
    pub trade_sleeve: bool,
    /// Share of the account held as BTC+USD when `trade_sleeve` is on.
    pub core_share: f64,
}

impl Policy {
    /// What the bot actually runs.
    pub const LIVE: Policy = Policy { trade_sleeve: TRADE_SLEEVE_ENABLED, core_share: CORE_SHARE };
    /// The configuration the sleeves ran under until 2026-09-21. Kept so
    /// that behaviour stays under test while it is switched off.
    pub const WITH_SLEEVES: Policy = Policy { trade_sleeve: true, core_share: CORE_SHARE };
}

/// Whether the ETH/SOL 1h books may touch the wallet at all.
///
/// **Off since 2026-09-21.** The sleeves lost to buy-and-hold on the 60-day
/// holdout by $325 (SOL) and $280 (ETH) on a $1000 book, and ZERO of the 18
/// cells in that run's leaders table beat BH — see docs/research.md. The
/// repo's own gate is "a green holdout that loses to BH or fails WF is not
/// an add-to-live"; both sleeves pass the WF half and fail the BH half, and
/// the gate is an AND.
///
/// The books keep STEPPING with this off. They just never reach the wallet.
/// That is deliberate: every bar they record is out-of-sample evidence
/// about whether the strategy works, collected at no risk, which is worth
/// more than deleting them and starting the question over later.
///
/// Turning this back on is a deliberate edit, and should follow a run where
/// a sleeve actually clears the gate rather than a good week.
pub const TRADE_SLEEVE_ENABLED: bool = false;

/// Share of the whole account held as the BTC+USD sleeve when the trade
/// sleeve IS enabled; the rest is trading capital. Operator's call,
/// 2026-09-21. Unused while `TRADE_SLEEVE_ENABLED` is false — see
/// `hold_base_usd`.
pub const CORE_SHARE: f64 = 0.50;
/// BTC's share WITHIN the hold sleeve. 0.70 × 0.50 = 35% of the account.
pub const BTC_TARGET: f64 = 0.70;
/// Drift allowed before rebalancing, in points of the hold sleeve — so ±10
/// here is ±5 points of the account at CORE_SHARE 0.50.
pub const BTC_BAND: f64 = 0.10;
/// Kraken `costmin` for all three USD pairs (verified against
/// /0/public/AssetPairs, 2026-09-21).
pub const COST_MIN_USD: f64 = 0.50;
/// Kraken `ordermin`, same source. XBT 0.00005 / ETH 0.001 / SOL 0.06.
pub const MIN_BTC: f64 = 0.00005;
pub const MIN_ETH: f64 = 0.001;
pub const MIN_SOL: f64 = 0.06;

/// Last trade price for each pair the policy needs to mark the account.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Marks {
    pub btc: f64,
    pub eth: f64,
    pub sol: f64,
}

impl Marks {
    pub fn from_pairs(marks: &[(String, f64)]) -> Self {
        let mut m = Self::default();
        for (pair, px) in marks {
            match pair.as_str() {
                "XBTUSD" => m.btc = *px,
                "ETHUSD" => m.eth = *px,
                "SOLUSD" => m.sol = *px,
                _ => {}
            }
        }
        m
    }

    /// Whether every price needed to value the account is present. A missing
    /// mark makes the account look SMALLER than it is, which would pull every
    /// target down and could trade real coins on a bad number — so callers
    /// must refuse to rebalance rather than proceed with a partial view.
    pub fn complete(&self) -> bool {
        self.btc > 0.0 && self.eth > 0.0 && self.sol > 0.0
    }
}

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

/// The whole account marked to USD — the one number every target derives
/// from, and the reason a deposit needs no bookkeeping to be picked up.
pub fn total_usd(w: &Wallet, m: Marks) -> f64 {
    w.usd + w.btc * m.btc.max(0.0) + w.eth * m.eth.max(0.0) + w.sol * m.sol.max(0.0)
}

/// What the account should look like, in dollars, right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Targets {
    pub total: f64,
    /// Dollars of BTC the hold sleeve wants.
    pub btc: f64,
    /// Dollars of USD the hold sleeve wants; never spent on ETH or SOL.
    pub hold_cash: f64,
    /// Dollars the trade sleeve wants free for a signal, after what ETH and
    /// SOL are already worth. Zero once inventory fills the sleeve.
    pub trade_cash: f64,
}

/// Dollars the BTC/USD hold sleeve spans — the denominator every target and
/// the drift band are measured against.
///
/// With the trade sleeve ON it is `CORE_SHARE` of the account. With it OFF
/// the hold is simply BTC + USD: whatever ETH and SOL are sitting there are
/// FROZEN leftovers, excluded from the mix rather than counted into it.
/// Counting them would pull the BTC target up by their value and make the
/// bot buy bitcoin to offset coins it has decided not to trade.
pub fn hold_base_usd(w: &Wallet, m: Marks, p: Policy) -> f64 {
    let total = total_usd(w, m);
    if p.trade_sleeve {
        return p.core_share * total;
    }
    let deployed = w.eth * m.eth.max(0.0) + w.sol * m.sol.max(0.0);
    (total - deployed).max(0.0)
}

pub fn targets(w: &Wallet, m: Marks, p: Policy) -> Targets {
    let total = total_usd(w, m);
    let deployed = w.eth * m.eth.max(0.0) + w.sol * m.sol.max(0.0);
    let hold = hold_base_usd(w, m, p);
    Targets {
        total,
        btc: BTC_TARGET * hold,
        hold_cash: (1.0 - BTC_TARGET) * hold,
        trade_cash: if p.trade_sleeve {
            ((1.0 - p.core_share) * total - deployed).max(0.0)
        } else {
            0.0
        },
    }
}

/// BTC as a share of the hold sleeve's TARGET size, so this reads on the
/// same scale as `BTC_TARGET` (0.70) rather than as a share of everything.
///
/// The denominator is the target `CORE_SHARE × total`, deliberately not the
/// sleeve's current contents. Measuring against what BTC and cash happen to
/// add up to right now makes the number drift with its own numerator, and
/// the first version did exactly that: it read 76% — comfortably inside the
/// band — for a wallet the rebalancer had already decided to sell. Against
/// the target, `|btc_weight − BTC_TARGET| > BTC_BAND` is algebraically the
/// same test `btc_rebalance` applies, so the log line and the order can no
/// longer disagree.
pub fn btc_weight(w: &Wallet, m: Marks, p: Policy) -> f64 {
    let hold = hold_base_usd(w, m, p);
    if hold <= 0.0 {
        return 0.0;
    }
    (w.btc * m.btc.max(0.0)) / hold
}

/// USD the 1h books may spend on ETH or SOL right now.
///
/// Capped twice, and both caps matter: by the trade sleeve's own headroom,
/// so a rallying SOL position does not justify buying more of it; and by the
/// cash actually on hand once the hold sleeve's dollars are set aside, so a
/// signal can never eat the BTC sleeve's reserve.
pub fn trade_cash_usd(w: &Wallet, m: Marks, p: Policy) -> f64 {
    let t = targets(w, m, p);
    t.trade_cash
        .min((w.usd_available() - t.hold_cash).max(0.0))
        .max(0.0)
}

#[derive(Clone, Debug)]
pub struct Rebalance {
    pub pair: &'static str,
    pub side: i8,
    pub qty: f64,
    pub price: f64,
}

/// Move BTC back toward its target when it has drifted outside the band.
///
/// Both directions. The sell side trims a BTC run-up; the buy side is what
/// absorbs a deposit — new USD lands in the account, every target scales up
/// with the total, BTC is suddenly underweight, and this buys it back to
/// `Targets::btc`. Nothing has to tell the bot that money arrived.
///
/// Requires a COMPLETE set of marks. A missing ETH or SOL price understates
/// the total, which understates every target — and on the sell side that is
/// an order for real BTC computed from a number known to be wrong.
pub fn btc_rebalance(w: &Wallet, m: Marks, p: Policy) -> Option<Rebalance> {
    if !m.complete() {
        return None;
    }
    let t = targets(w, m, p);
    if t.total < 1.0 {
        return None;
    }
    let btc_usd = w.btc * m.btc;
    // The band is in points of the HOLD sleeve, so convert it to dollars
    // through that sleeve's size rather than the whole account's.
    let hold = hold_base_usd(w, m, p);
    let delta_usd = t.btc - btc_usd;
    if delta_usd.abs() <= BTC_BAND * hold {
        return None;
    }
    if delta_usd.abs() < COST_MIN_USD {
        return None;
    }
    if delta_usd > 0.0 {
        // Buy, but only with cash that is actually there. The hold sleeve
        // being short exactly mirrors USD being long, so in the ordinary
        // case this cap does not bind; it bites only when ETH/SOL inventory
        // has overrun its own sleeve and there is no spare dollar.
        let spend = delta_usd.min(w.usd_available());
        let q = floor_qty(spend / m.btc);
        if q < MIN_BTC || spend < COST_MIN_USD {
            return None;
        }
        return Some(Rebalance {
            pair: "XBTUSD",
            side: 1,
            qty: q,
            price: m.btc,
        });
    }
    let q = floor_qty(((-delta_usd) / m.btc).min(w.btc));
    if q < MIN_BTC {
        return None;
    }
    Some(Rebalance {
        pair: "XBTUSD",
        side: -1,
        qty: q,
        price: m.btc,
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
    m: Marks,
    live_qty: f64,
    p: Policy,
) -> LiveAction {
    if strategy == "buy_hold" {
        return LiveAction::Skip("buy_hold is mark-only");
    }
    if !p.trade_sleeve {
        // Off entirely -- no buys AND no sells. Selling on an exit would
        // liquidate the ETH/SOL that is deliberately being left alone, which
        // is the opposite of "stop trading these".
        return LiveAction::Skip("trade sleeve disabled — paper only");
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
            if !m.complete() {
                // Same rule as the rebalancer: a partial mark set understates
                // the account, and here that would understate the trade
                // sleeve and silently under-buy.
                return LiveAction::Skip("incomplete marks — not sizing a buy");
            }
            let spend = trade_cash_usd(w, m, p);
            let q = floor_qty(spend / mark);
            if q < min_q || spend < COST_MIN_USD {
                return LiveAction::Skip("long signal but trade sleeve cash below min order");
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

    /// These assert the sleeve-ON behaviour. It is switched off live, and
    /// stays tested here so re-arming is not a leap into cold code.
    const P: Policy = Policy::WITH_SLEEVES;

    /// Prices near the live ones on 2026-09-21.
    fn marks() -> Marks {
        Marks {
            btc: 81_300.0,
            eth: 2_600.0,
            sol: 111.0,
        }
    }

    /// The real Kraken wallet on 2026-09-21: BTC 48.1% of a hold sleeve that
    /// wanted 70%, no ETH, dust SOL.
    fn live_wallet() -> Wallet {
        Wallet {
            usd: 96.69,
            btc: 0.00110007,
            eth: 0.0,
            sol: 0.00000939,
            usd_held: 0.0,
        }
    }

    fn apply(w: &Wallet, r: &Rebalance) -> Wallet {
        let mut out = w.clone();
        let cost = r.qty * r.price;
        if r.side > 0 {
            out.btc += r.qty;
            out.usd -= cost;
        } else {
            out.btc -= r.qty;
            out.usd += cost;
        }
        out
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
    fn sleeves_add_up_to_the_whole_account() {
        let w = live_wallet();
        let t = targets(&w, marks(), P);
        let deployed = w.eth * marks().eth + w.sol * marks().sol;
        let sum = t.btc + t.hold_cash + t.trade_cash + deployed;
        assert!((sum - t.total).abs() < 1e-6, "sum={sum} total={}", t.total);
    }

    #[test]
    fn an_underweight_account_buys_btc_back() {
        // THE capability this change adds. The old rebalancer only ever
        // sold, so an account short of BTC could never be corrected and the
        // mix ratcheted further down every time ETH or SOL sold into USD.
        let w = Wallet {
            usd: 300.0,
            btc: 0.0005,
            eth: 0.0,
            sol: 0.0,
            usd_held: 0.0,
        };
        let r = btc_rebalance(&w, marks(), P).expect("underweight BTC must be bought back");
        assert_eq!(r.side, 1);
        assert_eq!(r.pair, "XBTUSD");
        let after = apply(&w, &r);
        let t = targets(&after, marks(), P);
        assert!(
            (after.btc * marks().btc - t.btc).abs() < 1.0,
            "after={} target={}",
            after.btc * marks().btc,
            t.btc
        );
    }

    #[test]
    fn the_real_account_is_btc_HEAVY_under_the_fifty_fifty_split() {
        // Worth pinning because it is counter-intuitive and it is what
        // happens with real money on the first bar after deploying. Against
        // the OLD policy (70/30 of BTC+USD alone) this wallet was UNDER
        // weight at 48%. Against the new one BTC wants 35% of the account
        // and holds 48%, so the very first rebalance SELLS roughly $24 of
        // BTC -- that is the 50/50 choice being applied, not a malfunction.
        let w = live_wallet();
        let t = targets(&w, marks(), P);
        assert!(w.btc * marks().btc > t.btc);
        let r = btc_rebalance(&w, marks(), P).expect("out of band");
        assert_eq!(r.side, -1);
        let proceeds = r.qty * r.price;
        assert!((20.0..30.0).contains(&proceeds), "proceeds={proceeds}");
        // And the money freed lands in the trade sleeve, which is the point.
        let after = apply(&w, &r);
        assert!(trade_cash_usd(&after, marks(), P) > trade_cash_usd(&w, marks(), P));
    }

    #[test]
    fn a_deposit_is_absorbed_without_being_recorded_anywhere() {
        // "When I add money over time, it will always rebalance." Nothing
        // tells the bot a deposit happened -- it re-reads the wallet, the
        // total jumps, and every target moves with it.
        let mut w = live_wallet();
        w.usd += 500.0;
        let r = btc_rebalance(&w, marks(), P).expect("a deposit must pull BTC back to target");
        assert_eq!(r.side, 1);
        let after = apply(&w, &r);
        let t = targets(&after, marks(), P);
        assert!((after.btc * marks().btc - t.btc).abs() < 1.0);
        // ...and the trade sleeve grew too, rather than the deposit being
        // swallowed whole by BTC.
        assert!(
            trade_cash_usd(&after, marks(), P) > 300.0,
            "trade cash {}",
            trade_cash_usd(&after, marks(), P)
        );
    }

    #[test]
    fn the_log_line_and_the_order_cannot_disagree() {
        // btc_weight is what the operator reads in the journal. Defined
        // against the sleeve's CURRENT contents it reported 76% -- inside
        // the 60-80 band -- for a wallet the rebalancer was about to sell.
        let w = live_wallet();
        let drift = (btc_weight(&w, marks(), P) - BTC_TARGET).abs();
        assert_eq!(
            drift > BTC_BAND,
            btc_rebalance(&w, marks(), P).is_some(),
            "weight says {:.3} off target, rebalancer says {:?}",
            drift,
            btc_rebalance(&w, marks(), P).map(|r| r.side)
        );
    }

    #[test]
    fn rebalancing_to_target_leaves_the_strategy_funded() {
        // The trap in the old model: funding ETH/SOL from "USD above 30% of
        // BTC+USD" is exactly zero once BTC+USD sits at 70/30, so a two-sided
        // rebalance alone would have driven the account to target and then
        // never bought another coin.
        let w = live_wallet();
        let after = apply(&w, &btc_rebalance(&w, marks(), P).unwrap());
        assert!(btc_rebalance(&after, marks(), P).is_none(), "should be in band now");
        assert!(
            trade_cash_usd(&after, marks(), P) > 40.0,
            "in-band account must still have trading capital, got {}",
            trade_cash_usd(&after, marks(), P)
        );
    }

    #[test]
    fn rebalance_sells_when_btc_heavy() {
        let w = Wallet {
            usd: 5.0,
            btc: 0.002,
            eth: 0.0,
            sol: 0.0,
            usd_held: 0.0,
        };
        let r = btc_rebalance(&w, marks(), P).expect("sell");
        assert_eq!(r.side, -1);
        assert!(r.qty >= MIN_BTC);
        let after = apply(&w, &r);
        assert!(btc_rebalance(&after, marks(), P).is_none());
    }

    #[test]
    fn inside_the_band_nothing_trades() {
        let w = live_wallet();
        let at_target = apply(&w, &btc_rebalance(&w, marks(), P).unwrap());
        assert!(btc_rebalance(&at_target, marks(), P).is_none());
        // And a small drift inside the band is left alone rather than
        // churning fees on every bar.
        let mut nudged = at_target.clone();
        nudged.btc *= 1.02;
        assert!(btc_rebalance(&nudged, marks(), P).is_none());
    }

    #[test]
    fn an_incomplete_mark_set_never_trades() {
        // fetch_marks returns an EMPTY vec on error, so a Kraken ticker
        // outage leaves prices at 0.0 -- which understates the total, which
        // understates every target. Sizing a real BTC order off that is the
        // one failure here that spends money on a number known to be wrong.
        let w = live_wallet();
        for bad in [
            Marks { btc: 0.0, ..marks() },
            Marks { eth: 0.0, ..marks() },
            Marks { sol: 0.0, ..marks() },
            Marks::default(),
        ] {
            assert!(btc_rebalance(&w, bad, P).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_long_signal_spends_the_trade_sleeve_not_the_hold() {
        let w = live_wallet();
        match signal_action(
            "structure_filtered",
            "ETHUSD",
            true,
            false,
            Some(1),
            marks().eth,
            &w,
            marks(),
            0.0,
            P,
        ) {
            LiveAction::Buy { qty, .. } => {
                let spent = qty * marks().eth;
                let t = targets(&w, marks(), P);
                assert!(spent <= t.trade_cash + 1e-6, "spent {spent} > sleeve {}", t.trade_cash);
                assert!(
                    w.usd - spent >= t.hold_cash - 1e-6,
                    "ate the hold sleeve's cash"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_long_signal_cannot_touch_the_hold_when_the_sleeve_is_full() {
        // ETH inventory already fills the trade sleeve, so there is no
        // headroom left even though plenty of USD is sitting there for BTC.
        let w = Wallet {
            usd: 200.0,
            btc: 0.0,
            eth: 0.1,
            sol: 0.0,
            usd_held: 0.0,
        };
        assert_eq!(trade_cash_usd(&w, marks(), P), 0.0);
        assert!(matches!(
            signal_action(
                "trendline_break",
                "SOLUSD",
                true,
                false,
                Some(1),
                marks().sol,
                &w,
                marks(),
                0.0,
                P,
            ),
            LiveAction::Skip(_)
        ));
    }

    #[test]
    fn buy_hold_and_btc_never_signal_trade() {
        let w = live_wallet();
        assert!(matches!(
            signal_action("buy_hold", "SOLUSD", true, false, Some(1), marks().sol, &w, marks(), 0.0, P),
            LiveAction::Skip(_)
        ));
        assert!(matches!(
            signal_action("trendline_break", "XBTUSD", true, false, Some(1), marks().btc, &w, marks(), 0.0, P),
            LiveAction::Skip(_)
        ));
    }

    #[test]
    fn long_adopts_existing_sol_pile() {
        let w = Wallet { sol: 0.169, ..live_wallet() };
        match signal_action(
            "trendline_break", "SOLUSD", true, false, Some(1), marks().sol, &w, marks(), 0.0, P,
        ) {
            LiveAction::Adopt { qty, .. } => assert!((qty - w.sol).abs() < 1e-8),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn exit_sells_sol_pile_not_paper_qty() {
        let w = Wallet { sol: 0.169, ..live_wallet() };
        match signal_action(
            "trendline_break", "SOLUSD", false, true, None, marks().sol, &w, marks(), w.sol, P,
        ) {
            LiveAction::Sell { qty, .. } => assert!((qty - w.sol).abs() < 1e-8),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn paper_exit_does_not_dump_wallet() {
        let w = Wallet { sol: 0.169, ..live_wallet() };
        assert!(matches!(
            signal_action("trendline_break", "SOLUSD", false, true, None, marks().sol, &w, marks(), 0.0, P),
            LiveAction::Skip(_)
        ));
    }

    #[test]
    fn short_on_flat_does_not_dump_eth() {
        let w = Wallet { eth: 0.0097, ..live_wallet() };
        assert!(matches!(
            signal_action("structure_filtered", "ETHUSD", true, false, Some(-1), marks().eth, &w, marks(), 0.0, P),
            LiveAction::Skip(_)
        ));
    }

    #[test]
    fn no_spot_short_without_inventory() {
        let w = live_wallet();
        assert!(matches!(
            signal_action("structure_filtered", "ETHUSD", true, false, Some(-1), marks().eth, &w, marks(), 0.0, P),
            LiveAction::Skip(_)
        ));
    }
}

#[cfg(test)]
mod held_tests {
    use super::*;

    const P: Policy = Policy::WITH_SLEEVES;

    fn marks() -> Marks {
        Marks { btc: 84_594.0, eth: 2_692.0, sol: 115.0 }
    }

    /// The live account at 10:05 UTC on 2026-09-21, straight off Kraken:
    /// ZUSD balance 120.41 with 37.38 held by an open ETH buy left behind
    /// by the previous binary.
    fn live() -> Wallet {
        Wallet {
            usd: 120.4135,
            usd_held: 37.3751,
            btc: 0.00078514,
            eth: 0.0010000036,
            sol: 0.0000093939,
        }
    }

    #[test]
    fn held_usd_still_counts_toward_what_the_account_is_worth() {
        // It is still your money -- it just cannot be spent twice. If the
        // total dropped by the held amount, every target would shrink and
        // the bot would trade to chase its own open order.
        let w = live();
        let with_hold = total_usd(&w, marks());
        let without = total_usd(&Wallet { usd_held: 0.0, ..w.clone() }, marks());
        assert!((with_hold - without).abs() < 1e-9);
    }

    #[test]
    fn spendable_cash_excludes_what_an_open_order_claimed() {
        let w = live();
        let free = trade_cash_usd(&w, marks(), P);
        let ignoring_hold = trade_cash_usd(&Wallet { usd_held: 0.0, ..w.clone() }, marks(), P);
        assert!(
            free < ignoring_hold - 30.0,
            "free={free} ignoring={ignoring_hold} — the hold was not subtracted"
        );
        assert!(free <= w.usd_available() + 1e-9);
    }

    #[test]
    fn a_buy_rebalance_cannot_commit_held_dollars() {
        // BTC deeply underweight, but nearly all the cash is spoken for.
        let w = Wallet {
            usd: 100.0,
            usd_held: 95.0,
            btc: 0.0001,
            eth: 0.0,
            sol: 0.0,
        };
        if let Some(r) = btc_rebalance(&w, marks(), P) {
            assert_eq!(r.side, 1);
            assert!(
                r.qty * r.price <= w.usd_available() + 1e-9,
                "sized {} against {} free",
                r.qty * r.price,
                w.usd_available()
            );
        }
    }

    #[test]
    fn the_rebalance_that_just_ran_left_btc_in_band() {
        // Verified against the real fill: sold 0.00031493 XBT at 84594.20,
        // leaving 0.00078514. That must now read as in-band, or the bot
        // would sell again tomorrow on an account it already fixed.
        assert!(btc_rebalance(&live(), marks(), P).is_none());
        let drift = (btc_weight(&live(), marks(), P) - BTC_TARGET).abs();
        assert!(drift <= BTC_BAND, "drift={drift}");
    }
}

#[cfg(test)]
mod sleeves_off_tests {
    use super::*;

    /// The policy the bot actually runs since 2026-09-21.
    const P: Policy = Policy::LIVE;

    fn marks() -> Marks {
        Marks { btc: 84_594.0, eth: 2_692.0, sol: 115.0 }
    }

    /// The real Kraken wallet, with the 0.001 ETH left over from the last
    /// signal fill still sitting in it.
    fn live() -> Wallet {
        Wallet {
            usd: 120.4135,
            usd_held: 0.0,
            btc: 0.00078514,
            eth: 0.0010000036,
            sol: 0.0000093939,
        }
    }

    #[test]
    fn the_live_policy_really_is_off() {
        assert!(!P.trade_sleeve, "LIVE must have the trade sleeve disabled");
    }

    #[test]
    fn no_signal_can_reach_the_wallet() {
        let w = live();
        // A long entry, an exit with inventory to sell, and a short signal.
        for (opened, closed, side, live_qty) in [
            (true, false, Some(1i8), 0.0),
            (false, true, None, 0.01),
            (true, false, Some(-1i8), 0.0),
        ] {
            for (strategy, pair, mark) in [
                ("trendline_break", "SOLUSD", marks().sol),
                ("structure_filtered", "ETHUSD", marks().eth),
            ] {
                let action =
                    signal_action(strategy, pair, opened, closed, side, mark, &w, marks(), live_qty, P);
                assert!(
                    matches!(action, LiveAction::Skip(_)),
                    "{strategy} {pair} opened={opened} closed={closed} produced {action:?}"
                );
            }
        }
    }

    #[test]
    fn an_exit_does_not_liquidate_the_leftover_coins() {
        // The specific case worth naming: "stop trading these" must not mean
        // "sell these". A book carrying a position when the sleeve went off
        // will eventually print an exit, and that exit must do nothing.
        let w = Wallet { eth: 0.05, ..live() };
        let action = signal_action(
            "structure_filtered", "ETHUSD", false, true, None, marks().eth, &w, marks(), 0.05, P,
        );
        assert!(matches!(action, LiveAction::Skip(_)), "{action:?}");
    }

    #[test]
    fn there_is_no_trading_cash() {
        assert_eq!(trade_cash_usd(&live(), marks(), P), 0.0);
        assert_eq!(targets(&live(), marks(), P).trade_cash, 0.0);
    }

    #[test]
    fn frozen_coins_are_excluded_from_the_hold_not_counted_into_it() {
        // The trap: if ETH/SOL were counted into the hold base, the BTC
        // target would rise by 70% of their value and the bot would buy
        // bitcoin to offset coins it has decided not to trade.
        let w = live();
        let hold = hold_base_usd(&w, marks(), P);
        let eth_sol = w.eth * marks().eth + w.sol * marks().sol;
        assert!(
            (hold - (total_usd(&w, marks()) - eth_sol)).abs() < 1e-9,
            "hold base must be BTC + USD only"
        );
        let with_more_eth = Wallet { eth: w.eth + 0.05, ..w.clone() };
        assert!(
            (targets(&with_more_eth, marks(), P).btc - targets(&w, marks(), P).btc).abs() < 1e-6,
            "more ETH must not move the BTC target"
        );
    }

    #[test]
    fn the_whole_btc_usd_balance_becomes_the_seventy_thirty_hold() {
        // What this change does to the real account on its first bar: the
        // hold is no longer half the account, it is all of BTC+USD, so BTC
        // is well UNDER target and gets bought up.
        let w = live();
        let t = targets(&w, marks(), P);
        let btc_usd = w.btc * marks().btc;
        assert!(t.btc > btc_usd, "BTC should be under target, not over");
        let r = btc_rebalance(&w, marks(), P).expect("out of band");
        assert_eq!(r.side, 1, "should BUY bitcoin");
        let spend = r.qty * r.price;
        assert!((55.0..75.0).contains(&spend), "spend={spend}");
        // ...and that lands it on target.
        let mut after = w.clone();
        after.btc += r.qty;
        after.usd -= spend;
        assert!(btc_rebalance(&after, marks(), P).is_none(), "should be in band after");
    }

    #[test]
    fn switching_the_sleeve_back_on_changes_the_answer() {
        // Guards against the flag being wired somewhere it does not actually
        // matter: the two policies must disagree about this wallet.
        let w = live();
        assert_ne!(
            targets(&w, marks(), Policy::LIVE).btc,
            targets(&w, marks(), Policy::WITH_SLEEVES).btc
        );
        assert!(trade_cash_usd(&w, marks(), Policy::WITH_SLEEVES) > 0.0);
    }
}
