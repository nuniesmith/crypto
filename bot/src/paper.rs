//! Persistent paper books: a $1,000-per-book SIMULATION of the signals.
//!
//! Fills at the closed bar's close with Kraken tier-3 maker fees (the same
//! assumption as the study). The live path reads the same decisions, but it
//! must never write back into a book — see `ledger.rs` for where real fills
//! go and why that separation had to be made.
//!
//! Every book always trades `NOTIONAL`. That invariant is the only reason the
//! numbers here mean anything: the question these books answer is "does this
//! signal have an edge at a size worth trading?", which cannot be answered by
//! a book whose size is whatever a small wallet could afford that hour.

use std::fs;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::features::{compute, Bar};
use crate::ledger::{Execution, LiveLedger, Sleeve, Totals};
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
    /// SIMULATED size: always `NOTIONAL / entry`. Nothing in the live path
    /// may write here — `LiveAction::Buy` used to, which is what reduced a
    /// $1,000 book to whatever the wallet could afford and left the entry fee
    /// charged on the $1,000 that was no longer there.
    pub qty: f64,
    pub entry: f64,
    pub entry_ts: i64,
    pub entry_bar: i64,
    /// Modelled fee on `qty * entry`. Paired with `qty` and meaningless
    /// without it: the two must always describe the same notional.
    pub entry_fee: f64,
    /// Qty actually adopted or bought on Kraken. 0 = paper-only, do not sell
    /// wallet. This is the ONLY field the live path writes, and it is what
    /// the exit sizes against.
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
    /// What the REAL money did, from real Kraken fills. Entirely separate
    /// from `books` above, which are a simulation at a size the account has
    /// never traded.
    #[serde(default)]
    pub live: LiveLedger,
    /// UTC date from which `books` is a pure `NOTIONAL` simulation.
    ///
    /// Trades recorded before this date were taken while the live path was
    /// still writing its own order sizes into the books, so their `qty`,
    /// `fees` and `pnl` describe neither the simulation nor the account. They
    /// are LEFT AS RECORDED — rewriting them would be inventing history — and
    /// this date is how a reader knows which side of the fix they fall on.
    #[serde(default)]
    pub paper_clean_since: String,
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
            live: LiveLedger::default(),
            paper_clean_since: String::new(),
        }
    }
}

/// Stamp the epoch both sets of books start from, and repair what can be
/// repaired without inventing anything.
///
/// Runs once, on the first load that finds no epoch. Everything it does is
/// additive or a restoration of a value this file already knows the formula
/// for; no recorded `Trade` is touched, because there is no honest way to
/// recover what a mixed trade would have been in either set of books.
pub fn migrate(state: &mut State, today: &str) -> Vec<String> {
    let mut notes = Vec::new();
    if !state.live.since.is_empty() {
        return notes;
    }
    // A ledger with no epoch has never recorded anything — `since`, `pairs`
    // and `fills` are all `serde(default)` and arrive together — so starting
    // a fresh one here cannot discard a fill.
    state.live = LiveLedger::starting(today);
    state.paper_clean_since = today.to_string();
    notes.push(format!(
        "LEDGER starts {today}. Book trades before this date mixed live order \
         sizes into a $1,000 simulation and are not comparable with either."
    ));
    for book in &mut state.books {
        if let Some(note) = repair_open_position(book) {
            notes.push(note);
        }
    }
    notes
}

