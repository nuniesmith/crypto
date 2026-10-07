//! Whole-account policy, chosen by the operator on 2026-10-05.
//!
//! # From two sleeves to one account
//!
//! Through 2026-10-04 this file ran two sleeves over one Kraken account: BTC
//! held against USD at a fixed split, and ETH+SOL trading a small slice on
//! the daily regime rule. BTC never traded on a signal; it only drifted back
//! to a band.
//!
//! The operator replaced that with ONE account targeting its TOTAL value
//! directly: **BTC 50%, ETH 25%, SOL 15%, cash 10%** at the bull floor, with
//! the SAME regime rule (`regime.rs`) now sizing all three coins instead of
//! two of them against a frozen BTC pile. A coin's EFFECTIVE target is its
//! base weight scaled by `regime::exposure` for the regime it is in: all of
//! it in a bull, half (`regime::CORE`) in a bear. Cash is whatever is left
//! once the three coins' effective targets are subtracted — 10% when every
//! coin is bull, up to 55% when all three are bear.
//!
//! `src/crypto/opt/portfolio.py` (branch `research/swing-tranches`) simulated
//! this account against a 5/25-rebalanced version of the same targets and a
//! buy-and-hold of the same split. The trend rule trading only on a flip beat
//! both: the 5/25 rebalancer added roughly 30 trades a year trimming ordinary
//! drift between coins and cash, for no extra return. **So there is no drift
//! rebalancing here.** A coin's holding is only ever touched by:
//!
//! 1. its own regime flipping (`gap_step`, below),
//! 2. the one-time move onto this policy (`State::policy_version`),
//! 3. a deposit being invested (`State::deposit_pending`, `main.rs`'s
//!    `deposit_top_up_pair` — a BUY-ONLY reuse of `gap_step` on pairs whose
//!    regime is already applied, so fresh cash flows into whatever it made
//!    underweight without ever selling a pair drift alone would leave
//!    untouched),
//! 4. the operator's explicit `rebalance` command.
//!
//! All four size the SAME way: the gap, in dollars, between what a coin is
//! worth now and its effective target, priced at the current touch so the
//! order can rest post-only without crossing. Between any of these events a
//! coin's weight drifts freely with its price, by design.
//!
//! # Tracking "is this pair done"
//!
//! `State::regime_applied` (pre-existing field) records the regime each pair
//! has been fully sized FOR — i.e. brought within one minimum order of its
//! effective target. `State::regime_bull` records the regime each pair is
//! actually IN, as of the last daily read. A pair has work outstanding
//! exactly when the two disagree:
//!
//! * an ordinary flip changes `regime_bull` and leaves `regime_applied` at
//!   the old value;
//! * the one-time policy move and the `rebalance` command instead CLEAR
//!   `regime_applied` outright, so every pair disagrees regardless of
//!   whether its regime actually changed.
//!
//! Either way, every work tick calls `gap_step` again for every disagreeing
//! pair. A post-only rejection or a partial fill simply leaves the gap open,
//! which the NEXT tick sizes fresh against the then-current wallet and
//! touch — so a multi-tick chase falls out of the ordinary flow rather than
//! needing its own state. Once `gap_step` reports `AtTarget`, the caller
//! writes `regime_applied` to match `regime_bull` and nothing acts on that
//! pair again until the next disagreement. That is the whole mechanism
//! behind "no drift rebalancing."
//!
//! # Stablecoins
//!
//! USDC and USDT are counted into the account total at their mark (~$1, no
//! ticker needed for that) but are not a target of anything: any balance at
//! or above Kraken's `ordermin` of 5 is simply sold to USD at the bid, as a
//! taker (`stable_sell`), every tick, until it is gone or under the minimum.

use std::collections::BTreeMap;

/// BTC's share of the account total at the bull floor (regime exposure 1.0).
pub const BASE_BTC: f64 = 0.50;
/// ETH's share of the account total at the bull floor.
pub const BASE_ETH: f64 = 0.25;
/// SOL's share of the account total at the bull floor.
pub const BASE_SOL: f64 = 0.15;
// Cash has no constant of its own: it is always `1 - sum(effective coin
// targets)`, which is how a flip that shrinks a coin's target grows cash
// automatically rather than needing its own rule.

