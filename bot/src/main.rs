//! Paper (default) / live-gated Kraken bot.
//!
//! Crates: `exchange-apiws` (Kraken), `indicators-ta` (EMA/ATR/VWAP),
//! `rustrade-framework` (Brain + ExchangeClient for the live path).
//!
//! Live wallet: BTC HODL 70/30 ±10% vs USD (no 1h signals). ETH/SOL 1h books
//! trade the Kraken pile only. sol_bh is mark-only (no $1k live buy).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use exchange_apiws::KrakenRestClient;
use tracing::{info, warn};

mod alloc;
mod brains;
mod discord;
mod features;
mod kraken_src;
mod live;
mod paper;
mod paper_ex;
mod signal;

use features::Bar;
use paper::{
    append_journal, fmt_ts, load_state, print_status, save_state, step_book,
};

fn load_dotenv() {
    // The repo's own .env, resolved from where this binary was COMPILED, not
    // from the cwd — the systemd unit sets a working directory but a hand-run
    // binary need not, and the keys have to be found either way.
    //
    // This used to be the literal string "/home/jordan/github/crypto/.env",
    // which meant any copy of this tree still read the ORIGINAL checkout's
    // secrets. That is how a supposedly isolated dry-run build ended up
    // posting to the real Discord webhook: the copy's own .env was never
    // consulted, because the hardcoded path had already set every key.
    let repo_env = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| root.join(".env"))
        .unwrap_or_else(|| PathBuf::from(".env"));
    for p in [repo_env.as_path(), Path::new(".env")] {
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().trim_matches('"');
                if std::env::var(k.trim()).is_err() {
                    std::env::set_var(k.trim(), v);
                }
            }
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "crypto-bot — Kraken paper/live
  exchange-apiws + indicators-ta + rustrade

Usage:
  crypto-bot paper              one cycle (fetch 1h bars, maybe trade, save)
  crypto-bot paper --loop       repeat every 60s (acts only on a new closed 1h bar)
  crypto-bot status             print books / equity
  crypto-bot report             send a Discord snapshot now (needs DISCORD_WEBHOOK_URL)
  crypto-bot live --dry-run     paper fills + log would-be Kraken orders
  crypto-bot live --confirm I_UNDERSTAND_REAL_MONEY
                                real Kraken limit orders (KRAKEN_API_KEY/SECRET)

Default is paper. Live refuses to start without the exact confirm string.
Discord: set DISCORD_WEBHOOK_URL for daily (15:00 UTC), weekly (Mon), monthly (1st).
Live reports fetch Kraken /0/private/Balance first.
Live sizing: BTC HODL 70/30 ±10%; ETH/SOL trade the wallet pile; no $1k buys.
"
    );
    std::process::exit(2);
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    load_dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    let cmd = args.remove(0);
    match cmd.as_str() {
        "paper" => {
            let looping = args.iter().any(|a| a == "--loop");
            run(Mode::Paper, looping).await
        }
        "status" => status_cmd().await,
        "report" => report_cmd().await,
        "live" => {
            let dry = args.iter().any(|a| a == "--dry-run");
            let confirm = args
                .windows(2)
                .any(|w| w[0] == "--confirm" && w[1] == "I_UNDERSTAND_REAL_MONEY");
            if dry {
                run(Mode::LiveDry, args.iter().any(|a| a == "--loop")).await
            } else if confirm {
                run(Mode::Live, args.iter().any(|a| a == "--loop")).await
            } else {
                anyhow::bail!(
                    "refusing live. Paper: `crypto-bot paper`. \
                     Dry-run: `crypto-bot live --dry-run`. \
                     Real money: `crypto-bot live --confirm I_UNDERSTAND_REAL_MONEY`"
                );
            }
        }
        _ => usage(),
    }
}

#[derive(Clone, Copy, Debug)]
enum Mode {
    Paper,
    LiveDry,
    Live,
}

async fn status_cmd() -> anyhow::Result<()> {
    let state = load_state()?;
    let client = KrakenRestClient::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let marks = fetch_marks(&client).await?;
    let account = if state.mode == "live" {
        load_account(None, &client, &marks).await
    } else {
        None
    };
    if let Some(a) = &account {
        a.print();
        println!();
    }
    print_status(&state, &marks);
    Ok(())
}

