//! Paper (default) / live-gated Kraken bot.
//!
//! Crates: `exchange-apiws` (Kraken), `indicators-ta` (EMA/ATR/VWAP, used by
//! the paper books' own signals), `rustrade-framework` (Brain +
//! ExchangeClient scaffolding for a parallel framework that is not wired
//! into the live path — see `brains.rs`/`kraken_src.rs`/`paper_ex.rs`'s
//! `#![allow(dead_code)]`).
//!
//! Live wallet, since 2026-10-05: ONE account targeting BTC/ETH/SOL/cash of
//! its TOTAL value, with the trend rule (`regime.rs`) sizing all three
//! coins. See `alloc.rs` for the policy itself and `work_tick` below for the
//! hourly loop that replaced the 1h-bar trigger. `state.books` (the three
//! $1,000 paper books) is left on disk as history: nothing here steps it or
//! places an order from it any more.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Timelike, Utc};
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

use paper::{append_journal, load_state, print_status, save_state};

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
  crypto-bot paper              one cycle (paper bookkeeping only)
  crypto-bot paper --loop       repeat every 60s
  crypto-bot status             print account / books
  crypto-bot report             send a Discord snapshot now (needs DISCORD_WEBHOOK_URL)
  crypto-bot live --dry-run     preview: real wallet read, no orders placed
  crypto-bot live --confirm I_UNDERSTAND_REAL_MONEY
                                real Kraken post-only orders (KRAKEN_API_KEY/SECRET)
  crypto-bot rebalance --dry-run
  crypto-bot rebalance --confirm I_UNDERSTAND_REAL_MONEY
                                mark every coin pending so the next work tick
                                brings the account to its effective targets

Default is paper. Live and rebalance refuse to run without the exact confirm
string. Discord: set DISCORD_WEBHOOK_URL for daily (15:00 UTC), weekly (Mon),
monthly (1st).

Policy (alloc.rs): one account, target weights BTC 50% / ETH 25% / SOL 15% /
cash 10% of the TOTAL, scaled by the daily regime rule (regime.rs) on every
coin. A coin trades only on its own regime flip, the one-time move onto this
policy, a deposit being invested, or `rebalance` — never on ordinary drift.
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
    let dry = args.iter().any(|a| a == "--dry-run");
    let confirm = args
        .windows(2)
        .any(|w| w[0] == "--confirm" && w[1] == "I_UNDERSTAND_REAL_MONEY");
    match cmd.as_str() {
        "paper" => run(Mode::Paper, args.iter().any(|a| a == "--loop")).await,
        "status" => status_cmd().await,
        "report" => report_cmd().await,
        "live" => {
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
        "rebalance" => {
            if confirm {
                rebalance_cmd(false)
            } else if dry {
                rebalance_cmd(true)
            } else {
                anyhow::bail!(
                    "refusing rebalance. Preview: `crypto-bot rebalance --dry-run`. \
                     Real money: `crypto-bot rebalance --confirm I_UNDERSTAND_REAL_MONEY`"
                );
            }
        }
        _ => usage(),
    }
}

/// Marks every coin pending, so the next work tick brings the whole account
/// to its effective targets — the same mechanism the one-time policy move
/// uses (`maybe_run_policy_migration`), just triggered by hand.
///
/// Pure file-state mutation: no Kraken call of any kind, private or public,
/// which is what makes `--dry-run` for this command just "don't save."
fn rebalance_cmd(dry_run: bool) -> anyhow::Result<()> {
    let mut state = load_state()?;
    let before: Vec<String> = state.regime_applied.keys().cloned().collect();
    state.regime_applied.clear();
    println!(
        "rebalance: cleared regime_applied for {} pair(s) (was: {before:?}) — \
         every coin is now pending its effective target",
        regime::PAIRS.len()
    );
    if dry_run {
        println!("--dry-run: state.json NOT written");
        return Ok(());
    }
    save_state(&state)?;
    println!(
        "state.json updated — the next work tick (within the hour, or on the \
         bot's next startup) brings the account to its effective targets"
    );
    Ok(())
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
    discord::maybe_report(&mut state, Some("startup"), account.as_ref()).await;
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

/// One wake of the 60s loop: settle whatever orders are outstanding, then —
/// at most once per UTC hour, plus once on startup — decide and place what
/// the policy wants.
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
    let now = Utc::now().timestamp();
    let mut events = Vec::new();

    // Before ANY sizing: release USD held by our own stale orders, so the
    // wallet read below reflects money we can actually spend.
    settle_pending_orders(&mut state, mode, live_gw, now, &mut events).await;

    let marks = fetch_marks(client).await.unwrap_or_default();
    // `fetch_marks` returns an EMPTY vec on any error, so an outage leaves
    // every price at 0.0 rather than stale. `Marks::complete()` is what the
    // policy checks before sizing anything off those zeros.
    let m = alloc::Marks::from_pairs(&marks);

    // ONE gateway for every private read this cycle makes. `live_gw` is only
    // built for Mode::Live, so a dry run reads through a throwaway client
    // built here — otherwise a dry run's preview would silently differ from
    // what live would actually see (open-order holds, true balances).
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
    // spoken for. A failure here leaves `usd_held` at 0, which OVERSTATES
    // what is spendable, so it is logged rather than passed over in silence.
    if let (Some(w), Some(gw)) = (wallet.as_mut(), read_gw) {
        match gw.held_usd().await {
            Ok(held) => w.usd_held = held,
            Err(e) => warn!("open-order check failed ({e:#}) — sizing may overstate free USD"),
        }
    }

    let is_startup = !*announced;
    let this_hour = Utc::now().format("%Y-%m-%dT%H").to_string();
    let due_for_work = is_startup || state.last_work_hour != this_hour;

    if matches!(mode, Mode::Live | Mode::LiveDry) && due_for_work {
        match wallet.as_ref() {
            Some(w) => {
                work_tick(
                    &mut state,
                    mode,
                    live_gw,
                    client,
                    w,
                    m,
                    alloc::Policy::LIVE,
                    now,
                    &mut events,
                )
                .await;
                state.last_work_hour = this_hour;
            }
            None => info!("work tick due but no wallet snapshot — retrying next tick"),
        }
    }

    let mut kinds = Vec::new();
    if is_startup {
        kinds.push("startup".into());
        *announced = true;
    }
    kinds.extend(discord::due_kinds(&state, None));
    let account = if !kinds.is_empty() && matches!(mode, Mode::Live) {
        load_account(live_gw, client, &marks).await
    } else {
        None
    };
    discord::send_kinds(&mut state, &kinds, account.as_ref()).await;
    save_state(&state)?;
    if !events.is_empty() {
        append_journal(&serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "mode": state.mode,
            "events": events,
            "kraken_usd": account.as_ref().map(|a| a.total_usd),
        }))?;
        if let Some(a) = &account {
            a.print();
        }
        print_status(&state, &marks);
    }
    Ok(())
}

/// Everything the hourly work tick does, in order. Each step is its own
/// function so a failure in one (a bad ticker, an unreadable ledger page)
/// cannot block the others — see each function's own doc comment for why it
/// fails the way it does.
#[allow(clippy::too_many_arguments)]
async fn work_tick(
    state: &mut paper::State,
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    client: &KrakenRestClient,
    w: &alloc::Wallet,
    m: alloc::Marks,
    policy: alloc::Policy,
    now: i64,
    events: &mut Vec<String>,
) {
    maybe_run_policy_migration(state, w, m, now, events);
    maybe_convert_stables(state, mode, live_gw, client, w, policy, now, events).await;
    maybe_scan_ledgers(state, mode, live_gw, m, now, events).await;
    maybe_read_regime(state, client, events).await;
    maybe_close_gaps(state, mode, live_gw, client, w, m, policy, now, events).await;
    maybe_append_history(state, w, m, events);
}

