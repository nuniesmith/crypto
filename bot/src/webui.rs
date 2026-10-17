//! Embedded WebUI for the crypto bot.
//!
//! `crypto-bot webui [--port 8090]` starts an HTTP server with:
//! - a live dashboard (regime per pair, targets vs holdings, P&L),
//! - start/stop controls for the trading loop (managed as a child process),
//! - the forecast calculator.
//!
//! The dashboard reads `state.json` for regime flags. Balances come from the
//! trading loop's snapshot while the loop runs (`paper::BalanceSnapshot`), so
//! the loop stays the only process calling Kraken's private API on the key;
//! with no loop running, the dashboard asks Kraken itself (best-effort:
//! without keys it shows regime and targets only).
//!
//! A watchdog owns the child process. While the operator wants the loop
//! running (autostart or Start, until Stop), a loop that exits is reported to
//! Discord and restarted with backoff, and `/api/status` answers 503 until it
//! is back. It also answers 503, and alerts once, when a running loop stops
//! writing its balance snapshot: alive but not working. The container's
//! healthcheck curls `/api/status`, so either way it turns unhealthy rather
//! than passing over a bot that is not trading. Until 2026-10-17 a dead loop
//! was noticed by nothing: the status answered 200 with `running: false`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, Json};
use axum::routing::{get, post};
use exchange_apiws::KrakenRestClient;
use serde::Serialize;
use tokio::process::Child;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::{alloc, discord, live, paper, regime};

/// Shared state: the managed trading loop, plus a short-lived cache of Kraken
/// holdings (the dashboard polls often; Kraken rate-limits aggressively).
pub struct WebState {
    bot: Mutex<LoopSlot>,
    holdings_cache: Mutex<Option<(Instant, CachedHoldings)>>,
}

/// The trading-loop process, if one was started, and what the watchdog knows
/// about it.
#[derive(Default)]
struct LoopSlot {
    child: Option<Child>,
    sup: Supervision,
}

#[derive(Clone, Default)]
struct CachedHoldings {
    holdings_usd: BTreeMap<&'static str, f64>,
    cash_usd: f64,
    kraken_ok: bool,
}

const HOLDINGS_TTL: Duration = Duration::from_secs(120);

/// How old the trading loop's balance snapshot may be for the dashboard to
/// show it. The loop writes one every minute.
const SNAPSHOT_MAX_AGE_SECS: i64 = 180;

/// How often the watchdog looks at the trading loop.
const WATCHDOG_EVERY: Duration = Duration::from_secs(10);
/// A run this long was a healthy one: the next exit starts the backoff over.
const STABLE_RUN: Duration = Duration::from_secs(3600);
/// The first restart waits this long; each further exit in a row doubles it.
const FIRST_BACKOFF: Duration = Duration::from_secs(30);
/// The longest wait between restarts of a loop that keeps dying.
const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
/// No balance snapshot for this long, while the loop runs, means it is alive
/// but not working: hung, or unable to read the account.
const QUIET_AFTER_SECS: i64 = 600;
/// A just-started loop has not written its first snapshot yet.
const STARTUP_GRACE: Duration = Duration::from_secs(300);

/// The watchdog's memory apart from the process handle, so every decision it
/// makes is a plain unit test.
#[derive(Debug, Default)]
struct Supervision {
    /// The operator wants the loop running: set by autostart and Start,
    /// cleared only by Stop.
    wanted: bool,
    /// Exits in a row. A run of `STABLE_RUN` resets it.
    failures: u32,
    /// When the current run began.
    started: Option<Instant>,
    /// When the next restart is due, while the loop is down.
    retry_at: Option<Instant>,
    /// The went-quiet alert is out for the current run.
    quiet_alerted: bool,
}

/// The wait before restart number `failures` (counting from 1).
fn backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    FIRST_BACKOFF
        .saturating_mul(1u32 << doublings)
        .min(MAX_BACKOFF)
}

impl Supervision {
    /// Start (or autostart) and Stop from the operator.
    fn set_wanted(&mut self, wanted: bool) {
        self.wanted = wanted;
        self.failures = 0;
        self.retry_at = None;
    }