async fn report_cmd() -> anyhow::Result<()> {
    let mut state = load_state()?;
    let client = KrakenRestClient::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let marks = fetch_marks(&client).await?;
    let account = if state.mode == "live" {
        load_account(None, &client, &marks).await
    } else {
        None
    };
    if let Some(a) = &account {
        a.print();
        println!();
    }
    print_status(&state, &marks);
    discord::maybe_report(&mut state, &marks, Some("startup"), account.as_ref()).await;
    save_state(&state)?;
    if discord::webhook_url().is_none() {
        anyhow::bail!("set DISCORD_WEBHOOK_URL to an https://discord.com/api/webhooks/... URL");
    }
    Ok(())
}

async fn run(mode: Mode, looping: bool) -> anyhow::Result<()> {
    let client = KrakenRestClient::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let live_gw = match mode {
        Mode::Live => Some(live::LiveKraken::from_env()?),
        _ => None,
    };
    info!(?mode, looping, "starting bot");
    let mut announced = false;
    loop {
        if let Err(e) = one_cycle(&client, live_gw.as_ref(), mode, &mut announced).await {
            warn!("cycle error: {e:#}");
        }
        if !looping {
            break;
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
    Ok(())
}

async fn one_cycle(
    client: &KrakenRestClient,
    live_gw: Option<&live::LiveKraken>,
    mode: Mode,
    announced: &mut bool,
) -> anyhow::Result<()> {
    let mut state = load_state()?;
    state.mode = match mode {
        Mode::Paper => "paper",
        Mode::LiveDry => "live-dry",
        Mode::Live => "live",
    }
    .into();

    let mut bars_by_pair: HashMap<String, Vec<Bar>> = HashMap::new();
    for pair in ["SOLUSD", "ETHUSD"] {
        bars_by_pair.insert(pair.to_string(), fetch_closed_1h(client, pair).await?);
    }
    let now = Utc::now().timestamp();
    let mut events = Vec::new();
    let mut new_bar = false;

    // Before ANY sizing: release USD held by our own stale orders, so the
    // wallet read below reflects money we can actually spend.
    settle_pending_orders(&mut state, mode, live_gw, now, &mut events).await;

    let marks = fetch_marks(client).await.unwrap_or_default();
    // `fetch_marks` returns an EMPTY vec on any error, so an outage leaves
    // every price at 0.0 rather than stale. `Marks::complete()` is what the
    // policy checks before sizing anything off those zeros.
    let marks_snapshot = alloc::Marks::from_pairs(&marks);
    // ONE gateway for every read this cycle makes.
    //
    // `live_gw` is only built for Mode::Live, so a dry run used to reach
    // Kraken through a throwaway client created inline for the balance call
    // and nowhere else. Anything added afterwards silently did nothing under
    // --dry-run: the open-order check below was exactly that, so a dry run
    // reported $91.99 of trade cash where live would compute $54.60 on the
    // same account. A preview that does not preview is worse than none.
    let owned_gw = match (live_gw, matches!(mode, Mode::Live | Mode::LiveDry)) {
        (None, true) if live::keys_present() => live::LiveKraken::from_env().ok(),
        _ => None,
    };
    let read_gw = live_gw.or(owned_gw.as_ref());

    let mut wallet = match (matches!(mode, Mode::Live | Mode::LiveDry), read_gw) {
        (true, Some(gw)) => gw
            .balances()
            .await
            .ok()
            .map(|b| alloc::Wallet::from_balances(&b)),
        _ => None,
    };

    // Ask Kraken what its open orders have already claimed. Without this the
    // policy sizes against `Balance`, which still counts money an order has
    // spoken for -- measured live at $37.38 held against a $120.41 balance.
    // A failure here leaves `usd_held` at 0, which OVERSTATES what is
    // spendable, so it is logged rather than passed over in silence.
    if let (Some(w), Some(gw)) = (wallet.as_mut(), read_gw) {
        match gw.held_usd().await {
            Ok(held) => w.usd_held = held,
            Err(e) => warn!("open-order check failed ({e:#}) — sizing may overstate free USD"),
        }
    }

    // Staged here rather than pushed straight onto `state.pending_orders`:
    // the loop below holds a mutable borrow of `state.books` for its whole
    // body, so the state struct cannot be touched again until it ends.
    let mut pending: Vec<paper::PendingOrder> = Vec::new();
    for book in &mut state.books {
        let Some(bars) = bars_by_pair.get(&book.pair) else {
            continue;
        };
        let before_n = book.trades.len();
        let before_pos = book.position.is_some();
        let prev_live_qty = book.position.as_ref().map(|p| p.live_qty).unwrap_or(0.0);
        let prev_bar = book.last_closed_bar;
        let notes = step_book(book, bars, now);
        if book.last_closed_bar > prev_bar {
            new_bar = true;
        }
        for n in &notes {
            info!("{n}");
            events.push(n.clone());
        }
        if book.strategy == "buy_hold" {
            if let (Some(w), Some(p)) = (wallet.as_ref(), book.position.as_mut()) {
                p.qty = w.sol;
            }
            continue;
        }
        let opened = !before_pos && book.position.is_some();
        let closed = book.trades.len() > before_n;
        if matches!(mode, Mode::LiveDry | Mode::Live) && (opened || closed) {
            let Some(w) = wallet.as_ref() else {
                warn!("{}: no wallet snapshot — skipping live order", book.id);
                continue;
            };
            let mark = marks
                .iter()
                .find(|(p, _)| p == &book.pair)
                .map(|(_, x)| *x)
                .unwrap_or(0.0);
            let action = alloc::signal_action(
                &book.strategy,
                &book.pair,
                opened,
                closed,
                book.position.as_ref().map(|p| p.side),
                mark,
                w,
                marks_snapshot,
                prev_live_qty,
                alloc::Policy::LIVE,
            );
            match action {
                alloc::LiveAction::None => {}
                alloc::LiveAction::Skip(why) => {
                    info!("{} live skip: {why}", book.id);
                    events.push(format!("{} SKIP {why}", book.id));
                    if opened {
                        if let Some(p) = book.position.take() {
                            book.fees_paid = (book.fees_paid - p.entry_fee).max(0.0);
                        }
                    }
                }
                alloc::LiveAction::Adopt { qty, .. } => {
                    if let Some(p) = book.position.as_mut() {
                        p.qty = qty;
                        p.live_qty = qty;
                    }
                    info!("{} ADOPT inventory qty={qty:.8} (no buy)", book.id);
                    events.push(format!("{} ADOPT {} qty={:.8}", book.id, book.pair, qty));
                }
                alloc::LiveAction::Buy { pair, qty, price } => {
                    if let Some(p) = book.position.as_mut() {
                        p.qty = qty;
                        p.live_qty = qty;
                    }
                    let placed = place_live(mode, live_gw, &pair, 1, qty, price, &mut events).await;
                    if let Placed::Live(txid) = &placed {
                        pending.push(paper::PendingOrder {
                            txid: txid.clone(),
                            pair: pair.clone(),
                            side: 1,
                            placed_at: now,
                            book: Some(book.id.clone()),
                            qty,
                        });
                    }
                    if !placed.acted() {
                        if let Some(p) = book.position.as_mut() {
                            p.live_qty = 0.0;
                        }
                    }
                }
                alloc::LiveAction::Sell { pair, qty, price } => {
                    let placed =
                        place_live(mode, live_gw, &pair, -1, qty, price, &mut events).await;
                    if let Placed::Live(txid) = &placed {
                        pending.push(paper::PendingOrder {
                            txid: txid.clone(),
                            pair: pair.clone(),
                            side: -1,
                            placed_at: now,
                            book: Some(book.id.clone()),
                            qty,
                        });
                    }
                }
            }
        }
    }

    state.pending_orders.append(&mut pending);

    if matches!(mode, Mode::Live | Mode::LiveDry) && new_bar {
        if let Some(w) = wallet.as_ref() {
            let t = alloc::targets(w, marks_snapshot, alloc::Policy::LIVE);
            info!(
                "wallet usd={:.2} btc={:.8} eth={:.8} sol={:.8} | total=${:.2} \
                 hold btc {:.1}% (${:.2} vs ${:.2}) cash ${:.2} | trade cash ${:.2}",
                w.usd,
                w.btc,
                w.eth,
                w.sol,
                t.total,
                100.0 * alloc::btc_weight(w, marks_snapshot, alloc::Policy::LIVE),
                w.btc * marks_snapshot.btc,
                t.btc,
                t.hold_cash,
                alloc::trade_cash_usd(w, marks_snapshot, alloc::Policy::LIVE)
            );
            if w.usd_held > 0.0 {
                info!(
                    "  (${:.2} of that USD is held by open orders — not spendable)",
                    w.usd_held
                );
            }
            maybe_btc_rebalance(&mut state, mode, live_gw, w, marks_snapshot, &mut events).await;
        }
    }

    let mut kinds = Vec::new();
    if !*announced {
        kinds.push("startup".into());
        *announced = true;
    }
    kinds.extend(discord::due_kinds(&state, None));
    let account = if !kinds.is_empty() && matches!(mode, Mode::Live) {
        load_account(live_gw, client, &marks).await
    } else {
        None
    };
    discord::send_kinds(&mut state, &marks, &kinds, account.as_ref()).await;
    save_state(&state)?;
    if new_bar || !events.is_empty() {
        append_journal(&serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "mode": state.mode,
            "events": events,
            "kraken_usd": account.as_ref().map(|a| a.total_usd),
            "equity": state.books.iter().map(|b| {
                let px = marks.iter().find(|(p,_)| p==&b.pair).map(|(_,x)| *x).unwrap_or(0.0);
                serde_json::json!({ "id": b.id, "pair": b.pair, "equity": b.equity(px), "pos": b.position.is_some() })
            }).collect::<Vec<_>>(),
        }))?;
        if let Some(a) = &account {
            a.print();
        }
        print_status(&state, &marks);
        if let Some(b) = bars_by_pair.get("SOLUSD").and_then(|v| v.last()) {
            info!("SOL last closed 1h {} close={:.4}", fmt_ts(b.time), b.close);
        }
    }
    Ok(())
}