/// The one-time move described in `alloc.rs`'s module docs: once per
/// `state.json`, clear `regime_applied` so every pair disagrees with
/// `regime_bull` (even pairs `regime_bull` has no reading for yet — they
/// simply wait, same as a brand new pair always has), and bump
/// `policy_version`. `maybe_close_gaps` does the actual trading, over
/// however many ticks it takes.
///
/// Also gives the (now unified) ledger a basis for whatever the wallet
/// already holds, via `LiveLedger::adopt`: BTC bought under the old
/// hold-sleeve rebalancer was recorded under `Sleeve::Hold`, and every new
/// fill from here on is `Sleeve::Trade` (there is no more sleeve split to
/// keep separate) — without this, the first regime-flip sell after the move
/// would find no basis on file for BTC and quarantine its proceeds as
/// untracked rather than real P&L.
fn maybe_run_policy_migration(
    state: &mut paper::State,
    w: &alloc::Wallet,
    m: alloc::Marks,
    now: i64,
    events: &mut Vec<String>,
) {
    if state.policy_version >= 2 {
        return;
    }
    let had = state.regime_applied.len();
    state.regime_applied.clear();
    for (pair, qty, mark) in [
        ("XBTUSD", w.btc, m.btc),
        ("ETHUSD", w.eth, m.eth),
        ("SOLUSD", w.sol, m.sol),
    ] {
        if qty > 0.0 && mark > 0.0 {
            state.live.adopt(ledger::Sleeve::Trade, pair, qty, mark);
        }
    }
    // CRITICAL: the ledger-scan baseline moves to NOW, not to whenever the
    // oldest historical deposit happened. Without this, the very first live
    // scan (`maybe_scan_ledgers`, `last_ledger_time` still 0) would ask
    // Kraken for every deposit ever made, find each one "new" against an
    // empty `flows`, and re-invest money that has been sitting invested for
    // months. Only a deposit that lands AFTER this move is ever acted on.
    state.last_ledger_time = now as f64;
    state.policy_version = 2;
    let msg = format!(
        "POLICY move to v2: one account targeting BTC {:.0}%/ETH {:.0}%/SOL {:.0}%/cash of the \
         total (regime-scaled) — cleared {had} previously-applied pair(s) so every coin is sized \
         to its effective target",
        alloc::BASE_BTC * 100.0,
        alloc::BASE_ETH * 100.0,
        alloc::BASE_SOL * 100.0,
    );
    info!("{msg}");
    events.push(msg);
}

/// A USDC or USDT balance at or above Kraken's ordermin is simply sold to
/// USD every tick until it is gone — see `alloc::stable_sell`.
#[allow(clippy::too_many_arguments)]
async fn maybe_convert_stables(
    state: &mut paper::State,
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    client: &KrakenRestClient,
    w: &alloc::Wallet,
    policy: alloc::Policy,
    now: i64,
    events: &mut Vec<String>,
) {
    for (balance, pair) in [(w.usdc, "USDCUSD"), (w.usdt, "USDTUSD")] {
        if balance < alloc::MIN_STABLE {
            continue;
        }
        if has_open_order(state, pair) {
            continue; // one open order per pair — wait for it to settle
        }
        let touch = match fetch_touch(client, pair).await {
            Ok(t) => t,
            Err(e) => {
                warn!("stable ticker {pair}: {e:#}");
                continue;
            }
        };
        convert_one_stable(
            state, mode, live_gw, balance, pair, touch, policy, now, events,
        )
        .await;
    }
}

/// The decide-and-place half of `maybe_convert_stables`, split out so it can
/// be unit tested against a hand-built `Touch` with no network call — the
/// fetch above is the only part that needs one.
#[allow(clippy::too_many_arguments)]
async fn convert_one_stable(
    state: &mut paper::State,
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    balance: f64,
    pair: &'static str,
    touch: alloc::Touch,
    policy: alloc::Policy,
    now: i64,
    events: &mut Vec<String>,
) {
    let Some(r) = alloc::stable_sell(balance, pair, touch) else {
        return;
    };
    if !policy.armed {
        info!("FROZEN — would sell {:.8} {pair}", r.qty);
        return;
    }
    if let Some(order) = place_rebalance(mode, live_gw, &r, Purpose::Stable, now, events).await {
        state.pending_orders.push(order);
    }
}

/// Which of the three things this bot ever trades for. Encoded into
/// `PendingOrder.book` — a pre-existing free-form `Option<String>` field —
/// rather than a new field, so `state.json`'s shape does not change at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    /// Closing a pair's gap to its effective target: a flip, the one-time
    /// policy move, or the `rebalance` command. Can be either side.
    Rebalance,
    /// Topping up a pair whose regime is already applied, while
    /// `deposit_pending` — see `deposit_top_up_pair`. BUY only, never a sell.
    DepositBuy,
    /// Selling a USDC/USDT balance to USD.
    Stable,
}

impl Purpose {
    const fn prefix(self) -> &'static str {
        match self {
            Purpose::Rebalance => "rebalance:",
            Purpose::DepositBuy => "deposit:",
            Purpose::Stable => "stable:",
        }
    }

    fn tag(self, pair: &str) -> String {
        format!("{}{pair}", self.prefix())
    }

    fn parse(tag: &str) -> Option<(Purpose, &str)> {
        for p in [Purpose::Rebalance, Purpose::DepositBuy, Purpose::Stable] {
            if let Some(rest) = tag.strip_prefix(p.prefix()) {
                return Some((p, rest));
            }
        }
        None
    }
}

/// Hard rule: one open order per pair at a time. Pulled out as its own
/// function so that invariant is a plain predicate to test, independent of
/// which of the three call sites (gap-closing, deposit-investing, stable
/// conversion) is asking.
fn has_open_order(state: &paper::State, pair: &str) -> bool {
    state.pending_orders.iter().any(|o| o.pair == pair)
}

/// Kraken asset code → the 6 this bot's accounting understands, or the code
/// unchanged for anything else (still recorded, at $0, with a warning —
/// never silently dropped).
fn normalize_asset(code: &str) -> &str {
    let base = code.split(['.', '-']).next().unwrap_or(code);
    match base {
        "ZUSD" | "USD" => "USD",
        "XXBT" | "XBT" | "BTC" => "BTC",
        "XETH" | "ETH" => "ETH",
        "SOL" => "SOL",
        "USDC" => "USDC",
        "USDT" => "USDT",
        other => other,
    }
}

/// USD value of one flow amount. `None` when this bot has no mark for the
/// asset (an outage, or an asset outside BTC/ETH/SOL/USD/USDC/USDT) — the
/// caller logs that and records $0 rather than guessing.
fn usd_value_of(asset: &str, amount: f64, m: alloc::Marks) -> Option<f64> {
    match asset {
        "USD" | "USDC" | "USDT" => Some(amount),
        "BTC" if m.btc > 0.0 => Some(amount * m.btc),
        "ETH" if m.eth > 0.0 => Some(amount * m.eth),
        "SOL" if m.sol > 0.0 => Some(amount * m.sol),
        _ => None,
    }
}

/// Apply one newly-seen Kraken ledger entry: record it in `state.flows` (the
/// dedup record), fold its USD value — NET of Kraken's own `fee` on the
/// entry — into `net_deposits_usd`, and — for a USD-like deposit only — set
/// `deposit_pending` so the next gap-closing pass tops up whatever it finds
/// underweight. A deposit counts as `amount - fee` (what actually landed); a
/// withdrawal counts as `amount + fee` (what actually left, including what
/// Kraken kept). Returns whether it was new (`false` means a refid already
/// on file, which is exactly the hard rule "a deposit refid is counted
/// exactly once, even across restarts and repeated ledger pages").
#[allow(clippy::too_many_arguments)]
fn apply_ledger_entry(
    state: &mut paper::State,
    refid: &str,
    ts: i64,
    entry_type: &str,
    raw_asset: &str,
    amount: f64,
    fee: f64,
    m: alloc::Marks,
) -> bool {
    if refid.trim().is_empty() || state.flows.iter().any(|f| f.refid == refid) {
        return false;
    }
    let kind = match entry_type {
        "deposit" => paper::FlowKind::Deposit,
        "withdrawal" => paper::FlowKind::Withdrawal,
        _ => return false,
    };
    if !amount.is_finite() {
        warn!("ledger {refid}: non-numeric amount — skipped");
        return false;
    }
    let fee = if fee.is_finite() { fee.max(0.0) } else { 0.0 };
    let net_units = match kind {
        paper::FlowKind::Deposit => (amount.abs() - fee).max(0.0),
        paper::FlowKind::Withdrawal => amount.abs() + fee,
    };
    let asset = normalize_asset(raw_asset).to_string();
    let usd_value = match usd_value_of(&asset, net_units, m) {
        Some(v) => v,
        None => {
            warn!("ledger {refid}: no mark for {asset} — recording at $0");
            0.0
        }
    };
    state.flows.push(paper::FlowRecord {
        refid: refid.to_string(),
        ts,
        kind,
        asset: asset.clone(),
        amount,
        usd_value,
    });
    match kind {
        paper::FlowKind::Deposit => {
            state.net_deposits_usd += usd_value;
            if matches!(asset.as_str(), "USD" | "USDC" | "USDT") {
                state.deposit_pending = true;
            }
        }
        paper::FlowKind::Withdrawal => state.net_deposits_usd -= usd_value,
    }
    true
}