    /// A loop process was just spawned.
    fn started(&mut self, now: Instant) {
        self.started = Some(now);
        self.retry_at = None;
        self.quiet_alerted = false;
    }

    /// The loop exited. The wait before its restart, or `None` when the
    /// operator does not want it running.
    fn exited(&mut self, now: Instant) -> Option<Duration> {
        if self
            .started
            .take()
            .is_some_and(|t| now.duration_since(t) >= STABLE_RUN)
        {
            self.failures = 0;
        }
        if !self.wanted {
            return None;
        }
        Some(self.schedule_retry(now))
    }

    /// Count a failure and set the next restart; returns the wait. Also the
    /// path for a loop that would not even spawn.
    fn schedule_retry(&mut self, now: Instant) -> Duration {
        self.failures += 1;
        let wait = backoff(self.failures);
        self.retry_at = Some(now + wait);
        wait
    }

    fn restart_due(&self, now: Instant) -> bool {
        self.wanted && self.retry_at.is_some_and(|t| now >= t)
    }

    /// A running loop with no balance snapshot for `QUIET_AFTER_SECS`, once
    /// it is past its startup grace. `snapshot_age` is `None` when there is no
    /// snapshot at all.
    fn quiet(&self, running: bool, now: Instant, snapshot_age: Option<i64>) -> bool {
        running
            && self
                .started
                .is_none_or(|t| now.duration_since(t) >= STARTUP_GRACE)
            && snapshot_age.is_none_or(|age| age > QUIET_AFTER_SECS)
    }

    /// Doing what the operator asked: running when wanted, and not quiet.
    fn healthy(&self, running: bool, quiet: bool) -> bool {
        (running || !self.wanted) && !quiet
    }
}

#[derive(Serialize)]
struct PairStatus {
    pair: String,
    bull: Option<bool>,
    target_pct: f64,
    holding_usd: f64,
    holding_pct: f64,
}

#[derive(Serialize)]
struct StatusResponse {
    running: bool,
    /// False while a wanted loop is down or has gone quiet; the response is
    /// then a 503.
    healthy: bool,
    total_usd: f64,
    cash_usd: f64,
    cash_target_pct: f64,
    pairs: Vec<PairStatus>,
    kraken_ok: bool,
}

/// Kraken asset code -> our pair name. `None` means cash/unknown.
fn pair_for_asset(code: &str) -> Option<&'static str> {
    match code {
        "XXBT" | "XBT" => Some("XBTUSD"),
        "XETH" | "ETH" => Some("ETHUSD"),
        "SOL" => Some("SOLUSD"),
        "LINK" => Some("LINKUSD"),
        "XXRP" | "XRP" => Some("XRPUSD"),
        "INJ" => Some("INJUSD"),
        _ => None,
    }
}

fn is_cash(code: &str) -> bool {
    matches!(code, "ZUSD" | "USD" | "USDC" | "USDT")
}

fn child_running(child: Option<&mut Child>) -> bool {
    matches!(child.map(Child::try_wait), Some(Ok(None)))
}

async fn bot_running(state: &Arc<WebState>) -> bool {
    // A managed loop that has exited stays in the slot: the watchdog collects
    // it within WATCHDOG_EVERY, alerts, and restarts it.
    if child_running(state.bot.lock().await.child.as_mut()) {
        return true;
    }
    // Fall back to detecting an externally-managed loop (e.g. systemd) so the
    // dashboard isn't wrong during migration to webui-managed control.
    external_bot_running()
}

/// True if any `crypto-bot live` process is running (systemd, manual, ...).
fn external_bot_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for e in entries.flatten() {
        let pid = e.file_name().to_string_lossy().into_owned();
        if !pid.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let parts: Vec<&str> = cmdline.split('\0').collect();
        if parts.iter().any(|p| p.ends_with("crypto-bot")) && parts.contains(&"live") {
            return true;
        }
    }
    false
}

/// Age in seconds of the trading loop's balance snapshot, if there is one.
fn snapshot_age() -> Option<i64> {
    paper::load_balances().map(|s| chrono::Utc::now().timestamp() - s.ts)
}