async fn load_account(
    live_gw: Option<&live::LiveKraken>,
    client: &KrakenRestClient,
    marks: &[(String, f64)],
) -> Option<live::AccountSnapshot> {
    match live_gw {
        Some(gw) => Some(snapshot_account(gw, client, marks).await),
        None if live::keys_present() => match live::LiveKraken::from_env() {
            Ok(gw) => Some(snapshot_account(&gw, client, marks).await),
            Err(e) => Some(live::AccountSnapshot::failed(e.to_string())),
        },
        None => None,
    }
}

async fn snapshot_account(
    gw: &live::LiveKraken,
    client: &KrakenRestClient,
    marks: &[(String, f64)],
) -> live::AccountSnapshot {
    match gw.balances().await {
        Ok(bals) => {
            let mut marks = marks.to_vec();
            for pair in live::needed_pairs(&bals) {
                if marks.iter().any(|(p, _)| p == &pair) {
                    continue;
                }
                match client.get_ticker(&pair).await {
                    Ok(t) => {
                        if let Some((_, tick)) = t.iter().next() {
                            marks.push((pair, tick.last_price()));
                        }
                    }
                    Err(e) => tracing::debug!("ticker {pair}: {e}"),
                }
            }
            live::value_balances(&bals, &marks)
        }
        Err(e) => live::AccountSnapshot::failed(e.to_string()),
    }
}

