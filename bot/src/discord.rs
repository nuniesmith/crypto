//! Discord webhook reports: daily / weekly / monthly progress.
//!
//! Since the 2026-10-05 policy (`alloc.rs`) this describes ONE account
//! against its effective targets, not two sleeves and three $1,000
//! simulation books — the paper books are history now (`paper.rs`'s module
//! docs) and have no section here any more.

use chrono::{Datelike, Days, Timelike, Utc};
use tracing::{info, warn};

use crate::live::AccountSnapshot;
use crate::paper::{fmt_ts, FlowKind, State};
use crate::regime;

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
    if state.last_weekly != iso_week && now.weekday().number_from_monday() == 1 && now.hour() >= 15
    {
        due.push("weekly".into());
    }
    if state.last_monthly != month && now.day() == 1 && now.hour() >= 15 {
        due.push("monthly".into());
    }
    due
}

/// Sum of one display asset code's USD value across `account.assets` (an
/// account can hold more than one row for a code, e.g. staked SOL shows as
/// a separate `"SOL (SOL.S)"` row — matched here by prefix so it still
/// counts toward the SOL bucket).
fn asset_usd(account: &AccountSnapshot, code: &str) -> f64 {
    account
        .assets
        .iter()
        .filter(|a| a.code == code || a.code.starts_with(&format!("{code} (")))
        .filter_map(|a| a.usd)
        .sum()
}

/// "wallet value and each asset's weight against its effective target" —
/// the first thing point 11 of the policy asks the report to show.
fn targets_block(state: &State, account: &AccountSnapshot) -> Vec<String> {
    let total = account.total_usd;
    let mut lines = vec![format!("**wallet** `${total:.2}` total (marked to USD)")];
    let mut coin_usd_sum = 0.0;
    for (pair, code) in [("XBTUSD", "BTC"), ("ETHUSD", "ETH"), ("SOLUSD", "SOL")] {
        let usd = asset_usd(account, code);
        coin_usd_sum += usd;
        let weight = if total > 0.0 {
            usd / total * 100.0
        } else {
            0.0
        };
        match state.regime_bull.get(pair) {
            Some(&bull) => {
                let target = crate::alloc::effective_weight(pair, bull) * 100.0;
                lines.push(format!(
                    "• **{code}** `${usd:.2}` — `{weight:.1}%` of account vs `{target:.1}%` target ({})",
                    if bull { "bull" } else { "bear" }
                ));
            }
            None => lines.push(format!(
                "• **{code}** `${usd:.2}` — `{weight:.1}%` of account (regime not yet read)"
            )),
        }
    }
    let cash_usd = (total - coin_usd_sum).max(0.0);
    let cash_weight_pct = if total > 0.0 {
        cash_usd / total * 100.0
    } else {
        0.0
    };
    match crate::alloc::cash_weight(&state.regime_bull) {
        Some(target) => lines.push(format!(
            "• **cash** `${cash_usd:.2}` — `{cash_weight_pct:.1}%` of account vs `{:.1}%` target",
            target * 100.0
        )),
        None => lines.push(format!(
            "• **cash** `${cash_usd:.2}` — `{cash_weight_pct:.1}%` of account (target incomplete — not every coin has been read)"
        )),
    }
    lines.push(String::new());
    lines
}

/// "each coin's regime with its distance from the 200-day average and the
/// price that would flip it."
fn regime_block(state: &State) -> Vec<String> {
    let mut lines = vec!["**regime** _(200-day SMA ± 5%)_".to_string()];
    for pair in regime::PAIRS {
        match state.regime_reading.get(pair) {
            Some(r) => {
                let flip = if r.bull {
                    r.sma * (1.0 - regime::BAND)
                } else {
                    r.sma * (1.0 + regime::BAND)
                };
                lines.push(format!(
                    "• **{pair}** {} — close `{:.2}` vs 200d `{:.2}` (`{:+.1}%`), flips at `{:.2}`",
                    if r.bull { "BULL" } else { "BEAR" },
                    r.close,
                    r.sma,
                    100.0 * r.distance(),
                    flip
                ));
            }
            None => lines.push(format!(
                "• **{pair}** not yet read (needs {} closed daily candles)",
                regime::SMA_DAYS
            )),
        }
    }
    lines.push(String::new());
    lines
}

