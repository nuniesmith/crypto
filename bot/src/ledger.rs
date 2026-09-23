//! What the LIVE sleeve actually did, in real Kraken dollars.
//!
//! # Why this is a second set of books
//!
//! The three books in `paper.rs` are a $1,000-per-book SIMULATION. They exist
//! to answer one question — *does this signal have an edge at a size worth
//! trading?* — and the answer is only meaningful if every book always takes
//! every signal at the same notional.
//!
//! The live sleeve is 20% of a small real account (`alloc::CORE_SHARE`), so
//! the very same ETH signal on 2026-09-22 was a $1,000 paper position and a
//! $2.70 real one at the same instant. Those are two different facts. They
//! were being stored in one place, and the result described neither:
//! `LiveAction::Buy` overwrote the paper `qty` with the live order size while
//! the entry fee stayed charged on $1,000 of notional, so the round trip
//! printed
//!
//! ```text
//! eth_1h_sf  entry 2696.09 -> exit 2748.29   (ETH +1.9%, the signal was RIGHT)
//! gross  +$0.05      <- on 0.001 ETH, the live fill
//! fees   -$0.16      <- a fee computed on $1,000 of notional
//! pnl    -$0.11
//! ```
//!
//! The simulation's answer to that trade is +$14.71 on $1,000. Kraken's answer
//! is about +4 cents on $2.70 after roughly 1.4 cents of real fee. `-$0.11` is
//! neither of them, and no amount of staring at it recovers either.
//!
//! So the split is: `paper.rs` keeps the simulation and never looks at a
//! wallet again, and everything that really happened lands here, built from
//! `vol_exec` / `cost` / `fee` exactly as `ClosedOrders` reports them.
//!
//! # What this ledger deliberately is not
//!
//! It is not a restatement of history. The fills before it started were never
//! recorded — the bot only learned to read `vol_exec` on 2026-09-21 and never
//! read `cost` or `fee` at all — so there is nothing to restate from. The
//! ledger begins on a stated date (`LiveLedger::since`) and says so.

use serde::{Deserialize, Serialize};

/// Volume below which a residue is arithmetic, not a coin.
///
/// Kraken quotes volumes to 8 decimals and `alloc::floor_qty` floors to that
/// same grid, so anything smaller came out of an f64 subtraction rather than
/// off the exchange. Left in place it prints forever as a "position" worth a
/// billionth of a cent, and every "is the sleeve flat?" check has to know
/// about it.
const DUST: f64 = 1e-8;

/// How many fills the state file keeps.
///
/// This list is also the duplicate guard, so it has to be long enough that a
/// txid cannot fall off the end and then be applied a second time. Settled
/// orders are dropped from `State::pending_orders` immediately, so a
/// re-settle should already be impossible; Kraken's `ClosedOrders` page is 50
/// and this bot places on the order of one order a day. 100 is months of
/// headroom in a state file that is rewritten every 60 seconds.
const MAX_FILLS: usize = 100;

/// Which sleeve a fill belongs to.
///
/// The two are not comparable and must never be summed into one P&L line: the
/// trade sleeve is trying to earn something and can be judged on whether it
/// does, while the hold sleeve's fills are rebalances — moving BTC back to a
/// target mix is not a bet and "profit" on it is meaningless.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sleeve {
    /// ETH + SOL, spent by the 1h books. The 20% the question is about.
    Trade,
    /// BTC + USD. Fills here are rebalances, not bets.
    Hold,
}

impl Sleeve {
    /// A fill belongs to the trade sleeve exactly when a BOOK placed it.
    ///
    /// `PendingOrder::book` is already the discriminator the settler routes
    /// on — the BTC rebalance owns no book and carries `None` — so reusing it
    /// means the ledger cannot disagree with the settler about whose order
    /// something was.
    pub fn of(book: Option<&str>) -> Self {
        match book {
            Some(_) => Sleeve::Trade,
            None => Sleeve::Hold,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Sleeve::Trade => "trade",
            Sleeve::Hold => "hold",
        }
    }
}

/// What one order actually did at Kraken, in Kraken's own fields.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Execution {
    /// `vol_exec` — base units that actually changed hands.
    pub vol_exec: f64,
    /// `cost` — GROSS quote volume. Kraken reports the fee in a separate
    /// field, so a buy really costs `cost + fee` and a sell really pays
    /// `cost - fee`. Reading `cost` as net understates every buy and
    /// overstates every sell, which is exactly the error that would make a
    /// losing sleeve look flat.
    pub cost: f64,
    /// `fee` — the real Kraken fee in quote currency. This is the number the
    /// whole exercise turns on: the books were charging a modelled 0.23% of
    /// $1,000 against a position worth $2.70.
    pub fee: f64,
    /// `opentm`, when Kraken supplies it. That is when the order OPENED, not
    /// when it filled; for a limit order reaped after ten minutes the two are
    /// within the same bar, and the settling cycle's clock is the fallback.
    pub at: Option<i64>,
}