async fn fetch_closed_1h(client: &KrakenRestClient, pair: &str) -> anyhow::Result<Vec<Bar>> {
    let ohlc = client
        .get_ohlc(pair, 60)
        .await
        .map_err(|e| anyhow::anyhow!("OHLC {pair}: {e}"))?;
    let now = Utc::now().timestamp();
    let mut bars: Vec<Bar> = ohlc
        .candles
        .iter()
        .map(|c| Bar {
            time: c.time,
            open: c.open_f64(),
            high: c.high_f64(),
            low: c.low_f64(),
            close: c.close_f64(),
            volume: c.volume_f64(),
        })
        .collect();
    if let Some(last) = bars.last() {
        if last.time + 3600 > now {
            bars.pop();
        }
    }
    Ok(bars)
}

/// Outcome of one order attempt.
#[derive(Clone, Debug, PartialEq)]
enum Placed {
    /// Nothing went out — a rejection, or live mode with no gateway.
    No,
    /// Dry run: the decision was made and logged, no order exists.
    Dry,
    /// Accepted by Kraken. The txid is what makes it cancellable later.
    Live(String),
}

impl Placed {
    fn acted(&self) -> bool {
        !matches!(self, Placed::No)
    }
}

async fn place_live(
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    pair: &str,
    side: i8,
    qty: f64,
    price: f64,
    events: &mut Vec<String>,
) -> Placed {
    let side_s = if side > 0 { "buy" } else { "sell" };
    let vol = format!("{:.8}", alloc::floor_qty(qty));
    let px = alloc::limit_price(pair, price);
    let msg = format!(
        "KRAKEN {} {side_s} {pair} vol={vol} px={px} (wallet-capped)",
        match mode {
            Mode::Live => "PLACE",
            _ => "WOULD PLACE",
        }
    );
    info!("{msg}");
    events.push(msg);
    if matches!(mode, Mode::Live) {
        if let Some(gw) = live_gw {
            match gw.place_limit(pair, side, &vol, &px).await {
                Ok(r) => {
                    info!("live order ok: {r}");
                    return Placed::Live(r);
                }
                Err(e) => {
                    warn!("live order failed: {e:#}");
                    return Placed::No;
                }
            }
        }
        return Placed::No;
    }
    Placed::Dry
}

