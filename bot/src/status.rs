//! The FKS status contract: a second, read-only HTTP server in the webui
//! process, started only when `BOT_STATUS_PORT` is set (see
//! `webui::serve`).
//!
//! FKS's `/exchanges` page and the spawner's net-worth sampler read a
//! `BotStatus` document from `GET /status` on this port — the shape is
//! fks-web's `src/lib/types/exchanges.ts` (`BotStatus`, `VenueStatus`,
//! `HoldingStatus`), and the field names below must match it exactly.
//! `/health` answers 200 while the process is up; `/metrics` renders the
//! same numbers as Prometheus gauges.
//!
//! The document is a MAPPING, not new plumbing: it is built fresh on every
//! request from the same sources the dashboard's `/api/status` uses —
//! regime flags and history from `paper::load_state`, targets from
//! `alloc`, holdings from the webui's 2-minute cache
//! (`webui::holdings_snapshot`). It makes no Kraken call of its own and
//! stores nothing: consumers refuse stale documents, so one is never
//! written down. It also changes no trading behaviour — it only reports.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use serde::Serialize;
use tracing::{error, info};

use crate::webui::{holdings_snapshot, CachedHoldings, WebState};
use crate::{alloc, paper, regime};

/// The whole document. Mirrors fks-web's `BotStatus` field for field.
#[derive(Serialize)]
pub struct BotStatus {
    pub bot: String,
    /// "spot" | "futures" — this bot is the spot one.
    pub market: String,
    pub mode: String,
    pub uptime_secs: u64,
    pub net_worth_usd: f64,
    pub pnl_usd: f64,
    pub signals_total: u64,
    pub trades_total: u64,
    pub wins: u64,
    pub losses: u64,
    pub win_rate: f64,
    pub exchanges: Vec<VenueStatus>,
    pub positions: Vec<PositionStatus>,
    pub recent_events: Vec<serde_json::Value>,
    /// How many venues this bot is configured to report. Always 1 here:
    /// Kraken is the only venue.
    pub expected_venues: u64,
}

/// One venue's status. Mirrors fks-web's `VenueStatus`.
#[derive(Serialize)]
pub struct VenueStatus {
    pub exchange: String,
    pub mode: String,
    pub cash_asset: String,
    pub cash: f64,
    pub total_value: f64,
    /// Max |weight − target_weight| across the holdings, 0–1.
    pub max_drift: f64,
    /// A rebalance is owed: some pair's applied regime no longer matches
    /// its last read regime, so the work tick still has sizing to do.
    pub triggered: bool,
    /// Epoch seconds of the newest live fill on record, if any.
    pub last_rebalance: Option<i64>,
    /// Epoch seconds this document was built.
    pub updated: i64,
    pub holdings: Vec<HoldingStatus>,
}

/// One holding. Mirrors fks-web's `HoldingStatus`.
#[derive(Serialize)]
pub struct HoldingStatus {
    /// Base asset ("BTC", "ETH", …), as `paper::DailySnapshot` names them.
    pub asset: String,
    pub qty: f64,
    /// The mark the value was computed with, USD.
    pub price: f64,
    pub value: f64,
    /// Share of the venue total, 0–1.
    pub weight: f64,
    /// Effective target weight today, 0–1 (bear = half the base weight).
    pub target_weight: f64,
}

/// One open position. Mirrors fks-web's `PositionStatus`. This bot holds
/// spot balances, not positions — v1 always reports an empty list — but
/// the type is part of the contract, so it is defined with the document.
#[derive(Serialize)]
pub struct PositionStatus {
    pub symbol: String,
    /// +1 long, −1 short.
    pub dir: i8,
    pub entry_px: f64,
    pub entry_ts_ms: i64,
    pub mark_px: f64,
    /// Signed return % on the position.
    pub ret_pct: f64,
    pub updated: i64,
}