/// Kraken `costmin` for every USD pair this bot trades (verified against
/// `/0/public/AssetPairs`, 2026-09-21; unchanged by this policy).
pub const COST_MIN_USD: f64 = 0.50;
/// Kraken `ordermin`s, same source. XBT 0.00005 / ETH 0.001 / SOL 0.06.
pub const MIN_BTC: f64 = 0.00005;
pub const MIN_ETH: f64 = 0.001;
pub const MIN_SOL: f64 = 0.06;
/// Kraken `ordermin` for the USDC/USDT-USD pairs, in units of the stablecoin.
pub const MIN_STABLE: f64 = 5.0;
/// A buy spends at most this fraction of `usd_available()`. Kraken reserves
/// the maker fee against a buy order's quote-currency notional at placement
/// time, so an order sized to exactly the free cash is refused every single
/// tick — this is what leaves a sliver behind for that reserve.
pub const FEE_RESERVE: f64 = 0.99;

/// Which wallet policy is in force.
///
/// A parameter rather than a compile-time constant so BOTH stay tested: the
/// armed policy is what runs, and `FROZEN` is a one-constant kill switch that
/// must already be proven to place nothing, not code nobody has exercised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Whether the account may place any order at all. Reading the wallet,
    /// the regime and the ledger still happens either way — only ACTING on
    /// them is gated — so monitoring and reporting never go dark.
    pub armed: bool,
}

impl Policy {
    /// What the bot actually runs.
    pub const LIVE: Policy = Policy { armed: true };
    /// Monitoring only: switch `main.rs`'s one call site from `LIVE` to this
    /// to freeze the account without touching any decision logic. Unused by
    /// the checked-in binary for exactly that reason — same as the old
    /// two-sleeve policy's `HOLD_ONLY` — but kept, and proven to place
    /// nothing, by `main::work_tick_tests::invariant_frozen_places_nothing`.
    #[allow(dead_code)]
    pub const FROZEN: Policy = Policy { armed: false };
}

/// The wallet, read from Kraken's `Balance`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Wallet {
    /// TOTAL USD as Kraken's `Balance` reports it — money reserved by an
    /// open order is still counted here, because it is still yours and the
    /// account is still worth it.
    pub usd: f64,
    /// USD an open buy order has already claimed. Not spendable twice.
    pub usd_held: f64,
    pub btc: f64,
    pub eth: f64,
    pub sol: f64,
    /// Counted into the account total at $1 each; never a rebalancing
    /// target — see `stable_sell`.
    pub usdc: f64,
    pub usdt: f64,
}

impl Wallet {
    /// USD that can actually be committed to a new order right now.
    ///
    /// Everything that VALUES the account uses `usd` (held money still
    /// counts); everything that SPENDS uses this.
    pub fn usd_available(&self) -> f64 {
        (self.usd - self.usd_held).max(0.0)
    }

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
                "USDC" => w.usdc += *amt,
                "USDT" => w.usdt += *amt,
                _ => {}
            }
        }
        w
    }

    /// Coins held of one of the three traded pairs; 0.0 for anything else.
    pub fn coin(&self, pair: &str) -> f64 {
        match pair {
            "XBTUSD" => self.btc,
            "ETHUSD" => self.eth,
            "SOLUSD" => self.sol,
            _ => 0.0,
        }
    }

    pub fn min_qty(pair: &str) -> f64 {
        match pair {
            "XBTUSD" => MIN_BTC,
            "ETHUSD" => MIN_ETH,
            "SOLUSD" => MIN_SOL,
            _ => f64::MAX,
        }
    }

    /// This pair's share of the account total at the bull floor; 0.0 for
    /// anything this policy does not target.
    pub fn base_weight(pair: &str) -> f64 {
        match pair {
            "XBTUSD" => BASE_BTC,
            "ETHUSD" => BASE_ETH,
            "SOLUSD" => BASE_SOL,
            _ => 0.0,
        }
    }
}

/// Last-trade price for each traded pair, used to VALUE the account.
///
/// Deliberately just the three coins: USD/USDC/USDT need no mark (counted at
/// $1), and order PRICING uses `Touch`, not this — see the module docs.
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
    /// must refuse to size anything rather than proceed with a partial view.
    pub fn complete(&self) -> bool {
        self.btc > 0.0 && self.eth > 0.0 && self.sol > 0.0
    }

    /// The price of one traded pair, 0.0 for one this struct does not mark.
    pub fn of(&self, pair: &str) -> f64 {
        match pair {
            "XBTUSD" => self.btc,
            "ETHUSD" => self.eth,
            "SOLUSD" => self.sol,
            _ => 0.0,
        }
    }
}