/// One real execution, attributed to the sleeve and book that caused it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    pub txid: String,
    pub ts: i64,
    pub pair: String,
    /// +1 bought, -1 sold.
    pub side: i8,
    pub sleeve: Sleeve,
    /// Which book placed it; `None` for the BTC rebalance.
    #[serde(default)]
    pub book: Option<String>,
    /// `vol_exec`.
    pub qty: f64,
    /// Gross quote volume, fee excluded.
    pub cost: f64,
    /// Real Kraken fee.
    pub fee: f64,
}

/// What happened to a fill offered to the ledger.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Recorded {
    /// Applied to the pair's running position and P&L.
    Applied,
    /// Already on the ledger. Real money must be counted exactly once.
    Duplicate,
    /// Not a usable fill. The reason is for the journal, not for control flow.
    Rejected(&'static str),
}

/// One pair's real position and real P&L within one sleeve.
///
/// Average cost, not FIFO. Spot lots of the same coin are fungible, the two
/// agree exactly on total realized P&L once the position is flat, and average
/// cost needs two numbers where FIFO needs a lot list that grows without
/// bound inside a state file rewritten every sixty seconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PairLedger {
    pub sleeve: Sleeve,
    pub pair: String,
    /// Coins this ledger has seen bought (or adopted) and not yet seen sold.
    pub qty: f64,
    /// What those coins cost, Kraken's buy fee included. Capitalising the fee
    /// here means the sell leg does not have to remember which buys it is
    /// closing in order to charge it.
    pub basis_usd: f64,
    /// Closed-out P&L, real fees deducted on both legs.
    pub realized_usd: f64,
    /// Real Kraken fees, both legs, whether or not the leg closed anything.
    pub fees_usd: f64,
    pub bought_usd: f64,
    pub sold_usd: f64,
    pub buys: u32,
    pub sells: u32,
    /// Coins taken onto the books at a mark instead of bought — see
    /// `LiveLedger::adopt`. Part of `basis_usd` that was never paid.
    pub adopted_qty: f64,
    pub adopted_usd: f64,
    /// Coins sold that this ledger never saw arrive, and what they fetched.
    ///
    /// Kept OUT of `realized_usd` on purpose. Proceeds with no basis are not
    /// profit, and quietly adding them would turn a wallet the operator
    /// topped up by hand into a winning strategy.
    pub untracked_sold_qty: f64,
    pub untracked_proceeds_usd: f64,
}

impl PairLedger {
    fn new(sleeve: Sleeve, pair: &str) -> Self {
        Self {
            sleeve,
            pair: pair.to_string(),
            qty: 0.0,
            basis_usd: 0.0,
            realized_usd: 0.0,
            fees_usd: 0.0,
            bought_usd: 0.0,
            sold_usd: 0.0,
            buys: 0,
            sells: 0,
            adopted_qty: 0.0,
            adopted_usd: 0.0,
            untracked_sold_qty: 0.0,
            untracked_proceeds_usd: 0.0,
        }
    }

    /// Inventory is gone once it is below the exchange's own precision.
    pub fn is_flat(&self) -> bool {
        self.qty <= DUST
    }

    /// Mark-to-market of the inventory. A mark of zero means "no price right
    /// now" (`fetch_marks` yields an empty vec on an outage), NOT "worthless".
    pub fn inventory_usd(&self, mark: f64) -> Option<f64> {
        if self.is_flat() {
            return Some(0.0);
        }
        if mark > 0.0 && mark.is_finite() {
            Some(self.qty * mark)
        } else {
            None
        }
    }

    fn apply(&mut self, f: &Fill) {
        // The fee left the account whichever leg this was and whether or not
        // anything was closed out, so it is added before any of the branching.
        self.fees_usd += f.fee;
        if f.side > 0 {
            self.qty += f.qty;
            self.basis_usd += f.cost + f.fee;
            self.bought_usd += f.cost;
            self.buys += 1;
            return;
        }
        self.sells += 1;
        self.sold_usd += f.cost;
        // Only the part of the sale this ledger has a basis for can produce a
        // P&L number. `record` guarantees `f.qty > 0`, so the share is safe.
        let tracked = f.qty.min(self.qty);
        let share = tracked / f.qty;
        let basis_out = if self.qty > 0.0 {
            self.basis_usd * (tracked / self.qty)
        } else {
            0.0
        };
        self.realized_usd += (f.cost - f.fee) * share - basis_out;
        self.qty -= tracked;
        self.basis_usd -= basis_out;
        let untracked = f.qty - tracked;
        if untracked > 0.0 {
            self.untracked_sold_qty += untracked;
            self.untracked_proceeds_usd += (f.cost - f.fee) * (1.0 - share);
        }
        // Selling out completely leaves an f64 residue behind — 0.01 minus
        // 0.003 minus 0.007 is not zero in binary — and a residual basis with
        // no coins behind it reads as a permanent unrealized loss.
        if self.is_flat() {
            self.qty = 0.0;
            self.basis_usd = 0.0;
        }
    }
}

