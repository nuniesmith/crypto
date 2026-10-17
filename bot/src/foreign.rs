//! Trades on this Kraken account that this bot did not place.
//!
//! On 2026-10-14 a second copy of the bot (the FKS spawner's) ran on the same
//! API key, and both invested the same deposit; nothing said so for a day.
//! Once an hour the loop now reads Kraken's closed orders and alerts on every
//! order that EXECUTED but is not one of its own: not still pending, and not
//! in the ledger's fills (`LiveLedger::fills` keeps the last 100, twice
//! Kraken's 50-order page, so none of the bot's own orders on that page can
//! have fallen off it).
//!
//! Orders opened before the watch began are ignored. The ledger does not reach
//! back that far, and the 2026-10-14 trades are already known.
//!
//! Alert only, deliberately. Stopping this bot would not stop the other
//! trader, and an order the operator places by hand is foreign too.

use std::collections::HashSet;

use exchange_apiws::kraken::KrakenOrder;
use serde::{Deserialize, Serialize};

/// Alerted txids the state file remembers. Kraken's page holds 50 orders, so
/// an alerted order falls off that page long before it falls off this list.
const MAX_ALERTED: usize = 200;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ForeignWatch {
    /// Unix seconds the watch began; 0 until the first check.
    #[serde(default)]
    pub since: f64,
    /// Foreign order txids already alerted on, oldest first.
    #[serde(default)]
    pub alerted: Vec<String>,
}

/// What an alert needs to say about one closed Kraken order.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedOrder {
    pub txid: String,
    /// When the order opened (Unix seconds).
    pub opened: Option<f64>,
    pub vol_exec: f64,
    /// Quote currency spent or received.
    pub cost: f64,
    pub pair: String,
    pub side: String,
    pub ordertype: String,
    pub price: String,
}

fn number(s: &str) -> f64 {
    s.parse()
        .ok()
        .filter(|v: &f64| v.is_finite())
        .unwrap_or(0.0)
}

impl ClosedOrder {
    pub fn from_kraken(txid: &str, o: &KrakenOrder) -> Self {
        let (pair, side, ordertype, price) = match &o.descr {
            Some(d) => (
                d.pair.clone(),
                d.side.clone(),
                d.ordertype.clone(),
                d.price.clone(),
            ),
            None => Default::default(),
        };
        Self {
            txid: txid.to_string(),
            opened: o.opentm.filter(|t| t.is_finite()),
            vol_exec: number(&o.vol_exec),
            cost: number(&o.cost),
            pair,
            side,
            ordertype,
            price,
        }
    }

    /// One line for the log, the journal and the alert.
    pub fn describe(&self) -> String {
        let opened = self
            .opened
            .map(|t| crate::paper::fmt_ts(t as i64))
            .unwrap_or_else(|| "at an unknown time".into());
        format!(
            "{} {} {} ({} at {}) for ${:.2}, opened {opened}, order {}",
            self.side.to_uppercase(),
            crate::live::fmt_qty(self.vol_exec),
            self.pair,
            self.ordertype,
            self.price,
            self.cost,
            self.txid,
        )
    }
}