/// Scan Kraken's Ledgers for deposits and withdrawals newer than
/// `state.last_ledger_time`, paging past Kraken's 50-per-call limit
/// (`LiveKraken::ledgers`). Live mode only — this is a PRIVATE endpoint, and
/// a dry run previews order sizing, not deposit bookkeeping.
async fn maybe_scan_ledgers(
    state: &mut paper::State,
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    m: alloc::Marks,
    now: i64,
    events: &mut Vec<String>,
) {
    if !matches!(mode, Mode::Live) {
        return;
    }
    // CRITICAL defensive baseline: an unset `last_ledger_time` means "ask
    // Kraken for everything since the dawn of time", which would find every
    // historical deposit "new" (nothing in `flows` yet) and re-invest money
    // that has been sitting invested for months. `maybe_run_policy_migration`
    // is supposed to have already set this to the moment of the move, but
    // this stands on its own — e.g. a state.json that predates this policy
    // and has somehow reached a live tick before its migration ran. Checked
    // BEFORE the gateway is even touched, so this is testable without one.
    if state.last_ledger_time <= 0.0 {
        state.last_ledger_time = now as f64;
        let msg = "ledger baseline set — historical deposits are not re-scanned".to_string();
        info!("{msg}");
        events.push(msg);
        return;
    }
    let Some(gw) = live_gw else { return };
    // Kraken's `start` is exclusive, and ledger time is a float, so this may
    // legitimately re-offer an entry already recorded — `apply_ledger_entry`
    // is what actually de-duplicates, by refid, not this cursor.
    let baseline = state.last_ledger_time;
    let start = Some(baseline.floor() as u64);
    let mut newest = baseline;
    for entry_type in ["deposit", "withdrawal"] {
        let entries = match gw.ledgers(entry_type, start).await {
            Ok(e) => e,
            Err(e) => {
                warn!("ledgers {entry_type}: {e:#} — retrying next tick");
                continue;
            }
        };
        apply_ledger_page(state, entries, baseline, m, &mut newest, events);
    }
    state.last_ledger_time = newest;
}

/// Apply one already-fetched page of Kraken ledger entries: skip anything at
/// or before `baseline` (defence in depth — Kraken's own `start` filter
/// should already exclude these, but this is what keeps a server-side quirk
/// or a future bug in the cursor from re-investing money that is already
/// invested), then hand the rest to `apply_ledger_entry` for its refid dedup.
/// Pulled out of `maybe_scan_ledgers` so it is testable against hand-built
/// entries with no network call.
fn apply_ledger_page(
    state: &mut paper::State,
    entries: Vec<exchange_apiws::kraken::KrakenLedgerEntry>,
    baseline: f64,
    m: alloc::Marks,
    newest: &mut f64,
    events: &mut Vec<String>,
) {
    for e in entries {
        if e.time > *newest {
            *newest = e.time;
        }
        if e.time <= baseline {
            continue;
        }
        let amount: f64 = match e.amount.parse() {
            Ok(v) => v,
            Err(_) => {
                warn!(
                    "ledger {}: unreadable amount {:?} — skipped",
                    e.refid, e.amount
                );
                continue;
            }
        };
        // An unreadable fee defaults to 0 rather than dropping the whole
        // entry — the amount is what makes this a deposit worth recording
        // at all; the fee only refines how much of it counts.
        let fee: f64 = e.fee.parse().unwrap_or(0.0);
        if apply_ledger_entry(
            state,
            &e.refid,
            e.time as i64,
            &e.entry_type,
            &e.asset,
            amount,
            fee,
            m,
        ) {
            let usd = state.flows.last().map(|f| f.usd_value).unwrap_or(0.0);
            let msg = format!(
                "LEDGER {} {} {amount:.8} refid={} (${usd:.2})",
                e.entry_type, e.asset, e.refid
            );
            info!("{msg}");
            events.push(msg);
        }
    }
}

/// Whether `maybe_read_regime` should actually attempt a read right now.
///
/// Normally once per UTC day (`last_regime_day`). But ALSO whenever any
/// `regime::PAIRS` entry is missing from `regime_bull` — e.g. right after
/// deploying this policy, when the OLD bot already set `last_regime_day` to
/// today before `regime_bull` (a brand new field) had ever been written, and
/// without this check BTC would have no reading — and so no effective
/// target — until tomorrow. Either way, never inside the first 5 minutes of
/// a UTC day: the daily candle has only just closed at 00:00 and needs a
/// moment to settle before it is trusted. Pulled out as its own pure
/// function so the gate is testable with a hand-built clock and no network.
fn should_read_regime(state: &paper::State, now_utc: chrono::DateTime<Utc>) -> bool {
    let today = now_utc.format("%Y-%m-%d").to_string();
    let missing_pair = regime::PAIRS
        .iter()
        .any(|p| !state.regime_bull.contains_key(*p));
    if state.last_regime_day == today && !missing_pair {
        return false;
    }
    if now_utc.hour() == 0 && now_utc.minute() < 5 {
        return false;
    }
    true
}

/// Read each coin's regime from its closed daily candles, once per UTC day,
/// not before 00:05 UTC (the daily candle closes at 00:00 and a few minutes'
/// buffer avoids trusting it the instant it rolls over). Updates
/// `state.regime_bull` only — `maybe_close_gaps` is what trades a change.
async fn maybe_read_regime(
    state: &mut paper::State,
    client: &KrakenRestClient,
    events: &mut Vec<String>,
) {
    let now_utc = Utc::now();
    if !should_read_regime(state, now_utc) {
        return;
    }
    let today = now_utc.format("%Y-%m-%d").to_string();
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
    for (pair, r) in &readings {
        let prev = state.regime_bull.get(*pair).copied();
        let label = if r.bull { "BULL" } else { "BEAR" };
        // The price that would flip THIS regime: down through the band from
        // a bull, up through it from a bear — `SMA * (1 ∓ BAND)`.
        let flip_px = if r.bull {
            r.sma * (1.0 - regime::BAND)
        } else {
            r.sma * (1.0 + regime::BAND)
        };
        let msg = format!(
            "regime {pair} {label}: close {:.2} vs 200d {:.2} ({:+.1}%) — flips at {:.2}{}",
            r.close,
            r.sma,
            100.0 * r.distance(),
            flip_px,
            match prev {
                None => " (first read)",
                Some(p) if p != r.bull => " (CHANGED)",
                Some(_) => "",
            }
        );
        info!("{msg}");
        if prev != Some(r.bull) {
            events.push(msg);
        }
        state.regime_bull.insert(pair.to_string(), r.bull);
        state.regime_reading.insert(pair.to_string(), *r);
    }
    state.last_regime_day = today;
}

/// Close each pair's gap to its effective target while `regime_applied`
/// disagrees with `regime_bull` — see `alloc.rs`'s module docs for why this
/// is the entire "no drift rebalancing" mechanism. While `deposit_pending`,
/// ALSO tops up (buy-only) every pair that is already applied, so fresh
/// deposit cash flows into whatever it makes underweight.
#[allow(clippy::too_many_arguments)]
async fn maybe_close_gaps(
    state: &mut paper::State,
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    client: &KrakenRestClient,
    w: &alloc::Wallet,
    m: alloc::Marks,
    policy: alloc::Policy,
    now: i64,
    events: &mut Vec<String>,
) {
    // ONE snapshot per tick: stable conversion (not a buy, so untouched by
    // this), gap-closing and deposit top-ups all read the SAME `w`, so
    // without a shared running total two pairs could each size a buy
    // against the full free cash and together overspend it. Started at the
    // raw `usd_available()` — `gap_step` applies `FEE_RESERVE` itself —
    // and decremented by each buy's cost as soon as it is DECIDED, whether
    // or not it is actually placed (FROZEN, or Kraken rejects it): the
    // point is that no later pair in this same tick sizes against dollars
    // an earlier pair already committed to.
    let mut spendable = w.usd_available();
    let mut any_deposit_buy_this_tick = false;
    for pair in regime::PAIRS {
        let Some(bull) = state.regime_bull.get(pair).copied() else {
            continue;
        };
        let applied = state.regime_applied.get(pair).copied() == Some(bull);
        if applied && !state.deposit_pending {
            continue; // nothing outstanding and no deposit to invest
        }
        if has_open_order(state, pair) {
            continue; // one open order per pair — wait for it to settle
        }
        let touch = match fetch_touch(client, pair).await {
            Ok(t) => t,
            Err(e) => {
                warn!("gap ticker {pair}: {e:#} — retrying next tick");
                continue;
            }
        };
        if applied {
            if deposit_top_up_pair(
                state,
                mode,
                live_gw,
                w,
                m,
                touch,
                pair,
                bull,
                policy,
                &mut spendable,
                now,
                events,
            )
            .await
            {
                any_deposit_buy_this_tick = true;
            }
        } else {
            close_one_gap(
                state,
                mode,
                live_gw,
                w,
                m,
                touch,
                pair,
                bull,
                policy,
                &mut spendable,
                now,
                events,
            )
            .await;
        }
    }
    maybe_clear_deposit_pending(state, w, any_deposit_buy_this_tick, events);
}