/// Every real fill the bot has caused since `since`, and what they add up to.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveLedger {
    /// UTC date `YYYY-MM-DD` this ledger began. Nothing before it is
    /// represented here and nothing here claims to be a complete account
    /// history — see the module docs.
    #[serde(default)]
    pub since: String,
    #[serde(default)]
    pub pairs: Vec<PairLedger>,
    /// The most recent `MAX_FILLS` fills, oldest first. Also the duplicate
    /// guard, which is why it is a list and not a counter.
    #[serde(default)]
    pub fills: Vec<Fill>,
}

impl LiveLedger {
    pub fn starting(today: &str) -> Self {
        Self {
            since: today.to_string(),
            ..Self::default()
        }
    }

    fn slot(&mut self, sleeve: Sleeve, pair: &str) -> &mut PairLedger {
        if let Some(i) = self
            .pairs
            .iter()
            .position(|p| p.sleeve == sleeve && p.pair == pair)
        {
            return &mut self.pairs[i];
        }
        self.pairs.push(PairLedger::new(sleeve, pair));
        self.pairs.last_mut().expect("just pushed")
    }

    pub fn get(&self, sleeve: Sleeve, pair: &str) -> Option<&PairLedger> {
        self.pairs
            .iter()
            .find(|p| p.sleeve == sleeve && p.pair == pair)
    }

    /// Put one real execution on the books.
    ///
    /// Everything that is not a usable fill is rejected rather than coerced.
    /// A zero-volume close is the ordinary case — a limit order reaped after
    /// ten minutes without trading — and recording it would add a row saying
    /// nothing while making the fill list churn.
    pub fn record(&mut self, f: Fill) -> Recorded {
        if f.txid.trim().is_empty() {
            return Recorded::Rejected("no txid");
        }
        if f.pair.trim().is_empty() {
            return Recorded::Rejected("no pair");
        }
        if f.side == 0 {
            return Recorded::Rejected("no side");
        }
        // NaN compares false against every bound, so each of these has to be
        // tested for finiteness explicitly rather than trusted to fail a
        // comparison. A single NaN here poisons realized P&L permanently:
        // there is no later fill that can bring a NaN total back.
        if !f.qty.is_finite() || !f.cost.is_finite() || !f.fee.is_finite() {
            return Recorded::Rejected("non-numeric execution");
        }
        if f.qty <= DUST {
            return Recorded::Rejected("executed nothing");
        }
        if f.cost < 0.0 || f.fee < 0.0 {
            return Recorded::Rejected("negative cost or fee");
        }
        if self.fills.iter().any(|x| x.txid == f.txid) {
            return Recorded::Duplicate;
        }
        self.slot(f.sleeve, &f.pair).apply(&f);
        self.fills.push(f);
        if self.fills.len() > MAX_FILLS {
            let drop = self.fills.len() - MAX_FILLS;
            self.fills.drain(0..drop);
        }
        Recorded::Applied
    }

    /// Take coins the wallet already held onto the books at today's mark.
    ///
    /// A long signal ADOPTS existing inventory instead of buying it whenever
    /// the wallet already holds enough (`alloc::signal_action`), so without
    /// this the ledger would later watch a sell of coins it never saw arrive
    /// and would have to either invent a profit or discard the trade. Marking
    /// them in at the adoption price is the honest third answer, and it says
    /// plainly what this ledger measures: the sleeve from the moment it took
    /// charge of the coins, not from whenever they were originally bought.
    ///
    /// Only the SHORTFALL is adopted, so running this on every entry is
    /// harmless — the second call finds the coins already on the books and
    /// does nothing. Inventory the ledger thinks it has but the wallet does
    /// not is deliberately left alone: a withdrawal is not a trade, and
    /// realizing one would invent a loss the sleeve never took.
    pub fn adopt(&mut self, sleeve: Sleeve, pair: &str, wallet_qty: f64, mark: f64) -> Recorded {
        if pair.trim().is_empty() {
            return Recorded::Rejected("no pair");
        }
        if !wallet_qty.is_finite() || !mark.is_finite() {
            return Recorded::Rejected("non-numeric adoption");
        }
        if mark <= 0.0 {
            // Adopting at a zero mark books the coins in free, which would
            // show up later as pure profit on the first sale.
            return Recorded::Rejected("no mark to adopt at");
        }
        let slot = self.slot(sleeve, pair);
        let missing = wallet_qty - slot.qty;
        if missing <= DUST {
            return Recorded::Duplicate;
        }
        slot.qty += missing;
        slot.basis_usd += missing * mark;
        slot.adopted_qty += missing;
        slot.adopted_usd += missing * mark;
        Recorded::Applied
    }

