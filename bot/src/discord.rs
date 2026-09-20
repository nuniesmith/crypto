//! Discord webhook reports: daily / weekly / monthly progress.

use chrono::{Datelike, Timelike, Utc};
use tracing::{info, warn};

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
            }
            None => {
                lines.push("**Kraken account**".into());
                lines.push("_no API keys in this process — cannot show live balance_".into());
                lines.push(String::new());
            }
        }
        lines.push("**strategy books** _(internal $1k each — not Kraken cash)_".into());
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
    lines.push(String::new());
    lines.push(format!(
        "combined equity `${:.2}` vs start `${:.0}`",
        total,
        crate::paper::NOTIONAL * state.books.len() as f64
    ));
    lines.push(if live {
        "_SOL 1h trendline · ETH 1h VWAP+EMA filter · SOL buy-hold. LIVE Kraken limit orders. Strategy books are internal $1k trackers, not account cash._".into()
    } else {
        "_SOL 1h trendline · ETH 1h VWAP+EMA filter · SOL buy-hold. Maker tier-3 fees. Paper only — no exchange orders._".into()
    });
    lines.join("\n")
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
        assert!(body.contains("not Kraken cash"));
        assert!(body.contains("internal $1k"));
        let kraken_at = body.find("Kraken account").unwrap();
        let books_at = body.find("strategy books").unwrap();
        let paper_eq = body.find("equity `$").unwrap();
        assert!(kraken_at < books_at);
        assert!(books_at < paper_eq);
    }
}
