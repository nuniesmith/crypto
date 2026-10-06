//! Persistent paper books: a $1,000-per-book SIMULATION, kept as HISTORY.
//!
//! Through 2026-10-04 these books were stepped every cycle and the live path
//! read their open/close decisions to place real orders. **Since the
//! 2026-10-05 policy (`alloc.rs`), nothing steps them and nothing reads a
//! decision out of them**: `main.rs` no longer fetches 1h bars or calls
//! `step_book`, and the live wallet is sized entirely from `alloc::gap_step`
//! and friends instead. `State::books` stays in the file exactly as it was
//! left — untouched, not deleted, not migrated — because it is a real record
//! of what those signals did, and inventing a reason to touch it would only
//! risk the one thing this file still guarantees: that old data means
//! exactly what it always meant. `PendingOrder`, `plan_settlement` and
//! `disposition` are the parts of this file the live path still uses, for
//! the post-only coin/deposit/stablecoin orders `main.rs` places now.
//!
//! Every book always traded `NOTIONAL`. That invariant is the only reason the
//! historical numbers mean anything: the question these books were built to
//! answer is "does this signal have an edge at a size worth trading?", which
//! cannot be answered by a book whose size is whatever a small wallet could
//! afford that hour.

use std::fs;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::ledger::{Execution, LiveLedger, Sleeve, Totals};

// Kraken TIER 1 -- the account's ACTUAL schedule, confirmed 2026-09-23.
//
// These were 0.0022 / 0.0038, roughly tier 3, so the simulation charged about
// HALF what the account really pays: maker understated 1.82x, taker 2.11x.
// Every "is this +EV" judgement the books have ever printed was made at fees
// the account does not get.
//
// This is not a rounding matter. A maker round trip costs 0.80%, not 0.46% --
// the move a signal must beat before it earns anything nearly doubles. The
// Python research (src/crypto/sim/fees.py) already models tier 1 as exactly
// (0.0040, 0.0080) and its verdict at that tier is blunt: "Retail taker
// (tier 1) is a dead end: 0/7 default combos were +EV in-sample."
//
// Kept only for `fee()`, below, which `repair_open_position` still uses on
// historical data — nothing new is ever priced at these any more.
pub const MAKER_FEE: f64 = 0.0040;
pub const TAKER_FEE: f64 = 0.0080;
pub const SLIP: f64 = 0.0001;
pub const NOTIONAL: f64 = 1_000.0;

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
    ///
    /// Vestigial since the 2026-10-05 policy (`alloc.rs`): BTC now trades on
    /// the same regime-flip rule as every other coin, with no separate band
    /// check. Left in the file format rather than removed — see
    /// `State::last_work_hour` for what replaced it.
    #[serde(default)]
    pub last_btc_rebalance: String,
    /// UTC date `YYYY-MM-DD` the regime rule last evaluated every coin.
    #[serde(default)]
    pub last_regime_day: String,
    /// The regime (true = bull) each pair was last SIZED for — i.e. fully
    /// moved to its effective target, within one minimum order.
    ///
    /// A pair trades only when this disagrees with `regime_bull`: an ordinary
    /// flip changes `regime_bull` and leaves this at the old value; the
    /// one-time policy move and the operator's `rebalance` command instead
    /// clear this map outright, so every pair disagrees regardless of
    /// whether its regime actually changed. Either way the work tick closes
    /// the gap and then writes this to match, and drift after that is left
    /// alone — see `alloc.rs`.
    #[serde(default)]
    pub regime_applied: std::collections::BTreeMap<String, bool>,
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

    // ── 2026-10-05 policy: one account, BTC+ETH+SOL+cash targets ──────────
    /// Which wallet policy `state.json` has been moved to. `0` (the serde
    /// default, so every pre-2026-10-05 file reads as this) means the old
    /// two-sleeve policy; `2` means the one-time move in `alloc.rs`'s module
    /// docs has run. There is no `1` — the version jumps straight to the one
    /// this file format actually describes, so a reader never has to wonder
    /// whether an intermediate policy applies to data on disk.
    #[serde(default)]
    pub policy_version: u32,
    /// Each pair's regime as of the last successful daily read. Compare
    /// against `regime_applied` to find a pair with unfinished work. Absent
    /// = never read yet (distinct from a stale reading, which this never
    /// holds — a failed read leaves the previous value in place untouched).
    #[serde(default)]
    pub regime_bull: std::collections::BTreeMap<String, bool>,
    /// The FULL last reading (close, 200d average, bull/bear) behind each
    /// `regime_bull` entry, kept only for Discord's regime section — trading
    /// decisions use `regime_bull` alone. Updated alongside it, never
    /// independently.
    #[serde(default)]
    pub regime_reading: std::collections::BTreeMap<String, crate::regime::Reading>,
    /// USD still waiting to be invested from a deposit, across BTC/ETH/SOL by
    /// their effective target weights. Reduced only by what an order actually
    /// FILLED (cost + fee), never by what was ordered — see `alloc::invest`.
    #[serde(default)]
    pub deposit_backlog_usd: f64,
    /// Every deposit/withdrawal this bot has recorded from Kraken's Ledgers,
    /// most recent last. This is ALSO the dedup record: a refid already in
    /// here is never applied twice, even across a restart or a re-fetched
    /// ledger page.
    #[serde(default)]
    pub flows: Vec<FlowRecord>,
    /// The latest Kraken ledger `time` (Unix seconds, fractional) this bot has
    /// scanned, so each scan asks Kraken for only what might be newer.
    /// Deliberately not the dedup mechanism by itself — `flows` is — because
    /// Kraken's `start` filter is exclusive and float time can tie, so a scan
    /// may legitimately re-see an entry it already recorded.
    #[serde(default)]
    pub last_ledger_time: f64,
    /// Deposits minus withdrawals, in USD, since this bot started watching.
    /// The denominator a return (as opposed to a balance change) needs: a
    /// $500 deposit must not read as a $500 gain.
    #[serde(default)]
    pub net_deposits_usd: f64,
    /// One snapshot per UTC day — total value, each asset's qty and mark, and
    /// net deposits to date — shaped for a future web UI's daily / weekly /
    /// monthly / yearly returns net of deposits. No UI reads this yet.
    #[serde(default)]
    pub history: Vec<DailySnapshot>,
    /// UTC date `YYYY-MM-DD` `history` last gained an entry, so it appends
    /// once per day regardless of how often the tick loop wakes.
    #[serde(default)]
    pub last_history_day: String,
    /// UTC hour (`%Y-%m-%dT%H`) the work tick last ran. Replaces the 1h-bar
    /// trigger: order SETTLEMENT still runs every wake (see `main.rs`), but
    /// deciding what to trade runs once per UTC hour and once on startup.
    #[serde(default)]
    pub last_work_hour: String,
}