async fn fetch_holdings(loop_running: bool) -> CachedHoldings {
    let mut out = CachedHoldings::default();
    let balances = if loop_running {
        // The loop is the only process on the key while it runs: a second
        // private caller is what produced `EAPI:Invalid nonce`. No fresh
        // snapshot means the loop is not reading balances either, and the
        // dashboard says so (kraken_ok false) rather than asking Kraken
        // itself.
        match paper::fresh_balances(
            paper::load_balances(),
            chrono::Utc::now().timestamp(),
            SNAPSHOT_MAX_AGE_SECS,
        ) {
            Some(b) => b,
            None => {
                warn!("webui: no fresh balance snapshot from the trading loop");
                return out;
            }
        }
    } else {
        if !live::keys_present() {
            return out;
        }
        let gw = match live::LiveKraken::from_env() {
            Ok(g) => g,
            Err(e) => {
                warn!("webui: kraken gateway failed: {e:#}");
                return out;
            }
        };
        match gw.balances().await {
            Ok(b) => b,
            Err(e) => {
                warn!("webui: kraken balances failed: {e:#}");
                return out;
            }
        }
    };
    // Prices are public (no key, no nonce).
    let client = match KrakenRestClient::new() {
        Ok(c) => c,
        Err(e) => {
            warn!("webui: kraken client failed: {e}");
            return out;
        }
    };
    // Marks for the six pairs.
    let mut marks: BTreeMap<&str, f64> = BTreeMap::new();
    for pair in regime::PAIRS {
        if let Ok(t) = client.get_ticker(pair).await {
            if let Some((_, tick)) = t.iter().next() {
                marks.insert(pair, tick.last_price());
            }
        }
    }
    for (code, qty) in &balances {
        if *qty <= 0.0 {
            continue;
        }
        if is_cash(code) {
            out.cash_usd += qty;
        } else if let Some(pair) = pair_for_asset(code) {
            if let Some(px) = marks.get(pair) {
                *out.holdings_usd.entry(pair).or_insert(0.0) += qty * px;
            }
        }
    }
    out.kraken_ok = true;
    out
}

async fn api_status(State(state): State<Arc<WebState>>) -> (StatusCode, Json<StatusResponse>) {
    let running = bot_running(&state).await;
    let healthy = {
        let slot = state.bot.lock().await;
        let quiet = slot.sup.quiet(running, Instant::now(), snapshot_age());
        slot.sup.healthy(running, quiet)
    };

    // Regime flags from state.json (always available).
    let regime_bull: BTreeMap<String, bool> = paper::load_state()
        .map(|s| s.regime_bull)
        .unwrap_or_default();

    let cash_target_pct = alloc::cash_weight(&regime_bull).unwrap_or(0.0) * 100.0;

    // Holdings from Kraken (best-effort, cached 2 min — Kraken rate-limits).
    let cached = {
        let guard = state.holdings_cache.lock().await;
        guard
            .clone()
            .filter(|(at, _)| at.elapsed() < HOLDINGS_TTL)
            .map(|(_, c)| c)
    };
    let holdings = match cached {
        Some(c) => c,
        None => {
            let fresh = fetch_holdings(running).await;
            *state.holdings_cache.lock().await = Some((Instant::now(), fresh.clone()));
            fresh
        }
    };
    let holdings_usd = holdings.holdings_usd;
    let cash_usd = holdings.cash_usd;
    let kraken_ok = holdings.kraken_ok;

    let total_usd = cash_usd + holdings_usd.values().sum::<f64>();

    let pairs: Vec<PairStatus> = regime::PAIRS
        .iter()
        .map(|pair| {
            let bull = regime_bull.get(*pair).copied();
            let target_pct = bull
                .map(|b| alloc::effective_weight(pair, b))
                .unwrap_or(0.0)
                * 100.0;
            let holding_usd = holdings_usd.get(*pair).copied().unwrap_or(0.0);
            let holding_pct = if total_usd > 0.0 {
                holding_usd / total_usd * 100.0
            } else {
                0.0
            };
            PairStatus {
                pair: pair.to_string(),
                bull,
                target_pct,
                holding_usd,
                holding_pct,
            }
        })
        .collect();

    // The dashboard reads the body either way; the code is for the
    // container healthcheck (`curl -f`).
    let code = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        Json(StatusResponse {
            running,
            healthy,
            total_usd,
            cash_usd,
            cash_target_pct,
            pairs,
            kraken_ok,
        }),
    )
}

