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
mod ledger;
mod live;
mod paper;
mod paper_ex;
mod regime;
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
    // Same reason as `pending`: the loop below holds `state.books` borrowed
    // for its whole body, so `state.live` cannot be touched until it ends.
    let mut adopted: Vec<(String, f64, f64)> = Vec::new();
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
            // Deliberately does NOTHING to the position. This used to set
            // `p.qty = w.sol`, which replaced a $1,000 simulated hold with
            // whatever SOL the wallet happened to hold -- 0.0000094 of a coin
            // -- and pinned the buy-and-hold benchmark at a flat -$3.90 while
            // SOL moved. That benchmark is exactly what the signals have to
            // beat, so killing it made the whole comparison unreadable.
            // `sol_bh` is mark-only: it never places an order, so it has no
            // live inventory to reconcile against.
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
                    // The SIMULATION keeps the trade. The signal fired, and
                    // whether a small wallet could act on it that hour says
                    // nothing about whether the signal has an edge -- which is
                    // the only question these books exist to answer. Taking
                    // the entry back off the book made the simulation silently
                    // skip exactly the trades the account was too poor for.
                    info!("{} live skip: {why}", book.id);
                    events.push(format!("{} SKIP {why}", book.id));
                    if opened {
                        if let Some(p) = book.position.as_mut() {
                            p.live_qty = 0.0;
                        }
                    }
                }
                alloc::LiveAction::Adopt { pair, qty } => {
                    if let Some(p) = book.position.as_mut() {
                        p.live_qty = qty;
                    }
                    // Adopted coins were bought before the ledger was looking,
                    // so they have no basis it can know. Staged here and
                    // marked in after the loop -- see `adopted` below.
                    adopted.push((pair.clone(), qty, mark));
                    info!("{} ADOPT inventory qty={qty:.8} (no buy)", book.id);
                    events.push(format!("{} ADOPT {} qty={:.8}", book.id, book.pair, qty));
                }
                alloc::LiveAction::Buy { pair, qty, price } => {
                    if let Some(p) = book.position.as_mut() {
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
    // Only real mode writes the real ledger. A dry run shares this state file
    // (see `data_dir`), and a preview must not leave fills in it.
    if matches!(mode, Mode::Live) {
        for (pair, qty, mark) in adopted {
            let r = state.live.adopt(ledger::Sleeve::Trade, &pair, qty, mark);
            if let ledger::Recorded::Applied = r {
                let msg = format!("LEDGER adopt {pair} {qty:.8} @ {mark:.4} (marked in, not bought)");
                info!("{msg}");
                events.push(msg);
            }
        }
    }

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
            maybe_regime_rebalance(&mut state, mode, live_gw, client, w, marks_snapshot, &mut events)
                .await;
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

/// Closes of CLOSED daily candles, oldest first: Kraken's last daily candle
/// is the day still forming, and the regime rule must never read it.
async fn fetch_closed_daily(client: &KrakenRestClient, pair: &str) -> anyhow::Result<Vec<f64>> {
    let ohlc = client
        .get_ohlc(pair, 1440)
        .await
        .map_err(|e| anyhow::anyhow!("daily OHLC {pair}: {e}"))?;
    let now = Utc::now().timestamp();
    Ok(ohlc
        .candles
        .iter()
        .filter(|c| c.time + 86_400 <= now)
        .map(|c| c.close_f64())
        .collect())
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
    let executed = match gw.executed_fills().await {
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
            paper::OrderAction::Settle(order, exec) => settle_one(state, &order, exec, now, events),
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

/// Apply one settled order: the real fill to the live ledger, and the live
/// inventory correction to the book that placed it.
///
/// TWO destinations on purpose. The ledger gets what Kraken says happened, in
/// Kraken's dollars. The book gets nothing but a corrected `live_qty` -- the
/// simulation's size, entry and fees are its own and are not a function of
/// what a small wallet managed to fill.
fn settle_one(
    state: &mut paper::State,
    order: &paper::PendingOrder,
    exec: ledger::Execution,
    now: i64,
    events: &mut Vec<String>,
) {
    let got = exec.vol_exec;
    record_fill(state, order, exec, now, events);
    if let Some(pair) = order.book.as_deref().and_then(|b| b.strip_prefix(REGIME_BOOK_PREFIX)) {
        // A regime order that executed nothing never really applied its
        // change, so forget it and let the next cycle size the coin again.
        // A partial fill is kept, like the BTC rebalance's: re-sizing on a
        // later flip absorbs the remainder.
        if order.qty > 0.0 && got <= 1e-9 {
            let msg = format!("regime {pair} order {} never filled — will retry", order.txid);
            info!("{msg}");
            events.push(msg);
            state.regime_applied.remove(pair);
            state.last_regime_day.clear();
        } else if got + 1e-9 < order.qty {
            let msg = format!("regime {pair} PARTIAL fill {got:.8} of {:.8}", order.qty);
            info!("{msg}");
            events.push(msg);
        }
        return;
    }
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
    match paper::settle_live_buy(book, order.qty, got) {
        paper::Settled::Filled => {}
        paper::Settled::Partial { ordered, got } => {
            let msg =
                format!("{book_id} PARTIAL fill {got:.8} of {ordered:.8} — live qty corrected");
            info!("{msg}");
            events.push(msg);
        }
        paper::Settled::Nothing => {
            let msg = format!(
                "{book_id} NO fill on {} — simulation keeps the trade, wallet claim dropped",
                order.txid
            );
            info!("{msg}");
            events.push(msg);
        }
    }
}

/// Put one real execution on the live ledger.
///
/// Attribution comes from the PendingOrder rather than from Kraken's own
/// `descr`: this bot must only ever account for orders it placed itself, and
/// the txid list is the only thing that distinguishes those from limit orders
/// the operator entered by hand on the same account.
fn record_fill(
    state: &mut paper::State,
    order: &paper::PendingOrder,
    exec: ledger::Execution,
    now: i64,
    events: &mut Vec<String>,
) {
    let fill = ledger::Fill {
        txid: order.txid.clone(),
        ts: exec.at.unwrap_or(now),
        pair: order.pair.clone(),
        side: order.side,
        sleeve: ledger::Sleeve::of(order.book.as_deref()),
        book: order.book.clone(),
        qty: exec.vol_exec,
        cost: exec.cost,
        fee: exec.fee,
    };
    match state.live.record(fill) {
        ledger::Recorded::Applied => {
            let msg = format!(
                "LEDGER {} {} {:.8} cost ${:.4} fee ${:.4}",
                if order.side > 0 { "buy" } else { "sell" },
                order.pair,
                exec.vol_exec,
                exec.cost,
                exec.fee
            );
            info!("{msg}");
            events.push(msg);
        }
        ledger::Recorded::Duplicate => {
            warn!("LEDGER {} already recorded — not counted twice", order.txid)
        }
        // The common one is "executed nothing", which is a reaped limit
        // order: a real event for the settler above and a non-event here.
        ledger::Recorded::Rejected(why) => {
            tracing::debug!("LEDGER skip {}: {why}", order.txid)
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

/// Once per UTC day: read each coin's regime from its closed daily candles,
/// and size the coin when its regime has changed since it was last sized.
///
/// Both coins are read before either is acted on, and a failed read leaves
/// the day open, so an outage is retried next hour rather than skipping a
/// day. A coin whose regime is unchanged is left alone (see
/// `alloc::regime_rebalance` for why the drift in between is not traded).
async fn maybe_regime_rebalance(
    state: &mut paper::State,
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    client: &KrakenRestClient,
    w: &alloc::Wallet,
    m: alloc::Marks,
    events: &mut Vec<String>,
) {
    let p = alloc::Policy::LIVE;
    if !p.trade_sleeve || !p.regime {
        return;
    }
    let today = Utc::now().format("%Y-%m-%d").to_string();
    if state.last_regime_day == today {
        return;
    }
    if !m.complete() {
        info!("regime: marks incomplete — retrying next cycle");
        return;
    }
    let mut readings = Vec::new();
    for pair in regime::PAIRS {
        let closes = match fetch_closed_daily(client, pair).await {
            Ok(c) => c,
            Err(e) => {
                warn!("regime: {e:#} — retrying next cycle");
                return;
            }
        };
        let Some(r) = regime::evaluate(&closes) else {
            warn!(
                "regime: {pair} has {} closed daily candles, needs {} — retrying next cycle",
                closes.len(),
                regime::SMA_DAYS
            );
            return;
        };
        readings.push((pair, r));
    }

    // Never spend the hold sleeve's cash; what is left is shared by both
    // coins, so each buy placed comes off it before the next is sized.
    let mut spendable = (w.usd_available() - alloc::targets(w, m, p).hold_cash).max(0.0);
    let now = Utc::now().timestamp();
    for (pair, r) in readings {
        let label = if r.bull { "BULL" } else { "BEAR" };
        let applied = state.regime_applied.get(pair).copied();
        let changed = applied != Some(r.bull);
        let msg = format!(
            "regime {pair} {label}: close {:.2} vs 200d {:.2} ({:+.1}%) → hold {:.0}% of its sleeve{}",
            r.close,
            r.sma,
            100.0 * r.distance(),
            100.0 * regime::exposure(r.bull),
            match applied {
                None => " (first sizing)",
                Some(_) if changed => " (CHANGED — resizing)",
                Some(_) => " (unchanged)",
            }
        );
        info!("{msg}");
        if !changed {
            continue;
        }
        events.push(msg);
        match alloc::regime_rebalance(w, m, p, pair, r.bull, spendable) {
            alloc::RegimeStep::AtTarget => {
                state.regime_applied.insert(pair.to_string(), r.bull);
            }
            alloc::RegimeStep::NoCash => {
                let msg = format!("regime {pair}: under target but no spare cash — retrying tomorrow");
                info!("{msg}");
                events.push(msg);
            }
            alloc::RegimeStep::Unpriced => {
                info!("regime {pair}: unpriced — retrying next cycle");
                return;
            }
            alloc::RegimeStep::Order(rb) => {
                let placed =
                    place_live(mode, live_gw, rb.pair, rb.side, rb.qty, rb.price, events).await;
                if let Placed::Live(txid) = &placed {
                    state.pending_orders.push(paper::PendingOrder {
                        txid: txid.clone(),
                        pair: rb.pair.to_string(),
                        side: rb.side,
                        placed_at: now,
                        book: Some(format!("{REGIME_BOOK_PREFIX}{}", rb.pair)),
                        qty: rb.qty,
                    });
                }
                if placed.acted() {
                    state.regime_applied.insert(pair.to_string(), r.bull);
                    if rb.side > 0 {
                        spendable -= rb.qty * rb.price;
                    }
                }
            }
        }
    }
    state.last_regime_day = today;
}

/// Marks a pending order as the regime rule's rather than a 1h book's. The
/// ledger counts any order with a book name as the trade sleeve's, which is
/// right for these too.
const REGIME_BOOK_PREFIX: &str = "regime:";

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

    /// A fill at roughly the live ETH price, so the ledger sees real money
    /// move rather than a volume with no cost attached.
    fn exec(vol: f64) -> ledger::Execution {
        ledger::Execution { vol_exec: vol, cost: vol * 2_700.0, fee: vol * 2_700.0 * 0.0026, at: None }
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

    fn regime_order(side: i8, qty: f64) -> paper::PendingOrder {
        paper::PendingOrder {
            txid: "OREGIME-1".into(),
            pair: "ETHUSD".into(),
            side,
            placed_at: 0,
            book: Some(format!("{REGIME_BOOK_PREFIX}ETHUSD")),
            qty,
        }
    }

    #[test]
    fn an_unfilled_regime_order_forgets_its_change_so_it_is_retried() {
        // Both directions: a bear's sell that never filled leaves the coins
        // there, and a bull's buy that never filled leaves the cash. Either
        // way the regime was never applied, and keeping it marked as applied
        // would mean waiting for the NEXT flip, possibly months away.
        for side in [1, -1] {
            let mut s = state();
            s.last_regime_day = "2026-10-04".into();
            s.regime_applied.insert("ETHUSD".into(), true);
            s.regime_applied.insert("SOLUSD".into(), true);
            let mut events = Vec::new();
            settle_one(&mut s, &regime_order(side, 0.01), exec(0.0), 1_790_000_000, &mut events);
            assert!(!s.regime_applied.contains_key("ETHUSD"), "side {side}: ETH must be re-sized");
            assert_eq!(s.regime_applied.get("SOLUSD"), Some(&true), "SOL is untouched");
            assert!(s.last_regime_day.is_empty(), "side {side}: retried next cycle, not tomorrow");
            assert!(events.iter().any(|e| e.contains("never filled")));
        }
    }

    #[test]
    fn a_filled_regime_order_stays_applied_and_touches_no_book() {
        let mut s = state();
        s.last_regime_day = "2026-10-04".into();
        s.regime_applied.insert("ETHUSD".into(), false);
        let snapshot = |s: &paper::State| -> Vec<(f64, f64)> {
            s.books
                .iter()
                .map(|b| b.position.as_ref().map(|p| (p.qty, p.live_qty)).unwrap_or_default())
                .collect()
        };
        let before = snapshot(&s);
        let mut events = Vec::new();
        settle_one(&mut s, &regime_order(-1, 0.01), exec(0.01), 1_790_000_000, &mut events);
        assert_eq!(s.regime_applied.get("ETHUSD"), Some(&false));
        assert_eq!(s.last_regime_day, "2026-10-04");
        let after = snapshot(&s);
        assert_eq!(before, after, "the 1h simulations are not the regime rule's to settle");
    }

    #[test]
    fn an_unfilled_buy_corrects_only_the_book_that_placed_it() {
        // Routing by id, not by position in the list. Correcting the wrong
        // book would destroy a good record AND leave the bad one standing.
        let mut s = state();
        let mut events = Vec::new();
        settle_one(&mut s, &buy(Some("eth_1h_sf"), 0.0149), exec(0.0), 1_790_000_000, &mut events);

        let by_id = |id: &str| s.books.iter().find(|b| b.id == id).unwrap();
        let placed = by_id("eth_1h_sf").position.as_ref().unwrap();
        assert_eq!(placed.live_qty, 0.0, "nothing arrived, so no wallet claim");
        assert_eq!(placed.qty, 0.0149, "the simulated trade stands");
        for other in ["sol_1h_tl", "sol_bh"] {
            assert_eq!(
                by_id(other).position.as_ref().unwrap().live_qty,
                0.0149,
                "{other} must be untouched"
            );
        }
    }

    #[test]
    fn a_real_fill_lands_on_the_live_ledger_and_not_on_the_book() {
        // The whole split, in one settlement. Kraken's own cost and fee go to
        // the ledger; the book learns only how much of the wallet it owns.
        let mut s = state();
        s.live.since = "2026-09-23".into();
        let mut events = Vec::new();
        let e = ledger::Execution { vol_exec: 0.001, cost: 2.69609, fee: 0.00701, at: Some(42) };
        settle_one(&mut s, &buy(Some("eth_1h_sf"), 0.01488212), e, 1_790_000_000, &mut events);

        let p = s.live.get(ledger::Sleeve::Trade, "ETHUSD").expect("a ledger slot");
        assert!((p.qty - 0.001).abs() < 1e-12);
        assert!((p.basis_usd - (2.69609 + 0.00701)).abs() < 1e-12, "fee is capitalised");
        assert!((p.fees_usd - 0.00701).abs() < 1e-12, "the REAL fee, not a modelled one");
        assert_eq!(s.live.fills[0].ts, 42, "Kraken's own timestamp wins");

        let book = s.books.iter().find(|b| b.id == "eth_1h_sf").unwrap();
        assert_eq!(book.fees_paid, 0.0, "a real fee never touches a paper book");
        assert_eq!(book.position.as_ref().unwrap().qty, 0.0149, "simulation untouched");
        assert_eq!(book.position.as_ref().unwrap().live_qty, 0.001);
    }

    #[test]
    fn a_settlement_seen_twice_is_only_counted_once() {
        // `ClosedOrders` returns the whole recent page every cycle, so the
        // same txid can be offered again. Money counted twice never unwinds.
        let mut s = state();
        s.live.since = "2026-09-23".into();
        let mut events = Vec::new();
        let e = ledger::Execution { vol_exec: 0.001, cost: 2.69609, fee: 0.00701, at: None };
        let order = buy(Some("eth_1h_sf"), 0.001);
        settle_one(&mut s, &order, e, 1_790_000_000, &mut events);
        settle_one(&mut s, &order, e, 1_790_000_060, &mut events);
        let p = s.live.get(ledger::Sleeve::Trade, "ETHUSD").unwrap();
        assert_eq!(p.buys, 1);
        assert!((p.qty - 0.001).abs() < 1e-12);
    }

    #[test]
    fn a_rebalance_fill_is_booked_to_the_hold_sleeve() {
        // The BTC rebalance owns no book. Its fills are real money and belong
        // on the ledger, but summing them into the trade sleeve would answer
        // a question nobody asked and bury the one that was.
        let mut s = state();
        s.live.since = "2026-09-23".into();
        let mut events = Vec::new();
        let e = ledger::Execution { vol_exec: 0.0003, cost: 25.4, fee: 0.066, at: None };
        settle_one(&mut s, &buy(None, 0.0003), e, 1_790_000_000, &mut events);
        assert!(s.live.get(ledger::Sleeve::Trade, "ETHUSD").is_none());
        let hold = s.live.get(ledger::Sleeve::Hold, "ETHUSD").expect("hold slot");
        assert!((hold.fees_usd - 0.066).abs() < 1e-12);
    }

    #[test]
    fn an_order_naming_an_unknown_book_is_ignored() {
        let mut s = state();
        let mut events = Vec::new();
        settle_one(&mut s, &buy(Some("deleted_book"), 0.0149), exec(0.0), 1_790_000_000, &mut events);
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
        settle_one(&mut s, &buy(None, 0.0003), exec(0.0), 1_790_000_000, &mut events);
        assert!(s.last_btc_rebalance.is_empty(), "the day must be retryable");
        assert!(events.iter().any(|e| e.contains("never filled")));
    }

    #[test]
    fn a_filled_btc_rebalance_keeps_the_day_spent() {
        let mut s = state();
        let mut events = Vec::new();
        settle_one(&mut s, &buy(None, 0.0003), exec(0.0003), 1_790_000_000, &mut events);
        assert_eq!(s.last_btc_rebalance, "2026-09-21");
    }

    #[test]
    fn a_partially_filled_btc_rebalance_does_not_retry_the_whole_size() {
        // It moved the account toward the band. Retrying the FULL size today
        // would overshoot; the next day's pass sizes against reality.
        let mut s = state();
        let mut events = Vec::new();
        settle_one(&mut s, &buy(None, 0.0003), exec(0.0002), 1_790_000_000, &mut events);
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
        settle_one(&mut s, &order, exec(0.0), 1_790_000_000, &mut events);

        assert!(
            s.books.iter().all(|b| b.position.is_some()),
            "a sell must not run the buy correction"
        );
        assert!(events.iter().any(|e| e.starts_with("UNSOLD")));
    }

    #[test]
    fn a_sell_that_filled_reports_the_fill_and_nothing_else() {
        let mut s = state();
        s.live.since = "2026-09-23".into();
        let mut events = Vec::new();
        let mut order = buy(Some("eth_1h_sf"), 0.0149);
        order.side = -1;
        settle_one(&mut s, &order, exec(0.0149), 1_790_000_000, &mut events);
        assert!(!events.iter().any(|e| e.starts_with("UNSOLD")), "nothing is unsold");
        assert!(events.iter().any(|e| e.starts_with("LEDGER sell")), "{events:?}");
    }
}
