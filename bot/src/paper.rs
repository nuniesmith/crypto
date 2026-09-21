//! Persistent paper books. Fills at the closed bar's close with Kraken tier-3 maker fees
//! (same assumption as the study). Live Kraken spot later uses the same decisions.

use std::fs;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::features::{compute, Bar};
use crate::signal::{structure_filtered, trendline_break};

pub const MAKER_FEE: f64 = 0.0022;
pub const TAKER_FEE: f64 = 0.0038;
pub const SLIP: f64 = 0.0001;
pub const NOTIONAL: f64 = 1_000.0;
pub const HOLD_BARS: i64 = 24; // 24 × 1h
pub const ATR_STOP: f64 = 1.5;
pub const PIVOT_LB: usize = 8;
pub const VOL_MULT: f64 = 1.3;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Position {
    pub side: i8, // +1 long, -1 short
    pub qty: f64,
    pub entry: f64,
    pub entry_ts: i64,
    pub entry_bar: i64,
    pub entry_fee: f64,
    /// Qty actually adopted or bought on Kraken. 0 = paper-only, do not sell wallet.
    #[serde(default)]
    pub live_qty: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trade {
    pub ts: i64,
    pub side: i8,
    pub entry: f64,
    pub exit: f64,
    pub qty: f64,
    pub pnl: f64,
    pub fees: f64,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Book {
    pub id: String,
    pub pair: String,
    pub strategy: String, // "trendline_break" | "structure_filtered" | "buy_hold"
    pub cash_usd: f64,
    pub realized_pnl: f64,
    pub fees_paid: f64,
    pub position: Option<Position>,
    pub trades: Vec<Trade>,
    pub last_closed_bar: i64,
    pub marks: Vec<(i64, f64)>, // (ts, equity) sparse
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub started_at: String,
    pub mode: String, // "paper" | "live-dry" | "live"
    pub books: Vec<Book>,
    #[serde(default)]
    pub last_daily: String,
    #[serde(default)]
    pub last_weekly: String,
    #[serde(default)]
    pub last_monthly: String,
    /// UTC date `YYYY-MM-DD` of the last BTC rebalance actually placed.
    #[serde(default)]
    pub last_btc_rebalance: String,
    /// Live limit orders this bot placed that have not been seen to fill.
    #[serde(default)]
    pub pending_orders: Vec<PendingOrder>,
}

/// One live order we placed and are still responsible for.
///
/// Kraken limit orders do not expire, `place_order` here takes no `expiretm`
/// or `userref`, and nothing used to cancel them — so an order priced at the
/// last trade and left unfilled sat on the book forever, holding USD that
/// `Balance` still reports as available. The next cycle would then size a
/// second order against money the first one had already reserved.
///
/// Only orders in this list are ever cancelled. Cancelling everything open
/// would be one line, and would also kill limit orders the operator placed
/// by hand in the Kraken UI on an account that is also theirs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingOrder {
    pub txid: String,
    pub pair: String,
    pub side: i8,
    /// Unix seconds. Age is what decides the CANCEL — a 1h-bar decision that
    /// has not filled in minutes is stale regardless of why. What the book
    /// records is a separate question, settled by `vol_exec`; see `settle`.
    pub placed_at: i64,
    /// Which book placed it. `None` = the BTC rebalance, which owns no book.
    #[serde(default)]
    pub book: Option<String>,
    /// Volume ordered, so a partial fill can be told from a full one.
    #[serde(default)]
    pub qty: f64,
}

impl State {
    pub fn default_paper() -> Self {
        Self {
            started_at: Utc::now().to_rfc3339(),
            mode: "paper".into(),
            books: vec![
                Book::new("sol_1h_tl", "SOLUSD", "trendline_break"),
                Book::new("eth_1h_sf", "ETHUSD", "structure_filtered"),
                Book::new("sol_bh", "SOLUSD", "buy_hold"),
            ],
            last_daily: String::new(),
            last_weekly: String::new(),
            last_monthly: String::new(),
            last_btc_rebalance: String::new(),
            pending_orders: Vec::new(),
        }
    }
}

impl Book {
    fn new(id: &str, pair: &str, strategy: &str) -> Self {
        Self {
            id: id.into(),
            pair: pair.into(),
            strategy: strategy.into(),
            cash_usd: NOTIONAL,
            realized_pnl: 0.0,
            fees_paid: 0.0,
            position: None,
            trades: Vec::new(),
            last_closed_bar: 0,
            marks: Vec::new(),
        }
    }

    pub fn equity(&self, mark: f64) -> f64 {
        let (unreal, open_fee) = match &self.position {
            None => (0.0, 0.0),
            Some(p) => (p.side as f64 * p.qty * (mark - p.entry), p.entry_fee),
        };
        NOTIONAL + self.realized_pnl + unreal - open_fee
    }
}

fn fee(notional: f64, maker: bool) -> f64 {
    notional * ((if maker { MAKER_FEE } else { TAKER_FEE }) + SLIP)
}

/// What to do with one tracked order this cycle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Disposition {
    /// Closed at Kraken, having executed this volume. Apply it and stop tracking.
    Settle(f64),
    /// Still open and past its TTL. Cancel, but KEEP tracking: the cancel is
    /// what makes it closed, and only a closed order reports what it executed.
    Cancel,
    /// Still open and young enough to fill. Leave it.
    Wait,
    /// Never closed and never cancelled. Stop tracking so it cannot leak.
    GiveUp,
}