/// Put an open paper position back on the simulation's scale.
///
/// A position carried across the fix may have had its `qty` overwritten with
/// a live order size and its `entry_fee` scaled down to a partial fill, and
/// there is no other record of what the book thought it held. Both fields
/// have a formula — `NOTIONAL / entry` and the fee on that notional — so
/// restoring them is arithmetic, not invention.
///
/// The fee is restored at the MAKER rate because it is the lower of the two
/// and a repair must never invent a cost that may not have been charged. The
/// live case this was written for is `sol_bh`, whose `qty` was being
/// overwritten every single cycle with the wallet's SOL dust — 0.0000094 of a
/// coin — which pinned the buy-and-hold benchmark at a flat -$3.90 while SOL
/// moved. That benchmark is what the signals have to beat, so a dead one
/// makes the entire comparison unreadable.
pub fn repair_open_position(book: &mut Book) -> Option<String> {
    let (entry, was_qty, was_fee) = match &book.position {
        Some(p) => (p.entry, p.qty, p.entry_fee),
        None => return None,
    };
    if !(entry.is_finite() && entry > 0.0) {
        return None;
    }
    let want_qty = NOTIONAL / entry;
    let want_fee = fee(NOTIONAL, true);
    // A hair of float drift is not damage. Anything bigger is the live path
    // having written here, which it no longer does.
    let qty_wrong = (was_qty - want_qty).abs() > want_qty * 1e-9;
    let fee_wrong = was_fee < want_fee;
    if !qty_wrong && !fee_wrong {
        return None;
    }
    if fee_wrong {
        book.fees_paid += want_fee - was_fee;
    }
    let p = book.position.as_mut()?;
    p.qty = want_qty;
    if fee_wrong {
        p.entry_fee = want_fee;
    }
    Some(format!(
        "{} REPAIR open paper position qty {was_qty:.8} -> {want_qty:.8}, \
         entry fee {was_fee:.2} -> {:.2} (live sizing had overwritten it)",
        book.id,
        if fee_wrong { want_fee } else { was_fee }
    ))
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
    /// Closed at Kraken. Apply what it executed and stop tracking.
    ///
    /// Carries the whole `Execution` rather than a volume so that "this order
    /// is settleable" and "here is what it did" cannot come apart. They were
    /// two values once, and reassembling them downstream needed a branch for
    /// a state that could not happen -- dead code in the one function that
    /// decides what a real order is worth.
    Settle(Execution),
    /// Still open and past its TTL. Cancel, but KEEP tracking: the cancel is
    /// what makes it closed, and only a closed order reports what it executed.
    Cancel,
    /// Still open and young enough to fill. Leave it.
    Wait,
    /// Never closed and never cancelled. Stop tracking so it cannot leak.
    GiveUp,
}