impl ForeignWatch {
    /// Executed orders in `closed` that this bot did not place and has not
    /// alerted on yet, oldest first; they are recorded as alerted. The first
    /// call only starts the watch and finds nothing.
    pub fn check(
        &mut self,
        closed: &[ClosedOrder],
        own: &HashSet<&str>,
        now: f64,
    ) -> Vec<ClosedOrder> {
        if self.since <= 0.0 {
            self.since = now;
            return Vec::new();
        }
        let since = self.since;
        let mut found: Vec<ClosedOrder> = closed
            .iter()
            .filter(|o| o.vol_exec > 0.0)
            // No open time means no way to tell it from history: skip it
            // rather than raise an alarm that may be months old.
            .filter(|o| o.opened.is_some_and(|t| t >= since))
            .filter(|o| !own.contains(o.txid.as_str()))
            .filter(|o| !self.alerted.contains(&o.txid))
            .cloned()
            .collect();
        found.sort_by(|a, b| {
            a.opened
                .partial_cmp(&b.opened)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.alerted.extend(found.iter().map(|o| o.txid.clone()));
        let excess = self.alerted.len().saturating_sub(MAX_ALERTED);
        self.alerted.drain(..excess);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(txid: &str, opened: f64, vol_exec: f64) -> ClosedOrder {
        ClosedOrder {
            txid: txid.into(),
            opened: Some(opened),
            vol_exec,
            cost: vol_exec * 100.0,
            pair: "XBTUSD".into(),
            side: "buy".into(),
            ordertype: "limit".into(),
            price: "100.0".into(),
        }
    }

    fn watching_since(at: f64) -> ForeignWatch {
        ForeignWatch {
            since: at,
            alerted: Vec::new(),
        }
    }

    fn none() -> HashSet<&'static str> {
        HashSet::new()
    }

    #[test]
    fn the_first_check_starts_the_watch_and_flags_nothing() {
        let mut w = ForeignWatch::default();
        let found = w.check(&[order("HISTORY", 50.0, 1.0)], &none(), 100.0);
        assert!(found.is_empty());
        assert_eq!(w.since, 100.0);
    }

    #[test]
    fn an_executed_order_the_bot_did_not_place_is_flagged_once() {
        let mut w = watching_since(100.0);
        let closed = [order("THEIRS", 150.0, 0.5)];
        assert_eq!(w.check(&closed, &none(), 200.0), closed.to_vec());
        assert!(
            w.check(&closed, &none(), 300.0).is_empty(),
            "already alerted on"
        );
    }

    #[test]
    fn the_bots_own_orders_are_never_flagged() {
        let mut w = watching_since(100.0);
        let own: HashSet<&str> = ["MINE"].into_iter().collect();
        assert!(w
            .check(&[order("MINE", 150.0, 1.0)], &own, 200.0)
            .is_empty());
    }

    #[test]
    fn unexecuted_and_pre_watch_orders_are_ignored() {
        let mut w = watching_since(100.0);
        let closed = [order("CANCELLED", 150.0, 0.0), order("BEFORE", 99.0, 1.0)];
        assert!(w.check(&closed, &none(), 200.0).is_empty());
    }

    #[test]
    fn an_order_without_an_open_time_is_skipped_rather_than_guessed() {
        let mut w = watching_since(100.0);
        let mut o = order("UNDATED", 150.0, 1.0);
        o.opened = None;
        assert!(w.check(&[o], &none(), 200.0).is_empty());
    }

    #[test]
    fn findings_come_oldest_first_and_the_alerted_list_stays_bounded() {
        let mut w = watching_since(1.0);
        w.alerted = (0..MAX_ALERTED).map(|i| format!("A{i}")).collect();
        let found = w.check(
            &[order("LATE", 300.0, 1.0), order("EARLY", 200.0, 1.0)],
            &none(),
            400.0,
        );
        let ids: Vec<&str> = found.iter().map(|o| o.txid.as_str()).collect();
        assert_eq!(ids, ["EARLY", "LATE"]);
        assert_eq!(w.alerted.len(), MAX_ALERTED);
        assert_eq!(
            w.alerted.first().map(String::as_str),
            Some("A2"),
            "oldest dropped"
        );
        assert_eq!(w.alerted.last().map(String::as_str), Some("LATE"));
    }

    #[test]
    fn a_state_file_from_before_the_watch_loads_it_unstarted() {
        let w: ForeignWatch = serde_json::from_str("{}").unwrap();
        assert_eq!(w, ForeignWatch::default());
    }

    #[test]
    fn a_kraken_closed_order_reads_into_what_the_alert_says() {
        let o: KrakenOrder = serde_json::from_value(serde_json::json!({
            "status": "closed",
            "opentm": 1_760_680_000.5,
            "vol": "0.00100000",
            "vol_exec": "0.00100000",
            "cost": "65.00",
            "fee": "0.26",
            "descr": {"pair": "XBTUSD", "type": "sell", "ordertype": "limit", "price": "65000.0"}
        }))
        .unwrap();
        let c = ClosedOrder::from_kraken("OABC12-DEF34-GHI56", &o);
        assert_eq!(c.vol_exec, 0.001);
        assert_eq!(c.cost, 65.0);
        assert_eq!(c.opened, Some(1_760_680_000.5));
        assert_eq!(
            c.describe(),
            "SELL 0.001 XBTUSD (limit at 65000.0) for $65.00, opened 2025-10-17 05:46 UTC, \
             order OABC12-DEF34-GHI56"
        );
    }
}