    pub fn totals(&self, sleeve: Sleeve, marks: &[(String, f64)]) -> Totals {
        let mut t = Totals::default();
        for p in self.pairs.iter().filter(|p| p.sleeve == sleeve) {
            t.realized_usd += p.realized_usd;
            t.fees_usd += p.fees_usd;
            t.basis_usd += p.basis_usd;
            t.adopted_usd += p.adopted_usd;
            t.untracked_proceeds_usd += p.untracked_proceeds_usd;
            t.fills += p.buys + p.sells;
            let mark = marks
                .iter()
                .find(|(k, _)| k == &p.pair)
                .map(|(_, px)| *px)
                .unwrap_or(0.0);
            match p.inventory_usd(mark) {
                Some(usd) => {
                    t.inventory_usd += usd;
                    t.unrealized_usd += usd - p.basis_usd;
                }
                // No price for coins we are holding. Counting them at zero
                // would print the entire basis as a loss the moment Kraken's
                // ticker hiccups, so the inventory is left out and the reader
                // is told the total is partial.
                None => t.unpriced += 1,
            }
        }
        t
    }
}

/// One sleeve's answer to "what did it make or lose, net of real fees?".
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    /// Closed round trips, real fees already deducted on both legs.
    pub realized_usd: f64,
    /// Open inventory versus what it cost. Zero when flat.
    pub unrealized_usd: f64,
    /// Real Kraken fees paid, for scale against the P&L above.
    pub fees_usd: f64,
    pub inventory_usd: f64,
    pub basis_usd: f64,
    /// Of `basis_usd`, how much was marked in rather than paid for.
    pub adopted_usd: f64,
    /// Proceeds from coins with no basis, excluded from `realized_usd`.
    pub untracked_proceeds_usd: f64,
    /// Pairs holding inventory that could not be priced. Non-zero means
    /// `unrealized_usd` and `net_usd` are incomplete, not that they are zero.
    pub unpriced: u32,
    pub fills: u32,
}

impl Totals {
    /// The bottom line: what the sleeve has made or lost since the ledger
    /// began, net of every real Kraken fee on both legs.
    pub fn net_usd(&self) -> f64 {
        self.realized_usd + self.unrealized_usd
    }

