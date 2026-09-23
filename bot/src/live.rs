//! Live `rustrade::ExchangeClient` via `exchange-apiws` Kraken private REST.
//! Only constructed when the operator passes the confirm string.

use async_trait::async_trait;
use exchange_apiws::kraken::KrakenOrder;
use exchange_apiws::{KrakenCredentials, KrakenPrivateClient};
use rustrade::{Capability, ExchangeClient, Order, Position, Result, Side, Symbol};
use std::collections::HashMap;

use crate::ledger::Execution;

/// Read one closed order into the three numbers the ledger needs.
///
/// A missing `vol_exec` is the only fatal absence: an order that cannot say
/// how much it traded cannot settle at all. `cost` and `fee` are defaulted to
/// zero rather than dropped, because losing the whole record over an
/// unparseable fee would leave the book believing an entry never arrived —
/// and the settler would then withdraw a position the wallet really holds.
/// Zero cost on a real fill is visible in the ledger; a vanished fill is not.
pub fn execution_of(o: &KrakenOrder) -> Option<Execution> {
    let vol_exec: f64 = o.vol_exec.parse().ok()?;
    if !vol_exec.is_finite() {
        return None;
    }
    Some(Execution {
        vol_exec,
        cost: o.cost.parse().ok().filter(|v: &f64| v.is_finite()).unwrap_or(0.0),
        fee: o.fee.parse().ok().filter(|v: &f64| v.is_finite()).unwrap_or(0.0),
        // `opentm` is when the order opened, not when it filled. These are
        // limit orders reaped after ten minutes, so the two sit in the same
        // bar; the settling cycle's clock is the fallback.
        at: o.opentm.filter(|t| t.is_finite()).map(|t| t as i64),
    })
}

pub struct LiveKraken {
    client: KrakenPrivateClient,
}

/// One non-dust Kraken asset, marked to USD when we have a ticker.
#[derive(Clone, Debug)]
pub struct SnapshotAsset {
    pub code: String,
    pub amount: f64,
    pub usd: Option<f64>,
}

/// Spot balances from `POST /0/private/Balance`, marked to USD.
#[derive(Clone, Debug)]
pub struct AccountSnapshot {
    pub assets: Vec<SnapshotAsset>,
    pub total_usd: f64,
    pub error: Option<String>,
}

impl AccountSnapshot {
    pub fn failed(msg: impl Into<String>) -> Self {
        Self {
            assets: Vec::new(),
            total_usd: 0.0,
            error: Some(msg.into()),
        }
    }

    pub fn print(&self) {
        if let Some(err) = &self.error {
            println!("kraken account: fetch failed: {err}");
            return;
        }
        println!("kraken account  ${:.2}  (marked to USD)", self.total_usd);
        if self.assets.is_empty() {
            println!("  (no balances above $0.01)");
            return;
        }
        for a in &self.assets {
            match a.usd {
                Some(u) => println!(
                    "  {:16} {:>14}  ${:.2}",
                    a.code,
                    fmt_qty(a.amount),
                    u
                ),
                None => println!(
                    "  {:16} {:>14}  (unpriced)",
                    a.code,
                    fmt_qty(a.amount)
                ),
            }
        }
    }
}

enum Quote {
    Usd,
    Pair(String),
}

fn base_code(raw: &str) -> &str {
    raw.split(['.', '-']).next().unwrap_or(raw)
}

fn display_base(raw: &str) -> String {
    let b = base_code(raw);
    match b {
        "XXBT" | "XBT" => "BTC".into(),
        "XXDG" | "XDG" => "DOGE".into(),
        b if b.len() == 4 && (b.starts_with('X') || b.starts_with('Z')) => b[1..].to_string(),
        other => other.to_string(),
    }
}

fn display_asset(raw: &str) -> String {
    let pretty = display_base(raw);
    if raw.contains('.') {
        format!("{pretty} ({raw})")
    } else {
        pretty
    }
}

fn quote_for(raw: &str) -> Quote {
    let b = base_code(raw);
    match b {
        "ZUSD" | "USD" | "USDT" | "USDC" | "DAI" | "PYUSD" | "KFEE" => Quote::Usd,
        "XXBT" | "XBT" | "BTC" => Quote::Pair("XBTUSD".into()),
        "XXDG" | "XDG" | "DOGE" => Quote::Pair("XDGUSD".into()),
        other => {
            let disp = display_base(other);
            if matches!(disp.as_str(), "USD" | "USDT" | "USDC" | "DAI" | "PYUSD") {
                Quote::Usd
            } else {
                Quote::Pair(format!("{disp}USD"))
            }
        }
    }
}

