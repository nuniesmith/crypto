//! Embedded WebUI for the crypto bot.
//!
//! `crypto-bot webui [--port 8090]` starts an HTTP server with:
//! - a live dashboard (regime per pair, targets vs holdings, P&L),
//! - start/stop controls for the trading loop (managed as a child process),
//! - the forecast calculator.
//!
//! The dashboard reads `state.json` for regime flags and queries Kraken for
//! live balances (best-effort: without keys it shows regime and targets only).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::response::{Html, Json};
use axum::routing::{get, post};
use exchange_apiws::KrakenRestClient;
use serde::Serialize;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::{alloc, live, paper, regime};

/// Shared state: the managed trading-loop child process, if running,
/// plus a short-lived cache of Kraken holdings (the dashboard polls often;
/// Kraken rate-limits aggressively).
pub struct WebState {
    bot_child: Mutex<Option<tokio::process::Child>>,
    holdings_cache: Mutex<Option<(std::time::Instant, CachedHoldings)>>,
}

#[derive(Clone, Default)]
struct CachedHoldings {
    holdings_usd: BTreeMap<&'static str, f64>,
    cash_usd: f64,
    kraken_ok: bool,
}

const HOLDINGS_TTL: std::time::Duration = std::time::Duration::from_secs(120);

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

async fn bot_running(state: &Arc<WebState>) -> bool {
    let mut guard = state.bot_child.lock().await;
    match guard.as_mut() {
        Some(child) => match child.try_wait() {
            Ok(None) => return true, // still running (managed)
            _ => *guard = None,      // exited — drop the handle, fall through
        },
        None => {}
    }
    drop(guard);
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
        if parts.iter().any(|p| p.ends_with("crypto-bot")) && parts.iter().any(|p| *p == "live") {
            return true;
        }
    }
    false
}

async fn fetch_holdings() -> CachedHoldings {
    let mut out = CachedHoldings::default();
    if !live::keys_present() {
        return out;
    }
    let client = match KrakenRestClient::new() {
        Ok(c) => c,
        Err(e) => {
            warn!("webui: kraken client failed: {e}");
            return out;
        }
    };
    let gw = match live::LiveKraken::from_env() {
        Ok(g) => g,
        Err(e) => {
            warn!("webui: kraken gateway failed: {e:#}");
            return out;
        }
    };
    let balances = match gw.balances().await {
        Ok(b) => b,
        Err(e) => {
            warn!("webui: kraken balances failed: {e:#}");
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

async fn api_status(State(state): State<Arc<WebState>>) -> Json<StatusResponse> {
    let running = bot_running(&state).await;

    // Regime flags from state.json (always available).
    let regime_bull: BTreeMap<String, bool> = paper::load_state()
        .map(|s| s.regime_bull)
        .unwrap_or_default();

    let cash_target_pct = alloc::cash_weight(&regime_bull).unwrap_or(0.0) * 100.0;

    // Holdings from Kraken (best-effort, cached 2 min — Kraken rate-limits).
    let cached = {
        let guard = state.holdings_cache.lock().await;
        guard.clone().filter(|(at, _)| at.elapsed() < HOLDINGS_TTL).map(|(_, c)| c)
    };
    let holdings = match cached {
        Some(c) => c,
        None => {
            let fresh = fetch_holdings().await;
            *state.holdings_cache.lock().await =
                Some((std::time::Instant::now(), fresh.clone()));
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
            let target_pct = bull.map(|b| alloc::effective_weight(pair, b)).unwrap_or(0.0) * 100.0;
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

    Json(StatusResponse {
        running,
        total_usd,
        cash_usd,
        cash_target_pct,
        pairs,
        kraken_ok,
    })
}

async fn start_bot(state: &Arc<WebState>) -> Result<(), String> {
    let exe = std::env::current_exe().unwrap_or_else(|_| "crypto-bot".into());
    match tokio::process::Command::new(exe)
        .arg("live")
        .arg("--confirm")
        .arg("I_UNDERSTAND_REAL_MONEY")
        .arg("--loop")
        .spawn()
    {
        Ok(child) => {
            info!("webui: started trading loop (pid {:?})", child.id());
            *state.bot_child.lock().await = Some(child);
            Ok(())
        }
        Err(e) => {
            warn!("webui: failed to start bot: {e:#}");
            Err(format!("{e:#}"))
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
    let mut guard = state.bot_child.lock().await;
    match guard.as_mut() {
        Some(child) => match child.kill().await {
            Ok(()) => {
                info!("webui: stopped trading loop");
                *guard = None;
                Json(serde_json::json!({"ok": true}))
            }
            Err(e) => Json(serde_json::json!({"ok": false, "error": format!("{e:#}")})),
        },
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
        bot_child: Mutex::new(None),
        holdings_cache: Mutex::new(None),
    });
    if autostart && !bot_running(&state).await {
        info!("webui: autostart enabled — launching trading loop");
        start_bot(&state).await;
    }
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