/// The best bid and ask for one pair, read fresh right before an order is
/// sized and placed off it — a buy rests at `bid`, a sell at `ask`
/// (`KrakenTicker::bid_price`/`ask_price`), so neither order can cross and
/// both stay post-only.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Touch {
    pub bid: f64,
    pub ask: f64,
}

impl Touch {
    /// A missing or crossed book must not size an order either — the same
    /// reasoning as `Marks::complete`, at the level of one pair.
    pub fn complete(&self) -> bool {
        self.bid.is_finite() && self.ask.is_finite() && self.bid > 0.0 && self.ask > 0.0
    }
}

pub fn floor_qty(qty: f64) -> f64 {
    if !qty.is_finite() || qty <= 0.0 {
        return 0.0;
    }
    (qty * 1e8).floor() / 1e8
}

/// Format a price to the pair's tick size for a limit order's `price` field.
pub fn limit_price(pair: &str, px: f64) -> String {
    match pair {
        "XBTUSD" => format!("{px:.1}"),
        "ETHUSD" | "SOLUSD" => format!("{px:.2}"),
        "USDCUSD" | "USDTUSD" => format!("{px:.4}"),
        _ => format!("{px:.4}"),
    }
}

/// The whole account marked to USD. Stablecoins count at $1 — the one place
/// this policy treats them as cash rather than as something to hold.
pub fn total_usd(w: &Wallet, m: Marks) -> f64 {
    w.usd
        + w.btc * m.btc.max(0.0)
        + w.eth * m.eth.max(0.0)
        + w.sol * m.sol.max(0.0)
        + w.usdc.max(0.0)
        + w.usdt.max(0.0)
}

/// A pair's effective target weight of the account total today: its base
/// weight scaled by the regime it is in — all of it in a bull, the
/// `regime::CORE` half in a bear.
pub fn effective_weight(pair: &str, bull: bool) -> f64 {
    Wallet::base_weight(pair) * crate::regime::exposure(bull)
}

/// Cash's target weight, given every pair's current regime. `None` if any
/// pair has not been read yet — there is no honest number until all three
/// are known, which `main.rs` reports as "regime not yet read" rather than
/// guessing.
pub fn cash_weight(regime_bull: &BTreeMap<String, bool>) -> Option<f64> {
    let mut invested = 0.0;
    for pair in crate::regime::PAIRS {
        let bull = *regime_bull.get(pair)?;
        invested += effective_weight(pair, bull);
    }
    Some((1.0 - invested).max(0.0))
}

/// One order this policy wants placed.
#[derive(Clone, Debug, PartialEq)]
pub struct Rebalance {
    pub pair: &'static str,
    pub side: i8,
    pub qty: f64,
    pub price: f64,
}

/// What to do about one pair's gap to its effective target right now.
#[derive(Clone, Debug, PartialEq)]
pub enum GapStep {
    /// Within one minimum order already — this pair's work is done, and the
    /// caller should record `regime_applied` as matching `regime_bull`.
    AtTarget,
    /// Outside the minimum, but under-funded (buy) or under-held (sell) by
    /// enough that no order can be placed yet. NOT done — retry next tick.
    NotYet,
    /// Marks or touch incomplete: the account or this pair cannot be priced.
    Unpriced,
    /// The order that closes (or narrows) the gap.
    Order(Rebalance),
}

