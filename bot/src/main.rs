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
    reap_pending_orders(&mut state, mode, live_gw, now, &mut events).await;

    let marks = fetch_marks(client).await.unwrap_or_default();
    // `fetch_marks` returns an EMPTY vec on any error, so an outage leaves
    // every price at 0.0 rather than stale. `Marks::complete()` is what the
    // policy checks before sizing anything off those zeros.
    let marks_snapshot = alloc::Marks::from_pairs(&marks);
    let mut wallet = if matches!(mode, Mode::Live | Mode::LiveDry) {
        match live_gw {
            Some(gw) => gw.balances().await.ok().map(|b| alloc::Wallet::from_balances(&b)),
            None if live::keys_present() => match live::LiveKraken::from_env() {
                Ok(gw) => gw.balances().await.ok().map(|b| alloc::Wallet::from_balances(&b)),
                Err(_) => None,
            },
            None => None,
        }
    } else {
        None
    };

    // Ask Kraken what its open orders have already claimed. Without this the
    // policy sizes against `Balance`, which still counts money an order has
    // spoken for -- measured live at $37.38 held against a $120.41 balance.
    // A failure here leaves `usd_held` at 0, which OVERSTATES what is
    // spendable, so it is logged rather than passed over in silence.
    if let (Some(w), Some(gw)) = (wallet.as_mut(), live_gw) {
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
                        });
                    }
                }
            }
        }
    }

    state.pending_orders.append(&mut pending);

    if matches!(mode, Mode::Live | Mode::LiveDry) && new_bar {
        if let Some(w) = wallet.as_ref() {
            let t = alloc::targets(w, marks_snapshot);
            info!(
                "wallet usd={:.2} btc={:.8} eth={:.8} sol={:.8} | total=${:.2} \
                 hold btc {:.1}% (${:.2} vs ${:.2}) cash ${:.2} | trade cash ${:.2}",
                w.usd,
                w.btc,
                w.eth,
                w.sol,
                t.total,
                100.0 * alloc::btc_weight(w, marks_snapshot),
                w.btc * marks_snapshot.btc,
                t.btc,
                t.hold_cash,
                alloc::trade_cash_usd(w, marks_snapshot)
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

/// Cancel our own stale orders and forget them.
///
/// Only touches txids this bot recorded. `cancel_all_orders` would be
/// simpler and would also wipe limit orders the operator placed by hand on
/// the same account.
async fn reap_pending_orders(
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
    let (stale, fresh) =
        paper::partition_stale(std::mem::take(&mut state.pending_orders), now, ORDER_TTL_SECS);
    state.pending_orders = fresh;
    for order in stale {
        match gw.cancel(&order.txid).await {
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
            // Already filled or already gone. Dropping it is the right
            // move either way: we only ever cancel by our own txid, so a
            // txid Kraken does not recognise is nothing left to manage.
            Err(e) => info!("cancel {}: {e:#} — dropping", order.txid),
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
    let Some(r) = alloc::btc_rebalance(w, m) else {
        return;
    };
    info!(
        "BTC {:.1}% of hold vs target {:.0}% +/-{:.0} — rebalance {} {:.8}",
        100.0 * alloc::btc_weight(w, m),
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