/// Seconds an unfilled limit order is left alone before being cancelled.
///
/// These are 1h-bar decisions priced at the last trade. One that has not
/// filled in ten minutes is answering a question the next bar will ask
/// again, and until it is cancelled it holds USD that `Balance` still
/// reports as spendable — so the following cycle can size an order against
/// money that is not there.
const ORDER_TTL_SECS: i64 = 600;

/// Seconds before an order that never settles is abandoned.
///
/// Far longer than the cancel TTL on purpose: this is the leak guard, not the
/// trading rule. Anything reaching it is a Kraken-side anomaly, not a slow fill.
const SETTLE_GIVE_UP_SECS: i64 = 86_400;

/// Settle our own orders against what they actually executed, and cancel
/// the stale ones still sitting open.
///
/// An order stays tracked until it has been SETTLED, not until it has been
/// cancelled. A cancel only takes effect at Kraken; the executed volume shows
/// up in `ClosedOrders` on the tick after, and that number is what the book
/// needs. Dropping the order at cancel time is what used to leave books long
/// positions the account never received.
///
/// Only touches txids this bot recorded. `cancel_all_orders` would be
/// simpler and would also wipe limit orders the operator placed by hand on
/// the same account.
async fn settle_pending_orders(
    state: &mut paper::State,
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    now: i64,
    events: &mut Vec<String>,
) {
    if !matches!(mode, Mode::Live) || state.pending_orders.is_empty() {
        return;
    }
    let Some(gw) = live_gw else { return };
    // On an outage: cancel nothing, settle nothing, keep every order tracked.
    // Treating an unreachable exchange as "nothing executed" would withdraw
    // real entries from the books.
    let executed = match gw.executed_volumes().await {
        Ok(map) => map,
        Err(e) => {
            warn!("closed orders unreadable ({e:#}) — deferring settlement");
            return;
        }
    };

    let (actions, still_pending) = paper::plan_settlement(
        std::mem::take(&mut state.pending_orders),
        &executed,
        now,
        ORDER_TTL_SECS,
        SETTLE_GIVE_UP_SECS,
    );
    state.pending_orders = still_pending;

    for action in actions {
        match action {
            paper::OrderAction::Settle(order, got) => settle_one(state, &order, got, events),
            paper::OrderAction::Cancel(order) => match gw.cancel(&order.txid).await {
                Ok(()) => {
                    let msg = format!(
                        "CANCEL stale {} {} {} (unfilled {}s)",
                        if order.side > 0 { "buy" } else { "sell" },
                        order.pair,
                        order.txid,
                        now - order.placed_at
                    );
                    info!("{msg}");
                    events.push(msg);
                }
                Err(e) => info!("cancel {}: {e:#}", order.txid),
            },
            paper::OrderAction::GiveUp(order) => warn!(
                "giving up on {} after {}s unsettled — book left as placed",
                order.txid,
                now - order.placed_at
            ),
        }
    }
}