/// One thing the settler must actually do at the exchange or to a book.
#[derive(Clone, Debug, PartialEq)]
pub enum OrderAction {
    Cancel(PendingOrder),
    Settle(PendingOrder, f64),
    GiveUp(PendingOrder),
}

/// Plan a whole cycle: `(actions to perform, orders still tracked afterwards)`.
///
/// The list-level rule is the one the original bug broke, so it is checked
/// here rather than left implicit in the async loop: an order that is
/// CANCELLED this cycle is still tracked next cycle. Dropping it at cancel
/// time is precisely what left books holding positions that never arrived.
pub fn plan_settlement(
    orders: Vec<PendingOrder>,
    executed: &std::collections::HashMap<String, f64>,
    now: i64,
    ttl_secs: i64,
    give_up_secs: i64,
) -> (Vec<OrderAction>, Vec<PendingOrder>) {
    let mut actions = Vec::new();
    let mut tracked = Vec::new();
    for order in orders {
        let seen = executed.get(&order.txid).copied();
        match disposition(&order, seen, now, ttl_secs, give_up_secs) {
            Disposition::Settle(got) => actions.push(OrderAction::Settle(order, got)),
            Disposition::Cancel => {
                actions.push(OrderAction::Cancel(order.clone()));
                tracked.push(order);
            }
            Disposition::Wait => tracked.push(order),
            Disposition::GiveUp => actions.push(OrderAction::GiveUp(order)),
        }
    }
    (actions, tracked)
}

/// Decide one order's fate from its age and whether Kraken reports it closed.
///
/// Pulled out of the async settler so the rules can be checked against exact
/// timestamps without a Kraken gateway — the same reason the wallet policy in
/// alloc.rs is plain functions over a `Wallet`.
///
/// `executed` is `None` when the txid is absent from `ClosedOrders`, which
/// means still open. "Not closed yet" and "closed having filled nothing" are
/// different facts and must not collapse into one.
pub fn disposition(
    order: &PendingOrder,
    executed: Option<f64>,
    now: i64,
    ttl_secs: i64,
    give_up_secs: i64,
) -> Disposition {
    if let Some(got) = executed {
        return Disposition::Settle(got);
    }
    let age = now - order.placed_at;
    if age >= give_up_secs {
        return Disposition::GiveUp;
    }
    if age >= ttl_secs {
        return Disposition::Cancel;
    }
    Disposition::Wait
}

/// Tolerance for "this order is done". Both sides of the comparison are
/// decimal strings that round-tripped through f64, so only the last ulp is
/// ever in question — anything larger is a genuine partial fill.
const FILL_TOL: f64 = 1e-9;

/// What an order actually did, once Kraken reports `vol_exec`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Settled {
    /// Executed in full. The book already says the right thing.
    Filled,
    /// Executed in part. The book has been shrunk to what really arrived.
    Partial { ordered: f64, got: f64 },
    /// Executed not at all. The entry it recorded has been withdrawn.
    Nothing,
}