/// Size one pair to `effective_weight(pair, bull) * total`, priced at the
/// touch so the result can be placed post-only.
///
/// `spendable` is the USD this call may commit to a buy — ordinarily
/// `w.usd_available()`, but when a tick makes more than one `gap_step` call
/// (gap-closing AND a deposit top-up can both want to buy in the same
/// tick), the caller threads a single running total through every call,
/// decremented as each buy is decided, so two pairs can never size against
/// the same dollars. The sell branch never reads this — a sell is capped
/// by what is held, not by cash.
///
/// See the module docs for when this is called: only while `regime_applied`
/// disagrees with `regime_bull` for this pair, which is also the fact that
/// keeps this safe to call every tick without rebalancing ordinary drift —
/// once `AtTarget`, the caller stops disagreeing and stops calling this.
pub fn gap_step(
    w: &Wallet,
    m: Marks,
    touch: Touch,
    pair: &str,
    bull: bool,
    spendable: f64,
) -> GapStep {
    if !m.complete() || !touch.complete() {
        return GapStep::Unpriced;
    }
    let pair: &'static str = match pair {
        "XBTUSD" => "XBTUSD",
        "ETHUSD" => "ETHUSD",
        "SOLUSD" => "SOLUSD",
        _ => return GapStep::AtTarget,
    };
    let total = total_usd(w, m);
    let target = effective_weight(pair, bull) * total;
    let have_usd = w.coin(pair) * m.of(pair);
    let delta = target - have_usd;
    let min_q = Wallet::min_qty(pair);
    if delta > 0.0 {
        let price = touch.bid;
        let spend = delta.min(spendable.max(0.0) * FEE_RESERVE);
        let q = floor_qty(spend / price);
        if q < min_q || spend < COST_MIN_USD {
            // Distinguish "already there" (the UNCAPPED gap is itself below
            // a minimum order — nothing could ever fill it) from "can't
            // afford it right now" (capped by cash) — only the second is
            // worth retrying on a later tick.
            let wanted = floor_qty(delta / price);
            return if wanted < min_q || delta < COST_MIN_USD {
                GapStep::AtTarget
            } else {
                GapStep::NotYet
            };
        }
        return GapStep::Order(Rebalance {
            pair,
            side: 1,
            qty: q,
            price,
        });
    }
    let price = touch.ask;
    let have = w.coin(pair);
    let q = floor_qty(((-delta) / price).min(have));
    if q < min_q || q * price < COST_MIN_USD {
        return GapStep::AtTarget;
    }
    GapStep::Order(Rebalance {
        pair,
        side: -1,
        qty: q,
        price,
    })
}

