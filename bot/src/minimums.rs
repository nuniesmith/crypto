//! Kraken's minimum order sizes, read live from `/0/public/AssetPairs`.
//!
//! Every sizing decision asks `ordermin` / `costmin` here. Until 2026-10-15
//! they were constants in alloc.rs, verified once by hand -- and Kraken moves
//! its minimums as prices move. By 10-15 three were stale (XRP 1.65 against
//! Kraken's 4, ETH 0.001 against 0.002, BTC 0.00005 against 0.00006), so the
//! bot sized orders Kraken refused: a 3.3 XRP deposit top-up was rejected
//! ("volume minimum not met") every hour, and because the bot believed that
//! buy was possible, the deposit it belonged to never finished investing and
//! every daily report showed a deposit backlog.
//!
//! The table is refreshed from Kraken's PUBLIC endpoint (no key, so no
//! nonce) before each hourly work tick. alloc.rs's constants stay as the
//! fallback, for a pair Kraken did not answer for or before the first read
//! succeeds, and are kept at Kraken's values as of the date beside them.

use std::collections::BTreeMap;
use std::sync::RwLock;

use exchange_apiws::KrakenRestClient;

use crate::alloc;

/// Every pair this bot sizes orders for, as Kraken's `altname`s.
pub const PAIRS: [&str; 8] = [
    "XBTUSD", "ETHUSD", "SOLUSD", "LINKUSD", "XRPUSD", "INJUSD", "USDCUSD", "USDTUSD",
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Minimum {
    /// Smallest order Kraken accepts, in units of the base asset.
    pub ordermin: f64,
    /// Smallest order Kraken accepts, in USD.
    pub costmin: f64,
}

pub type Table = BTreeMap<String, Minimum>;

/// The table in force. `None` until the first successful refresh.
static LIVE: RwLock<Option<Table>> = RwLock::new(None);

/// alloc.rs's constants for `pair`; `None` for a pair this bot does not trade.
pub fn fallback(pair: &str) -> Option<Minimum> {
    let ordermin = match pair {
        "XBTUSD" => alloc::MIN_BTC,
        "ETHUSD" => alloc::MIN_ETH,
        "SOLUSD" => alloc::MIN_SOL,
        "LINKUSD" => alloc::MIN_LINK,
        "XRPUSD" => alloc::MIN_XRP,
        "INJUSD" => alloc::MIN_INJ,
        "USDCUSD" | "USDTUSD" => alloc::MIN_STABLE,
        _ => return None,
    };
    Some(Minimum {
        ordermin,
        costmin: alloc::COST_MIN_USD,
    })
}

/// A table from AssetPairs rows `(altname, ordermin, costmin)`.
///
/// Only this bot's pairs are kept. A row whose `ordermin` is missing or not a
/// positive number is dropped, so that pair's fallback stands; a missing or
/// unusable `costmin` takes the fallback's.
pub fn table_from<'a>(
    rows: impl IntoIterator<Item = (&'a str, Option<&'a str>, Option<&'a str>)>,
) -> Table {
    let mut table = Table::new();
    for (altname, ordermin, costmin) in rows {
        let Some(fb) = fallback(altname) else {
            continue;
        };
        let Some(ordermin) = ordermin
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
        else {
            continue;
        };
        let costmin = costmin
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(fb.costmin);
        table.insert(altname.to_string(), Minimum { ordermin, costmin });
    }
    table
}

/// `pair`'s minimum: the live entry if there is one, else the fallback. A
/// pair this bot does not trade can never be ordered.
pub fn lookup(live: Option<&Table>, pair: &str) -> Minimum {
    live.and_then(|t| t.get(pair).copied())
        .or_else(|| fallback(pair))
        .unwrap_or(Minimum {
            ordermin: f64::MAX,
            costmin: f64::MAX,
        })
}

fn current(pair: &str) -> Minimum {
    let guard = LIVE.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    lookup(guard.as_ref(), pair)
}

/// Kraken's minimum order size for `pair`, in coins.
pub fn ordermin(pair: &str) -> f64 {
    current(pair).ordermin
}

/// Kraken's minimum order cost for `pair`, in USD.
pub fn costmin(pair: &str) -> f64 {
    current(pair).costmin
}

/// How `new` differs from what was in force (`old`, or the fallback where
/// `old` has no entry). A steady table reports nothing.
pub fn changes(old: Option<&Table>, new: &Table) -> Vec<String> {
    new.iter()
        .filter_map(|(pair, now)| {
            let before = lookup(old, pair);
            (before != *now).then(|| {
                format!(
                    "{pair}: ordermin {} -> {}, costmin {} -> {}",
                    before.ordermin, now.ordermin, before.costmin, now.costmin
                )
            })
        })
        .collect()
}

/// Kraken's current minimums for this bot's pairs, without putting them in
/// force. A public call: no key, no nonce.
pub async fn fetch(client: &KrakenRestClient) -> anyhow::Result<Table> {
    let pairs = client
        .get_asset_pairs(Some(&PAIRS.join(",")))
        .await
        .map_err(|e| anyhow::anyhow!("AssetPairs: {e}"))?;
    let table = table_from(pairs.values().map(|p| {
        (
            p.altname.as_str(),
            p.ordermin.as_deref(),
            p.costmin.as_deref(),
        )
    }));
    if table.is_empty() {
        anyhow::bail!("AssetPairs answered for none of this bot's pairs");
    }
    Ok(table)
}

/// Put `new` in force, returning what changed.
pub fn install(new: Table) -> Vec<String> {
    let mut guard = LIVE
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let notes = changes(guard.as_ref(), &new);
    *guard = Some(new);
    notes
}

/// Read Kraken's current minimums and put them in force, returning what
/// changed. On any error the table already in force is kept.
pub async fn refresh(client: &KrakenRestClient) -> anyhow::Result<Vec<String>> {
    Ok(install(fetch(client).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_this_bots_pairs_and_reads_their_minimums() {
        let t = table_from([
            ("XRPUSD", Some("4"), Some("0.5")),
            ("ETHUSD", Some("0.002"), Some("0.5")),
            ("DOGEUSD", Some("50"), Some("0.5")),
        ]);
        assert_eq!(t.len(), 2);
        assert_eq!(
            t["XRPUSD"],
            Minimum {
                ordermin: 4.0,
                costmin: 0.5
            }
        );
        assert!((t["ETHUSD"].ordermin - 0.002).abs() < 1e-12);
    }

    #[test]
    fn an_unusable_ordermin_leaves_the_fallback_in_force() {
        let t = table_from([
            ("XRPUSD", None, Some("0.5")),
            ("SOLUSD", Some("0"), None),
            ("INJUSD", Some("abc"), None),
        ]);
        assert!(t.is_empty());
        assert_eq!(lookup(Some(&t), "XRPUSD"), fallback("XRPUSD").unwrap());
    }

    #[test]
    fn an_unusable_costmin_takes_the_fallback_costmin() {
        let t = table_from([("LINKUSD", Some("0.35"), Some("-1"))]);
        assert_eq!(
            t["LINKUSD"],
            Minimum {
                ordermin: 0.35,
                costmin: alloc::COST_MIN_USD
            }
        );
    }

    #[test]
    fn the_live_table_wins_over_the_fallback() {
        let t = table_from([("XRPUSD", Some("10"), Some("1"))]);
        assert_eq!(
            lookup(Some(&t), "XRPUSD"),
            Minimum {
                ordermin: 10.0,
                costmin: 1.0
            }
        );
        // A pair the live table lacks still has its fallback.
        assert_eq!(lookup(Some(&t), "XBTUSD"), fallback("XBTUSD").unwrap());
        assert_eq!(lookup(None, "XBTUSD"), fallback("XBTUSD").unwrap());
    }

    #[test]
    fn a_pair_this_bot_does_not_trade_can_never_be_ordered() {
        assert_eq!(lookup(None, "DOGEUSD").ordermin, f64::MAX);
    }

    #[test]
    fn the_fallback_matches_krakens_minimums_as_of_2026_10_15() {
        // Kraken /0/public/AssetPairs on 2026-10-15. A stale entry here is
        // exactly the bug this module exists for, should a refresh ever fail.
        for (pair, ordermin) in [
            ("XBTUSD", 0.00006),
            ("ETHUSD", 0.002),
            ("SOLUSD", 0.05),
            ("LINKUSD", 0.35),
            ("XRPUSD", 4.0),
            ("INJUSD", 0.7),
            ("USDCUSD", 5.0),
            ("USDTUSD", 5.0),
        ] {
            let fb = fallback(pair).unwrap();
            assert!((fb.ordermin - ordermin).abs() < 1e-12, "{pair}");
            assert!((fb.costmin - 0.5).abs() < 1e-12, "{pair}");
        }
    }

    #[test]
    fn reports_only_what_changed() {
        let old = table_from([("XRPUSD", Some("4"), Some("0.5"))]);
        let same = table_from([("XRPUSD", Some("4"), Some("0.5"))]);
        assert!(changes(Some(&old), &same).is_empty());
        let raised = table_from([("XRPUSD", Some("5"), Some("0.5"))]);
        let notes = changes(Some(&old), &raised);
        assert_eq!(notes, vec!["XRPUSD: ordermin 4 -> 5, costmin 0.5 -> 0.5"]);
    }

    /// Kraken's real response, end to end: its own pair names (XXBTZUSD,
    /// USDTZUSD...) must map to every pair this bot trades. Opt-in, because
    /// it needs the network: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "network: calls Kraken's public AssetPairs"]
    async fn fetch_reads_every_pair_from_kraken() {
        let client = KrakenRestClient::new().expect("public client");
        let table = fetch(&client).await.expect("AssetPairs");
        for pair in PAIRS {
            let m = table
                .get(pair)
                .unwrap_or_else(|| panic!("{pair} missing: {table:?}"));
            assert!(m.ordermin > 0.0 && m.costmin >= 0.0, "{pair}: {m:?}");
        }
    }
}