/// Correct a book against what its BUY order actually executed.
///
/// `LiveAction::Buy` writes `qty`/`live_qty` into the book at the moment
/// Kraken ACCEPTS the order, because that is the only moment it has a number.
/// A limit order priced at the last trade frequently does not fill, and the
/// age-based reaper then cancels it without telling the book — so the book was
/// left long a position the account never received. That is not a safety bug
/// (exits size against the wallet, so nothing tries to sell coins that are not
/// there) but it is a measurement bug, and measuring whether the strategy pays
/// for itself is the entire reason these books exist.
///
/// Withdrawing an unfilled entry mirrors `LiveAction::Skip` exactly: take the
/// position back off the book and refund the entry fee that was charged with it.
pub fn settle_buy(book: &mut Book, ordered: f64, got: f64) -> Settled {
    // A number that is not a number cannot be compared into a decision. Both
    // comparisons below are FALSE against NaN, so without this the function
    // would fall through to `Nothing` and withdraw a real position because a
    // parse went wrong.
    if !ordered.is_finite() || !got.is_finite() {
        return Settled::Filled;
    }
    // Also the upgrade path: orders already in the live state.json deserialize
    // with `qty: 0.0` (`serde(default)`), which means "unknown", not "ordered
    // nothing". Zero lands here as satisfied-in-full and the book is left
    // exactly as placed, which is the only safe reading of an unknown.
    if got + FILL_TOL >= ordered {
        return Settled::Filled;
    }
    if got <= FILL_TOL {
        if let Some(p) = book.position.take() {
            book.fees_paid = (book.fees_paid - p.entry_fee).max(0.0);
        }
        return Settled::Nothing;
    }
    if let Some(p) = book.position.as_mut() {
        // Fees were charged on the notional we asked for, not the notional we
        // got. Scale rather than recompute: the fee model lives in `fee()` and
        // a second copy of it here would be a second thing to keep in step.
        let share = got / ordered;
        book.fees_paid = (book.fees_paid - p.entry_fee * (1.0 - share)).max(0.0);
        p.entry_fee *= share;
        p.qty = got;
        p.live_qty = got;
    }
    Settled::Partial { ordered, got }
}

pub fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("data")
        .join("paper")
}

pub fn state_path() -> PathBuf {
    data_dir().join("state.json")
}

pub fn journal_path() -> PathBuf {
    data_dir().join("journal.jsonl")
}

pub fn load_state() -> anyhow::Result<State> {
    let p = state_path();
    if p.exists() {
        let s = fs::read_to_string(&p)?;
        Ok(serde_json::from_str(&s)?)
    } else {
        Ok(State::default_paper())
    }
}

pub fn save_state(state: &State) -> anyhow::Result<()> {
    fs::create_dir_all(data_dir())?;
    let tmp = state_path().with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    fs::rename(tmp, state_path())?;
    Ok(())
}

pub fn append_journal(value: &serde_json::Value) -> anyhow::Result<()> {
    fs::create_dir_all(data_dir())?;
    use std::io::Write;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path())?;
    writeln!(f, "{}", serde_json::to_string(value)?)?;
    Ok(())
}

fn signal_at(strategy: &str, bars: &[Bar], i: usize) -> i8 {
    let feat = compute(bars);
    let sig = match strategy {
        "trendline_break" => trendline_break(bars, &feat, PIVOT_LB, VOL_MULT),
        "structure_filtered" => structure_filtered(bars, &feat, PIVOT_LB, VOL_MULT),
        _ => return 0,
    };
    sig.get(i).copied().unwrap_or(0)
}

fn atr_at(bars: &[Bar], i: usize) -> f64 {
    compute(bars).atr14.get(i).copied().unwrap_or(0.0)
}

