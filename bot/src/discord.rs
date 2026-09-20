//! Discord webhook reports: daily / weekly / monthly paper progress.

use chrono::{Datelike, Timelike, Utc};
use tracing::{info, warn};

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

pub fn format_report(kind: &str, state: &State, marks: &[(String, f64)]) -> String {
    let live = state.mode == "live";
    let mut lines = vec![
        format!(
            "**crypto-bot {kind}** · {}",
            if live { "LIVE Kraken" } else { "paper" }
        ),
        format!("started `{}`", state.started_at),
        String::new(),
    ];
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
        "_SOL 1h trendline · ETH 1h VWAP+EMA filter · SOL buy-hold. LIVE Kraken limit orders._".into()
    } else {
        "_SOL 1h trendline · ETH 1h VWAP+EMA filter · SOL buy-hold. Maker tier-3 fees. Paper only — no exchange orders._".into()
    });
    lines.join("\n")
}

/// Send due reports. Daily ~15:00 UTC, weekly Monday, monthly on the 1st.
pub async fn maybe_report(state: &mut State, marks: &[(String, f64)], force: Option<&str>) {
    if webhook_url().is_none() {
        return;
    }
    let now = Utc::now();
    let today = now.format("%Y-%m-%d").to_string();
    let iso_week = now.format("%G-W%V").to_string();
    let month = now.format("%Y-%m").to_string();

    let mut due: Vec<&str> = Vec::new();
    if let Some(k) = force {
        due.push(k);
    } else {
        if state.last_daily != today && now.hour() >= 15 {
            due.push("daily");
        }
        if state.last_weekly != iso_week && now.weekday().number_from_monday() == 1 && now.hour() >= 15
        {
            due.push("weekly");
        }
        if state.last_monthly != month && now.day() == 1 && now.hour() >= 15 {
            due.push("monthly");
        }
    }
    for kind in due {
        let body = format_report(kind, state, marks);
        match send_raw(&body).await {
            Ok(()) => {
                info!("discord {kind} sent");
                match kind {
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