/// For a pair whose regime is ALREADY applied (no flip outstanding): while
/// `deposit_pending`, size it against its effective target anyway — fresh
/// cash makes an up-to-date pair's weight look smaller against the bigger
/// total — but act ONLY on a buy. A sell here would mean ordinary price
/// drift pushed the pair over its target, which is exactly the drift this
/// policy otherwise leaves alone; a deposit must never be the reason an
/// untouched pair gets sold. Returns whether a buy was wanted this tick
/// (placed or not), which is what `maybe_clear_deposit_pending` needs to
/// know "is there still work to do" rather than "did Kraken accept it."
#[allow(clippy::too_many_arguments)]
async fn deposit_top_up_pair(
    state: &mut paper::State,
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    w: &alloc::Wallet,
    m: alloc::Marks,
    touch: alloc::Touch,
    pair: &str,
    bull: bool,
    policy: alloc::Policy,
    spendable: &mut f64,
    now: i64,
    events: &mut Vec<String>,
) -> bool {
    let r = match alloc::gap_step(w, m, touch, pair, bull, *spendable) {
        alloc::GapStep::Order(r) if r.side > 0 => r,
        _ => return false, // AtTarget / NotYet / Unpriced / a sell: not ours to act on
    };
    *spendable -= r.qty * r.price;
    events.push(format!(
        "{pair}: investing a deposit toward its (already-applied) target"
    ));
    if !policy.armed {
        info!(
            "FROZEN — would invest ${:.2} into {pair} from a deposit",
            r.qty * r.price
        );
        return true;
    }
    if let Some(order) = place_rebalance(mode, live_gw, &r, Purpose::DepositBuy, now, events).await
    {
        state.pending_orders.push(order);
    }
    true
}

/// Clear `deposit_pending` once there is nothing left it could still do: no
/// pair wanted (or could afford) a top-up buy this tick, no USDC/USDT
/// balance is still waiting on `stable_sell`, and no deposit-buy order is
/// currently open (one placed on an earlier tick, still awaiting fill or
/// settlement, must not be abandoned just because THIS tick's pass over the
/// other pairs found nothing new to do).
fn maybe_clear_deposit_pending(
    state: &mut paper::State,
    w: &alloc::Wallet,
    any_deposit_buy_this_tick: bool,
    events: &mut Vec<String>,
) {
    if !state.deposit_pending || any_deposit_buy_this_tick {
        return;
    }
    let stable_awaiting_conversion = w.usdc >= alloc::MIN_STABLE || w.usdt >= alloc::MIN_STABLE;
    let deposit_order_open = state.pending_orders.iter().any(|o| {
        o.book.as_deref().and_then(Purpose::parse).map(|(p, _)| p) == Some(Purpose::DepositBuy)
    });
    if stable_awaiting_conversion || deposit_order_open {
        return;
    }
    state.deposit_pending = false;
    let msg = "deposit investing complete — deposit_pending cleared".to_string();
    info!("{msg}");
    events.push(msg);
}

/// The decide-and-place half of `maybe_close_gaps`, split out so it can be
/// unit tested against a hand-built `Touch` with no network call.
#[allow(clippy::too_many_arguments)]
async fn close_one_gap(
    state: &mut paper::State,
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    w: &alloc::Wallet,
    m: alloc::Marks,
    touch: alloc::Touch,
    pair: &str,
    bull: bool,
    policy: alloc::Policy,
    spendable: &mut f64,
    now: i64,
    events: &mut Vec<String>,
) {
    match alloc::gap_step(w, m, touch, pair, bull, *spendable) {
        alloc::GapStep::AtTarget => {
            info!(
                "{pair}: at its effective target ({})",
                if bull { "bull" } else { "bear" }
            );
            state.regime_applied.insert(pair.to_string(), bull);
        }
        alloc::GapStep::NotYet => {
            info!("{pair}: outside its effective target but under-funded — retrying next tick");
        }
        alloc::GapStep::Unpriced => {
            info!("{pair}: marks or touch incomplete — retrying next tick");
        }
        alloc::GapStep::Order(r) => {
            // Only a BUY claims shared cash — a sell raises cash (once it
            // settles) rather than spending it, so it never competes with
            // another pair's buy for the SAME dollars this tick.
            if r.side > 0 {
                *spendable -= r.qty * r.price;
            }
            events.push(format!(
                "{pair}: closing gap to its effective target ({})",
                if bull { "bull" } else { "bear" }
            ));
            if !policy.armed {
                info!(
                    "FROZEN — would {} {:.8} {pair}",
                    if r.side > 0 { "buy" } else { "sell" },
                    r.qty
                );
                return;
            }
            if let Some(order) =
                place_rebalance(mode, live_gw, &r, Purpose::Rebalance, now, events).await
            {
                state.pending_orders.push(order);
            }
        }
    }
}

/// Once per UTC day: snapshot the account for the daily/weekly/monthly/yearly
/// performance view a future web UI will build — see `State::history`.
fn maybe_append_history(
    state: &mut paper::State,
    w: &alloc::Wallet,
    m: alloc::Marks,
    events: &mut Vec<String>,
) {
    if !m.complete() {
        return;
    }
    let today = Utc::now().format("%Y-%m-%d").to_string();
    if state.last_history_day == today {
        return;
    }
    let qty: std::collections::BTreeMap<String, f64> = [
        ("BTC".to_string(), w.btc),
        ("ETH".to_string(), w.eth),
        ("SOL".to_string(), w.sol),
        ("USD".to_string(), w.usd),
        ("USDC".to_string(), w.usdc),
        ("USDT".to_string(), w.usdt),
    ]
    .into_iter()
    .collect();
    let mark: std::collections::BTreeMap<String, f64> = [
        ("BTC".to_string(), m.btc),
        ("ETH".to_string(), m.eth),
        ("SOL".to_string(), m.sol),
        ("USD".to_string(), 1.0),
        ("USDC".to_string(), 1.0),
        ("USDT".to_string(), 1.0),
    ]
    .into_iter()
    .collect();
    let total = alloc::total_usd(w, m);
    state.history.push(paper::DailySnapshot {
        date: today.clone(),
        total_usd: total,
        qty,
        mark,
        net_deposits_usd_to_date: state.net_deposits_usd,
    });
    events.push(format!(
        "HISTORY {today}: total ${total:.2}, net deposits ${:.2} to date",
        state.net_deposits_usd
    ));
    state.last_history_day = today;
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

/// The best bid/ask for one pair, read fresh right before sizing an order off
/// it — see `alloc::Touch`.
async fn fetch_touch(client: &KrakenRestClient, pair: &str) -> anyhow::Result<alloc::Touch> {
    let t = client
        .get_ticker(pair)
        .await
        .map_err(|e| anyhow::anyhow!("ticker {pair}: {e}"))?;
    let Some((_, tick)) = t.iter().next() else {
        anyhow::bail!("ticker {pair}: empty response");
    };
    Ok(alloc::Touch {
        bid: tick.bid_price(),
        ask: tick.ask_price(),
    })
}

/// A 32-lowercase-hex-char client order id, unique per order. Kraken only
/// requires uniqueness among the account's currently-OPEN orders, which a
/// handful of orders a day with a multi-minute TTL clears easily — this does
/// not need to be cryptographically random, just practically distinct.
fn new_client_order_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64;
    let mut h1 = RandomState::new().build_hasher();
    h1.write_u64(nanos);
    h1.write_u64(n);
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u64(n ^ 0x9E37_79B9_7F4A_7C15);
    h2.write_u64(nanos.rotate_left(17));
    format!("{:016x}{:016x}", h1.finish(), h2.finish())
}

/// Place one post-only order for a `Rebalance` the policy wants, logging it
/// either way. Returns the `PendingOrder` to track only when Kraken actually
/// accepted it in true live mode — paper and dry-run never have anything to
/// track, which is what makes `--dry-run` a preview rather than a simulation
/// of settlement too.
async fn place_rebalance(
    mode: Mode,
    live_gw: Option<&live::LiveKraken>,
    r: &alloc::Rebalance,
    purpose: Purpose,
    now: i64,
    events: &mut Vec<String>,
) -> Option<paper::PendingOrder> {
    let side_s = if r.side > 0 { "buy" } else { "sell" };
    let vol = format!("{:.8}", alloc::floor_qty(r.qty));
    let px = alloc::limit_price(r.pair, r.price);
    let cid = new_client_order_id();
    let tag = purpose.tag(r.pair);
    let msg = format!(
        "KRAKEN {} POST-ONLY {side_s} {} vol={vol} px={px} [{tag}] cid={cid}",
        match mode {
            Mode::Live => "PLACE",
            _ => "WOULD PLACE",
        },
        r.pair,
    );
    info!("{msg}");
    events.push(msg);
    if !matches!(mode, Mode::Live) {
        return None;
    }
    let gw = live_gw?;
    match gw
        .place_post_only(r.pair, r.side, &vol, &px, &cid, ORDER_TTL_SECS as u64)
        .await
    {
        Ok(txid) => Some(paper::PendingOrder {
            txid,
            pair: r.pair.to_string(),
            side: r.side,
            placed_at: now,
            book: Some(tag),
            qty: r.qty,
        }),
        Err(e) => {
            warn!("live order failed: {e:#}");
            None
        }
    }
}