pub fn step_book(book: &mut Book, bars: &[Bar], now_ts: i64) -> Vec<String> {
    let mut log = Vec::new();
    if bars.len() < 40 {
        log.push(format!("{}: not enough bars ({})", book.id, bars.len()));
        return log;
    }
    // Drop a still-forming last candle.
    let interval = 3600;
    let mut closed = bars.to_vec();
    if let Some(last) = closed.last() {
        if last.time + interval > now_ts {
            closed.pop();
        }
    }
    if closed.len() < 40 {
        return log;
    }
    let i = closed.len() - 1;
    let bar = &closed[i];
    if bar.time <= book.last_closed_bar {
        // still mark
        if let Some(p) = &book.position {
            let eq = book.cash_usd + p.side as f64 * p.qty * bar.close;
            book.marks.push((bar.time, eq));
            if book.marks.len() > 500 {
                let drop = book.marks.len() - 400;
                book.marks.drain(0..drop);
            }
        }
        return log;
    }

    if book.strategy == "buy_hold" {
        if book.position.is_none() {
            enter(book, 1, bar, false, &mut log);
        }
        book.last_closed_bar = bar.time;
        mark(book, bar.close, bar.time);
        return log;
    }

    // manage open position first
    if let Some(p) = book.position.clone() {
        let held = (bar.time - p.entry_bar) / interval;
        let atr = atr_at(&closed, i);
        let mut exit_now = false;
        let mut reason = String::new();
        if held >= HOLD_BARS {
            exit_now = true;
            reason = "TIME".into();
        }
        if atr > 0.0 {
            if p.side > 0 && bar.low < p.entry - ATR_STOP * atr {
                exit_now = true;
                reason = "STOP".into();
            }
            if p.side < 0 && bar.high > p.entry + ATR_STOP * atr {
                exit_now = true;
                reason = "STOP".into();
            }
        }
        let sig = signal_at(&book.strategy, &closed, i);
        if sig == -p.side {
            exit_now = true;
            reason = "FLIP".into();
        }
        if exit_now {
            exit(book, bar, true, &reason, &mut log);
            // flip: enter opposite on same bar after exit
            if reason == "FLIP" && sig != 0 {
                enter(book, sig, bar, true, &mut log);
            }
        }
    } else {
        let sig = signal_at(&book.strategy, &closed, i);
        if sig != 0 {
            enter(book, sig, bar, true, &mut log);
        }
    }

    book.last_closed_bar = bar.time;
    mark(book, bar.close, bar.time);
    log
}

fn mark(book: &mut Book, px: f64, ts: i64) {
    let eq = book.equity(px);
    book.marks.push((ts, eq));
    if book.marks.len() > 500 {
        let drop = book.marks.len() - 400;
        book.marks.drain(0..drop);
    }
}

fn enter(book: &mut Book, side: i8, bar: &Bar, maker: bool, log: &mut Vec<String>) {
    let px = bar.close;
    let qty = NOTIONAL / px;
    let f = fee(NOTIONAL, maker);
    book.position = Some(Position {
        side,
        qty,
        entry: px,
        entry_ts: bar.time,
        entry_bar: bar.time,
        entry_fee: f,
        live_qty: 0.0,
    });
    book.fees_paid += f;
    let dir = if side > 0 { "BUY" } else { "SELL SHORT" };
    log.push(format!(
        "{} ENTER {dir} {} qty={:.6} px={:.4} fee={:.2}",
        book.id, book.pair, qty, px, f
    ));
}

fn exit(book: &mut Book, bar: &Bar, maker: bool, reason: &str, log: &mut Vec<String>) {
    let Some(p) = book.position.take() else {
        return;
    };
    let px = bar.close;
    let f = fee(p.qty * px, maker);
    let gross = p.side as f64 * p.qty * (px - p.entry);
    let pnl = gross - p.entry_fee - f;
    book.realized_pnl += pnl;
    book.fees_paid += f;
    book.cash_usd = NOTIONAL + book.realized_pnl;
    book.trades.push(Trade {
        ts: bar.time,
        side: p.side,
        entry: p.entry,
        exit: px,
        qty: p.qty,
        pnl,
        fees: p.entry_fee + f,
        reason: reason.into(),
    });
    log.push(format!(
        "{} EXIT {} {} px={:.4} pnl={:+.2} reason={reason}",
        book.id,
        if p.side > 0 { "SELL" } else { "COVER" },
        book.pair,
        px,
        pnl
    ));
}

pub fn fmt_ts(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}