/// One real deposit or withdrawal, as Kraken's Ledgers reported it.
///
/// Kept forever (not capped like `LiveLedger::fills`): deposits are rare
/// enough that this never grows large, and it is the only record of what the
/// operator put in or took out, which a return calculation needs for good.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowKind {
    Deposit,
    Withdrawal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlowRecord {
    /// Kraken's ledger id. The dedup key: `flows` is scanned for this before
    /// a newly-fetched ledger entry is ever applied.
    pub refid: String,
    /// Unix seconds (Kraken's ledger `time`, truncated).
    pub ts: i64,
    pub kind: FlowKind,
    /// Kraken's asset code, normalised to USD/USDC/USDT/BTC/ETH/SOL where
    /// recognised, or left as Kraken sent it otherwise.
    pub asset: String,
    /// Signed amount in `asset`'s own units (Kraken's sign: a withdrawal is
    /// negative). Kept signed so a reader can tell the two kinds apart even
    /// without the `kind` field.
    pub amount: f64,
    /// USD value at the time: 1:1 for USD/USDC/USDT, `amount * mark` for a
    /// crypto deposit, `0.0` for an asset this bot has no mark for (logged
    /// as a warning where it happens, never guessed).
    pub usd_value: f64,
}

/// One UTC day's account snapshot for a future performance UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DailySnapshot {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub total_usd: f64,
    /// Asset code ("BTC"/"ETH"/"SOL"/"USD"/"USDC"/"USDT") → quantity held.
    pub qty: std::collections::BTreeMap<String, f64>,
    /// Asset code → USD mark used to value it that day (1.0 for the
    /// stablecoins and USD itself, included anyway so a reader never has to
    /// assume it).
    pub mark: std::collections::BTreeMap<String, f64>,
    /// `State::net_deposits_usd` at the moment this snapshot was taken.
    pub net_deposits_usd_to_date: f64,
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
            last_regime_day: String::new(),
            regime_applied: Default::default(),
            pending_orders: Vec::new(),
            live: LiveLedger::default(),
            paper_clean_since: String::new(),
            policy_version: 0,
            regime_bull: Default::default(),
            regime_reading: Default::default(),
            deposit_backlog_usd: 0.0,
            flows: Vec::new(),
            last_ledger_time: 0.0,
            net_deposits_usd: 0.0,
            history: Vec::new(),
            last_history_day: String::new(),
            last_work_hour: String::new(),
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
mod fee_tests {
    use super::*;