/// Sell a USDC or USDT balance to USD at the BID, if it is at or above
/// Kraken's ordermin of 5. This is the one order the bot lets take liquidity:
/// a stablecoin pair's price barely moves, so a post-only sell resting at the
/// ask sat behind a deep queue and never filled (live, 2026-10-07: every
/// hourly attempt expired unfilled), leaving the deposit uninvested. Crossing
/// costs the spread (about $0.0001) plus Kraken's fee on a few dollars. Sells the WHOLE balance (floored to
/// Kraken's precision) — there is no target to leave any of it at, unlike a
/// traded coin.
pub fn stable_sell(balance: f64, pair: &'static str, touch: Touch) -> Option<Rebalance> {
    if !touch.complete() || !balance.is_finite() || balance < MIN_STABLE {
        return None;
    }
    let q = floor_qty(balance);
    if q < MIN_STABLE || q * touch.bid < COST_MIN_USD {
        return None;
    }
    Some(Rebalance {
        pair,
        side: -1,
        qty: q,
        price: touch.bid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks() -> Marks {
        Marks {
            btc: 85_000.0,
            eth: 2_700.0,
            sol: 120.0,
        }
    }

    fn touch(bid: f64, ask: f64) -> Touch {
        Touch { bid, ask }
    }

    /// Roughly the account the operator described migrating FROM: BTC 56%,
    /// ETH 10%, SOL 10%, USD 24% of a total near $1,000.
    fn pre_migration_wallet() -> Wallet {
        Wallet {
            usd: 240.0,
            usd_held: 0.0,
            btc: 560.0 / marks().btc,
            eth: 100.0 / marks().eth,
            sol: 100.0 / marks().sol,
            usdc: 0.0,
            usdt: 0.0,
        }
    }

    #[test]
    fn parses_kraken_codes_including_stablecoins() {
        let w = Wallet::from_balances(&[
            ("ZUSD".into(), 10.0),
            ("XXBT".into(), 0.001),
            ("XETH".into(), 0.02),
            ("SOL".into(), 0.5),
            ("USDC".into(), 12.0),
            ("USDT".into(), 3.0),
            ("SOL.S".into(), 0.1),
        ]);
        assert!((w.usd - 10.0).abs() < 1e-9);
        assert!((w.btc - 0.001).abs() < 1e-12);
        assert!((w.eth - 0.02).abs() < 1e-12);
        assert!((w.sol - 0.6).abs() < 1e-12);
        assert!((w.usdc - 12.0).abs() < 1e-9);
        assert!((w.usdt - 3.0).abs() < 1e-9);
    }

    #[test]
    fn stablecoins_count_into_the_total_at_one_dollar() {
        let w = Wallet {
            usdc: 12.0,
            usdt: 3.0,
            ..Wallet::default()
        };
        assert!((total_usd(&w, Marks::default()) - 15.0).abs() < 1e-9);
    }

    #[test]
    fn effective_weights_sum_to_ninety_percent_in_an_all_bull_account() {
        let sum = effective_weight("XBTUSD", true)
            + effective_weight("ETHUSD", true)
            + effective_weight("SOLUSD", true);
        assert!((sum - 0.90).abs() < 1e-9, "sum={sum}");
        let mut bulls = BTreeMap::new();
        bulls.insert("XBTUSD".to_string(), true);
        bulls.insert("ETHUSD".to_string(), true);
        bulls.insert("SOLUSD".to_string(), true);
        assert!((cash_weight(&bulls).unwrap() - 0.10).abs() < 1e-9);
    }

    #[test]
    fn an_all_bear_account_wants_up_to_fifty_five_percent_cash() {
        let mut bears = BTreeMap::new();
        for p in crate::regime::PAIRS {
            bears.insert(p.to_string(), false);
        }
        assert!((cash_weight(&bears).unwrap() - 0.55).abs() < 1e-9);
    }

    #[test]
    fn cash_weight_is_unknown_until_every_pair_has_been_read() {
        let mut partial = BTreeMap::new();
        partial.insert("XBTUSD".to_string(), true);
        partial.insert("ETHUSD".to_string(), true);
        // SOLUSD missing.
        assert!(cash_weight(&partial).is_none());
    }

    #[test]
    fn migration_sells_btc_and_buys_eth_and_sol() {
        let w = pre_migration_wallet();
        let t = touch(marks().btc - 1.0, marks().btc + 1.0);
        match gap_step(&w, marks(), t, "XBTUSD", true, w.usd_available()) {
            GapStep::Order(r) => assert_eq!(r.side, -1, "56% must sell down toward 50%"),
            other => panic!("{other:?}"),
        }
        for pair in ["ETHUSD", "SOLUSD"] {
            let t = touch(marks().of(pair) - 0.1, marks().of(pair) + 0.1);
            match gap_step(&w, marks(), t, pair, true, w.usd_available()) {
                GapStep::Order(r) => {
                    assert_eq!(r.side, 1, "{pair} at 10% must buy up toward its target")
                }
                other => panic!("{pair}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_coin_already_at_its_bull_target_places_nothing() {
        let total = 1_000.0;
        let w = Wallet {
            usd: total * 0.10,
            btc: (total * BASE_BTC) / marks().btc,
            eth: (total * BASE_ETH) / marks().eth,
            sol: (total * BASE_SOL) / marks().sol,
            ..Wallet::default()
        };
        for pair in crate::regime::PAIRS {
            let t = touch(marks().of(pair), marks().of(pair));
            assert!(
                matches!(
                    gap_step(&w, marks(), t, pair, true, w.usd_available()),
                    GapStep::AtTarget
                ),
                "{pair}"
            );
        }
    }

    #[test]
    fn invariant_never_sells_a_coin_below_its_bear_core() {
        // ETH sitting exactly at its bear-core target already: a bear read
        // must not try to sell it any further.
        let total = 1_000.0;
        let core_usd = total * BASE_ETH * crate::regime::CORE;
        let w = Wallet {
            usd: total - core_usd,
            eth: core_usd / marks().eth,
            ..Wallet::default()
        };
        let t = touch(marks().eth - 0.1, marks().eth + 0.1);
        assert!(matches!(
            gap_step(&w, marks(), t, "ETHUSD", false, w.usd_available()),
            GapStep::AtTarget
        ));

        // And from ABOVE the core, a bear sell must land exactly at it, never
        // through it.
        let mut richer = w.clone();
        richer.eth *= 2.0;
        richer.usd -= core_usd;
        if let GapStep::Order(r) =
            gap_step(&richer, marks(), t, "ETHUSD", false, richer.usd_available())
        {
            assert_eq!(r.side, -1);
            let after = richer.eth - r.qty;
            assert!(
                after * marks().eth >= core_usd - COST_MIN_USD,
                "sold through the core: {after}"
            );
        }
    }

    #[test]
    fn invariant_never_spends_more_usd_than_is_available() {
        // Deeply underweight BTC, but almost all the cash is already held by
        // another open order.
        let w = Wallet {
            usd: 1_000.0,
            usd_held: 995.0,
            btc: 0.0,
            eth: 0.2,
            sol: 2.0,
            ..Wallet::default()
        };
        let t = touch(marks().btc - 1.0, marks().btc + 1.0);
        if let GapStep::Order(r) = gap_step(&w, marks(), t, "XBTUSD", true, w.usd_available()) {
            assert_eq!(r.side, 1);
            assert!(
                r.qty * r.price <= w.usd_available() + 1e-9,
                "spent more than usd_available()"
            );
        }
    }

    #[test]
    fn invariant_never_sells_more_than_is_held() {
        // SOL is effectively the WHOLE account, so a bear flip wants to sell
        // most of it. Valuation marks and the order touch come from two
        // SEPARATE fetches (`fetch_marks` vs `fetch_touch` in main.rs), so
        // they can disagree — here the ask is well below the valuation mark,
        // which is exactly the condition under which sizing off touch alone
        // (uncapped) would ask for more than the wallet actually holds.
        let w = Wallet {
            usd: 0.0,
            btc: 0.0,
            eth: 0.0,
            sol: 100.0,
            ..Wallet::default()
        };
        let t = touch(99.0, 100.0); // well below marks().sol == 120.0
        match gap_step(&w, marks(), t, "SOLUSD", false, w.usd_available()) {
            GapStep::Order(r) => {
                assert_eq!(r.side, -1);
                assert!(r.qty <= w.sol + 1e-12, "sold {} of {} held", r.qty, w.sol);
            }
            other => panic!("expected a sell, got {other:?}"),
        }
    }

    #[test]
    fn invariant_respects_ordermin_and_costmin() {
        // SOL already sitting almost exactly at its bear-core target: the
        // residual gap ($0.30) is both below SOL's ordermin (0.06 coin) and
        // below costmin ($0.50), so nothing is attempted even though it is
        // not EXACTLY on target.
        let w = Wallet {
            usd: 70.0,
            sol: 0.05,
            ..Wallet::default()
        };
        let t = touch(marks().sol - 0.01, marks().sol + 0.01);
        assert!(matches!(
            gap_step(&w, marks(), t, "SOLUSD", false, w.usd_available()),
            GapStep::AtTarget
        ));

        // costmin binding INDEPENDENTLY of ordermin: at real Kraken minimums
        // ordermin's dollar value always exceeds $0.50 for BTC/ETH/SOL at
        // realistic prices, so this needs an artificially cheap price to
        // reach — a $2 account wanting its full 15% SOL target is a gap of
        // $0.30: above SOL's 0.06-coin ordermin (q=0.30 at $1/SOL) but below
        // the $0.50 costmin.
        let cheap_sol = Marks {
            sol: 1.0,
            ..marks()
        };
        let w_tiny = Wallet {
            usd: 2.0,
            ..Wallet::default()
        };
        let t_tiny = touch(1.0, 1.0);
        assert!(
            matches!(
                gap_step(
                    &w_tiny,
                    cheap_sol,
                    t_tiny,
                    "SOLUSD",
                    true,
                    w_tiny.usd_available()
                ),
                GapStep::AtTarget
            ),
            "a $0.30 gap must be refused on costmin alone"
        );

        // A cost below $0.50 on a pair with a tiny ordermin (hypothetically)
        // is still refused — costmin binds independently of ordermin.
        assert!(
            stable_sell(4.99, "USDCUSD", touch(0.999, 1.0)).is_none(),
            "below ordermin 5"
        );
        assert!(
            stable_sell(5.0, "USDCUSD", touch(0.0, 0.0)).is_none(),
            "no touch, no sell"
        );
    }

    #[test]
    fn invariant_incomplete_marks_or_touch_place_nothing() {
        let w = pre_migration_wallet();
        let good_touch = touch(marks().btc, marks().btc);
        for bad in [
            Marks {
                btc: 0.0,
                ..marks()
            },
            Marks {
                eth: 0.0,
                ..marks()
            },
            Marks {
                sol: 0.0,
                ..marks()
            },
            Marks::default(),
        ] {
            assert!(
                matches!(
                    gap_step(&w, bad, good_touch, "XBTUSD", true, w.usd_available()),
                    GapStep::Unpriced
                ),
                "{bad:?}"
            );
        }
        for bad in [
            Touch {
                bid: 0.0,
                ask: 100.0,
            },
            Touch {
                bid: 100.0,
                ask: 0.0,
            },
            Touch::default(),
        ] {
            assert!(
                matches!(
                    gap_step(&w, marks(), bad, "XBTUSD", true, w.usd_available()),
                    GapStep::Unpriced
                ),
                "{bad:?}"
            );
            assert!(stable_sell(10.0, "USDCUSD", bad).is_none());
        }
    }

    #[test]
    fn stable_sell_takes_the_whole_balance_at_the_bid() {
        let r = stable_sell(12.345, "USDTUSD", touch(0.999, 1.0001)).expect("above ordermin");
        assert_eq!(r.pair, "USDTUSD");
        assert_eq!(r.side, -1);
        assert!((r.qty - floor_qty(12.345)).abs() < 1e-12);
        assert_eq!(
            r.price, 0.999,
            "a stablecoin sale crosses to the BID so it actually fills"
        );
    }

    #[test]
    fn invariant_a_buy_reserves_one_percent_of_free_cash_for_the_fee() {
        // BTC deeply underweight against a total dominated by ETH, so the
        // wanted buy ($135,500) is far more than the $1,000 free cash —
        // the buy is capped by cash either way, which is exactly the
        // condition under which the reserve must be what leaves a sliver
        // behind rather than spending every last cent.
        let w = Wallet {
            usd: 1_000.0,
            eth: 100.0, // $270,000 at marks().eth — dominates the total
            ..Wallet::default()
        };
        let t = touch(marks().btc, marks().btc);
        match gap_step(&w, marks(), t, "XBTUSD", true, w.usd_available()) {
            GapStep::Order(r) => {
                let spend = r.qty * r.price;
                assert!(
                    spend <= w.usd_available() * FEE_RESERVE + 1e-9,
                    "spend {spend} must respect the {FEE_RESERVE} reserve"
                );
                assert!(
                    spend < w.usd_available(),
                    "a buy must never spend the ENTIRE free balance"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn gap_step_buy_prices_at_the_bid_and_sell_at_the_ask() {
        let w = Wallet {
            usd: 10_000.0,
            ..Wallet::default()
        };
        let t = touch(100.0, 101.0);
        if let GapStep::Order(r) = gap_step(
            &w,
            Marks {
                btc: 100.5,
                eth: 2_700.0,
                sol: 120.0,
            },
            t,
            "XBTUSD",
            true,
            w.usd_available(),
        ) {
            assert_eq!(r.side, 1);
            assert_eq!(r.price, 100.0, "a buy prices at the BID touch");
        } else {
            panic!("expected a buy");
        }
        // BTC as effectively the WHOLE account, so even its bear-core target
        // is well below what is held, forcing a sell.
        let rich = Wallet {
            usd: 0.0,
            btc: 100.0,
            eth: 0.0,
            sol: 0.0,
            ..Wallet::default()
        };
        if let GapStep::Order(r) = gap_step(
            &rich,
            Marks {
                btc: 100.5,
                eth: 2_700.0,
                sol: 120.0,
            },
            t,
            "XBTUSD",
            false,
            rich.usd_available(),
        ) {
            assert_eq!(r.side, -1);
            assert_eq!(r.price, 101.0, "a sell prices at the ASK touch");
        } else {
            panic!("expected a sell");
        }
    }

    #[test]
    fn floor_qty_matches_krakens_eight_decimal_grid() {
        assert_eq!(floor_qty(0.123456789), 0.12345678);
        assert_eq!(floor_qty(f64::NAN), 0.0);
        assert_eq!(floor_qty(-1.0), 0.0);
        assert_eq!(floor_qty(0.0), 0.0);
    }
}