/// Spawn the trading loop: this same binary, `live --loop`.
fn spawn_loop() -> std::io::Result<Child> {
    let exe = std::env::current_exe().unwrap_or_else(|_| "crypto-bot".into());
    tokio::process::Command::new(exe)
        .arg("live")
        .arg("--confirm")
        .arg("I_UNDERSTAND_REAL_MONEY")
        .arg("--loop")
        .spawn()
}

/// Start the loop for the operator (Start, or autostart). From here until
/// Stop the watchdog keeps it running, so a failed spawn is retried too.
async fn start_bot(state: &Arc<WebState>) -> Result<(), String> {
    let mut slot = state.bot.lock().await;
    let slot = &mut *slot;
    slot.sup.set_wanted(true);
    let now = Instant::now();
    match spawn_loop() {
        Ok(child) => {
            info!("webui: started trading loop (pid {:?})", child.id());
            slot.child = Some(child);
            slot.sup.started(now);
            Ok(())
        }
        Err(e) => {
            let wait = slot.sup.schedule_retry(now);
            warn!(
                "webui: failed to start bot: {e:#}; the watchdog retries in {}s",
                wait.as_secs()
            );
            Err(format!("{e:#}"))
        }
    }
}

/// "30 s", "4 min".
fn human(d: Duration) -> String {
    let s = d.as_secs();
    if s < 120 {
        format!("{s} s")
    } else {
        format!("{} min", s / 60)
    }
}