/// "pending work: flips not yet applied, deposit backlog, open orders" —
/// "deposit backlog" is now just whether `deposit_pending` is set; there is
/// no dollar amount left to track (see `paper::State::deposit_pending`).
fn pending_work_block(state: &State) -> Vec<String> {
    let mut lines = vec!["**pending work**".to_string()];
    let flips: Vec<&str> = regime::PAIRS
        .iter()
        .copied()
        .filter(|p| state.regime_bull.contains_key(*p))
        .filter(|p| state.regime_applied.get(*p).copied() != state.regime_bull.get(*p).copied())
        .collect();
    if flips.is_empty() {
        lines.push("• no flips outstanding".into());
    } else {
        lines.push(format!(
            "• flip(s) not yet fully applied: **{}**",
            flips.join(", ")
        ));
    }
    if state.deposit_pending {
        lines.push("• investing a deposit into whatever it makes underweight".to_string());
    }
    if state.pending_orders.is_empty() {
        lines.push("• no open orders".into());
    } else {
        for o in &state.pending_orders {
            lines.push(format!(
                "• open order: {} `{:.8}` {} placed `{}`",
                if o.side > 0 { "buy" } else { "sell" },
                o.qty,
                o.pair,
                fmt_ts(o.placed_at)
            ));
        }
    }
    lines.push(String::new());
    lines
}

/// The UTC timestamp this report's period began at: today for daily/startup,
/// this ISO week's Monday for weekly, the 1st of the month for monthly.
fn period_start(kind: &str) -> chrono::NaiveDate {
    let today = Utc::now().date_naive();
    match kind {
        "weekly" => {
            let back = Utc::now().weekday().number_from_monday() as u64 - 1;
            today - Days::new(back)
        }
        "monthly" => today.with_day(1).expect("every month has a 1st"),
        _ => today,
    }
}

/// "deposits this period."
fn deposits_block(state: &State, kind: &str) -> Vec<String> {
    let start = period_start(kind);
    let start_ts = start
        .and_hms_opt(0, 0, 0)
        .expect("midnight always exists")
        .and_utc()
        .timestamp();
    let deposits: Vec<_> = state
        .flows
        .iter()
        .filter(|f| f.kind == FlowKind::Deposit && f.ts >= start_ts)
        .collect();
    let mut lines = vec![format!("**deposits this period** _(since {start})_")];
    if deposits.is_empty() {
        lines.push("• none".into());
    } else {
        for d in &deposits {
            lines.push(format!(
                "• {} `{:.8}` (`${:.2}`) refid={}",
                d.asset, d.amount, d.usd_value, d.refid
            ));
        }
        let total: f64 = deposits.iter().map(|d| d.usd_value).sum();
        lines.push(format!(
            "• period total `${total:.2}` · net deposits to date `${:.2}`",
            state.net_deposits_usd
        ));
    }
    lines.push(String::new());
    lines
}

pub fn format_report(kind: &str, state: &State, account: Option<&AccountSnapshot>) -> String {
    let live = state.mode == "live";
    let mut lines = vec![
        format!(
            "**crypto-bot {kind}** · {}",
            if live { "LIVE Kraken" } else { "paper" }
        ),
        format!("started `{}`", state.started_at),
        String::new(),
    ];
    if !live {
        lines.push("_paper mode — no live wallet to report_".into());
        return lines.join("\n");
    }
    match account {
        Some(acct) if acct.error.is_some() => {
            lines.push("**Kraken account**".into());
            lines.push(format!(
                "_could not fetch: {}_",
                acct.error.as_deref().unwrap_or("unknown")
            ));
        }
        Some(acct) => {
            lines.extend(targets_block(state, acct));
            lines.extend(regime_block(state));
            lines.extend(pending_work_block(state));
            lines.extend(deposits_block(state, kind));
        }
        None => {
            lines.push("**Kraken account**".into());
            lines.push("_no API keys in this process — cannot show live balance_".into());
        }
    }
    lines.push("_policy: one account, BTC/ETH/SOL/cash targets scaled by the daily regime rule. alloc.rs._".into());
    lines.join("\n")
}

/// Send due reports. Daily ~15:00 UTC, weekly Monday, monthly on the 1st.
pub async fn maybe_report(
    state: &mut State,
    force: Option<&str>,
    account: Option<&AccountSnapshot>,
) {
    send_kinds(state, &due_kinds(state, force), account).await;
}