/// Tickers we still need to mark `balances` to USD.
pub fn needed_pairs(balances: &[(String, f64)]) -> Vec<String> {
    let mut pairs: Vec<String> = balances
        .iter()
        .filter(|(_, n)| *n > 0.0)
        .filter_map(|(code, _)| match quote_for(code) {
            Quote::Usd => None,
            Quote::Pair(p) => Some(p),
        })
        .collect();
    pairs.sort();
    pairs.dedup();
    pairs
}

pub fn value_balances(balances: &[(String, f64)], marks: &[(String, f64)]) -> AccountSnapshot {
    let mut assets = Vec::new();
    for (code, amount) in balances {
        if !amount.is_finite() || *amount <= 0.0 {
            continue;
        }
        let usd = match quote_for(code) {
            Quote::Usd => Some(*amount),
            Quote::Pair(p) => marks
                .iter()
                .find(|(k, _)| k == &p)
                .map(|(_, px)| amount * *px),
        };
        if matches!(usd, Some(u) if u < 0.01) {
            continue;
        }
        assets.push(SnapshotAsset {
            code: display_asset(code),
            amount: *amount,
            usd,
        });
    }
    assets.sort_by(|a, b| {
        b.usd
            .unwrap_or(0.0)
            .partial_cmp(&a.usd.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let total_usd = assets.iter().filter_map(|a| a.usd).sum();
    AccountSnapshot {
        assets,
        total_usd,
        error: None,
    }
}

pub fn fmt_qty(x: f64) -> String {
    if x.abs() >= 100.0 {
        format!("{x:.2}")
    } else if x.abs() >= 1.0 {
        format!("{x:.4}")
    } else {
        let s = format!("{x:.8}");
        s.trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

pub fn keys_present() -> bool {
    let key = std::env::var("KRAKEN_API_KEY").unwrap_or_default();
    let secret = std::env::var("KRAKEN_API_SECRET").unwrap_or_default();
    !key.trim().is_empty() && !secret.trim().is_empty()
}

impl LiveKraken {
    pub fn from_env() -> anyhow::Result<Self> {
        let creds = KrakenCredentials::from_env()
            .map_err(|e| anyhow::anyhow!("Kraken credentials: {e}"))?;
        let client = KrakenPrivateClient::new(creds)
            .map_err(|e| anyhow::anyhow!("Kraken private client: {e}"))?;
        Ok(Self { client })
    }

    /// All non-zero spot balances (`asset`, amount).
    pub async fn balances(&self) -> anyhow::Result<Vec<(String, f64)>> {
        let map = self
            .client
            .get_balance()
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut out: Vec<(String, f64)> = map
            .into_iter()
            .filter_map(|(k, v)| v.parse::<f64>().ok().map(|n| (k, n)))
            .filter(|(_, n)| n.is_finite() && *n > 0.0)
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// USD that open BUY orders have already claimed.
    ///
    /// Kraken's `Balance` reports the total including this, so without it
    /// the policy sizes against money an order already spoke for. Counts
    /// EVERY open buy, not only this bot's: an order placed by hand in the
    /// Kraken UI, or by an earlier build of this binary, holds funds just
    /// as effectively. (Cancelling is the opposite case and stays limited
    /// to our own txids -- see `reap_pending_orders`.)
    pub async fn held_usd(&self) -> anyhow::Result<f64> {
        let open = self
            .client
            .get_open_orders()
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut held = 0.0;
        for order in open.open.values() {
            let Some(descr) = order.descr.as_ref() else {
                continue;
            };
            if descr.side != "buy" || !descr.pair.ends_with("USD") {
                continue;
            }
            let vol: f64 = order.vol.parse().unwrap_or(0.0);
            let done: f64 = order.vol_exec.parse().unwrap_or(0.0);
            let price: f64 = descr.price.parse().unwrap_or(0.0);
            held += ((vol - done).max(0.0)) * price;
        }
        Ok(held)
    }

    /// Executed volume for every order that has left the book, by txid.
    ///
    /// ONE call covers every order we are tracking, which is what makes
    /// settling against real fills affordable — the per-order query this
    /// originally avoided would have been a private call each.
    ///
    /// An order still open is simply absent: `ClosedOrders` is the closed set,
    /// and "not settled yet" and "settled at zero" must not look alike.
    ///
    /// Status is deliberately ignored. A `canceled` order can still have
    /// executed — 2026-09-21's ETH entry came back `canceled` with
    /// `vol_exec=0.00100000` of `0.01488212`, which is exactly the number the
    /// book needed and exactly what the old age-only reaper threw away.
    ///
    /// Kraken returns the most recent page only (50 of 134 on that account),
    /// which is ample for orders minutes old. One that somehow aged past the
    /// page would never settle, and `SETTLE_GIVE_UP_SECS` bounds that.
    /// Kraken reports `cost` and `fee` alongside `vol_exec`, and this used to
    /// throw both away. They are the only record of what a trade actually
    /// cost: the books were charging a MODELLED 0.23% of $1,000 against
    /// positions worth a couple of dollars. See `ledger.rs`.
    pub async fn executed_fills(&self) -> anyhow::Result<HashMap<String, Execution>> {
        let closed = self
            .client
            .get_closed_orders()
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(closed
            .closed
            .into_iter()
            .filter_map(|(txid, o)| execution_of(&o).map(|e| (txid, e)))
            .collect())
    }

    /// Cancel one order we placed. A txid Kraken no longer knows about
    /// (already filled, already cancelled) comes back as an error, which is
    /// not a failure for our purposes — the caller drops it either way.
    pub async fn cancel(&self, txid: &str) -> anyhow::Result<()> {
        self.client
            .cancel_order(txid)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    pub async fn place_limit(
        &self,
        pair: &str,
        side: i8,
        volume: &str,
        price: &str,
    ) -> anyhow::Result<String> {
        let side_s = if side > 0 { "buy" } else { "sell" };
        let resp = self
            .client
            .place_order(pair, side_s, "limit", volume, Some(price))
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(resp
            .txid
            .first()
            .cloned()
            .unwrap_or_else(|| format!("{resp:?}")))
    }
}

#[async_trait]
impl ExchangeClient for LiveKraken {
    fn name(&self) -> &str {
        "kraken"
    }

    fn supports(&self, c: Capability) -> bool {
        matches!(c, Capability::ReduceOnly)
    }

    async fn place_order(&self, order: &Order) -> Result<String> {
        let side = match order.side {
            Side::Buy => "buy",
            Side::Sell => "sell",
        };
        let (ordertype, price) = match order.limit_price {
            Some(p) => ("limit", Some(format!("{:.4}", p.value()))),
            None => ("market", None),
        };
        let vol = format!("{:.8}", order.size.value());
        let resp = self
            .client
            .place_order(
                order.symbol.as_str(),
                side,
                ordertype,
                &vol,
                price.as_deref(),
            )
            .await
            .map_err(|e| rustrade::Error::Exchange(e.to_string()))?;
        Ok(resp
            .txid
            .first()
            .cloned()
            .unwrap_or_else(|| "kraken-ok".into()))
    }

    async fn cancel_all(&self, _symbol: &Symbol) -> Result<usize> {
        let r = self
            .client
            .cancel_all_orders()
            .await
            .map_err(|e| rustrade::Error::Exchange(e.to_string()))?;
        Ok(r.count as usize)
    }

    async fn close_position(&self, symbol: &Symbol, position: &Position) -> Result<String> {
        if position.is_flat() {
            return Ok("flat".into());
        }
        let side = if position.is_long() {
            Side::Sell
        } else {
            Side::Buy
        };
        let order = Order::market(symbol.clone(), side, rustrade::Volume(position.qty.abs()));
        self.place_order(&order).await
    }

    async fn get_position(&self, _symbol: &Symbol) -> Result<Position> {
        // Spot: the brain + paper books are the source of truth until we map
        // Kraken balances into a Position. Returning FLAT here would fight
        // the execution cache — live should not be enabled until that's done.
        Ok(Position::FLAT)
    }

    async fn get_balance(&self, currency: &str) -> Result<f64> {
        let map = self
            .client
            .get_balance()
            .await
            .map_err(|e| rustrade::Error::Exchange(e.to_string()))?;
        let key = match currency {
            "USD" => "ZUSD",
            "SOL" => "SOL",
            "ETH" => "XETH",
            other => other,
        };
        let s = map.get(key).or_else(|| map.get(currency));
        Ok(s.and_then(|v| v.parse().ok()).unwrap_or(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_usd_sol_eth_and_skips_dust() {
        let bals = vec![
            ("ZUSD".into(), 250.0),
            ("SOL".into(), 10.0),
            ("XETH".into(), 0.5),
            ("XXBT".into(), 0.01),
            ("SOL.S".into(), 2.0),
            ("XXDG".into(), 1.0), // 1 * $0.001 = $0.001 dust, skipped
        ];
        let marks = vec![
            ("SOLUSD".into(), 100.0),
            ("ETHUSD".into(), 4000.0),
            ("XBTUSD".into(), 100_000.0),
            ("XDGUSD".into(), 0.001),
        ];
        let snap = value_balances(&bals, &marks);
        assert!(snap.error.is_none());
        assert_eq!(snap.total_usd, 250.0 + 1000.0 + 2000.0 + 1000.0 + 200.0);
        let codes: Vec<_> = snap.assets.iter().map(|a| a.code.as_str()).collect();
        assert!(codes.contains(&"USD"));
        assert!(codes.contains(&"SOL"));
        assert!(codes.contains(&"ETH"));
        assert!(codes.contains(&"BTC"));
        assert!(codes.contains(&"SOL (SOL.S)"));
        assert!(!codes.iter().any(|c| c.contains("DOGE")));
    }

    fn closed(vol_exec: &str, cost: &str, fee: &str, opentm: Option<f64>) -> KrakenOrder {
        serde_json::from_value(serde_json::json!({
            "status": "closed",
            "opentm": opentm,
            "vol": "0.01488212",
            "vol_exec": vol_exec,
            "cost": cost,
            "fee": fee,
            "descr": { "pair": "ETHUSD", "type": "buy", "ordertype": "limit", "price": "2692.32" }
        }))
        .expect("Kraken's own shape")
    }

    #[test]
    fn a_closed_order_yields_the_cost_and_fee_the_ledger_needs() {
        // These two fields were being read off the wire and thrown away, so
        // the only fee the bot knew was a MODELLED 0.23% of $1,000 -- charged
        // against a position worth $2.70.
        let e = execution_of(&closed("0.00100000", "2.69609", "0.00701", Some(1790064512.4)))
            .expect("a parseable fill");
        assert!((e.vol_exec - 0.001).abs() < 1e-12);
        assert!((e.cost - 2.69609).abs() < 1e-12);
        assert!((e.fee - 0.00701).abs() < 1e-12);
        assert_eq!(e.at, Some(1_790_064_512), "Kraken's clock, truncated to seconds");
    }

    #[test]
    fn an_order_that_traded_nothing_still_parses() {
        // The ordinary reaped limit order. It must come back as a FACT --
        // closed, executed zero -- and not as an absence, which the settler
        // reads as "still open".
        let e = execution_of(&closed("0.00000000", "0.00000", "0.00000", None)).expect("a fact");
        assert_eq!(e.vol_exec, 0.0);
        assert_eq!(e.at, None);
    }

    #[test]
    fn an_unreadable_cost_or_fee_does_not_lose_the_whole_fill() {
        // Dropping the record would leave the settler believing the order
        // never closed, and eventually the book would drop a wallet claim on
        // coins that really arrived. A zero cost is visible in the ledger; a
        // vanished fill is not.
        let e = execution_of(&closed("0.00100000", "", "oops", None)).expect("still a fill");
        assert!((e.vol_exec - 0.001).abs() < 1e-12);
        assert_eq!(e.cost, 0.0);
        assert_eq!(e.fee, 0.0);
    }

    #[test]
    fn an_unreadable_volume_is_not_a_fill_at_all() {
        // Without a volume there is nothing to settle against, and guessing
        // zero would withdraw a real entry.
        assert!(execution_of(&closed("", "2.69", "0.007", None)).is_none());
        assert!(execution_of(&closed("nan", "2.69", "0.007", None)).is_none());
    }

    #[test]
    fn needed_pairs_skips_stables() {
        let bals = vec![
            ("ZUSD".into(), 1.0),
            ("USDT".into(), 2.0),
            ("SOL".into(), 1.0),
            ("XETH".into(), 1.0),
        ];
        let pairs = needed_pairs(&bals);
        assert_eq!(pairs, vec!["ETHUSD".to_string(), "SOLUSD".to_string()]);
    }
}