pub fn print_status(state: &State, marks: &[(String, f64)]) {
    println!("mode={}  started={}", state.mode, state.started_at);
    println!(
        "{:14} {:8} {:20} {:>10} {:>10} {:>8} {:>6}",
        "book", "pair", "pos", "equity", "realized", "fees", "n"
    );
    for b in &state.books {
        let mark = marks
            .iter()
            .find(|(p, _)| p == &b.pair)
            .map(|(_, px)| *px)
            .unwrap_or(0.0);
        let eq = b.equity(mark);
        let pos = match &b.position {
            None => "flat".into(),
            Some(p) => format!(
                "{} {:.4} @{:.4}",
                if p.side > 0 { "long" } else { "short" },
                p.qty,
                p.entry
            ),
        };
        println!(
            "{:14} {:8} {:20} {:>10.2} {:>10.2} {:>8.2} {:>6}",
            b.id,
            b.pair,
            pos,
            eq,
            b.realized_pnl,
            b.fees_paid,
            b.trades.len()
        );
    }
}



#[cfg(test)]
mod order_tests {
    use super::*;

    fn order(txid: &str, placed_at: i64) -> PendingOrder {
        PendingOrder {
            txid: txid.into(),
            pair: "ETHUSD".into(),
            side: 1,
            placed_at,
            book: Some("eth_1h_sf".into()),
            qty: 0.01,
        }
    }

    const TTL: i64 = 600;
    const GIVE_UP: i64 = 86_400;

    fn fate(placed_at: i64, executed: Option<f64>, now: i64) -> Disposition {
        disposition(&order("A", placed_at), executed, now, TTL, GIVE_UP)
    }

    #[test]
    fn an_order_younger_than_the_ttl_is_left_alone() {
        assert_eq!(fate(1_000, None, 1_500), Disposition::Wait);
    }

    #[test]
    fn an_order_at_or_past_the_ttl_is_cancelled() {
        assert_eq!(fate(1_000, None, 1_600), Disposition::Cancel);
    }

    #[test]
    fn each_order_is_judged_on_its_own_age() {
        assert_eq!(fate(0, None, 1_500), Disposition::Cancel);
        assert_eq!(fate(1_400, None, 1_500), Disposition::Wait);
    }

    #[test]
    fn a_clock_that_goes_backwards_cancels_nothing() {
        // An NTP step or a stale `now` must not look like extreme age and
        // sweep away an order that was placed seconds ago.
        assert_eq!(fate(2_000, None, 1_000), Disposition::Wait);
    }

    #[test]
    fn a_closed_order_settles_whatever_its_age() {
        // Fill state outranks age in both directions: a young order that
        // already filled must settle now, and an ancient one must settle
        // rather than be abandoned unrecorded.
        assert_eq!(fate(1_000, Some(0.01), 1_100), Disposition::Settle(0.01));
        assert_eq!(fate(0, Some(0.01), 200_000), Disposition::Settle(0.01));
    }

    #[test]
    fn a_closed_order_that_filled_nothing_still_settles() {
        // The bug this whole path exists for: `Some(0.0)` is a FACT about the
        // order (it closed empty) and must not be confused with `None`
        // (still open). Collapsing them leaves the book long a phantom.
        assert_eq!(fate(1_000, Some(0.0), 1_100), Disposition::Settle(0.0));
    }

    #[test]
    fn a_cancelled_order_keeps_being_tracked_until_it_settles() {
        // Cancel does not end our interest in an order. Kraken reports the
        // executed volume only once the order is CLOSED, and the cancel is
        // what closes it -- so the tick that cancels cannot also settle.
        assert_eq!(fate(1_000, None, 1_700), Disposition::Cancel);
        // next tick, now closed:
        assert_eq!(fate(1_000, Some(0.004), 1_760), Disposition::Settle(0.004));
    }

    fn plan(orders: Vec<PendingOrder>, executed: &[(&str, f64)], now: i64)
        -> (Vec<OrderAction>, Vec<PendingOrder>)
    {
        let map = executed.iter().map(|(t, v)| (t.to_string(), *v)).collect();
        plan_settlement(orders, &map, now, TTL, GIVE_UP)
    }

    #[test]
    fn a_cancelled_order_stays_tracked_for_the_next_cycle() {
        // THE bug, at the level it actually lived. The old code dropped an
        // order the moment it cancelled it, so the executed volume -- which
        // Kraken only publishes once the order is closed, i.e. after the
        // cancel -- was never read, and the book kept a position the account
        // never received.
        let (actions, tracked) = plan(vec![order("A", 1_000)], &[], 1_700);
        assert_eq!(actions, vec![OrderAction::Cancel(order("A", 1_000))]);
        assert_eq!(tracked, vec![order("A", 1_000)], "must still be tracked");
    }