pub async fn send_kinds(state: &mut State, kinds: &[String], account: Option<&AccountSnapshot>) {
    if kinds.is_empty() || webhook_url().is_none() {
        return;
    }
    let now = Utc::now();
    let today = now.format("%Y-%m-%d").to_string();
    let iso_week = now.format("%G-W%V").to_string();
    let month = now.format("%Y-%m").to_string();
    for kind in kinds {
        let body = format_report(kind, state, account);
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
    use crate::paper::{FlowRecord, PendingOrder};
    use crate::regime::Reading;

    fn live_state() -> State {
        let mut s = State::default_paper();
        s.mode = "live".into();
        s.books.clear();
        s
    }

    fn account(bals: &[(&str, f64)], marks: &[(&str, f64)]) -> AccountSnapshot {
        let bals: Vec<(String, f64)> = bals.iter().map(|(c, n)| (c.to_string(), *n)).collect();
        let marks: Vec<(String, f64)> = marks.iter().map(|(c, n)| (c.to_string(), *n)).collect();
        value_balances(&bals, &marks)
    }

    #[test]
    fn paper_mode_skips_straight_to_a_note_with_no_books_section() {
        let mut s = live_state();
        s.mode = "paper".into();
        let body = format_report("daily", &s, None);
        assert!(body.contains("paper mode"));
        assert!(!body.contains("strategy books"));
        assert!(!body.contains("SIMULATION"));
    }

    #[test]
    fn the_1h_books_section_is_gone_from_the_live_report_too() {
        let s = live_state();
        let acct = account(&[("ZUSD", 1_000.0)], &[]);
        let body = format_report("startup", &s, Some(&acct));
        assert!(!body.contains("strategy books"));
        assert!(!body.contains("SIMULATION"));
        assert!(!body.contains("sol_1h_tl"));
    }

    #[test]
    fn each_asset_weight_is_shown_against_its_effective_target() {
        let mut s = live_state();
        s.regime_bull.insert("XBTUSD".into(), true);
        s.regime_bull.insert("ETHUSD".into(), true);
        s.regime_bull.insert("SOLUSD".into(), false);
        s.regime_bull.insert("LINKUSD".into(), true);
        s.regime_bull.insert("XRPUSD".into(), true);
        s.regime_bull.insert("INJUSD".into(), true);
        // $5,000 BTC / $2,500 ETH / $750 SOL / $1,750 cash = $10,000 total.
        let acct = account(
            &[
                ("ZUSD", 1_750.0),
                ("XXBT", 5_000.0 / 85_000.0),
                ("XETH", 2_500.0 / 2_700.0),
                ("SOL", 750.0 / 120.0),
            ],
            &[("XBTUSD", 85_000.0), ("ETHUSD", 2_700.0), ("SOLUSD", 120.0),
              ("LINKUSD", 14.0), ("XRPUSD", 1.5), ("INJUSD", 7.4)],
        );
        let body = format_report("daily", &s, Some(&acct));
        assert!(body.contains("**wallet** `$10000.00`"), "{body}");
        assert!(
            body.contains("**BTC**")
                && body.contains("50.0%")
                && body.contains("vs `50.0%` target (bull)"),
            "{body}"
        );
        assert!(
            body.contains("**SOL**") && body.contains("vs `7.5%` target (bear)"),
            "{body}"
        );
        // Cash target with every pair read: 50+25+7.5+2+2+2 = 88.5 invested, 11.5 cash.
        assert!(
            body.contains("**cash**") && body.contains("vs `11.5%` target"),
            "{body}"
        );
    }

    #[test]
    fn a_coin_with_no_regime_reading_yet_says_so_instead_of_a_target() {
        let s = live_state();
        let acct = account(&[("ZUSD", 1_000.0)], &[]);
        let body = format_report("startup", &s, Some(&acct));
        assert!(body.contains("regime not yet read"), "{body}");
        assert!(body.contains("target incomplete"), "{body}");
    }

    #[test]
    fn regime_block_shows_distance_and_the_flip_price() {
        let mut s = live_state();
        s.regime_reading.insert(
            "ETHUSD".into(),
            Reading {
                close: 2_835.0,
                sma: 2_700.0,
                bull: true,
            },
        );
        let acct = account(&[("ZUSD", 1_000.0)], &[]);
        let body = format_report("daily", &s, Some(&acct));
        // distance = 2835/2700 - 1 = +5.0%; a BULL flips at sma*(1-BAND) = 2565.00.
        assert!(body.contains("ETHUSD** BULL"), "{body}");
        assert!(body.contains("+5.0%"), "{body}");
        assert!(body.contains("flips at `2565.00`"), "{body}");
        assert!(body.contains("SOLUSD** not yet read"), "{body}");
    }

    #[test]
    fn pending_work_lists_an_unapplied_flip_a_pending_deposit_and_an_open_order() {
        let mut s = live_state();
        s.regime_bull.insert("ETHUSD".into(), false);
        s.regime_applied.insert("ETHUSD".into(), true); // stale: bull, now bear
        s.regime_bull.insert("SOLUSD".into(), true);
        s.regime_applied.insert("SOLUSD".into(), true); // up to date
        s.deposit_pending = true;
        s.pending_orders.push(PendingOrder {
            txid: "O1".into(),
            pair: "ETHUSD".into(),
            side: -1,
            placed_at: 0,
            book: Some("rebalance:ETHUSD".into()),
            qty: 0.01,
        });
        let acct = account(&[("ZUSD", 1_000.0)], &[]);
        let body = format_report("daily", &s, Some(&acct));
        assert!(
            body.contains("flip(s) not yet fully applied: **ETHUSD**"),
            "{body}"
        );
        assert!(
            !body.contains("ETHUSD, SOLUSD"),
            "SOLUSD is up to date and must not be listed"
        );
        assert!(
            body.contains("investing a deposit into whatever it makes underweight"),
            "{body}"
        );
        assert!(
            body.contains("open order: sell `0.01000000` ETHUSD"),
            "{body}"
        );
    }

    #[test]
    fn no_outstanding_work_is_reported_plainly() {
        let s = live_state();
        let acct = account(&[("ZUSD", 1_000.0)], &[]);
        let body = format_report("daily", &s, Some(&acct));
        assert!(body.contains("no flips outstanding"));
        assert!(body.contains("no open orders"));
        assert!(!body.contains("investing a deposit"));
    }

    #[test]
    fn deposits_this_period_lists_a_recent_flow_and_excludes_an_old_one() {
        let mut s = live_state();
        let now = Utc::now().timestamp();
        s.flows.push(FlowRecord {
            refid: "LNEW".into(),
            ts: now - 60,
            kind: FlowKind::Deposit,
            asset: "USD".into(),
            amount: 100.0,
            usd_value: 100.0,
        });
        s.flows.push(FlowRecord {
            refid: "LOLD".into(),
            ts: now - 30 * 86_400,
            kind: FlowKind::Deposit,
            asset: "USD".into(),
            amount: 50.0,
            usd_value: 50.0,
        });
        s.net_deposits_usd = 150.0;
        let acct = account(&[("ZUSD", 1_000.0)], &[]);
        let body = format_report("daily", &s, Some(&acct));
        assert!(body.contains("LNEW"), "{body}");
        assert!(
            !body.contains("LOLD"),
            "a deposit from 30 days ago is not in TODAY's period: {body}"
        );
        assert!(body.contains("net deposits to date `$150.00`"), "{body}");
    }

    #[test]
    fn a_withdrawal_never_appears_in_the_deposits_block() {
        let mut s = live_state();
        s.flows.push(FlowRecord {
            refid: "LWD".into(),
            ts: Utc::now().timestamp(),
            kind: FlowKind::Withdrawal,
            asset: "USD".into(),
            amount: -20.0,
            usd_value: 20.0,
        });
        let acct = account(&[("ZUSD", 1_000.0)], &[]);
        let body = format_report("daily", &s, Some(&acct));
        assert!(body.contains("**deposits this period**"));
        assert!(body.contains("• none"), "{body}");
    }

    #[test]
    fn a_failed_kraken_fetch_is_shown_without_panicking() {
        let s = live_state();
        let acct = AccountSnapshot::failed("timeout");
        let body = format_report("daily", &s, Some(&acct));
        assert!(body.contains("could not fetch: timeout"));
    }

    #[test]
    fn no_api_keys_is_reported_distinctly_from_a_fetch_failure() {
        let s = live_state();
        let body = format_report("daily", &s, None);
        assert!(body.contains("no API keys in this process"));
    }
}