    /// Whether every number above is backed by a price.
    pub fn complete(&self) -> bool {
        self.unpriced == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buy(txid: &str, pair: &str, qty: f64, cost: f64, fee: f64) -> Fill {
        Fill {
            txid: txid.into(),
            ts: 1_790_000_000,
            pair: pair.into(),
            side: 1,
            sleeve: Sleeve::Trade,
            book: Some("eth_1h_sf".into()),
            qty,
            cost,
            fee,
        }
    }

    fn sell(txid: &str, pair: &str, qty: f64, cost: f64, fee: f64) -> Fill {
        Fill {
            side: -1,
            ..buy(txid, pair, qty, cost, fee)
        }
    }

    fn marks() -> Vec<(String, f64)> {
        vec![
            ("ETHUSD".into(), 2_748.29),
            ("SOLUSD".into(), 115.0),
            ("XBTUSD".into(), 84_594.0),
        ]
    }

    #[test]
    fn a_round_trip_nets_out_to_gross_minus_both_real_fees() {
        // THE trade this whole module exists for, at Kraken's real scale:
        // 0.001 ETH bought at 2696.09 and sold at 2748.29, with the fee
        // Kraken actually charges on $2.70 rather than on $1,000.
        let mut l = LiveLedger::starting("2026-09-23");
        assert_eq!(l.record(buy("B1", "ETHUSD", 0.001, 2.69609, 0.00701)), Recorded::Applied);
        assert_eq!(l.record(sell("S1", "ETHUSD", 0.001, 2.74829, 0.00714)), Recorded::Applied);

        let t = l.totals(Sleeve::Trade, &marks());
        let gross = 2.74829 - 2.69609;
        let fees = 0.00701 + 0.00714;
        assert!((t.realized_usd - (gross - fees)).abs() < 1e-12, "{t:?}");
        assert!((t.fees_usd - fees).abs() < 1e-12);
        assert_eq!(t.unrealized_usd, 0.0, "flat means nothing left to mark");
        // And the answer is a few cents of PROFIT, where the mixed book
        // reported -$0.11 on the identical fills.
        assert!(t.net_usd() > 0.0, "net {}", t.net_usd());
        assert!(t.net_usd() < 0.05);
    }

    #[test]
    fn the_fee_is_taken_off_both_legs_and_not_one() {
        // Deducting only the sell fee would flatter every round trip by the
        // buy fee, which on this sleeve is most of the cost.
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "ETHUSD", 1.0, 100.0, 1.0));
        l.record(sell("S1", "ETHUSD", 1.0, 100.0, 1.0));
        let t = l.totals(Sleeve::Trade, &marks());
        assert!((t.realized_usd - -2.0).abs() < 1e-12, "realized {}", t.realized_usd);
    }

    #[test]
    fn a_flat_round_trip_at_the_same_price_loses_exactly_the_fees() {
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "SOLUSD", 2.0, 230.0, 0.6));
        l.record(sell("S1", "SOLUSD", 2.0, 230.0, 0.6));
        let t = l.totals(Sleeve::Trade, &marks());
        assert!((t.net_usd() + t.fees_usd).abs() < 1e-12, "{t:?}");
    }

    #[test]
    fn an_open_position_is_marked_against_what_it_cost_including_the_buy_fee() {
        // Buying is not free: a position bought at the mark is already down
        // by the fee. Marking against `cost` alone would report it as flat.
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "ETHUSD", 0.01, 27.4829, 0.07));
        let t = l.totals(Sleeve::Trade, &marks());
        assert!((t.inventory_usd - 27.4829).abs() < 1e-9);
        assert!((t.unrealized_usd - -0.07).abs() < 1e-9, "unrealized {}", t.unrealized_usd);
        assert_eq!(t.realized_usd, 0.0, "nothing has been closed");
    }

    #[test]
    fn unrealized_follows_the_mark() {
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "SOLUSD", 1.0, 100.0, 0.0));
        let up = l.totals(Sleeve::Trade, &[("SOLUSD".into(), 120.0)]);
        let down = l.totals(Sleeve::Trade, &[("SOLUSD".into(), 80.0)]);
        assert!((up.unrealized_usd - 20.0).abs() < 1e-9);
        assert!((down.unrealized_usd - -20.0).abs() < 1e-9);
    }

    #[test]
    fn a_missing_mark_is_reported_as_unpriced_not_as_a_total_loss() {
        // `fetch_marks` returns an EMPTY vec on any error, so an outage would
        // otherwise print the whole basis as a loss and a healthy sleeve as a
        // catastrophe — on a day when nothing traded at all.
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "ETHUSD", 0.01, 27.48, 0.07));
        let t = l.totals(Sleeve::Trade, &[]);
        assert_eq!(t.unpriced, 1);
        assert!(!t.complete());
        assert_eq!(t.unrealized_usd, 0.0, "must not book a fake loss");
        assert_eq!(t.inventory_usd, 0.0);
    }

    #[test]
    fn a_flat_pair_is_complete_even_with_no_mark() {
        // Nothing to price means nothing missing. Otherwise every report
        // would carry an "incomplete" warning forever after the first exit.
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "ETHUSD", 0.01, 27.48, 0.07));
        l.record(sell("S1", "ETHUSD", 0.01, 27.60, 0.07));
        let t = l.totals(Sleeve::Trade, &[]);
        assert_eq!(t.unpriced, 0);
        assert!(t.complete());
    }

    #[test]
    fn selling_half_realizes_half_the_basis() {
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "SOLUSD", 2.0, 200.0, 0.0));
        l.record(sell("S1", "SOLUSD", 1.0, 120.0, 0.0));
        let p = l.get(Sleeve::Trade, "SOLUSD").unwrap();
        assert!((p.realized_usd - 20.0).abs() < 1e-9, "realized {}", p.realized_usd);
        assert!((p.qty - 1.0).abs() < 1e-9);
        assert!((p.basis_usd - 100.0).abs() < 1e-9, "basis {}", p.basis_usd);
    }

    #[test]
    fn average_cost_survives_a_second_buy_at_a_different_price() {
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "SOLUSD", 1.0, 100.0, 0.0));
        l.record(buy("B2", "SOLUSD", 1.0, 200.0, 0.0));
        l.record(sell("S1", "SOLUSD", 1.0, 150.0, 0.0));
        let p = l.get(Sleeve::Trade, "SOLUSD").unwrap();
        // Average cost is 150, so this sale is exactly flat.
        assert!(p.realized_usd.abs() < 1e-9, "realized {}", p.realized_usd);
        assert!((p.basis_usd - 150.0).abs() < 1e-9);
    }

    #[test]
    fn selling_all_of_it_leaves_no_residue_behind() {
        // 1.0 minus 0.7 minus 0.3 is 5.6e-17 in binary, not zero. A leftover
        // basis with no coins under it reads as a permanent unrealized loss,
        // and a leftover qty makes a flat sleeve print an open position
        // forever. The split is chosen for that residue specifically:
        // 0.01/0.003/0.007 happens to come out exact and proves nothing.
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "SOLUSD", 1.0, 115.0, 0.3));
        l.record(sell("S1", "SOLUSD", 0.7, 80.5, 0.21));
        l.record(sell("S2", "SOLUSD", 0.3, 34.5, 0.09));
        let p = l.get(Sleeve::Trade, "SOLUSD").unwrap();
        assert!(p.is_flat());
        assert_eq!(p.qty, 0.0, "exactly zero, not nearly");
        assert_eq!(p.basis_usd, 0.0, "no basis without coins under it");
        assert_eq!(l.totals(Sleeve::Trade, &marks()).unrealized_usd, 0.0);
    }

    #[test]
    fn the_net_is_what_is_closed_plus_what_is_still_open() {
        // The headline number. Subtracting the open leg instead of adding it
        // reports a sleeve that is up on paper as though it were down.
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "SOLUSD", 2.0, 200.0, 0.0));
        l.record(sell("S1", "SOLUSD", 1.0, 120.0, 0.0));
        let t = l.totals(Sleeve::Trade, &[("SOLUSD".into(), 130.0)]);
        assert!((t.realized_usd - 20.0).abs() < 1e-9, "{t:?}");
        assert!((t.unrealized_usd - 30.0).abs() < 1e-9, "{t:?}");
        assert!((t.net_usd() - 50.0).abs() < 1e-9, "net {}", t.net_usd());
    }

    #[test]
    fn a_sale_with_no_basis_is_quarantined_not_counted_as_profit() {
        // The operator tops the wallet up by hand, a book exits, and the sale
        // is of coins this ledger never watched arrive. Their proceeds are
        // not profit; booking them would turn a deposit into an edge.
        let mut l = LiveLedger::starting("2026-09-23");
        assert_eq!(l.record(sell("S1", "SOLUSD", 1.0, 115.0, 0.3)), Recorded::Applied);
        let t = l.totals(Sleeve::Trade, &marks());
        assert_eq!(t.realized_usd, 0.0, "no basis means no P&L");
        assert!((t.untracked_proceeds_usd - 114.7).abs() < 1e-9);
        assert!((t.fees_usd - 0.3).abs() < 1e-12, "the fee was still real");
    }

    #[test]
    fn a_sale_larger_than_the_position_splits_at_the_boundary() {
        // Half the sale closes a real position and half has no basis. The
        // first half must produce a P&L and the second must not.
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "SOLUSD", 1.0, 100.0, 0.0));
        l.record(sell("S1", "SOLUSD", 2.0, 240.0, 0.0));
        let p = l.get(Sleeve::Trade, "SOLUSD").unwrap();
        assert!((p.realized_usd - 20.0).abs() < 1e-9, "realized {}", p.realized_usd);
        assert!((p.untracked_sold_qty - 1.0).abs() < 1e-9);
        assert!((p.untracked_proceeds_usd - 120.0).abs() < 1e-9);
        assert!(p.is_flat());
    }

    #[test]
    fn adopting_wallet_inventory_gives_it_a_basis_at_todays_mark() {
        // Without this, the sale below would land in the untracked bucket and
        // the sleeve's move on coins it was actually managing would vanish.
        let mut l = LiveLedger::starting("2026-09-23");
        assert_eq!(l.adopt(Sleeve::Trade, "ETHUSD", 0.001, 2_696.09), Recorded::Applied);
        l.record(sell("S1", "ETHUSD", 0.001, 2.74829, 0.00714));
        let p = l.get(Sleeve::Trade, "ETHUSD").unwrap();
        assert_eq!(p.untracked_sold_qty, 0.0, "adopted coins have a basis");
        let expected = 2.74829 - 0.00714 - 2.69609;
        assert!((p.realized_usd - expected).abs() < 1e-9, "realized {}", p.realized_usd);
    }

    #[test]
    fn adopting_twice_does_not_double_the_inventory() {
        // `LiveAction::Adopt` fires on every entry that finds coins already
        // in the wallet, so this runs again and again on the same pile.
        let mut l = LiveLedger::starting("2026-09-23");
        assert_eq!(l.adopt(Sleeve::Trade, "SOLUSD", 0.2, 115.0), Recorded::Applied);
        assert_eq!(l.adopt(Sleeve::Trade, "SOLUSD", 0.2, 130.0), Recorded::Duplicate);
        let p = l.get(Sleeve::Trade, "SOLUSD").unwrap();
        assert!((p.qty - 0.2).abs() < 1e-12);
        assert!((p.basis_usd - 23.0).abs() < 1e-9, "basis {}", p.basis_usd);
    }

    #[test]
    fn adopting_only_books_the_coins_the_ledger_is_missing() {
        // The wallet grew by a hand deposit between entries. Only the new
        // coins get a mark-in basis; the ones already tracked keep theirs.
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "SOLUSD", 1.0, 100.0, 0.0));
        assert_eq!(l.adopt(Sleeve::Trade, "SOLUSD", 3.0, 115.0), Recorded::Applied);
        let p = l.get(Sleeve::Trade, "SOLUSD").unwrap();
        assert!((p.qty - 3.0).abs() < 1e-12);
        assert!((p.basis_usd - 330.0).abs() < 1e-9, "basis {}", p.basis_usd);
        assert!((p.adopted_qty - 2.0).abs() < 1e-12);
        assert!((p.adopted_usd - 230.0).abs() < 1e-9);
    }

    #[test]
    fn a_wallet_smaller_than_the_ledger_is_left_alone() {
        // A withdrawal is not a trade. Shrinking the position here would
        // realize a P&L the sleeve never took.
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "SOLUSD", 2.0, 200.0, 0.0));
        assert_eq!(l.adopt(Sleeve::Trade, "SOLUSD", 0.5, 115.0), Recorded::Duplicate);
        let p = l.get(Sleeve::Trade, "SOLUSD").unwrap();
        assert!((p.qty - 2.0).abs() < 1e-12);
        assert!((p.basis_usd - 200.0).abs() < 1e-9);
    }

    #[test]
    fn adopting_at_no_mark_is_refused_rather_than_booked_free() {
        // A zero mark is what a ticker outage looks like. Coins booked in at
        // zero would show up as pure profit on the first sale.
        let mut l = LiveLedger::starting("2026-09-23");
        assert_eq!(
            l.adopt(Sleeve::Trade, "ETHUSD", 0.01, 0.0),
            Recorded::Rejected("no mark to adopt at")
        );
        assert!(l.get(Sleeve::Trade, "ETHUSD").is_none_or(|p| p.qty == 0.0));
    }

    #[test]
    fn the_same_fill_is_never_counted_twice() {
        // `ClosedOrders` returns the whole recent page on every cycle. One
        // double-count is a permanent error in a number nothing recomputes.
        let mut l = LiveLedger::starting("2026-09-23");
        assert_eq!(l.record(buy("B1", "ETHUSD", 0.001, 2.69, 0.007)), Recorded::Applied);
        assert_eq!(l.record(buy("B1", "ETHUSD", 0.001, 2.69, 0.007)), Recorded::Duplicate);
        let p = l.get(Sleeve::Trade, "ETHUSD").unwrap();
        assert_eq!(p.buys, 1);
        assert!((p.qty - 0.001).abs() < 1e-12);
        assert_eq!(l.fills.len(), 1);
    }

    #[test]
    fn a_close_that_executed_nothing_is_not_a_fill() {
        // The ordinary case: a limit order reaped after ten minutes without
        // trading. It is a real event for the settler and a non-event here.
        let mut l = LiveLedger::starting("2026-09-23");
        assert_eq!(
            l.record(buy("B1", "ETHUSD", 0.0, 0.0, 0.0)),
            Recorded::Rejected("executed nothing")
        );
        assert!(l.fills.is_empty());
        assert!(l.pairs.is_empty(), "no pair slot for a non-event");
    }

    #[test]
    fn a_nonsense_execution_never_reaches_the_arithmetic() {
        // A NaN in realized P&L is unrecoverable: no later fill brings a NaN
        // total back, so the sleeve's whole record would be destroyed by one
        // bad parse.
        for bad in [
            buy("B1", "ETHUSD", f64::NAN, 2.69, 0.007),
            buy("B2", "ETHUSD", 0.001, f64::NAN, 0.007),
            buy("B3", "ETHUSD", 0.001, 2.69, f64::NAN),
            buy("B4", "ETHUSD", f64::INFINITY, 2.69, 0.007),
        ] {
            let mut l = LiveLedger::starting("2026-09-23");
            assert_eq!(
                l.record(bad.clone()),
                Recorded::Rejected("non-numeric execution"),
                "{bad:?}"
            );
            assert!(l.totals(Sleeve::Trade, &marks()).net_usd().is_finite());
        }
    }

    #[test]
    fn a_negative_cost_or_fee_is_refused() {
        let mut l = LiveLedger::starting("2026-09-23");
        assert_eq!(
            l.record(buy("B1", "ETHUSD", 0.001, -2.69, 0.007)),
            Recorded::Rejected("negative cost or fee")
        );
        assert_eq!(
            l.record(buy("B2", "ETHUSD", 0.001, 2.69, -0.007)),
            Recorded::Rejected("negative cost or fee")
        );
    }

    #[test]
    fn a_fill_with_no_txid_is_refused_because_it_cannot_be_deduplicated() {
        let mut l = LiveLedger::starting("2026-09-23");
        assert_eq!(
            l.record(buy("", "ETHUSD", 0.001, 2.69, 0.007)),
            Recorded::Rejected("no txid")
        );
    }

    #[test]
    fn a_fill_with_no_side_is_refused() {
        let mut l = LiveLedger::starting("2026-09-23");
        let mut f = buy("B1", "ETHUSD", 0.001, 2.69, 0.007);
        f.side = 0;
        assert_eq!(l.record(f), Recorded::Rejected("no side"));
    }

    #[test]
    fn the_rebalance_sleeve_is_kept_apart_from_the_trade_sleeve() {
        // Rebalancing BTC toward a target mix is not a bet and its "P&L" is
        // meaningless. Summing the two would answer a question nobody asked
        // and hide the one that was.
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "ETHUSD", 0.001, 2.69, 0.007));
        l.record(Fill {
            sleeve: Sleeve::Hold,
            book: None,
            pair: "XBTUSD".into(),
            ..buy("R1", "XBTUSD", 0.0005, 42.3, 0.11)
        });
        let trade = l.totals(Sleeve::Trade, &marks());
        let hold = l.totals(Sleeve::Hold, &marks());
        assert_eq!(trade.fills, 1);
        assert_eq!(hold.fills, 1);
        assert!((trade.fees_usd - 0.007).abs() < 1e-12);
        assert!((hold.fees_usd - 0.11).abs() < 1e-12);
    }

    #[test]
    fn a_fill_is_routed_by_which_book_placed_it() {
        assert_eq!(Sleeve::of(Some("eth_1h_sf")), Sleeve::Trade);
        assert_eq!(Sleeve::of(None), Sleeve::Hold, "the BTC rebalance owns no book");
    }

    #[test]
    fn each_pair_keeps_its_own_position() {
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "ETHUSD", 0.01, 27.0, 0.06));
        l.record(buy("B2", "SOLUSD", 1.0, 115.0, 0.3));
        l.record(sell("S1", "ETHUSD", 0.01, 28.0, 0.06));
        assert!(l.get(Sleeve::Trade, "ETHUSD").unwrap().is_flat());
        assert!(!l.get(Sleeve::Trade, "SOLUSD").unwrap().is_flat());
        let t = l.totals(Sleeve::Trade, &marks());
        assert!((t.realized_usd - (28.0 - 0.06 - 27.06)).abs() < 1e-9);
    }

    #[test]
    fn the_fill_list_is_capped_without_losing_the_totals() {
        // The state file is rewritten every sixty seconds; an unbounded fill
        // list would grow into it forever. The running totals are what the
        // question is answered from, so trimming the log must not touch them.
        let mut l = LiveLedger::starting("2026-09-23");
        for i in 0..(MAX_FILLS + 25) {
            l.record(buy(&format!("B{i}"), "SOLUSD", 1.0, 100.0, 0.0));
        }
        assert_eq!(l.fills.len(), MAX_FILLS);
        assert_eq!(l.fills[0].txid, format!("B{}", 25), "oldest dropped first");
        let p = l.get(Sleeve::Trade, "SOLUSD").unwrap();
        assert_eq!(p.buys as usize, MAX_FILLS + 25);
        assert!((p.qty - (MAX_FILLS + 25) as f64).abs() < 1e-9);
    }

    #[test]
    fn a_ledger_round_trips_through_the_state_file() {
        let mut l = LiveLedger::starting("2026-09-23");
        l.record(buy("B1", "ETHUSD", 0.001, 2.69609, 0.00701));
        l.record(sell("S1", "ETHUSD", 0.001, 2.74829, 0.00714));
        l.adopt(Sleeve::Trade, "SOLUSD", 0.2, 115.0);
        let text = serde_json::to_string(&l).unwrap();
        let back: LiveLedger = serde_json::from_str(&text).unwrap();
        assert_eq!(back, l);
        assert_eq!(back.since, "2026-09-23");
    }
}
