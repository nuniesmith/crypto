//! Paper (default) / live-gated Kraken bot.
//!
//! Crates: `exchange-apiws` (Kraken), `indicators-ta` (EMA/ATR/VWAP),
//! `rustrade-framework` (Brain + ExchangeClient for the live path).
//!
//! Books (direction study — 1m/15m scalps are dead):
//!   sol_1h_tl  SOLUSD 1h trendline_break, 24h hold, $1k, maker fees
//!   eth_1h_sf  ETHUSD 1h structure_filtered (VWAP+EMA gate)
//!   sol_bh     SOLUSD $1k buy-and-hold benchmark

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use exchange_apiws::KrakenRestClient;
use tracing::{info, warn};

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
    for p in [
        Path::new("/home/jordan/github/crypto/.env"),
        Path::new(".env"),
    ] {
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
    print_status(&state, &marks);
    Ok(())
}

async fn report_cmd() -> anyhow::Result<()> {
    let mut state = load_state()?;
    let client = KrakenRestClient::new().map_err(|e| anyhow::anyhow!("{e}"))?;
    let marks = fetch_marks(&client).await?;
    print_status(&state, &marks);
    discord::maybe_report(&mut state, &marks, Some("startup")).await;
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

    for book in &mut state.books {
        let Some(bars) = bars_by_pair.get(&book.pair) else {
            continue;
        };
        let before_n = book.trades.len();
        let before_pos = book.position.is_some();
        let prev_bar = book.last_closed_bar;
        let notes = step_book(book, bars, now);
        if book.last_closed_bar > prev_bar {
            new_bar = true;
        }
        for n in &notes {
            info!("{n}");
            events.push(n.clone());
        }
        let opened = !before_pos && book.position.is_some();
        let closed = book.trades.len() > before_n;
        if matches!(mode, Mode::LiveDry | Mode::Live) && (opened || closed) {
            if let Some(p) = &book.position {
                let side = if p.side > 0 { "buy" } else { "sell" };
                let msg = format!(
                    "KRAKEN {} {} {} vol={:.6} px={:.4}",
                    match mode {
                        Mode::Live => "PLACE",
                        _ => "WOULD PLACE",
                    },
                    side,
                    book.pair,
                    p.qty,
                    p.entry
                );
                info!("{msg}");
                events.push(msg);
                if matches!(mode, Mode::Live) {
                    if let Some(gw) = live_gw {
                        match gw
                            .place_limit(
                                &book.pair,
                                p.side,
                                &format!("{:.8}", p.qty),
                                &format!("{:.4}", p.entry),
                            )
                            .await
                        {
                            Ok(r) => info!("live order ok: {r}"),
                            Err(e) => warn!("live order failed: {e:#}"),
                        }
                    }
                }
            }
        }
    }

    let marks = fetch_marks(client).await.unwrap_or_default();
    if !*announced {
        discord::maybe_report(&mut state, &marks, Some("startup")).await;
        *announced = true;
    }
    discord::maybe_report(&mut state, &marks, None).await;
    save_state(&state)?;
    if new_bar || !events.is_empty() {
        append_journal(&serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "mode": state.mode,
            "events": events,
            "equity": state.books.iter().map(|b| {
                let px = marks.iter().find(|(p,_)| p==&b.pair).map(|(_,x)| *x).unwrap_or(0.0);
                serde_json::json!({ "id": b.id, "pair": b.pair, "equity": b.equity(px), "pos": b.position.is_some() })
            }).collect::<Vec<_>>(),
        }))?;
        print_status(&state, &marks);
        if let Some(b) = bars_by_pair.get("SOLUSD").and_then(|v| v.last()) {
            info!("SOL last closed 1h {} close={:.4}", fmt_ts(b.time), b.close);
        }
    }
    Ok(())
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

async fn fetch_marks(client: &KrakenRestClient) -> anyhow::Result<Vec<(String, f64)>> {
    let mut out = Vec::new();
    for pair in ["SOLUSD", "ETHUSD"] {
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