/// Apply one settled order to the book that placed it.
fn settle_one(state: &mut paper::State, order: &paper::PendingOrder, got: f64, events: &mut Vec<String>) {
    if order.side < 0 {
        // A sell is an EXIT, and `step_book` already closed the position and
        // wrote the Trade before the order went out. Unwinding that would mean
        // rewriting trade history; an exit that did not fill leaves real coins
        // in the wallet instead, which the operator needs to see rather than
        // have quietly reconciled away.
        if order.qty > 0.0 && got + 1e-9 < order.qty {
            let msg = format!(
                "UNSOLD {} {:.8} of {:.8} did not fill — wallet still holds it",
                order.pair,
                order.qty - got,
                order.qty
            );
            warn!("{msg}");
            events.push(msg);
        }
        return;
    }

    let Some(book_id) = order.book.as_deref() else {
        // No book: this was the BTC rebalance. If it executed nothing, the
        // day's single attempt was never really spent, so let it retry rather
        // than leave the account outside its band until tomorrow.
        if order.qty > 0.0 && got <= 1e-9 {
            let msg = format!("BTC rebalance {} never filled — will retry", order.txid);
            info!("{msg}");
            events.push(msg);
            state.last_btc_rebalance.clear();
        }
        return;
    };
    let Some(book) = state.books.iter_mut().find(|b| b.id == book_id) else {
        return;
    };
    match paper::settle_buy(book, order.qty, got) {
        paper::Settled::Filled => {}
        paper::Settled::Partial { ordered, got } => {
            let msg = format!("{book_id} PARTIAL fill {got:.8} of {ordered:.8} — book resized");
            info!("{msg}");
            events.push(msg);
        }
        paper::Settled::Nothing => {
            let msg = format!("{book_id} NO fill on {} — entry withdrawn", order.txid);
            info!("{msg}");
            events.push(msg);
        }
    }
}

async fn maybe_btc_rebalance(
    state: &mut paper::State,
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    w: &alloc::Wallet,
    m: alloc::Marks,
    events: &mut Vec<String>,
) {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    if state.last_btc_rebalance == today {
        return;
    }
    let Some(r) = alloc::btc_rebalance(w, m, alloc::Policy::LIVE) else {
        return;
    };
    info!(
        "BTC {:.1}% of hold vs target {:.0}% +/-{:.0} — rebalance {} {:.8}",
        100.0 * alloc::btc_weight(w, m, alloc::Policy::LIVE),
        100.0 * alloc::BTC_TARGET,
        100.0 * alloc::BTC_BAND,
        if r.side > 0 { "buy" } else { "sell" },
        r.qty
    );
    // Only burn today's single attempt if the order actually went out.
    // Marking the day done on a rejected order left the account outside its
    // band for another 24h with nothing retrying it.
    let placed = place_live(mode, live_gw, r.pair, r.side, r.qty, r.price, events).await;
    if let Placed::Live(txid) = &placed {
        state.pending_orders.push(paper::PendingOrder {
            txid: txid.clone(),
            pair: r.pair.to_string(),
            side: r.side,
            placed_at: Utc::now().timestamp(),
            book: None,
            qty: r.qty,
        });
    }
    if placed.acted() {
        state.last_btc_rebalance = today;
    }
}

