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
    /// UTC date `YYYY-MM-DD` of the last BTC 70/30 rebalance attempt.
    #[serde(default)]
    pub last_btc_rebalance: String,
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


