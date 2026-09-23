//! Discord webhook reports: daily / weekly / monthly progress.

use chrono::{Datelike, Timelike, Utc};
use tracing::{info, warn};

use crate::ledger::Sleeve;
use crate::live::{fmt_qty, AccountSnapshot};
use crate::paper::State;

pub fn webhook_url() -> Option<String> {
    std::env::var("DISCORD_WEBHOOK_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("https://"))
}

pub async fn send_raw(content: &str) -> anyhow::Result<()> {
    let Some(url) = webhook_url() else {
        anyhow::bail!("DISCORD_WEBHOOK_URL is not set");
    };
    exchange_apiws::ensure_crypto_provider();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let body = serde_json::json!({ "content": content });
    let resp = client.post(&url).json(&body).send().await?;
    let status = resp.status();
    if !status.is_success() {
        let t = resp.text().await.unwrap_or_default();
        anyhow::bail!("discord webhook {status}: {t}");
    }
    Ok(())
}

pub fn due_kinds(state: &State, force: Option<&str>) -> Vec<String> {
    let now = Utc::now();
    let today = now.format("%Y-%m-%d").to_string();
    let iso_week = now.format("%G-W%V").to_string();
    let month = now.format("%Y-%m").to_string();
    let mut due: Vec<String> = Vec::new();
    if let Some(k) = force {
        due.push(k.to_string());
        return due;
    }
    if state.last_daily != today && now.hour() >= 15 {
        due.push("daily".into());
    }
    if state.last_weekly != iso_week && now.weekday().number_from_monday() == 1 && now.hour() >= 15 {
        due.push("weekly".into());
    }
    if state.last_monthly != month && now.day() == 1 && now.hour() >= 15 {
        due.push("monthly".into());
    }
    due
}

pub fn format_report(
    kind: &str,
    state: &State,
    marks: &[(String, f64)],
    account: Option<&AccountSnapshot>,
) -> String {
    let live = state.mode == "live";
    let mut lines = vec![
        format!(
            "**crypto-bot {kind}** · {}",
            if live { "LIVE Kraken" } else { "paper" }
        ),
        format!("started `{}`", state.started_at),
        String::new(),
    ];
    if live {
        match account {
            Some(acct) if acct.error.is_some() => {
                lines.push("**Kraken account**".into());
                lines.push(format!(
                    "_could not fetch: {}_",
                    acct.error.as_deref().unwrap_or("unknown")
                ));
                lines.push(String::new());
            }
            Some(acct) => {
                lines.push(format!(
                    "**Kraken account**  `${:.2}` marked to USD",
                    acct.total_usd
                ));
                if acct.assets.is_empty() {
                    lines.push("_(no balances above $0.01)_".into());
                }
                for a in &acct.assets {
                    match a.usd {
                        Some(u) => lines.push(format!(
                            "• **{}** `{}`  `${:.2}`",
                            a.code,
                            fmt_qty(a.amount),
                            u
                        )),
                        None => lines.push(format!(
                            "• **{}** `{}`  _(unpriced)_",
                            a.code,
                            fmt_qty(a.amount)
                        )),
                    }
                }
                lines.push(String::new());
                lines.push("**policy** BTC HODL 70/30 ±10% of BTC+USD. ETH/SOL trade the wallet pile only. `sol_bh` is mark-only — no $1k buys.".into());
                lines.push(String::new());
            }
            None => {
                lines.push("**Kraken account**".into());
                lines.push("_no API keys in this process — cannot show live balance_".into());
                lines.push(String::new());
            }
        }
        // The live ledger goes ABOVE the books for the same reason the Kraken
        // balance does: it is the only section on this report that describes
        // real money. The books underneath are a simulation at a size this
        // account has never traded, and reading them as live P&L is exactly
        // the confusion this section exists to end.
        lines.extend(live_ledger_block(state, marks));
        lines.push(
            "**strategy books** _(pure $1,000-per-book SIMULATION — not the live sleeve above)_"
                .into(),
        );
    }
    for b in &state.books {
        let px = marks
            .iter()
            .find(|(p, _)| p == &b.pair)
            .map(|(_, x)| *x)
            .unwrap_or(0.0);
        let eq = b.equity(px);
        let pnl = eq - crate::paper::NOTIONAL;
        let pos = match &b.position {
            None => "flat".to_string(),
            Some(p) => format!(
                "{} {:.4} @{:.3}",
                if p.side > 0 { "long" } else { "short" },
                p.qty,
                p.entry
            ),
        };
        lines.push(format!(
            "• **{}** ({}) {}  equity `${:.2}`  pnl `{:+.2}`  fees `${:.2}`  trades `{}`",
            b.id,
            b.pair,
            pos,
            eq,
            pnl,
            b.fees_paid,
            b.trades.len()
        ));
    }
    lines.push(String::new());
    if !live {
        let total: f64 = state
            .books
            .iter()
            .map(|b| {
                let px = marks
                    .iter()
                    .find(|(p, _)| p == &b.pair)
                    .map(|(_, x)| *x)
                    .unwrap_or(0.0);
                b.equity(px)
            })
            .sum();
        lines.push(format!(
            "combined equity `${:.2}` vs start `${:.0}`",
            total,
            crate::paper::NOTIONAL * state.books.len() as f64
        ));
    }
    lines.push(if live {
        "_SOL 1h trendline · ETH 1h VWAP+EMA filter. BTC is HODL. Live limits use real Kraken balances, not $1k books._".into()
    } else {
        "_SOL 1h trendline · ETH 1h VWAP+EMA filter · SOL buy-hold. Maker tier-3 fees. Paper only — no exchange orders._".into()
    });
    lines.join("\n")
}

/// What the real money did, net of real Kraken fees.
fn live_ledger_block(state: &State, marks: &[(String, f64)]) -> Vec<String> {
    if state.live.since.is_empty() {
        return Vec::new();
    }
    let t = state.live.totals(Sleeve::Trade, marks);
    let mut lines = vec![format!(
        "**live sleeve** _(real Kraken fills since {})_",
        state.live.since
    )];
    if t.fills == 0 {
        lines.push("_no real fills recorded yet_".into());
        lines.push(String::new());
        return lines;
    }
    lines.push(format!(
        "• **net `{:+.4}`** = realized `{:+.4}` + unrealized `{:+.4}`  ·  real fees `${:.4}` over `{}` fills",
        t.net_usd(),
        t.realized_usd,
        t.unrealized_usd,
        t.fees_usd,
        t.fills
    ));
    for pair in ["ETHUSD", "SOLUSD"] {
        let Some(p) = state.live.get(Sleeve::Trade, pair) else {
            continue;
        };
        if p.is_flat() {
            continue;
        }
        lines.push(format!(
            "• holding **{}** `{}` at a cost of `${:.4}`",
            pair,
            fmt_qty(p.qty),
            p.basis_usd
        ));
    }
    if !t.complete() {
        lines.push(format!(
            "_⚠ {} pair(s) unpriced — the net above is partial, not zero_",
            t.unpriced
        ));
    }
    if t.adopted_usd > 0.0 {
        lines.push(format!(
            "_`${:.2}` of that cost basis was marked in at the adoption price, not paid_",
            t.adopted_usd
        ));
    }
    let hold = state.live.totals(Sleeve::Hold, marks);
    if hold.fills > 0 {
        lines.push(format!(
            "_BTC rebalancing (not a bet): `{}` fills, `${:.4}` of real fees_",
            hold.fills, hold.fees_usd
        ));
    }
    lines.push(String::new());
    lines
}

/// Send due reports. Daily ~15:00 UTC, weekly Monday, monthly on the 1st.
pub async fn maybe_report(
    state: &mut State,
    marks: &[(String, f64)],
    force: Option<&str>,
    account: Option<&AccountSnapshot>,
) {
    send_kinds(state, marks, &due_kinds(state, force), account).await;
}

pub async fn send_kinds(
    state: &mut State,
    marks: &[(String, f64)],
    kinds: &[String],
    account: Option<&AccountSnapshot>,
) {
    if kinds.is_empty() || webhook_url().is_none() {
        return;
    }
    let now = Utc::now();
    let today = now.format("%Y-%m-%d").to_string();
    let iso_week = now.format("%G-W%V").to_string();
    let month = now.format("%Y-%m").to_string();
    for kind in kinds {
        let body = format_report(kind, state, marks, account);
        match send_raw(&body).await {
            Ok(()) => {
                info!("discord {kind} sent");
                match kind.as_str() {
                    "daily" => state.last_daily = today.clone(),
                    "weekly" => state.last_weekly = iso_week.clone(),
                    "monthly" => state.last_monthly = month.clone(),
                    "startup" => {}
                    _ => {}
                }
            }
            Err(e) => warn!("discord {kind} failed: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::value_balances;
    use crate::paper::{Book, State};

    fn live_state() -> State {
        State {
            started_at: "2026-09-20T18:42:16Z".into(),
            mode: "live".into(),
            books: vec![Book {
                id: "sol_bh".into(),
                pair: "SOLUSD".into(),
                strategy: "buy_hold".into(),
                cash_usd: 1000.0,
                realized_pnl: 0.0,
                fees_paid: 3.9,
                position: None,
                trades: vec![],
                last_closed_bar: 0,
                marks: vec![],
            }],
            last_daily: String::new(),
            last_weekly: String::new(),
            last_monthly: String::new(),
            last_btc_rebalance: String::new(),
            pending_orders: Vec::new(),
            live: crate::ledger::LiveLedger::default(),
            paper_clean_since: String::new(),
        }
    }

    #[test]
    fn live_report_leads_with_kraken_not_paper_books() {
        let state = live_state();
        let marks = vec![("SOLUSD".into(), 110.0)];
        let snap = value_balances(&[("ZUSD".into(), 4321.0), ("SOL".into(), 5.0)], &marks);
        let body = format_report("startup", &state, &marks, Some(&snap));
        assert!(body.contains("LIVE Kraken"));
        assert!(body.contains("**Kraken account**  `$4871.00`"));
        assert!(body.contains("**USD** `4321.00`"));
        assert!(body.contains("**SOL** `5.0000`"));
        assert!(body.contains("wallet pile"));
        assert!(body.contains("no $1k buys"));
        let kraken_at = body.find("Kraken account").unwrap();
        let books_at = body.find("strategy books").unwrap();
        let paper_eq = body.find("equity `$").unwrap();
        assert!(kraken_at < books_at);
        assert!(books_at < paper_eq);
        assert!(body.contains("SIMULATION"), "the books must be labelled as such");
    }

    #[test]
    fn the_live_sleeve_is_reported_above_the_simulation() {
        // Ordering is the whole point: a reader who stops after the first
        // number must have read a REAL one. The report used to lead with
        // paper equity and a `pnl` line that looked exactly like live P&L.
        use crate::ledger::{Fill, Sleeve};
        let mut state = live_state();
        state.live.since = "2026-09-23".into();
        let f = |txid: &str, side: i8, cost: f64, fee: f64| Fill {
            txid: txid.into(), ts: 0, pair: "ETHUSD".into(), side,
            sleeve: Sleeve::Trade, book: Some("eth_1h_sf".into()),
            qty: 0.001, cost, fee,
        };
        state.live.record(f("B1", 1, 2.69609, 0.00701));
        state.live.record(f("S1", -1, 2.74829, 0.00714));
        let body = format_report("daily", &state, &[("SOLUSD".into(), 110.0)], None);

        assert!(body.contains("real Kraken fills since 2026-09-23"), "{body}");
        // Net of the fees Kraken really charged, the round trip made about
        // four cents. The mixed book printed -$0.11 on the identical fills.
        assert!(body.contains("**net `+0.0380`**"), "{body}");
        assert!(body.contains("real fees `$0.0141`"), "{body}");
        assert!(
            body.find("live sleeve").unwrap() < body.find("strategy books").unwrap(),
            "real money must be read before the simulation"
        );
    }

    #[test]
    fn an_unpriced_holding_is_flagged_rather_than_silently_dropped() {
        use crate::ledger::{Fill, Sleeve};
        let mut state = live_state();
        state.live.since = "2026-09-23".into();
        state.live.record(Fill {
            txid: "B1".into(), ts: 0, pair: "ETHUSD".into(), side: 1,
            sleeve: Sleeve::Trade, book: Some("eth_1h_sf".into()),
            qty: 0.01, cost: 27.48, fee: 0.07,
        });
        let body = format_report("daily", &state, &[], None);
        assert!(body.contains("unpriced"), "{body}");
        assert!(body.contains("holding **ETHUSD**"), "{body}");
    }
}