/// The pair's base asset, in the spelling `paper::DailySnapshot` uses for
/// its qty/mark maps (Kraken's code for the first pair is XXBT; the asset
/// is BTC).
fn asset_for_pair(pair: &str) -> &str {
    match pair {
        "XBTUSD" => "BTC",
        "ETHUSD" => "ETH",
        "SOLUSD" => "SOL",
        "LINKUSD" => "LINK",
        "XRPUSD" => "XRP",
        "INJUSD" => "INJ",
        _ => pair,
    }
}

/// The contract's mode vocabulary (fks-web's `VenueStatus` comment) is
/// `"paper" | "dry-run" | "live"`, and the spawner classifies venues by
/// these strings — paper venues are excluded from real-money totals.
/// The bot's own spellings differ (`"live-dry"` for its dry-run), so
/// translate at this boundary instead of passing them through: anything
/// that is not exactly live or paper is a dry run.
fn contract_mode(bot_mode: &str) -> String {
    match bot_mode {
        "live" => "live",
        "paper" => "paper",
        _ => "dry-run",
    }
    .to_string()
}

/// Build the document from one state file read and one holdings snapshot.
///
/// Pure over its inputs so the mapping is testable without a server.
/// `now` is epoch seconds, stamped into the venue's `updated`.
///
/// v1 simplifications, chosen rather than invented: `signals_total`,
/// `wins`, `losses` and `win_rate` are 0. Nothing counts live regime
/// evaluations, and the live ledger (`paper::State::live`) aggregates
/// realized P&L per pair without attributing round trips, so a win count
/// would be fabricated accounting. `trades_total` IS real: the ledger's
/// cumulative live fills (buys + sells over every pair it tracks).
/// `pnl_usd` is the plan's v1 derivation — net worth minus the earliest
/// daily snapshot total in the state history — and only when Kraken
/// answered: with no live total there is no honest P&L, so it is 0,
/// and the venue itself is omitted from `exchanges` (see below).
fn build_status(
    saved: Option<&paper::State>,
    holdings: &CachedHoldings,
    uptime_secs: u64,
    now: i64,
) -> BotStatus {
    let mode = contract_mode(saved.map(|s| s.mode.as_str()).unwrap_or("live"));
    let total = holdings.cash_usd + holdings.holdings_usd.values().sum::<f64>();

    let mut venue_holdings: Vec<HoldingStatus> = Vec::new();
    let mut max_drift = 0.0f64;
    for pair in regime::PAIRS {
        let qty = holdings.qty.get(pair).copied().unwrap_or(0.0);
        if qty <= 0.0 {
            continue;
        }
        let value = holdings.holdings_usd.get(pair).copied().unwrap_or(0.0);
        let weight = if total > 0.0 { value / total } else { 0.0 };
        // No regime read for this pair yet means no target to claim —
        // the same 0 the dashboard shows in that case.
        let target_weight = saved
            .and_then(|s| s.regime_bull.get(pair).copied())
            .map(|bull| alloc::effective_weight(pair, bull))
            .unwrap_or(0.0);
        max_drift = max_drift.max((weight - target_weight).abs());
        venue_holdings.push(HoldingStatus {
            asset: asset_for_pair(pair).to_string(),
            qty,
            price: holdings.price.get(pair).copied().unwrap_or(0.0),
            value,
            weight,
            target_weight,
        });
    }

    let triggered = saved.is_some_and(|s| {
        regime::PAIRS.iter().any(|pair| {
            matches!(s.regime_bull.get(*pair), Some(bull)
                if s.regime_applied.get(*pair) != Some(bull))
        })
    });
    let last_rebalance = saved.and_then(|s| s.live.fills.last().map(|f| f.ts));
    let trades_total = saved
        .map(|s| {
            s.live
                .pairs
                .iter()
                .map(|p| u64::from(p.buys) + u64::from(p.sells))
                .sum::<u64>()
        })
        .unwrap_or(0);
    let pnl_usd = match (holdings.kraken_ok, saved.and_then(|s| s.history.first())) {
        (true, Some(first)) => total - first.total_usd,
        _ => 0.0,
    };

    // When Kraken didn't answer, the venue is omitted entirely: with
    // `expected_venues: 1` still declared, an empty `exchanges` is the
    // contract's "configured but not reporting" signal, and the
    // spawner's net-worth sampler refuses the sample as unverifiable.
    // A venue present with zeroed figures would instead be recorded
    // as a real $0 balance — fabricated data for the length of an
    // outage.
    let exchanges = if holdings.kraken_ok {
        vec![VenueStatus {
            exchange: "kraken".to_string(),
            mode: mode.clone(),
            cash_asset: "USD".to_string(),
            cash: holdings.cash_usd,
            total_value: total,
            max_drift,
            triggered,
            last_rebalance,
            updated: now,
            holdings: venue_holdings,
        }]
    } else {
        Vec::new()
    };
    BotStatus {
        bot: "crypto-regime".to_string(),
        market: "spot".to_string(),
        mode,
        uptime_secs,
        net_worth_usd: total,
        pnl_usd,
        signals_total: 0,
        trades_total,
        wins: 0,
        losses: 0,
        win_rate: 0.0,
        exchanges,
        positions: Vec::new(),
        recent_events: Vec::new(),
        expected_venues: 1,
    }
}