    #[test]
    fn fees_match_the_accounts_real_kraken_tier() {
        // Tier 1: maker 0.40%, taker 0.80%. The books previously used 0.22% /
        // 0.38% -- about tier 3 -- and so reported roughly half the true cost
        // of every trade. A flattering fee makes a losing strategy look
        // marginal, which is the one error this file must not make.
        assert_eq!(MAKER_FEE, 0.0040);
        assert_eq!(TAKER_FEE, 0.0080);
    }

    #[test]
    fn a_maker_round_trip_costs_what_tier_one_charges() {
        // What the signal must beat before it earns anything. At the old
        // constants this was 0.46%; it is really 0.80% plus slippage.
        let notional = 1_000.0;
        let round_trip = fee(notional, true) * 2.0;
        assert!((round_trip - 8.2).abs() < 1e-9, "got {round_trip}");
    }

    #[test]
    fn taker_costs_strictly_more_than_maker() {
        assert!(fee(1_000.0, false) > fee(1_000.0, true));
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
        Execution {
            vol_exec: vol,
            cost: vol * 2_700.0,
            fee: vol * 2_700.0 * 0.0026,
            at: None,
        }
    }

    fn fate(placed_at: i64, executed: Option<f64>, now: i64) -> Disposition {
        disposition(
            &order("A", placed_at),
            executed.map(exec),
            now,
            TTL,
            GIVE_UP,
        )
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
        assert_eq!(
            fate(1_000, Some(0.01), 1_100),
            Disposition::Settle(exec(0.01))
        );
        assert_eq!(
            fate(0, Some(0.01), 200_000),
            Disposition::Settle(exec(0.01))
        );
    }

    #[test]
    fn a_closed_order_that_filled_nothing_still_settles() {
        // The bug this whole path exists for: `Some(0.0)` is a FACT about the
        // order (it closed empty) and must not be confused with `None`
        // (still open). Collapsing them leaves the book long a phantom.
        assert_eq!(
            fate(1_000, Some(0.0), 1_100),
            Disposition::Settle(exec(0.0))
        );
    }

    #[test]
    fn a_cancelled_order_keeps_being_tracked_until_it_settles() {
        // Cancel does not end our interest in an order. Kraken reports the
        // executed volume only once the order is CLOSED, and the cancel is
        // what closes it -- so the tick that cancels cannot also settle.
        assert_eq!(fate(1_000, None, 1_700), Disposition::Cancel);
        // next tick, now closed:
        assert_eq!(
            fate(1_000, Some(0.004), 1_760),
            Disposition::Settle(exec(0.004))
        );
    }

    fn plan(
        orders: Vec<PendingOrder>,
        executed: &[(&str, f64)],
        now: i64,
    ) -> (Vec<OrderAction>, Vec<PendingOrder>) {
        let map = executed
            .iter()
            .map(|(t, v)| (t.to_string(), exec(*v)))
            .collect();
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
        assert_eq!(
            actions,
            vec![OrderAction::Settle(order("A", 1_000), exec(0.01))]
        );
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
        assert!(
            tracked.is_empty(),
            "the point of giving up is to stop leaking"
        );
    }