    #[test]
    fn a_settled_order_stops_being_tracked() {
        let (actions, tracked) = plan(vec![order("A", 1_000)], &[("A", 0.01)], 1_700);
        assert_eq!(actions, vec![OrderAction::Settle(order("A", 1_000), 0.01)]);
        assert!(tracked.is_empty(), "settled means done");
    }

    #[test]
    fn a_waiting_order_is_tracked_with_nothing_to_do() {
        let (actions, tracked) = plan(vec![order("A", 1_000)], &[], 1_100);
        assert!(actions.is_empty());
        assert_eq!(tracked.len(), 1);
    }

    #[test]
    fn an_abandoned_order_stops_being_tracked() {
        let (actions, tracked) = plan(vec![order("A", 0)], &[], GIVE_UP);
        assert_eq!(actions, vec![OrderAction::GiveUp(order("A", 0))]);
        assert!(tracked.is_empty(), "the point of giving up is to stop leaking");
    }

    #[test]
    fn a_mixed_cycle_routes_each_order_to_its_own_fate() {
        let (actions, tracked) = plan(
            vec![order("filled", 1_000), order("stale", 1_000), order("young", 1_650)],
            &[("filled", 0.01)],
            1_700,
        );
        assert_eq!(
            actions,
            vec![
                OrderAction::Settle(order("filled", 1_000), 0.01),
                OrderAction::Cancel(order("stale", 1_000)),
            ]
        );
        assert_eq!(
            tracked.iter().map(|o| o.txid.as_str()).collect::<Vec<_>>(),
            ["stale", "young"]
        );
    }

    #[test]
    fn a_fill_is_matched_to_its_OWN_txid() {
        // A closed order belonging to some other txid must not settle this one.
        let (actions, tracked) = plan(vec![order("A", 1_000)], &[("B", 0.01)], 1_100);
        assert!(actions.is_empty());
        assert_eq!(tracked.len(), 1);
    }

    #[test]
    fn an_order_that_never_closes_is_eventually_abandoned() {
        assert_eq!(fate(0, None, GIVE_UP - 1), Disposition::Cancel);
        assert_eq!(fate(0, None, GIVE_UP), Disposition::GiveUp);
    }

    fn long_book(qty: f64, entry_fee: f64) -> Book {
        let mut b = Book::new("eth_1h_sf", "ETHUSD", "structure_filtered");
        b.fees_paid = entry_fee;
        b.position = Some(Position {
            side: 1,
            qty,
            entry: 2_696.09,
            entry_ts: 0,
            entry_bar: 0,
            entry_fee,
            live_qty: qty,
        });
        b
    }

    #[test]
    fn a_buy_that_filled_in_full_leaves_the_book_alone() {
        let mut b = long_book(0.0149, 2.30);
        assert_eq!(settle_buy(&mut b, 0.0149, 0.0149), Settled::Filled);
        assert_eq!(b.position.as_ref().unwrap().qty, 0.0149);
        assert_eq!(b.fees_paid, 2.30);
    }

    #[test]
    fn a_buy_that_never_filled_withdraws_the_entry() {
        // 2026-09-21: eth_1h_sf sat "long 0.0149 @2696.09" against a wallet
        // holding 0.001. The limit was priced under the market, never filled,
        // and the age-based reaper cancelled it without telling the book.
        let mut b = long_book(0.0149, 2.30);
        assert_eq!(settle_buy(&mut b, 0.0149, 0.0), Settled::Nothing);
        assert!(b.position.is_none(), "phantom position must be withdrawn");
        assert_eq!(b.fees_paid, 0.0, "a fee on a trade that never happened");
    }

    #[test]
    fn a_partial_fill_resizes_the_book_to_what_arrived() {
        let mut b = long_book(0.0149, 2.40);
        // A QUARTER fill, deliberately not a half: at exactly half, charging
        // the filled share and refunding the unfilled share give the same
        // number, so a half-fill fixture cannot tell the two apart.
        let got = 0.003725;
        assert_eq!(
            settle_buy(&mut b, 0.0149, got),
            Settled::Partial { ordered: 0.0149, got }
        );
        let p = b.position.as_ref().unwrap();
        assert_eq!(p.qty, got);
        assert_eq!(p.live_qty, got, "the wallet holds this much and no more");
        assert!((p.entry_fee - 0.60).abs() < 1e-9, "fee scales with the fill");
        assert!((b.fees_paid - 0.60).abs() < 1e-9);
    }