/// Seconds an unfilled post-only order is left alone before being cancelled.
/// Also sent to Kraken as the order's own `expire_after_secs` (defence in
/// depth: if this bot never wakes up to cancel it, Kraken does).
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
/// up in `ClosedOrders` on the tick after, and that number is what the
/// bookkeeping needs. Dropping the order at cancel time would leave the
/// backlog or the gap-closing logic believing money moved that never did.
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
    // real fills from the bookkeeping.
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
                "giving up on {} after {}s unsettled",
                order.txid,
                now - order.placed_at
            ),
        }
    }
}

/// Apply one settled order: always to the real ledger (`record_fill`, so real
/// P&L stays tracked regardless of what the order was FOR), then whatever its
/// `Purpose` needs. `regime_applied` is deliberately left untouched here for
/// a `Rebalance` order either way — `maybe_close_gaps` recomputes the gap
/// from the wallet fresh next tick and either finds it closed or places the
/// remainder, which is what makes a partial fill retry correctly without any
/// bookkeeping of "how much is left" beyond the wallet itself.
fn settle_one(
    state: &mut paper::State,
    order: &paper::PendingOrder,
    exec: ledger::Execution,
    now: i64,
    events: &mut Vec<String>,
) {
    let got = exec.vol_exec;
    record_fill(state, order, exec, now, events);
    let Some((purpose, pair)) = order.book.as_deref().and_then(Purpose::parse) else {
        return;
    };
    match purpose {
        // Both a flip/migration/command rebalance and a deposit top-up buy
        // settle the same way: there is no separate tally to reduce (the
        // deposit backlog this used to decrement is gone — see alloc.rs's
        // module docs), so a fill just needs its retry/partial-fill message,
        // and the ledger entry above already recorded the real money.
        Purpose::Rebalance | Purpose::DepositBuy => {
            if order.qty > 0.0 && got <= 1e-9 {
                let msg = format!("{pair} order {} never filled — will retry", order.txid);
                info!("{msg}");
                events.push(msg);
            } else if got + 1e-9 < order.qty {
                let msg = format!(
                    "{pair} PARTIAL fill {got:.8} of {:.8} — the remainder is retried",
                    order.qty
                );
                info!("{msg}");
                events.push(msg);
            }
        }
        Purpose::Stable => {
            if got <= 1e-9 {
                let msg = format!(
                    "{pair} conversion order {} never filled — will retry",
                    order.txid
                );
                info!("{msg}");
                events.push(msg);
            }
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
        ledger::Recorded::Rejected(why) => {
            tracing::debug!("LEDGER skip {}: {why}", order.txid)
        }
    }
}

#[cfg(test)]
mod work_tick_tests {
    use super::*;

    fn marks() -> alloc::Marks {
        alloc::Marks {
            btc: 85_000.0,
            eth: 2_700.0,
            sol: 120.0,
        }
    }

    fn touch_at(px: f64) -> alloc::Touch {
        alloc::Touch {
            bid: px - 0.5,
            ask: px + 0.5,
        }
    }

    #[test]
    fn client_order_ids_are_32_lowercase_hex_and_unique() {
        let a = new_client_order_id();
        let b = new_client_order_id();
        assert_eq!(a.len(), 32, "{a}");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "{a}"
        );
        assert_ne!(a, b);
    }

    #[test]
    fn invariant_one_open_order_per_pair() {
        let mut s = paper::State::default_paper();
        assert!(!has_open_order(&s, "XBTUSD"));
        s.pending_orders.push(paper::PendingOrder {
            txid: "O1".into(),
            pair: "XBTUSD".into(),
            side: 1,
            placed_at: 0,
            book: Some(Purpose::Rebalance.tag("XBTUSD")),
            qty: 0.001,
        });
        assert!(
            has_open_order(&s, "XBTUSD"),
            "an order for this pair is already open"
        );
        assert!(
            !has_open_order(&s, "ETHUSD"),
            "a different pair is unaffected"
        );
    }

    #[test]
    fn purpose_tags_round_trip_through_the_pending_order_book_field() {
        for (p, pair) in [
            (Purpose::Rebalance, "XBTUSD"),
            (Purpose::DepositBuy, "ETHUSD"),
            (Purpose::Stable, "USDCUSD"),
        ] {
            let tag = p.tag(pair);
            assert_eq!(Purpose::parse(&tag), Some((p, pair)));
        }
        assert_eq!(
            Purpose::parse("eth_1h_sf"),
            None,
            "an old book id must not parse as a purpose"
        );
    }

    #[test]
    fn a_pre_policy_fixture_starts_the_move_on_the_next_load() {
        // hard rule #2: the fixture must load AND start the policy move.
        let raw = include_str!("../tests/fixtures/state_v1.json");
        let mut s: paper::State = serde_json::from_str(raw).expect("fixture parses");
        assert!(s.policy_version < 2);
        assert!(
            !s.regime_applied.is_empty(),
            "the fixture has prior applied regimes to clear"
        );
        // Roughly the account the brief describes migrating FROM: BTC 56%,
        // ETH 10%, SOL 10%, USD 24%.
        let w = alloc::Wallet {
            usd: 240.0,
            btc: 0.0066,
            eth: 0.037,
            sol: 0.87,
            ..alloc::Wallet::default()
        };
        let m = alloc::Marks {
            btc: 85_000.0,
            eth: 2_700.0,
            sol: 120.0,
        };
        let now = 1_728_000_000;
        let mut events = Vec::new();
        maybe_run_policy_migration(&mut s, &w, m, now, &mut events);
        assert_eq!(s.policy_version, 2);
        assert!(
            s.regime_applied.is_empty(),
            "every pair must be pending after the move"
        );
        assert!(events.iter().any(|e| e.contains("POLICY move")));
        // Books are untouched by the move itself.
        assert_eq!(s.books.len(), 3);
        // The live ledger's one PRE-EXISTING fill survives, and the move
        // gave the wallet's current holdings a basis to sell against.
        assert_eq!(s.live.fills.len(), 1);
        for pair in regime::PAIRS {
            assert!(
                s.live
                    .get(ledger::Sleeve::Trade, pair)
                    .is_some_and(|p| p.qty > 0.0),
                "{pair} must have an adopted basis after the move"
            );
        }
        // CRITICAL: the ledger-scan baseline moves to NOW, so the next scan
        // never asks Kraken for deposits from before the move.
        assert_eq!(
            s.last_ledger_time, now as f64,
            "migration must set the ledger baseline to NOW"
        );

        // Idempotent: a second run (e.g. the very next tick) does nothing.
        let mut again = Vec::new();
        maybe_run_policy_migration(&mut s, &w, m, now + 3600, &mut again);
        assert!(again.is_empty());
        assert_eq!(s.policy_version, 2);
        assert_eq!(
            s.last_ledger_time, now as f64,
            "a second run must not move the baseline again"
        );
    }

    #[test]
    fn migration_sets_the_ledger_baseline_so_old_deposits_are_never_rescanned() {
        // The CRITICAL bug this fixes: on a fresh state.json, last_ledger_time
        // starts at 0, which would make the first ledger scan ask Kraken for
        // every deposit ever made and re-invest money already invested.
        let mut s = paper::State::default_paper();
        assert_eq!(s.last_ledger_time, 0.0);
        let w = alloc::Wallet {
            usd: 100.0,
            ..alloc::Wallet::default()
        };
        let m = marks();
        let now = 1_728_000_000;
        maybe_run_policy_migration(&mut s, &w, m, now, &mut Vec::new());
        assert_eq!(
            s.last_ledger_time, now as f64,
            "migration must set the ledger baseline to NOW, not leave it at 0"
        );
    }

    #[tokio::test]
    async fn the_first_live_scan_sets_the_baseline_and_applies_nothing() {
        // Defence in depth for the same CRITICAL bug, at the scan itself: if
        // `last_ledger_time` is somehow still <= 0 when a live tick reaches
        // the scan (e.g. a state.json that predates this policy and has not
        // yet seen a migration tick), the scan must set the baseline and
        // return WITHOUT ever asking Kraken for anything — `live_gw: None`
        // here proves no network call happens on this path.
        let mut s = paper::State::default_paper();
        assert_eq!(s.last_ledger_time, 0.0);
        let now = 1_728_000_000;
        let mut events = Vec::new();
        maybe_scan_ledgers(&mut s, Mode::Live, None, marks(), now, &mut events).await;
        assert_eq!(s.last_ledger_time, now as f64);
        assert!(s.flows.is_empty());
        assert_eq!(s.net_deposits_usd, 0.0);
        assert!(events.iter().any(|e| e.contains("baseline set")));
    }

    fn ledger_entry(
        refid: &str,
        time: f64,
        entry_type: &str,
        asset: &str,
        amount: &str,
    ) -> exchange_apiws::kraken::KrakenLedgerEntry {
        ledger_entry_with_fee(refid, time, entry_type, asset, amount, "0.00000000")
    }

    fn ledger_entry_with_fee(
        refid: &str,
        time: f64,
        entry_type: &str,
        asset: &str,
        amount: &str,
        fee: &str,
    ) -> exchange_apiws::kraken::KrakenLedgerEntry {
        serde_json::from_value(serde_json::json!({
            "refid": refid,
            "time": time,
            "type": entry_type,
            "subtype": "",
            "aclass": "currency",
            "asset": asset,
            "amount": amount,
            "fee": fee,
            "balance": "0.00000000",
        }))
        .expect("Kraken's own ledger-entry shape")
    }

    #[test]
    fn a_migrated_state_scanned_against_a_page_of_old_deposits_records_none_of_them() {
        // The exact scenario the review called out: even if Kraken's own
        // `start` filter somehow still returned deposits from before the
        // policy move, the baseline check here is a second, client-side
        // guarantee that they are never applied.
        let mut s = paper::State::default_paper();
        let baseline = 1_728_000_000.0;
        s.last_ledger_time = baseline;
        let old_page = vec![
            ledger_entry(
                "LOLD1",
                baseline - 10_000.0,
                "deposit",
                "ZUSD",
                "500.00000000",
            ),
            ledger_entry("LOLD2", baseline - 5_000.0, "deposit", "XXBT", "0.01000000"),
            // Exactly AT the baseline is also excluded: `start` is exclusive.
            ledger_entry("LOLD3", baseline, "deposit", "ZUSD", "10.00000000"),
        ];
        let mut newest = baseline;
        let mut events = Vec::new();
        apply_ledger_page(
            &mut s,
            old_page,
            baseline,
            marks(),
            &mut newest,
            &mut events,
        );
        assert!(s.flows.is_empty(), "no old deposit must be recorded");
        assert_eq!(s.net_deposits_usd, 0.0);
        assert!(events.is_empty());
    }

    #[test]
    fn a_ledger_page_still_applies_entries_genuinely_after_the_baseline() {
        let mut s = paper::State::default_paper();
        let baseline = 1_728_000_000.0;
        s.last_ledger_time = baseline;
        let page = vec![ledger_entry(
            "LNEW1",
            baseline + 10.0,
            "deposit",
            "ZUSD",
            "75.00000000",
        )];
        let mut newest = baseline;
        let mut events = Vec::new();
        apply_ledger_page(&mut s, page, baseline, marks(), &mut newest, &mut events);
        assert_eq!(s.flows.len(), 1, "a genuinely new deposit must be recorded");
        assert_eq!(s.net_deposits_usd, 75.0);
        assert_eq!(newest, baseline + 10.0);
    }

    #[test]
    fn rebalance_command_clears_applied_regimes_the_same_way_migration_does() {
        let mut s = paper::State::default_paper();
        s.policy_version = 2;
        s.regime_applied.insert("XBTUSD".into(), true);
        s.regime_applied.insert("ETHUSD".into(), false);
        s.regime_bull.insert("XBTUSD".into(), true);
        s.regime_bull.insert("ETHUSD".into(), false);
        // Same mechanism `rebalance_cmd` uses, exercised directly (no file IO
        // needed to test the actual effect).
        s.regime_applied.clear();
        assert!(s.regime_applied.is_empty());
        // Both pairs now disagree with regime_bull, so the next gap-closing
        // pass will act on both even though neither regime actually flipped.
        for pair in ["XBTUSD", "ETHUSD"] {
            assert_ne!(
                s.regime_applied.get(pair).copied(),
                s.regime_bull.get(pair).copied()
            );
        }
    }

    #[tokio::test]
    async fn deposit_pending_never_sells_an_applied_pair_that_drifted_overweight() {
        // ETH is "applied" (no flip outstanding) but has drifted ABOVE its
        // target through ordinary price appreciation — exactly the drift
        // this policy otherwise leaves alone. A deposit sitting elsewhere
        // must never turn that drift into a sell: `deposit_top_up_pair` is
        // BUY-ONLY, full stop.
        let mut s = paper::State::default_paper();
        s.deposit_pending = true;
        s.regime_bull.insert("ETHUSD".into(), true);
        s.regime_applied.insert("ETHUSD".into(), true);
        let m = marks();
        let w = alloc::Wallet {
            usd: 10.0,
            eth: 10.0, // $27,000 of ETH — way past its 25% target
            ..alloc::Wallet::default()
        };
        let touch = touch_at(m.eth);
        let mut events = Vec::new();
        let mut spendable = w.usd_available();
        let wanted = deposit_top_up_pair(
            &mut s,
            Mode::LiveDry,
            None,
            &w,
            m,
            touch,
            "ETHUSD",
            true,
            alloc::Policy::LIVE,
            &mut spendable,
            0,
            &mut events,
        )
        .await;
        assert!(
            !wanted,
            "an overweight applied pair must not be sold to fund a deposit"
        );
        assert!(s.pending_orders.is_empty());
        assert!(!events.iter().any(|e| e.contains("investing a deposit")));
    }

    // ── invariant: FROZEN places nothing ───────────────────────────────────

    /// Drives `close_one_gap` / `deposit_top_up_pair` / `convert_one_stable`
    /// directly with hand-built `Touch` values and `Mode::LiveDry` +
    /// `live_gw: None` — the same combination `place_rebalance` treats as
    /// "log WOULD PLACE, place nothing", so these calls make no network
    /// request at all regardless of the policy gate being tested.
    #[tokio::test]
    async fn invariant_frozen_places_nothing() {
        let mut s = paper::State::default_paper();
        let m = alloc::Marks {
            btc: 85_000.0,
            eth: 2_700.0,
            sol: 120.0,
        };
        // A wallet sitting at 0% of target, with plenty of cash and plenty
        // of USDC — every one of the three decision paths clearly wants to
        // trade here under Policy::LIVE.
        let w = alloc::Wallet {
            usd: 10_000.0,
            usdc: 50.0,
            ..alloc::Wallet::default()
        };
        s.deposit_pending = true;
        let mut events = Vec::new();
        let mut spendable = w.usd_available();

        for pair in regime::PAIRS {
            let touch = alloc::Touch {
                bid: m.of(pair) - 0.1,
                ask: m.of(pair) + 0.1,
            };
            close_one_gap(
                &mut s,
                Mode::LiveDry,
                None,
                &w,
                m,
                touch,
                pair,
                true,
                alloc::Policy::FROZEN,
                &mut spendable,
                0,
                &mut events,
            )
            .await;
            deposit_top_up_pair(
                &mut s,
                Mode::LiveDry,
                None,
                &w,
                m,
                touch,
                pair,
                true,
                alloc::Policy::FROZEN,
                &mut spendable,
                0,
                &mut events,
            )
            .await;
        }
        convert_one_stable(
            &mut s,
            Mode::LiveDry,
            None,
            w.usdc,
            "USDCUSD",
            alloc::Touch {
                bid: 0.999,
                ask: 1.0,
            },
            alloc::Policy::FROZEN,
            0,
            &mut events,
        )
        .await;

        assert!(
            s.pending_orders.is_empty(),
            "FROZEN must place nothing: {:?}",
            s.pending_orders
        );
        assert!(
            s.regime_applied.is_empty(),
            "FROZEN must not even mark a gap as closed"
        );
        assert!(
            !events
                .iter()
                .any(|e| e.contains("KRAKEN") || e.contains("WOULD PLACE")),
            "FROZEN must never reach the order-placement log line: {events:?}"
        );
        assert!(
            events.iter().any(|e| e.contains("closing gap")),
            "the gap itself is still noticed, just not acted on"
        );

        // And the SAME wallet under Policy::LIVE clearly WOULD place orders —
        // proving the test above is a real gate, not a vacuous one (nothing
        // to place regardless of policy).
        let mut live_events = Vec::new();
        let touch = alloc::Touch {
            bid: m.btc - 0.1,
            ask: m.btc + 0.1,
        };
        let mut live_spendable = w.usd_available();
        close_one_gap(
            &mut s,
            Mode::LiveDry,
            None,
            &w,
            m,
            touch,
            "XBTUSD",
            true,
            alloc::Policy::LIVE,
            &mut live_spendable,
            0,
            &mut live_events,
        )
        .await;
        assert!(
            live_events.iter().any(|e| e.contains("WOULD PLACE")),
            "{live_events:?}"
        );
    }

    // ── a whole day, simulated with hand-fed wallet/marks/touch — no network ──

    #[tokio::test]
    async fn a_hundred_dollar_usd_deposit_is_invested_by_effective_weight() {
        let mut s = paper::State::default_paper();
        s.policy_version = 2;
        let m = marks();
        // The account is already at its targets (every pair "applied")
        // before the deposit lands — otherwise this would just be an
        // ordinary flip, not the deposit-driven buy-only path.
        for pair in regime::PAIRS {
            s.regime_bull.insert(pair.to_string(), true);
            s.regime_applied.insert(pair.to_string(), true);
        }
        let mut events = Vec::new();

        // Detect the deposit.
        assert!(apply_ledger_entry(
            &mut s,
            "LDEP1",
            1_700_000_000,
            "deposit",
            "ZUSD",
            100.0,
            0.0,
            m
        ));
        assert!(s.deposit_pending, "a USD deposit must set deposit_pending");
        assert_eq!(s.net_deposits_usd, 100.0);

        // The fresh $100 sits as free USD, making every pair underweight
        // against the now-bigger total — each gets topped up, buy-only.
        let w = alloc::Wallet {
            usd: 100.0,
            ..alloc::Wallet::default()
        };
        let mut any_buy = false;
        let mut spendable = w.usd_available();
        for pair in regime::PAIRS {
            let touch = touch_at(m.of(pair));
            if deposit_top_up_pair(
                &mut s,
                Mode::LiveDry,
                None,
                &w,
                m,
                touch,
                pair,
                true,
                alloc::Policy::LIVE,
                &mut spendable,
                0,
                &mut events,
            )
            .await
            {
                any_buy = true;
            }
        }
        assert!(any_buy, "something must have wanted the deposit cash");
        assert!(events.iter().any(|e| e.contains("investing a deposit")));
    }

    #[tokio::test]
    async fn a_usdc_deposit_is_converted_then_invested() {
        let mut s = paper::State::default_paper();
        let m = marks();
        s.regime_bull.insert("ETHUSD".into(), true);
        s.regime_applied.insert("ETHUSD".into(), true);
        assert!(apply_ledger_entry(
            &mut s,
            "LDEP2",
            1_700_000_100,
            "deposit",
            "USDC",
            50.0,
            0.0,
            m
        ));
        assert!(
            s.deposit_pending,
            "a USDC deposit must ALSO set deposit_pending"
        );
        assert_eq!(s.net_deposits_usd, 50.0);

        // The wallet actually holds USDC, not USD, until `stable_sell` + its
        // settlement convert it — the top-up reads `usd_available()`, which
        // does not include USDC, so nothing is wanted yet.
        let w_before_convert = alloc::Wallet {
            usd: 0.0,
            usdc: 50.0,
            ..alloc::Wallet::default()
        };
        let mut events = Vec::new();
        let mut spendable_before = w_before_convert.usd_available();
        assert!(
            !deposit_top_up_pair(
                &mut s,
                Mode::LiveDry,
                None,
                &w_before_convert,
                m,
                touch_at(m.eth),
                "ETHUSD",
                true,
                alloc::Policy::LIVE,
                &mut spendable_before,
                0,
                &mut events,
            )
            .await,
            "USDC is not spendable cash until it is converted"
        );

        // Convert: sell the USDC balance.
        let r = alloc::stable_sell(w_before_convert.usdc, "USDCUSD", touch_at(1.0))
            .expect("above ordermin");
        assert_eq!(r.side, -1);
        let got = r.qty;
        let cost = got * r.price;
        let order = paper::PendingOrder {
            txid: "O-USDC".into(),
            pair: "USDCUSD".into(),
            side: -1,
            placed_at: 0,
            book: Some(Purpose::Stable.tag("USDCUSD")),
            qty: got,
        };
        settle_one(
            &mut s,
            &order,
            ledger::Execution {
                vol_exec: got,
                cost,
                fee: 0.0,
                at: Some(2),
            },
            2,
            &mut events,
        );
        // Now the wallet (as Kraken would report it next tick) holds USD
        // instead, and the top-up finds it.
        let w_after_convert = alloc::Wallet {
            usd: cost,
            usdc: 0.0,
            ..alloc::Wallet::default()
        };
        let mut spendable_after = w_after_convert.usd_available();
        assert!(
            deposit_top_up_pair(
                &mut s,
                Mode::LiveDry,
                None,
                &w_after_convert,
                m,
                touch_at(m.eth),
                "ETHUSD",
                true,
                alloc::Policy::LIVE,
                &mut spendable_after,
                0,
                &mut events,
            )
            .await,
            "the converted USD must now be investable"
        );
    }

    #[test]
    fn a_missing_pair_forces_a_read_even_if_last_regime_day_is_already_today() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap();
        let today = now.format("%Y-%m-%d").to_string();

        // All three pairs already known, and today's read already ran:
        // nothing to do.
        let mut s = paper::State::default_paper();
        s.last_regime_day = today;
        for pair in regime::PAIRS {
            s.regime_bull.insert(pair.to_string(), true);
        }
        assert!(
            !should_read_regime(&s, now),
            "nothing missing, already read today"
        );

        // BTC missing — e.g. the OLD bot already set last_regime_day today,
        // before regime_bull (a brand new field) ever had an entry. Must
        // still read, despite last_regime_day == today.
        s.regime_bull.remove("XBTUSD");
        assert!(
            should_read_regime(&s, now),
            "a pair with no reading yet must force a read"
        );
    }

    #[test]
    fn should_read_regime_still_waits_out_the_candle_buffer_even_with_a_missing_pair() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 10, 6, 0, 2, 0).unwrap(); // 00:02 UTC
        let s = paper::State::default_paper(); // nothing read yet at all
        assert!(
            !should_read_regime(&s, now),
            "still inside the 00:00-00:05 settle buffer"
        );
    }

    #[tokio::test]
    async fn two_buys_in_one_tick_share_the_same_cash_instead_of_doubling_up() {
        // BTC already fills most of the account, so the free $1,000 is the
        // whole story: ETH (25% of a $10,000 total = $2,500 wanted) and SOL
        // (15% = $1,500 wanted) BOTH individually want far more than half
        // of that $1,000 — the exact condition where sizing each against
        // `w.usd_available()` independently would commit ~$990 TWICE,
        // overspending the real $1,000 by nearly 2x.
        let m = marks();
        let w = alloc::Wallet {
            usd: 1_000.0,
            btc: 9_000.0 / m.btc,
            ..alloc::Wallet::default()
        };
        let mut s = paper::State::default_paper();
        let mut spendable = w.usd_available();
        let mut events = Vec::new();

        close_one_gap(
            &mut s,
            Mode::LiveDry,
            None,
            &w,
            m,
            touch_at(m.eth),
            "ETHUSD",
            true,
            alloc::Policy::LIVE,
            &mut spendable,
            0,
            &mut events,
        )
        .await;
        let spent_on_eth = w.usd_available() - spendable;
        assert!(
            spent_on_eth > 500.0,
            "ETH alone should claim most of the cash, got {spent_on_eth}"
        );

        close_one_gap(
            &mut s,
            Mode::LiveDry,
            None,
            &w,
            m,
            touch_at(m.sol),
            "SOLUSD",
            true,
            alloc::Policy::LIVE,
            &mut spendable,
            0,
            &mut events,
        )
        .await;
        let total_spent = w.usd_available() - spendable;
        assert!(
            total_spent <= w.usd_available() + 1e-6,
            "two buys in one tick spent ${total_spent:.2} against only ${:.2} available — \
             the second must have been capped by what the first already claimed",
            w.usd_available()
        );
    }

    #[test]
    fn a_bear_flip_then_a_bull_flip_each_resize_correctly() {
        let total = 1_000.0;
        let mut w = alloc::Wallet {
            usd: total * 0.10,
            btc: (total * alloc::BASE_BTC) / marks().btc,
            eth: (total * alloc::BASE_ETH) / marks().eth,
            sol: (total * alloc::BASE_SOL) / marks().sol,
            ..alloc::Wallet::default()
        };
        let m = marks();

        // ETH flips to bear: must sell down to its core.
        let t = touch_at(m.eth);
        let r = match alloc::gap_step(&w, m, t, "ETHUSD", false, w.usd_available()) {
            alloc::GapStep::Order(r) => r,
            other => panic!("expected a sell into bear core, got {other:?}"),
        };
        assert_eq!(r.side, -1);
        w.eth -= r.qty;
        w.usd += r.qty * r.price;
        assert!(matches!(
            alloc::gap_step(&w, m, t, "ETHUSD", false, w.usd_available()),
            alloc::GapStep::AtTarget
        ));

        // And back to bull: must buy back up to the full target.
        let r2 = match alloc::gap_step(&w, m, t, "ETHUSD", true, w.usd_available()) {
            alloc::GapStep::Order(r) => r,
            other => panic!("expected a buy back to the bull target, got {other:?}"),
        };
        assert_eq!(r2.side, 1);
        w.eth += r2.qty;
        w.usd -= r2.qty * r2.price;
        assert!(matches!(
            alloc::gap_step(&w, m, t, "ETHUSD", true, w.usd_available()),
            alloc::GapStep::AtTarget
        ));
    }

    #[test]
    fn a_post_only_rejection_is_simply_retried_next_tick_at_the_new_touch() {
        // "Rejected" (Kraken cancels a crossing post-only with nothing
        // filled) and "cancelled by our own TTL" look identical to
        // `settle_one`: zero executed. Either way `regime_applied` is left
        // disagreeing, so the NEXT tick's `gap_step` call (here, simply
        // called again by hand with a fresh touch) sizes a fresh order.
        let mut s = paper::State::default_paper();
        let m = marks();
        let w = alloc::Wallet {
            usd: 10_000.0,
            ..alloc::Wallet::default()
        };

        let stale_touch = touch_at(m.btc);
        let first = match alloc::gap_step(&w, m, stale_touch, "XBTUSD", true, w.usd_available()) {
            alloc::GapStep::Order(r) => r,
            other => panic!("{other:?}"),
        };
        let order = paper::PendingOrder {
            txid: "OREJECTED".into(),
            pair: "XBTUSD".into(),
            side: first.side,
            placed_at: 0,
            book: Some(Purpose::Rebalance.tag("XBTUSD")),
            qty: first.qty,
        };
        let mut events = Vec::new();
        settle_one(
            &mut s,
            &order,
            ledger::Execution {
                vol_exec: 0.0,
                cost: 0.0,
                fee: 0.0,
                at: None,
            },
            0,
            &mut events,
        );
        assert!(events.iter().any(|e| e.contains("never filled")));
        assert!(
            !s.regime_applied.contains_key("XBTUSD"),
            "a fully-rejected order must not be marked applied"
        );

        // Next tick, price moved — the new touch is used, not the stale one.
        let new_touch = touch_at(m.btc * 1.01);
        match alloc::gap_step(&w, m, new_touch, "XBTUSD", true, w.usd_available()) {
            alloc::GapStep::Order(r2) => assert_eq!(r2.price, new_touch.bid),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_partial_fill_on_a_deposit_buy_is_logged_and_the_remainder_retried() {
        // There is no backlog counter left to reduce (see alloc.rs's module
        // docs) — a deposit-buy's partial fill settles exactly like an
        // ordinary rebalance's: the real fill goes to the ledger, and the
        // shortfall is simply left for the next tick's fresh `gap_step` to
        // notice and re-order, sized off the wallet as it actually stands.
        let mut s = paper::State::default_paper();
        s.live.since = "2026-10-05".into();
        let order = paper::PendingOrder {
            txid: "OPARTIAL".into(),
            pair: "ETHUSD".into(),
            side: 1,
            placed_at: 0,
            book: Some(Purpose::DepositBuy.tag("ETHUSD")),
            qty: 0.01,
        };
        // Ordered $27-ish of ETH, only a THIRD filled.
        let exec = ledger::Execution {
            vol_exec: 0.0033,
            cost: 9.0,
            fee: 0.036,
            at: Some(5),
        };
        let mut events = Vec::new();
        settle_one(&mut s, &order, exec, 5, &mut events);
        assert!(
            events
                .iter()
                .any(|e| e.contains("PARTIAL fill 0.00330000 of 0.01000000")),
            "{events:?}"
        );
        // The REAL fill (cost + fee) landed on the ledger regardless.
        let p = s
            .live
            .get(ledger::Sleeve::Trade, "ETHUSD")
            .expect("a ledger slot");
        assert!((p.basis_usd - (9.0 + 0.036)).abs() < 1e-9);
    }

    #[test]
    fn invariant_a_deposit_refid_is_counted_exactly_once() {
        let mut s = paper::State::default_paper();
        let m = marks();
        assert!(apply_ledger_entry(
            &mut s, "LDUP", 100, "deposit", "ZUSD", 50.0, 0.0, m
        ));
        assert!(s.deposit_pending);
        // The SAME refid, re-offered (a re-fetched page after a restart, or
        // simply Kraken's `start` boundary tying on the same second).
        s.deposit_pending = false; // reset, so a double-count would re-set it
        assert!(!apply_ledger_entry(
            &mut s, "LDUP", 100, "deposit", "ZUSD", 50.0, 0.0, m
        ));
        assert!(
            !s.deposit_pending,
            "a duplicate refid must not re-arm investing"
        );
        assert_eq!(s.flows.len(), 1);
    }

    #[test]
    fn a_withdrawal_is_recorded_but_places_no_trade_and_shrinks_net_deposits() {
        let mut s = paper::State::default_paper();
        let m = marks();
        assert!(apply_ledger_entry(
            &mut s, "LDEP", 100, "deposit", "ZUSD", 200.0, 0.0, m
        ));
        s.deposit_pending = false; // simulate: already fully invested
        assert!(apply_ledger_entry(
            &mut s,
            "LWD",
            200,
            "withdrawal",
            "ZUSD",
            -75.0,
            0.0,
            m
        ));
        assert_eq!(s.net_deposits_usd, 125.0);
        assert!(
            !s.deposit_pending,
            "a withdrawal never re-arms deposit investing"
        );
        assert_eq!(s.flows.len(), 2);
        assert_eq!(s.flows[1].kind, paper::FlowKind::Withdrawal);
    }

    #[test]
    fn a_deposit_counts_net_of_krakens_own_fee() {
        let mut s = paper::State::default_paper();
        let m = marks();
        // $100 arrived, but Kraken's ledger shows a $1.50 fee on the entry:
        // only $98.50 actually landed.
        assert!(apply_ledger_entry(
            &mut s, "LFEE1", 100, "deposit", "ZUSD", 100.0, 1.5, m
        ));
        assert_eq!(
            s.net_deposits_usd, 98.5,
            "a deposit counts as amount - fee, not the gross amount"
        );
        assert_eq!(s.flows[0].usd_value, 98.5);
        // The raw ledger amount is still kept, unmodified, for the record.
        assert_eq!(s.flows[0].amount, 100.0);
    }

    #[test]
    fn a_withdrawal_counts_the_fee_as_additional_money_leaving() {
        let mut s = paper::State::default_paper();
        let m = marks();
        // $75 was withdrawn, plus a $2 network fee Kraken kept — $77 total
        // left the account, even though only $75 is in `amount`.
        assert!(apply_ledger_entry(
            &mut s,
            "LFEE2",
            100,
            "withdrawal",
            "ZUSD",
            -75.0,
            2.0,
            m
        ));
        assert_eq!(
            s.net_deposits_usd, -77.0,
            "a withdrawal counts as amount + fee, not just the amount"
        );
        assert_eq!(s.flows[0].usd_value, 77.0);
    }

    #[test]
    fn a_deposit_fee_larger_than_the_amount_floors_at_zero_not_negative() {
        // An edge case the formula must not invert: a fee that (hypothetically)
        // exceeds the deposited amount must read as "nothing landed", never
        // as a negative deposit.
        let mut s = paper::State::default_paper();
        let m = marks();
        assert!(apply_ledger_entry(
            &mut s, "LFEE3", 100, "deposit", "ZUSD", 1.0, 5.0, m
        ));
        assert_eq!(s.net_deposits_usd, 0.0);
    }

    #[test]
    fn an_unreadable_fee_defaults_to_zero_rather_than_dropping_the_deposit() {
        let mut s = paper::State::default_paper();
        let baseline = 0.0;
        let page = vec![ledger_entry_with_fee(
            "LFEE4",
            100.0,
            "deposit",
            "ZUSD",
            "100.00000000",
            "not-a-number",
        )];
        let mut newest = baseline;
        let mut events = Vec::new();
        apply_ledger_page(&mut s, page, baseline, marks(), &mut newest, &mut events);
        assert_eq!(
            s.flows.len(),
            1,
            "the deposit itself must still be recorded"
        );
        assert_eq!(
            s.net_deposits_usd, 100.0,
            "an unreadable fee must default to 0, not drop the deposit"
        );
    }

    #[test]
    fn a_crypto_deposit_counts_for_performance_but_never_sets_deposit_pending() {
        let mut s = paper::State::default_paper();
        let m = marks();
        assert!(apply_ledger_entry(
            &mut s, "LBTC", 100, "deposit", "XXBT", 0.01, 0.0, m
        ));
        assert!((s.net_deposits_usd - 0.01 * m.btc).abs() < 1e-6);
        assert!(
            !s.deposit_pending,
            "BTC arriving is already invested — nothing to buy"
        );
    }

    #[test]
    fn an_unmarked_asset_deposit_is_recorded_at_zero_rather_than_dropped() {
        let mut s = paper::State::default_paper();
        assert!(apply_ledger_entry(
            &mut s,
            "LXRP",
            100,
            "deposit",
            "XRP",
            10.0,
            0.0,
            alloc::Marks::default()
        ));
        assert_eq!(s.flows[0].usd_value, 0.0);
        assert_eq!(s.net_deposits_usd, 0.0);
        assert_eq!(
            s.flows[0].asset, "XRP",
            "still recorded under its own code, not silently lost"
        );
    }
}