/// The current document: fresh state, fresh-from-cache holdings, built
/// for this request and never stored.
async fn current_status(state: &Arc<WebState>) -> BotStatus {
    let holdings = holdings_snapshot(state).await;
    let saved = paper::load_state().ok();
    build_status(
        saved.as_ref(),
        &holdings,
        state.started.elapsed().as_secs(),
        chrono::Utc::now().timestamp(),
    )
}

/// The gauges, rendered from the same document `/status` serves so the
/// two can never disagree.
fn metrics_text(doc: &BotStatus) -> String {
    let mut out = String::new();
    out.push_str("# HELP crypto_bot_net_worth_usd The bot's total account value, USD.\n");
    out.push_str("# TYPE crypto_bot_net_worth_usd gauge\n");
    out.push_str(&format!("crypto_bot_net_worth_usd {}\n", doc.net_worth_usd));
    out.push_str("# HELP crypto_bot_venue_total_usd Total value per venue, USD.\n");
    out.push_str("# TYPE crypto_bot_venue_total_usd gauge\n");
    for venue in &doc.exchanges {
        out.push_str(&format!(
            "crypto_bot_venue_total_usd{{exchange=\"{}\"}} {}\n",
            venue.exchange, venue.total_value
        ));
    }
    out
}

async fn health() -> StatusCode {
    StatusCode::OK
}

async fn status(State(state): State<Arc<WebState>>) -> Json<BotStatus> {
    Json(current_status(&state).await)
}

async fn metrics(State(state): State<Arc<WebState>>) -> impl IntoResponse {
    let doc = current_status(&state).await;
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics_text(&doc),
    )
}