    #[test]
    fn a_mixed_cycle_routes_each_order_to_its_own_fate() {
        let (actions, tracked) = plan(
            vec![
                order("filled", 1_000),
                order("stale", 1_000),
                order("young", 1_650),
            ],
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
    fn a_fill_is_matched_to_its_own_txid() {
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
    fn a_repair_restores_an_entry_fee_that_was_scaled_to_a_fill() {
        // `settle_buy` used to scale `entry_fee` by the filled share, so a
        // 6.7% fill left a $1,000 position carrying a $0.15 fee.
        let mut b = long_book(0.001, 0.1546);
        b.fees_paid = 0.1546;
        let note = repair_open_position(&mut b).expect("must repair");
        let p = b.position.as_ref().unwrap();
        assert!((p.qty - NOTIONAL / 2_696.09).abs() < 1e-9);
        assert!((p.entry_fee - fee(NOTIONAL, true)).abs() < 1e-12);
        assert!(
            (b.fees_paid - fee(NOTIONAL, true)).abs() < 1e-12,
            "fees {}",
            b.fees_paid
        );
        assert!(note.contains("REPAIR"));
    }

    #[test]
    fn a_repair_never_inflates_a_fee_that_was_already_charged() {
        // sol_bh entered as a TAKER: $8.10 at tier 1, higher than the $4.10
        // maker fee the repair would write. Raising a recorded cost is
        // inventing one, so the larger number stands and only the qty is
        // restored.
        //
        // The fixture used to say $3.90, a tier-3 taker fee. Once the
        // constants were corrected that was BELOW the tier-1 maker fee, so
        // the test stopped exercising its own invariant -- it would have
        // passed by measuring the repair raising a fee, which is the thing it
        // exists to forbid.
        let mut b = Book::new("sol_bh", "SOLUSD", "buy_hold");
        b.fees_paid = 8.10;
        b.position = Some(Position {
            side: 1,
            qty: 0.0,
            entry: 109.91,
            entry_ts: 0,
            entry_bar: 0,
            entry_fee: 8.10,
            live_qty: 0.0,
        });
        repair_open_position(&mut b).expect("qty is still wrong");
        assert_eq!(b.position.as_ref().unwrap().entry_fee, 8.10);
        assert_eq!(b.fees_paid, 8.10);
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
            txid: txid.into(),
            ts: 0,
            pair: "ETHUSD".into(),
            side,
            sleeve: Sleeve::Trade,
            book: Some("eth_1h_sf".into()),
            qty: 0.001,
            cost,
            fee,
        };
        s.live.record(f("B1", 1, 2.69609, 0.00701));
        s.live.record(f("S1", -1, 2.74829, 0.00714));
        let lines = live_ledger_lines(&s, &[("ETHUSD".into(), 2_748.29)]);
        assert!(lines[0].contains("since 2026-09-23"));
        let trade = lines
            .iter()
            .find(|l| l.contains("trade"))
            .expect("a trade line");
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

    #[test]
    fn a_pre_policy_state_file_loads_with_books_and_live_intact() {
        // `tests/fixtures/state_v1.json` was serialized from the struct shape
        // as it stood immediately BEFORE the 2026-10-05 one-account policy —
        // built in code from invented values, never from the live checkout's
        // data. Every field this change adds must default, and every field
        // that already existed must round-trip exactly.
        let raw = include_str!("../tests/fixtures/state_v1.json");
        let s: State = serde_json::from_str(raw).expect("a pre-policy state.json must still parse");

        // New fields default as though the move has never run.
        assert_eq!(
            s.policy_version, 0,
            "an old file predates every policy version"
        );
        assert!(s.regime_bull.is_empty());
        assert_eq!(s.deposit_backlog_usd, 0.0);
        assert!(s.flows.is_empty());
        assert_eq!(s.last_ledger_time, 0.0);
        assert_eq!(s.net_deposits_usd, 0.0);
        assert!(s.history.is_empty());
        assert_eq!(s.last_history_day, "");
        assert_eq!(s.last_work_hour, "");

        // Old data is untouched — `load_state`'s `migrate` is a no-op here
        // because the fixture already has `live.since` set.
        assert_eq!(s.books.len(), 3, "the historical books must still be there");
        assert_eq!(s.books[0].id, "sol_1h_tl");
        assert!(
            s.books[0].position.is_some(),
            "an open paper position must survive"
        );
        assert_eq!(
            s.books[0].trades.len(),
            1,
            "recorded trade history must not be rewritten"
        );
        assert_eq!(s.live.since, "2026-09-23");
        assert_eq!(
            s.live.fills.len(),
            1,
            "the one real fill on file must survive"
        );
        assert_eq!(s.pending_orders.len(), 1);
        assert_eq!(s.regime_applied.get("ETHUSD"), Some(&true));

        // `policy_version < 2` is exactly the condition `main.rs` checks to
        // start the one-time move (see `main::maybe_migrate_policy` and its
        // own test against this same fixture).
        assert!(
            s.policy_version < 2,
            "the one-time move has not run on this file yet"
        );
    }
}