async fn fetch_marks(client: &KrakenRestClient) -> anyhow::Result<Vec<(String, f64)>> {
    let mut out = Vec::new();
    for pair in ["SOLUSD", "ETHUSD", "XBTUSD"] {
        let t = client
            .get_ticker(pair)
            .await
            .map_err(|e| anyhow::anyhow!("ticker {pair}: {e}"))?;
        if let Some((_, tick)) = t.iter().next() {
            out.push((pair.to_string(), tick.last_price()));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod settle_tests {
    use super::*;

    fn state() -> paper::State {
        let mut s = paper::State::default_paper();
        s.mode = "live".into();
        s.last_btc_rebalance = "2026-09-21".into();
        for b in &mut s.books {
            b.position = Some(paper::Position {
                side: 1,
                qty: 0.0149,
                entry: 2_696.09,
                entry_ts: 0,
                entry_bar: 0,
                entry_fee: 2.30,
                live_qty: 0.0149,
            });
        }
        s
    }

    fn buy(book: Option<&str>, qty: f64) -> paper::PendingOrder {
        paper::PendingOrder {
            txid: "OTEST-1".into(),
            pair: "ETHUSD".into(),
            side: 1,
            placed_at: 0,
            book: book.map(str::to_string),
            qty,
        }
    }

    #[test]
    fn an_unfilled_buy_corrects_only_the_book_that_placed_it() {
        // Routing by id, not by position in the list. Correcting the wrong
        // book would destroy a good record AND leave the bad one standing.
        let mut s = state();
        let mut events = Vec::new();
        settle_one(&mut s, &buy(Some("eth_1h_sf"), 0.0149), 0.0, &mut events);

        let by_id = |id: &str| s.books.iter().find(|b| b.id == id).unwrap();
        assert!(by_id("eth_1h_sf").position.is_none(), "the phantom goes");
        assert!(by_id("sol_1h_tl").position.is_some(), "untouched");
        assert!(by_id("sol_bh").position.is_some(), "untouched");
    }

    #[test]
    fn an_order_naming_an_unknown_book_is_ignored() {
        let mut s = state();
        let mut events = Vec::new();
        settle_one(&mut s, &buy(Some("deleted_book"), 0.0149), 0.0, &mut events);
        assert!(s.books.iter().all(|b| b.position.is_some()));
    }

    #[test]
    fn an_unfilled_btc_rebalance_frees_the_day_to_retry() {
        // The rebalance runs once per UTC day. Marking the day spent on an
        // order that executed NOTHING leaves the account outside its band for
        // another 24h with nothing retrying -- the same reasoning that already
        // stops a REJECTED order from burning the day, extended to an accepted
        // order that never filled.
        let mut s = state();
        let mut events = Vec::new();
        settle_one(&mut s, &buy(None, 0.0003), 0.0, &mut events);
        assert!(s.last_btc_rebalance.is_empty(), "the day must be retryable");
        assert!(events.iter().any(|e| e.contains("never filled")));
    }

    #[test]
    fn a_filled_btc_rebalance_keeps_the_day_spent() {
        let mut s = state();
        let mut events = Vec::new();
        settle_one(&mut s, &buy(None, 0.0003), 0.0003, &mut events);
        assert_eq!(s.last_btc_rebalance, "2026-09-21");
    }

    #[test]
    fn a_partially_filled_btc_rebalance_does_not_retry_the_whole_size() {
        // It moved the account toward the band. Retrying the FULL size today
        // would overshoot; the next day's pass sizes against reality.
        let mut s = state();
        let mut events = Vec::new();
        settle_one(&mut s, &buy(None, 0.0003), 0.0002, &mut events);
        assert_eq!(s.last_btc_rebalance, "2026-09-21");
    }

    #[test]
    fn an_unfilled_sell_is_reported_and_never_rewrites_the_book() {
        // A sell is an exit: `step_book` already closed the position and wrote
        // the Trade. The coins are still in the wallet -- that is an operator
        // fact to surface, not a book to quietly reconcile.
        let mut s = state();
        let mut events = Vec::new();
        let mut order = buy(Some("eth_1h_sf"), 0.0149);
        order.side = -1;
        settle_one(&mut s, &order, 0.0, &mut events);

        assert!(
            s.books.iter().all(|b| b.position.is_some()),
            "a sell must not run the buy correction"
        );
        assert!(events.iter().any(|e| e.starts_with("UNSOLD")));
    }

    #[test]
    fn a_sell_that_filled_says_nothing() {
        let mut s = state();
        let mut events = Vec::new();
        let mut order = buy(Some("eth_1h_sf"), 0.0149);
        order.side = -1;
        settle_one(&mut s, &order, 0.0149, &mut events);
        assert!(events.is_empty());
    }
}