    #[test]
    fn the_2026_09_21_eth_entry_settles_to_what_the_wallet_actually_got() {
        // Real numbers, read back from Kraken ClosedOrders:
        //   O4MMNL-PYL4C-OMSGFN  canceled  buy ETHUSD
        //   vol 0.01488212  vol_exec 0.00100000
        // The book carried the full 0.0149 for the rest of the day while the
        // account held 0.001. Every equity line it printed was wrong by the
        // difference, which is the whole measurement the sleeve exists for.
        let mut b = long_book(0.01488212, 2.30);
        let got = 0.00100000;
        assert_eq!(
            settle_buy(&mut b, 0.01488212, got),
            Settled::Partial { ordered: 0.01488212, got }
        );
        let p = b.position.as_ref().unwrap();
        assert_eq!(p.qty, got);
        assert_eq!(p.live_qty, got);
        assert!(p.entry_fee < 0.20, "fee on 0.001 ETH, not on 0.0149");
    }

    #[test]
    fn a_partial_fill_is_not_rounded_away_to_a_full_one() {
        // A fill one satoshi short is still partial. If this ever reports
        // Filled, the book overstates the position by the shortfall forever.
        let mut b = long_book(0.0149, 2.30);
        assert!(matches!(
            settle_buy(&mut b, 0.0149, 0.0149 - 1e-6),
            Settled::Partial { .. }
        ));
    }

    #[test]
    fn an_order_recorded_before_qty_was_tracked_changes_nothing() {
        // `qty` is `#[serde(default)]`, so orders already in the live
        // state.json deserialize with 0.0. That means "unknown", not "ordered
        // nothing" -- reading it as the latter would withdraw a real entry
        // from a live book on the first tick after deploy.
        let mut b = long_book(0.0149, 2.30);
        assert_eq!(settle_buy(&mut b, 0.0, 0.0), Settled::Filled);
        assert!(b.position.is_some(), "must not wipe a live book on upgrade");
        assert_eq!(b.fees_paid, 2.30);
    }

    #[test]
    fn a_nonsense_fill_volume_leaves_the_book_alone() {
        // NaN compares false against everything, so an unguarded version falls
        // all the way through to `Nothing` and withdraws a REAL position
        // because a string failed to parse. Both sides have to be checked.
        for (ordered, got) in [(0.0149, f64::NAN), (f64::NAN, 0.0), (0.0149, f64::INFINITY)] {
            let mut b = long_book(0.0149, 2.30);
            assert_eq!(settle_buy(&mut b, ordered, got), Settled::Filled);
            assert!(b.position.is_some(), "ordered={ordered} got={got}");
            assert_eq!(b.fees_paid, 2.30);
        }
    }

    #[test]
    fn settling_a_book_that_is_already_flat_does_not_panic() {
        // The position can be gone by the time the order settles -- the
        // strategy may have closed it on a later bar.
        let mut b = Book::new("eth_1h_sf", "ETHUSD", "structure_filtered");
        assert_eq!(settle_buy(&mut b, 0.0149, 0.0), Settled::Nothing);
        assert!(b.position.is_none());
    }

    #[test]
    fn a_state_file_written_before_order_tracking_still_loads() {
        // The bot restarts onto an existing state.json. If the new field
        // were not `#[serde(default)]`, every deploy would fail to parse the
        // live state and the loop would come up with fresh $1k books.
        let old = r#"{
            "started_at": "2026-09-20T18:42:16Z",
            "mode": "live",
            "books": [],
            "last_daily": "",
            "last_weekly": "",
            "last_monthly": "",
            "last_btc_rebalance": "2026-09-20"
        }"#;
        let s: State = serde_json::from_str(old).expect("old state must still parse");
        assert!(s.pending_orders.is_empty());
        assert_eq!(s.last_btc_rebalance, "2026-09-20");
    }
}