/// One thing the settler must actually do at the exchange or to a book.
///
/// `Settle` carries the whole `Execution`, not just the volume: the volume is
/// what corrects `live_qty`, but `cost` and `fee` are what the live ledger
/// needs, and fetching them separately would mean two readings of the same
/// order that could disagree.
#[derive(Clone, Debug, PartialEq)]
pub enum OrderAction {
    Cancel(PendingOrder),
    Settle(PendingOrder, Execution),
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
    executed: &std::collections::HashMap<String, Execution>,
    now: i64,
    ttl_secs: i64,
    give_up_secs: i64,
) -> (Vec<OrderAction>, Vec<PendingOrder>) {
    let mut actions = Vec::new();
    let mut tracked = Vec::new();
    for order in orders {
        let seen = executed.get(&order.txid).copied();
        match disposition(&order, seen, now, ttl_secs, give_up_secs) {
            Disposition::Settle(exec) => actions.push(OrderAction::Settle(order, exec)),
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
    executed: Option<Execution>,
    now: i64,
    ttl_secs: i64,
    give_up_secs: i64,
) -> Disposition {
    if let Some(exec) = executed {
        return Disposition::Settle(exec);
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
    /// Executed in full. `live_qty` already says the right thing.
    Filled,
    /// Executed in part. `live_qty` has been cut to what really arrived.
    Partial { ordered: f64, got: f64 },
    /// Executed not at all. The book holds no live inventory.
    Nothing,
}

/// Correct a book's LIVE inventory against what its buy order executed.
///
/// `LiveAction::Buy` writes `live_qty` at the moment Kraken ACCEPTS the order,
/// because that is the only moment it has a number. A limit order priced at
/// the last trade frequently does not fill, and the age-based reaper then
/// cancels it — so without this the book believes the wallet holds coins it
/// never received, and the eventual exit sizes a sell against them.
///
/// It touches `live_qty` and NOTHING else. It used to rewrite `qty`,
/// `entry_fee` and `fees_paid` as well, which is half of how the two sets of
/// books got mixed: a $1,000 simulated entry would be silently reduced to the
/// 0.001 ETH the wallet could afford, and then the SIMULATION reported the
/// P&L of a $2.70 trade. The simulation takes every signal at `NOTIONAL`
/// whatever the account can do; what the account actually did is `ledger.rs`.
pub fn settle_live_buy(book: &mut Book, ordered: f64, got: f64) -> Settled {
    // A number that is not a number cannot be compared into a decision. Both
    // comparisons below are FALSE against NaN, so without this the function
    // would fall through to `Nothing` and discard live inventory that is
    // really there because a parse went wrong.
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
        if let Some(p) = book.position.as_mut() {
            // The paper position STAYS — the signal fired and the simulation
            // took it. Only the claim on the wallet goes, so the exit does
            // not try to sell coins that never arrived.
            p.live_qty = 0.0;
        }
        return Settled::Nothing;
    }
    if let Some(p) = book.position.as_mut() {
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
    let mut state = if p.exists() {
        let s = fs::read_to_string(&p)?;
        serde_json::from_str(&s)?
    } else {
        State::default_paper()
    };
    for note in migrate(&mut state, &Utc::now().format("%Y-%m-%d").to_string()) {
        tracing::warn!("{note}");
    }
    Ok(state)
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
    // The simulation's invariant, ENFORCED rather than hoped for: an open
    // position is always NOTIONAL-sized. `qty` has been overwritten from the
    // live path before and the damage was invisible for a day -- `sol_bh`,
    // the buy-and-hold benchmark every signal is judged against, had its
    // 9.098 SOL replaced each cycle by the wallet's 0.0000094 and printed a
    // flat -$3.90 while SOL moved. Nothing in the live path writes here any
    // more; this is what makes that true next month as well as today.
    if let Some(note) = repair_open_position(book) {
        log.push(note);
    }
    let i = closed.len() - 1;
    let bar = &closed[i];
    if bar.time <= book.last_closed_bar {
        // Still inside the same bar: re-mark, do not trade. This used to use
        // its own formula (`cash_usd + qty * close`), which counts the whole
        // position as profit instead of only its move and ignores the entry
        // fee — so the intra-bar marks and the end-of-bar mark described
        // different books. One equity function, used everywhere.
        if book.position.is_some() {
            mark(book, bar.close, bar.time);
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
    // On the position's OWN notional, not on the constant. Numerically these
    // are the same while `qty` is always `NOTIONAL / px` — which is now an
    // invariant — but writing the constant is what let the two drift apart:
    // the live path resized `qty` and the fee stayed charged on $1,000, so a
    // $2.70 position carried a $0.16 entry fee. The exit already charges
    // `p.qty * px`; entry and exit must read the same notional or neither
    // number means anything.
    let f = fee(qty * px, maker);
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

/// One line per sleeve answering the question the books cannot: what the real
/// money did, net of real Kraken fees.
pub fn live_ledger_lines(state: &State, marks: &[(String, f64)]) -> Vec<String> {
    if state.live.since.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!(
        "live ledger (real Kraken fills) since {}",
        state.live.since
    )];
    for sleeve in [Sleeve::Trade, Sleeve::Hold] {
        let t: Totals = state.live.totals(sleeve, marks);
        if t.fills == 0 {
            continue;
        }
        out.push(format!(
            "  {:5} net {:+8.4}  realized {:+8.4}  unrealized {:+8.4}  \
             real fees {:7.4}  inventory ${:.2}  fills {}{}{}",
            sleeve.label(),
            t.net_usd(),
            t.realized_usd,
            t.unrealized_usd,
            t.fees_usd,
            t.inventory_usd,
            t.fills,
            if t.complete() {
                String::new()
            } else {
                format!("  [{} pair(s) unpriced — total is partial]", t.unpriced)
            },
            if t.adopted_usd > 0.0 {
                format!("  [${:.2} of basis marked in, not paid]", t.adopted_usd)
            } else {
                String::new()
            },
        ));
    }
    if out.len() == 1 {
        out.push("  no real fills yet".into());
    }
    out
}

pub fn print_status(state: &State, marks: &[(String, f64)]) {
    println!("mode={}  started={}", state.mode, state.started_at);
    for line in live_ledger_lines(state, marks) {
        println!("{line}");
    }
    if !state.paper_clean_since.is_empty() {
        println!(
            "books below are a pure ${:.0} simulation from {} \
             (earlier trades mixed live sizing in and are not comparable)",
            NOTIONAL, state.paper_clean_since
        );
    }
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

    fn exec(vol: f64) -> Execution {
        Execution { vol_exec: vol, cost: vol * 2_700.0, fee: vol * 2_700.0 * 0.0026, at: None }
    }

    fn fate(placed_at: i64, executed: Option<f64>, now: i64) -> Disposition {
        disposition(&order("A", placed_at), executed.map(exec), now, TTL, GIVE_UP)
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
        assert_eq!(fate(1_000, Some(0.01), 1_100), Disposition::Settle(exec(0.01)));
        assert_eq!(fate(0, Some(0.01), 200_000), Disposition::Settle(exec(0.01)));
    }

    #[test]
    fn a_closed_order_that_filled_nothing_still_settles() {
        // The bug this whole path exists for: `Some(0.0)` is a FACT about the
        // order (it closed empty) and must not be confused with `None`
        // (still open). Collapsing them leaves the book long a phantom.
        assert_eq!(fate(1_000, Some(0.0), 1_100), Disposition::Settle(exec(0.0)));
    }

    #[test]
    fn a_cancelled_order_keeps_being_tracked_until_it_settles() {
        // Cancel does not end our interest in an order. Kraken reports the
        // executed volume only once the order is CLOSED, and the cancel is
        // what closes it -- so the tick that cancels cannot also settle.
        assert_eq!(fate(1_000, None, 1_700), Disposition::Cancel);
        // next tick, now closed:
        assert_eq!(fate(1_000, Some(0.004), 1_760), Disposition::Settle(exec(0.004)));
    }

    fn plan(orders: Vec<PendingOrder>, executed: &[(&str, f64)], now: i64)
        -> (Vec<OrderAction>, Vec<PendingOrder>)
    {
        let map = executed.iter().map(|(t, v)| (t.to_string(), exec(*v))).collect();
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
        assert_eq!(actions, vec![OrderAction::Settle(order("A", 1_000), exec(0.01))]);
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
                OrderAction::Settle(order("filled", 1_000), exec(0.01)),
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
        assert_eq!(settle_live_buy(&mut b, 0.0149, 0.0149), Settled::Filled);
        assert_eq!(b.position.as_ref().unwrap().live_qty, 0.0149);
        assert_eq!(b.fees_paid, 2.30);
    }

    #[test]
    fn a_buy_that_never_filled_drops_the_wallet_claim_and_keeps_the_trade() {
        // 2026-09-21: eth_1h_sf sat "long 0.0149 @2696.09" against a wallet
        // holding 0.001. The order never filled and the reaper cancelled it.
        //
        // The SIMULATION keeps the trade: the signal fired, and the books
        // exist to measure the signal. Withdrawing the entry here made the
        // simulation skip exactly the trades the account was too poor to
        // take, which is a selection bias, not a correction. Only the claim
        // on the wallet goes, so the exit does not sell absent coins.
        let mut b = long_book(0.0149, 2.30);
        assert_eq!(settle_live_buy(&mut b, 0.0149, 0.0), Settled::Nothing);
        let p = b.position.as_ref().expect("the simulated trade stands");
        assert_eq!(p.live_qty, 0.0, "nothing arrived in the wallet");
        assert_eq!(p.qty, 0.0149, "the simulated size is untouched");
        assert_eq!(b.fees_paid, 2.30, "the modelled fee belongs to the model");
    }

    #[test]
    fn a_partial_fill_corrects_the_wallet_claim_only() {
        let mut b = long_book(0.0149, 2.40);
        // A QUARTER fill, deliberately not a half: at exactly half, scaling
        // by the filled share and by the unfilled share give the same number,
        // so a half-fill fixture cannot tell a right answer from a wrong one.
        let got = 0.003725;
        assert_eq!(
            settle_live_buy(&mut b, 0.0149, got),
            Settled::Partial { ordered: 0.0149, got }
        );
        let p = b.position.as_ref().unwrap();
        assert_eq!(p.live_qty, got, "the wallet holds this much and no more");
        assert_eq!(p.qty, 0.0149, "the simulation is not resized by a fill");
        assert_eq!(p.entry_fee, 2.40, "nor is its fee");
        assert_eq!(b.fees_paid, 2.40);
    }

    #[test]
    fn the_2026_09_22_eth_entry_leaves_the_simulation_at_full_size() {
        // Real numbers, read back from Kraken ClosedOrders:
        //   O4MMNL-PYL4C-OMSGFN  canceled  buy ETHUSD
        //   vol 0.01488212  vol_exec 0.00100000
        // The old code wrote that 0.001 into the SIMULATION, so a $1,000 book
        // reported the P&L of a $2.70 trade while still carrying a fee
        // computed on $1,000 -- the mixing this whole change exists to end.
        let mut b = long_book(0.37090725, 2.30);
        let got = 0.00100000;
        assert_eq!(
            settle_live_buy(&mut b, 0.01488212, got),
            Settled::Partial { ordered: 0.01488212, got }
        );
        let p = b.position.as_ref().unwrap();
        assert_eq!(p.live_qty, got, "the wallet got 0.001 ETH");
        assert_eq!(p.qty, 0.37090725, "the simulation still holds $1,000 of ETH");
        assert_eq!(p.entry_fee, 2.30, "which is what the entry fee was charged on");
    }

    #[test]
    fn a_fill_short_by_exactly_the_tolerance_counts_as_full() {
        // The boundary itself. `FILL_TOL` exists because both numbers are
        // decimal strings that round-tripped through f64, so a shortfall of
        // exactly one tolerance is the rounding it was written for -- not a
        // partial fill to chase. Doubling is exact in binary, so this lands
        // on the comparison rather than near it.
        let mut b = long_book(0.0149, 2.30);
        assert_eq!(
            settle_live_buy(&mut b, 2.0 * FILL_TOL, FILL_TOL),
            Settled::Filled
        );
        assert_eq!(b.position.as_ref().unwrap().live_qty, 0.0149, "claim untouched");
    }

    #[test]
    fn a_partial_fill_is_not_rounded_away_to_a_full_one() {
        // A fill one satoshi short is still partial. If this ever reports
        // Filled, the book claims wallet inventory that is not there and the
        // exit sizes a sell against it.
        let mut b = long_book(0.0149, 2.30);
        assert!(matches!(
            settle_live_buy(&mut b, 0.0149, 0.0149 - 1e-6),
            Settled::Partial { .. }
        ));
    }

    #[test]
    fn an_order_recorded_before_qty_was_tracked_changes_nothing() {
        // `qty` is `#[serde(default)]`, so orders already in the live
        // state.json deserialize with 0.0. That means "unknown", not "ordered
        // nothing" -- reading it as the latter would drop a real wallet claim
        // on the first tick after deploy and strand the coins.
        let mut b = long_book(0.0149, 2.30);
        assert_eq!(settle_live_buy(&mut b, 0.0, 0.0), Settled::Filled);
        let p = b.position.as_ref().expect("must not wipe a live book on upgrade");
        assert_eq!(p.live_qty, 0.0149);
    }

    #[test]
    fn a_nonsense_fill_volume_leaves_the_book_alone() {
        // NaN compares false against everything, so an unguarded version falls
        // all the way through to `Nothing` and drops a REAL wallet claim
        // because a string failed to parse. Both sides have to be checked.
        for (ordered, got) in [(0.0149, f64::NAN), (f64::NAN, 0.0), (0.0149, f64::INFINITY)] {
            let mut b = long_book(0.0149, 2.30);
            assert_eq!(settle_live_buy(&mut b, ordered, got), Settled::Filled);
            assert_eq!(
                b.position.as_ref().unwrap().live_qty,
                0.0149,
                "ordered={ordered} got={got}"
            );
        }
    }

    #[test]
    fn settling_a_book_that_is_already_flat_does_not_panic() {
        // The position can be gone by the time the order settles -- the
        // strategy may have closed it on a later bar.
        let mut b = Book::new("eth_1h_sf", "ETHUSD", "structure_filtered");
        assert_eq!(settle_live_buy(&mut b, 0.0149, 0.0), Settled::Nothing);
        assert!(b.position.is_none());
    }

    fn bar(time: i64, close: f64) -> Bar {
        Bar { time, open: close, high: close, low: close, close, volume: 1.0 }
    }

    #[test]
    fn the_entry_fee_is_charged_on_the_position_that_was_opened() {
        // The bug, at its source. The entry fee used to be `fee(NOTIONAL)` --
        // a constant -- while the exit charged `p.qty * px`. As long as
        // nothing resized `qty` the two agreed; the moment the live path did,
        // a $2.70 position carried a $0.16 entry fee and the book reported a
        // loss on a trade the signal got right.
        let mut b = Book::new("eth_1h_sf", "ETHUSD", "structure_filtered");
        let mut log = Vec::new();
        enter(&mut b, 1, &bar(0, 2_696.09), true, &mut log);
        let p = b.position.as_ref().unwrap();
        assert!((p.qty * p.entry - NOTIONAL).abs() < 1e-9, "entries are NOTIONAL-sized");
        assert!(
            (p.entry_fee - fee(p.qty * p.entry, true)).abs() < 1e-12,
            "fee {} is not the fee on this position",
            p.entry_fee
        );
    }

    #[test]
    fn a_full_round_trip_reports_the_simulation_not_the_wallet() {
        // The 2026-09-22 ETH trade as the simulation sees it: in at 2696.09,
        // out at 2748.29, $1,000 of notional. The live path filled 0.001 of
        // it and the book printed -$0.11. At the size the book actually
        // simulates, the same signal made about +$14.71.
        let mut b = Book::new("eth_1h_sf", "ETHUSD", "structure_filtered");
        let mut log = Vec::new();
        enter(&mut b, 1, &bar(0, 2_696.09), true, &mut log);
        // What the live path is now allowed to touch, and all it may touch.
        b.position.as_mut().unwrap().live_qty = 0.001;
        exit(&mut b, &bar(3_600, 2_748.29), true, "TIME", &mut log);

        let t = b.trades.last().unwrap();
        assert!((t.qty * t.entry - NOTIONAL).abs() < 1e-9, "closed at paper size");
        assert!((t.pnl - 14.71).abs() < 0.02, "pnl {}", t.pnl);
        assert!(t.pnl > 0.0, "the signal was right and the book must say so");
        assert!((t.fees - 4.65).abs() < 0.02, "fees {}", t.fees);
    }

    #[test]
    fn a_second_tick_inside_one_bar_marks_the_same_book_as_the_first() {
        // The loop wakes every 60s and only TRADES on a new closed bar, so
        // most marks come from this path. It used to use its own formula --
        // cash plus the whole position rather than the position's move net of
        // the entry fee -- so the equity series jumped by a thousand dollars
        // between two ticks of the same bar and then jumped back.
        let mut b = Book::new("sol_bh", "SOLUSD", "buy_hold");
        let bars = flat_bars(60, 109.91);
        let now = 60 * 3_600 + 4_000;
        step_book(&mut b, &bars, now);
        let first = *b.marks.last().expect("the closing mark");
        step_book(&mut b, &bars, now);
        let second = *b.marks.last().expect("the intra-bar mark");
        assert_eq!(second.0, first.0, "same bar");
        assert!(
            (second.1 - first.1).abs() < 1e-9,
            "equity jumped from {} to {} inside one bar",
            first.1,
            second.1
        );
        assert!((second.1 - b.equity(109.91)).abs() < 1e-9);
    }

    #[test]
    fn an_intra_bar_mark_uses_the_same_equity_as_the_closing_one() {
        // The same-bar branch of `step_book` had its own formula -- cash plus
        // the whole position, rather than the position's MOVE net of the
        // entry fee -- so the marks series jumped by a thousand dollars
        // between two ticks of the same bar.
        let mut b = Book::new("eth_1h_sf", "ETHUSD", "structure_filtered");
        let mut log = Vec::new();
        enter(&mut b, 1, &bar(0, 2_696.09), true, &mut log);
        mark(&mut b, 2_748.29, 3_600);
        let (_, eq) = *b.marks.last().unwrap();
        assert!((eq - b.equity(2_748.29)).abs() < 1e-12);
        assert!((eq - NOTIONAL).abs() < 50.0, "equity {eq} is not on the book's scale");
    }

    fn mixed_state() -> State {
        // The live state.json as it stands: sol_bh's simulated 9.098 SOL was
        // overwritten every cycle with the wallet's dust, which pinned the
        // buy-and-hold benchmark at a flat -$3.90 while SOL moved.
        let mut s = State::default_paper();
        s.mode = "live".into();
        for b in &mut s.books {
            if b.id == "sol_bh" {
                b.fees_paid = 3.90;
                b.position = Some(Position {
                    side: 1,
                    qty: 0.0000093939,
                    entry: 109.91,
                    entry_ts: 0,
                    entry_bar: 0,
                    entry_fee: 3.90,
                    live_qty: 0.0,
                });
            }
        }
        s
    }

    fn flat_bars(n: usize, px: f64) -> Vec<Bar> {
        (0..n).map(|k| bar(k as i64 * 3_600, px)).collect()
    }

    #[test]
    fn stepping_a_book_puts_an_off_scale_position_back_on_scale() {
        // Defence in depth for the invariant above. The live path no longer
        // writes `qty`, but it did for two days and nothing noticed, so the
        // stepper checks rather than trusts.
        let mut b = Book::new("sol_bh", "SOLUSD", "buy_hold");
        b.position = Some(Position {
            side: 1, qty: 0.0000093939, entry: 109.91, entry_ts: 0, entry_bar: 0,
            entry_fee: 3.90, live_qty: 0.0,
        });
        let bars = flat_bars(60, 109.91);
        let log = step_book(&mut b, &bars, 60 * 3_600 + 4_000);
        assert!(log.iter().any(|l| l.contains("REPAIR")), "{log:?}");
        assert!((b.position.as_ref().unwrap().qty - NOTIONAL / 109.91).abs() < 1e-9);
    }

    #[test]
    fn stepping_a_healthy_book_repairs_nothing() {
        let mut b = Book::new("sol_bh", "SOLUSD", "buy_hold");
        let bars = flat_bars(60, 109.91);
        step_book(&mut b, &bars, 60 * 3_600 + 4_000);
        let log = step_book(&mut b, &bars, 60 * 3_600 + 4_000);
        assert!(!log.iter().any(|l| l.contains("REPAIR")), "{log:?}");
    }

    #[test]
    fn migration_stamps_the_epoch_both_sets_of_books_start_from() {
        let mut s = mixed_state();
        let notes = migrate(&mut s, "2026-09-23");
        assert_eq!(s.live.since, "2026-09-23");
        assert_eq!(s.paper_clean_since, "2026-09-23");
        assert!(notes.iter().any(|n| n.contains("not comparable")));
    }

    #[test]
    fn migration_never_rewrites_a_recorded_trade() {
        // The mixed trades cannot be recovered as either kind of number, so
        // they stay exactly as written and the epoch says which side of the
        // fix they are on. Silently restating them would be worse than
        // leaving them wrong.
        let mut s = mixed_state();
        s.books[1].trades.push(Trade {
            ts: 1_790_064_000,
            side: 1,
            entry: 2_696.09,
            exit: 2_748.29,
            qty: 0.001,
            pnl: -0.10866894055564948,
            fees: 0.16086894055564932,
            reason: "TIME".into(),
        });
        migrate(&mut s, "2026-09-23");
        let t = &s.books[1].trades[0];
        assert_eq!(t.pnl, -0.10866894055564948, "history is left alone");
        assert_eq!(t.qty, 0.001);
    }

    #[test]
    fn migration_runs_once_and_never_moves_the_epoch_again() {
        let mut s = mixed_state();
        migrate(&mut s, "2026-09-23");
        let notes = migrate(&mut s, "2026-11-01");
        assert!(notes.is_empty(), "a second run must be a no-op");
        assert_eq!(s.live.since, "2026-09-23", "the epoch cannot drift forward");
    }

    #[test]
    fn migration_revives_the_buy_and_hold_benchmark() {
        // 9.098 SOL is what `enter` wrote and what the benchmark means. The
        // signals are judged against this book, so a dead one makes the whole
        // comparison unreadable -- it was stuck at exactly -$3.90 forever.
        let mut s = mixed_state();
        let notes = migrate(&mut s, "2026-09-23");
        let bh = s.books.iter().find(|b| b.id == "sol_bh").unwrap();
        let p = bh.position.as_ref().unwrap();
        assert!((p.qty - NOTIONAL / 109.91).abs() < 1e-9, "qty {}", p.qty);
        assert!(notes.iter().any(|n| n.contains("REPAIR")));
        // ...and it now MOVES with SOL instead of printing a constant.
        assert!(bh.equity(130.0) > bh.equity(110.0) + 100.0);
    }

    #[test]
    fn a_repair_restores_an_entry_fee_that_was_scaled_to_a_fill() {
        // `settle_buy` used to scale `entry_fee` by the filled share, so a
        // 6.7% fill left a $1,000 position carrying a $0.15 fee.
        let mut b = long_book(0.001, 0.1546);
        b.fees_paid = 0.1546;
        let note = repair_open_position(&mut b).expect("must repair");
        let p = b.position.as_ref().unwrap();
        assert!((p.qty - NOTIONAL / 2_696.09).abs() < 1e-9);
        assert!((p.entry_fee - fee(NOTIONAL, true)).abs() < 1e-12);
        assert!((b.fees_paid - fee(NOTIONAL, true)).abs() < 1e-12, "fees {}", b.fees_paid);
        assert!(note.contains("REPAIR"));
    }

    #[test]
    fn a_repair_never_inflates_a_fee_that_was_already_charged() {
        // sol_bh entered as a TAKER: $3.90, higher than the maker fee the
        // repair would write. Raising a recorded cost is inventing one, so
        // the larger number stands and only the qty is restored.
        let mut b = Book::new("sol_bh", "SOLUSD", "buy_hold");
        b.fees_paid = 3.90;
        b.position = Some(Position {
            side: 1, qty: 0.0, entry: 109.91, entry_ts: 0, entry_bar: 0,
            entry_fee: 3.90, live_qty: 0.0,
        });
        repair_open_position(&mut b).expect("qty is still wrong");
        assert_eq!(b.position.as_ref().unwrap().entry_fee, 3.90);
        assert_eq!(b.fees_paid, 3.90);
    }

    #[test]
    fn a_healthy_book_is_not_repaired() {
        // The repair must be invisible after the first run, or every deploy
        // would nudge a correct book and log a scary line about it.
        let mut b = long_book(NOTIONAL / 2_696.09, fee(NOTIONAL, true));
        assert!(repair_open_position(&mut b).is_none());
        let mut flat = Book::new("sol_1h_tl", "SOLUSD", "trendline_break");
        assert!(repair_open_position(&mut flat).is_none());
    }

    #[test]
    fn a_repair_refuses_a_position_with_no_usable_entry_price() {
        // `NOTIONAL / entry` is the whole formula. A zero or NaN entry makes
        // it infinite, which would be a far worse book than the broken one.
        for entry in [0.0, -1.0, f64::NAN] {
            let mut b = long_book(0.001, 0.15);
            b.position.as_mut().unwrap().entry = entry;
            assert!(repair_open_position(&mut b).is_none(), "entry={entry}");
            assert!(b.position.as_ref().unwrap().qty.is_finite());
        }
    }

    #[test]
    fn the_status_line_reports_the_live_sleeve_separately_from_the_books() {
        use crate::ledger::{Fill, Sleeve};
        let mut s = mixed_state();
        migrate(&mut s, "2026-09-23");
        let f = |txid: &str, side: i8, cost: f64, fee: f64| Fill {
            txid: txid.into(), ts: 0, pair: "ETHUSD".into(), side,
            sleeve: Sleeve::Trade, book: Some("eth_1h_sf".into()),
            qty: 0.001, cost, fee,
        };
        s.live.record(f("B1", 1, 2.69609, 0.00701));
        s.live.record(f("S1", -1, 2.74829, 0.00714));
        let lines = live_ledger_lines(&s, &[("ETHUSD".into(), 2_748.29)]);
        assert!(lines[0].contains("since 2026-09-23"));
        let trade = lines.iter().find(|l| l.contains("trade")).expect("a trade line");
        assert!(trade.contains("fills 2"), "{trade}");
        // Net of REAL fees the round trip made a few cents. The book that
        // mixed the two printed -$0.11 on the same fills.
        assert!(trade.contains("net  +0.0380"), "{trade}");
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