/// Every `WATCHDOG_EVERY`: collect a loop that exited, restart it when due,
/// and notice one that runs but has gone quiet. Alerts are sent after the
/// lock is released.
async fn watchdog(state: Arc<WebState>) {
    let mut tick = tokio::time::interval(WATCHDOG_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let mut alerts: Vec<(&'static str, String)> = Vec::new();
        {
            let mut guard = state.bot.lock().await;
            let slot = &mut *guard;
            let now = Instant::now();

            let exit = match slot.child.as_mut().map(Child::try_wait) {
                Some(Ok(Some(status))) => Some(status.to_string()),
                Some(Err(e)) => Some(format!("could not be waited on: {e}")),
                _ => None,
            };
            if let Some(how) = exit {
                slot.child = None;
                match slot.sup.exited(now) {
                    Some(wait) => {
                        let n = slot.sup.failures;
                        error!(
                            "webui: trading loop exited ({how}); restart {n} in {}s",
                            wait.as_secs()
                        );
                        alerts.push((
                            "Crypto bot: the trading loop died",
                            format!(
                                "The trading loop exited ({how}). The bot is not trading until \
                                 it is back. Restart {n} in a row is due in {}.",
                                human(wait)
                            ),
                        ));
                    }
                    None => info!("webui: trading loop exited ({how}) after Stop"),
                }
            }

            if slot.child.is_none() && slot.sup.restart_due(now) {
                if external_bot_running() {
                    // Another `crypto-bot live` already runs (systemd, by hand):
                    // a second would put two private callers on one key.
                    info!("webui: restart deferred, another trading loop is running");
                } else {
                    match spawn_loop() {
                        Ok(child) => {
                            info!(
                                "webui: restarted trading loop (pid {:?}) after {} exit(s) in a row",
                                child.id(),
                                slot.sup.failures
                            );
                            slot.child = Some(child);
                            slot.sup.started(now);
                        }
                        Err(e) => {
                            let wait = slot.sup.schedule_retry(now);
                            error!(
                                "webui: could not restart the trading loop: {e}; next try in {}s",
                                wait.as_secs()
                            );
                            alerts.push((
                                "Crypto bot: the trading loop will not start",
                                format!(
                                    "Starting the trading loop failed ({e}). Next try in {}.",
                                    human(wait)
                                ),
                            ));
                        }
                    }
                }
            }

            let running = child_running(slot.child.as_mut());
            let quiet = slot.sup.quiet(running, now, snapshot_age());
            if quiet && !slot.sup.quiet_alerted {
                slot.sup.quiet_alerted = true;
                warn!("webui: trading loop runs but wrote no balance snapshot for {QUIET_AFTER_SECS}s");
                alerts.push((
                    "Crypto bot: the trading loop has gone quiet",
                    format!(
                        "The trading loop is running but has not read the Kraken balances for \
                         over {} minutes. It is hung, or cannot reach Kraken.",
                        QUIET_AFTER_SECS / 60
                    ),
                ));
            } else if !quiet && running && slot.sup.quiet_alerted {
                slot.sup.quiet_alerted = false;
                info!("webui: trading loop is reading balances again");
                alerts.push((
                    "Crypto bot: the trading loop is working again",
                    "It is reading the Kraken balances again.".to_string(),
                ));
            }
        }
        for (title, text) in alerts {
            discord::alert(title, &text).await;
        }
    }
}

async fn api_start(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    if bot_running(&state).await {
        return Json(serde_json::json!({"ok": false, "error": "bot already running"}));
    }
    match start_bot(&state).await {
        Ok(()) => Json(serde_json::json!({"ok": true})),
        Err(e) => Json(serde_json::json!({"ok": false, "error": e})),
    }
}

async fn api_stop(State(state): State<Arc<WebState>>) -> Json<serde_json::Value> {
    let mut guard = state.bot.lock().await;
    let slot = &mut *guard;
    let was_wanted = slot.sup.wanted;
    // Not wanted from here on, so the watchdog neither alerts on the exit
    // nor restarts the loop.
    slot.sup.set_wanted(false);
    match slot.child.as_mut() {
        Some(child) => match child.kill().await {
            Ok(()) => {
                info!("webui: stopped trading loop");
                slot.child = None;
                slot.sup.started = None;
                Json(serde_json::json!({"ok": true}))
            }
            Err(e) => Json(serde_json::json!({"ok": false, "error": format!("{e:#}")})),
        },
        // Down between restarts: Stop still means something, it ends them.
        None if was_wanted => {
            info!("webui: Stop while the loop was down; restarts cancelled");
            Json(
                serde_json::json!({"ok": true, "note": "the loop was down; its restarts are cancelled"}),
            )
        }
        None => Json(serde_json::json!({"ok": false, "error": "bot not running"})),
    }
}

async fn dashboard() -> Html<&'static str> {
    Html(include_str!("webui/dashboard.html"))
}

async fn forecast_page() -> Html<&'static str> {
    Html(include_str!("webui/forecast.html"))
}