/// Serve the FKS status contract on `port` until the process dies. Run as
/// a spawned task beside the dashboard (see `webui::serve`): a bind
/// failure is logged and ends this task, never the dashboard or the bot.
pub async fn serve(state: Arc<WebState>, port: u16) {
    let app = axum::Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .route("/status", get(status))
        .with_state(state);
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            error!("status server: could not bind 0.0.0.0:{port}: {e}");
            return;
        }
    };
    info!(
        "status server listening on http://0.0.0.0:{port} \
         (FKS contract: /health, /metrics, /status)"
    );
    if let Err(e) = axum::serve(listener, app).await {
        error!("status server stopped: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;

    use crate::ledger::{Fill, Sleeve};

    /// A live state: every pair read, ETH in a bear regime, everything
    /// applied except ETH (still sized for the bull it was in), one daily
    /// snapshot at $200, one recorded live fill.
    fn fixture_state() -> paper::State {
        let mut s = paper::State::default_paper();
        s.mode = "live".to_string();
        for pair in regime::PAIRS {
            s.regime_bull.insert(pair.to_string(), true);
            s.regime_applied.insert(pair.to_string(), true);
        }
        s.regime_bull.insert("ETHUSD".to_string(), false);
        s.history.push(paper::DailySnapshot {
            date: "2026-10-20".to_string(),
            total_usd: 200.0,
            qty: Default::default(),
            mark: Default::default(),
            net_deposits_usd_to_date: 0.0,
        });
        s.live = crate::ledger::LiveLedger::starting("2026-10-01");
        let _ = s.live.record(Fill {
            txid: "T1".to_string(),
            ts: 1_700_000_000,
            pair: "XBTUSD".to_string(),
            side: 1,
            sleeve: Sleeve::Hold,
            book: None,
            qty: 0.001,
            cost: 100.0,
            fee: 0.4,
        });
        s
    }

    /// $225 across BTC (bull) and ETH (bear), plus $25 cash.
    fn fixture_holdings() -> CachedHoldings {
        let mut h = CachedHoldings {
            cash_usd: 25.0,
            kraken_ok: true,
            ..Default::default()
        };
        h.holdings_usd.insert("XBTUSD", 150.0);
        h.qty.insert("XBTUSD", 0.0015);
        h.price.insert("XBTUSD", 100_000.0);
        h.holdings_usd.insert("ETHUSD", 50.0);
        h.qty.insert("ETHUSD", 0.02);
        h.price.insert("ETHUSD", 2_500.0);
        h
    }

    fn keys(v: &serde_json::Value) -> BTreeSet<&str> {
        v.as_object().unwrap().keys().map(|k| k.as_str()).collect()
    }

    #[test]
    fn the_documents_field_names_match_the_fks_contract_exactly() {
        // The contract is fks-web's `src/lib/types/exchanges.ts`. A renamed
        // or dropped field fails silently on the other side (the page just
        // shows nothing), so the serialized names are asserted as a set —
        // no missing, no extras.
        let state = fixture_state();
        let doc = build_status(Some(&state), &fixture_holdings(), 42, 1_700_000_100);
        let v = serde_json::to_value(&doc).unwrap();
        assert_eq!(
            keys(&v),
            BTreeSet::from([
                "bot",
                "market",
                "mode",
                "uptime_secs",
                "net_worth_usd",
                "pnl_usd",
                "signals_total",
                "trades_total",
                "wins",
                "losses",
                "win_rate",
                "exchanges",
                "positions",
                "recent_events",
                "expected_venues",
            ])
        );
        let venue = &v["exchanges"][0];
        assert_eq!(
            keys(venue),
            BTreeSet::from([
                "exchange",
                "mode",
                "cash_asset",
                "cash",
                "total_value",
                "max_drift",
                "triggered",
                "last_rebalance",
                "updated",
                "holdings",
            ])
        );
        let holding = &venue["holdings"][0];
        assert_eq!(
            keys(holding),
            BTreeSet::from(["asset", "qty", "price", "value", "weight", "target_weight"])
        );
        assert_eq!(v["expected_venues"], 1u64);
        assert_eq!(v["uptime_secs"], 42u64);
        assert!(
            v["positions"].as_array().unwrap().is_empty(),
            "v1 reports no positions"
        );
        assert!(
            v["recent_events"].as_array().unwrap().is_empty(),
            "v1 reports no events"
        );
    }

    #[test]
    fn holdings_map_with_bull_and_bear_target_weights() {
        let state = fixture_state();
        let doc = build_status(Some(&state), &fixture_holdings(), 42, 1_700_000_100);
        assert_eq!(doc.bot, "crypto-regime");
        assert_eq!(doc.market, "spot");
        assert_eq!(doc.mode, "live");
        assert_eq!(doc.expected_venues, 1);
        assert!((doc.net_worth_usd - 225.0).abs() < 1e-9, "cash + holdings");
        assert!(
            (doc.pnl_usd - 25.0).abs() < 1e-9,
            "net worth less the earliest snapshot (200)"
        );
        assert_eq!(doc.trades_total, 1, "the one recorded live fill");

        let venue = &doc.exchanges[0];
        assert_eq!(venue.exchange, "kraken");
        assert_eq!(venue.cash_asset, "USD");
        assert!((venue.cash - 25.0).abs() < 1e-9);
        assert!((venue.total_value - 225.0).abs() < 1e-9);
        assert_eq!(venue.updated, 1_700_000_100);
        assert_eq!(venue.last_rebalance, Some(1_700_000_000));
        assert!(
            venue.triggered,
            "ETH was applied as a bull and now reads bear: sizing is owed"
        );

        let btc = venue
            .holdings
            .iter()
            .find(|h| h.asset == "BTC")
            .expect("BTC holding");
        assert!((btc.qty - 0.0015).abs() < 1e-12);
        assert!((btc.price - 100_000.0).abs() < 1e-9);
        assert!((btc.value - 150.0).abs() < 1e-9);
        assert!((btc.weight - 150.0 / 225.0).abs() < 1e-12);
        assert!(
            (btc.target_weight - 0.44).abs() < 1e-12,
            "bull = the full base weight"
        );

        let eth = venue
            .holdings
            .iter()
            .find(|h| h.asset == "ETH")
            .expect("ETH holding");
        assert!(
            (eth.target_weight - 0.11).abs() < 1e-12,
            "bear = half of the 0.22 base weight"
        );
        assert!(
            (venue.max_drift - (150.0 / 225.0 - 0.44)).abs() < 1e-9,
            "BTC is the furthest from its target"
        );
    }

    #[test]
    fn an_all_bear_account_halves_every_target_weight() {
        let mut state = fixture_state();
        for pair in regime::PAIRS {
            state.regime_bull.insert(pair.to_string(), false);
        }
        let doc = build_status(Some(&state), &fixture_holdings(), 0, 0);
        let venue = &doc.exchanges[0];
        let btc = venue
            .holdings
            .iter()
            .find(|h| h.asset == "BTC")
            .expect("BTC holding");
        assert!((btc.target_weight - 0.22).abs() < 1e-12, "half of 0.44");
        let eth = venue
            .holdings
            .iter()
            .find(|h| h.asset == "ETH")
            .expect("ETH holding");
        assert!((eth.target_weight - 0.11).abs() < 1e-12, "half of 0.22");
    }

    #[test]
    fn without_kraken_or_history_the_document_claims_no_pnl() {
        let state = fixture_state();
        let mut down = fixture_holdings();
        down.kraken_ok = false;
        down.cash_usd = 0.0;
        down.holdings_usd.clear();
        down.qty.clear();
        down.price.clear();
        let doc = build_status(Some(&state), &down, 0, 5);
        assert_eq!(doc.net_worth_usd, 0.0);
        assert_eq!(
            doc.pnl_usd, 0.0,
            "no live total, so no P&L claim despite the history on disk"
        );
        assert!(
            doc.exchanges.is_empty(),
            "venue omitted, not zeroed, while Kraken is unreachable"
        );
        assert_eq!(doc.expected_venues, 1, "still configured to report one");

        let mut bare = fixture_state();
        bare.history.clear();
        let doc = build_status(Some(&bare), &fixture_holdings(), 0, 5);
        assert_eq!(doc.pnl_usd, 0.0, "no history, so no baseline to diff");
        assert_eq!(doc.exchanges[0].last_rebalance, Some(1_700_000_000));
    }

    #[test]
    fn mode_is_translated_to_the_contract_vocabulary() {
        assert_eq!(contract_mode("live"), "live");
        assert_eq!(contract_mode("paper"), "paper");
        assert_eq!(contract_mode("live-dry"), "dry-run");
        assert_eq!(contract_mode("anything-else"), "dry-run");
    }

    #[test]
    fn metrics_carry_the_net_worth_and_per_venue_total_gauges() {
        let state = fixture_state();
        let doc = build_status(Some(&state), &fixture_holdings(), 0, 0);
        let text = metrics_text(&doc);
        assert!(text.contains("crypto_bot_net_worth_usd 225\n"), "{text}");
        assert!(
            text.contains("crypto_bot_venue_total_usd{exchange=\"kraken\"} 225\n"),
            "{text}"
        );
    }
}