/// Start the WebUI HTTP server. Runs until killed.
/// If `autostart` is true, the trading loop is launched immediately.
pub async fn serve(port: u16, autostart: bool) -> anyhow::Result<()> {
    let state = Arc::new(WebState {
        bot: Mutex::new(LoopSlot::default()),
        holdings_cache: Mutex::new(None),
    });
    if autostart && !bot_running(&state).await {
        info!("webui: autostart enabled — launching trading loop");
        if let Err(e) = start_bot(&state).await {
            error!(
                "webui: autostart could not launch the trading loop, so this container is NOT \
                 trading yet (the watchdog keeps trying): {e}"
            );
        }
    }
    tokio::spawn(watchdog(state.clone()));
    let app = axum::Router::new()
        .route("/", get(dashboard))
        .route("/forecast", get(forecast_page))
        .route("/api/status", get(api_status))
        .route("/api/start", post(api_start))
        .route("/api/stop", post(api_stop))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    info!("webui listening on http://0.0.0.0:{port} (dashboard /, forecast /forecast)");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn wanted_and_running(now: Instant) -> Supervision {
        let mut s = Supervision::default();
        s.set_wanted(true);
        s.started(now);
        s
    }

    #[test]
    fn backoff_doubles_from_thirty_seconds_and_stops_at_thirty_minutes() {
        assert_eq!(backoff(1), secs(30));
        assert_eq!(backoff(2), secs(60));
        assert_eq!(backoff(3), secs(120));
        assert_eq!(backoff(6), secs(960));
        assert_eq!(backoff(7), MAX_BACKOFF, "30 s x 64 would be 32 min");
        assert_eq!(backoff(u32::MAX), MAX_BACKOFF);
    }

    #[test]
    fn a_wanted_loop_that_dies_is_restarted_after_the_backoff() {
        let now = Instant::now();
        let mut s = wanted_and_running(now);
        assert_eq!(s.exited(now + secs(5)), Some(FIRST_BACKOFF));
        assert!(!s.restart_due(now + secs(20)), "too early");
        assert!(s.restart_due(now + secs(35)));
        s.started(now + secs(35));
        assert!(!s.restart_due(now + secs(40)), "running again");
    }

    #[test]
    fn deaths_in_a_row_back_off_further_until_a_long_healthy_run() {
        let now = Instant::now();
        let mut s = wanted_and_running(now);
        assert_eq!(s.exited(now), Some(secs(30)));
        s.started(now);
        assert_eq!(s.exited(now), Some(secs(60)));
        s.started(now);
        // This run lasted an hour: the death after it counts as the first.
        assert_eq!(s.exited(now + STABLE_RUN), Some(secs(30)));
    }

    #[test]
    fn a_loop_the_operator_stopped_is_neither_restarted_nor_unhealthy() {
        let now = Instant::now();
        let mut s = wanted_and_running(now);
        s.set_wanted(false);
        assert_eq!(s.exited(now), None);
        assert!(!s.restart_due(now + MAX_BACKOFF));
        assert!(s.healthy(false, false));
    }

    #[test]
    fn a_loop_that_will_not_spawn_is_retried_on_the_same_backoff() {
        let now = Instant::now();
        let mut s = Supervision::default();
        s.set_wanted(true);
        assert_eq!(s.schedule_retry(now), secs(30));
        assert!(s.restart_due(now + secs(30)));
        assert_eq!(s.schedule_retry(now), secs(60));
    }

    #[test]
    fn start_from_the_operator_clears_the_backoff() {
        let now = Instant::now();
        let mut s = wanted_and_running(now);
        s.exited(now);
        s.started(now);
        s.exited(now);
        assert_eq!(s.failures, 2);
        s.set_wanted(true);
        assert_eq!(s.failures, 0);
        assert!(
            !s.restart_due(now + MAX_BACKOFF),
            "Start spawns at once instead"
        );
    }

    #[test]
    fn unhealthy_exactly_while_a_wanted_loop_is_down_or_quiet() {
        let mut s = Supervision::default();
        assert!(s.healthy(false, false), "nobody asked for a loop");
        s.set_wanted(true);
        assert!(!s.healthy(false, false), "wanted but down");
        assert!(s.healthy(true, false));
        assert!(!s.healthy(true, true), "running but quiet");
    }

    #[test]
    fn a_loop_goes_quiet_only_after_its_grace_and_without_a_fresh_snapshot() {
        let now = Instant::now();
        let s = wanted_and_running(now);
        let stale = Some(QUIET_AFTER_SECS + 1);
        assert!(
            !s.quiet(true, now + secs(60), stale),
            "still in its startup grace"
        );
        let later = now + STARTUP_GRACE;
        assert!(s.quiet(true, later, stale));
        assert!(s.quiet(true, later, None), "no snapshot at all");
        assert!(!s.quiet(true, later, Some(30)), "a fresh snapshot");
        assert!(
            !s.quiet(false, later, stale),
            "a stopped loop is down, not quiet"
        );
    }

    #[test]
    fn an_external_loop_has_no_grace_to_wait_out() {
        let s = Supervision::default();
        assert!(s.quiet(true, Instant::now(), None));
    }

    #[test]
    fn waits_read_in_seconds_then_minutes() {
        assert_eq!(human(secs(30)), "30 s");
        assert_eq!(human(secs(960)), "16 min");
    }
}
