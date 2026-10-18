//! The forward test frozen on 2026-10-16: the basket study's five portfolios,
//! scored only on Kraken daily candles from that day on.
//!
//! The basket study (`docs/basket-study-2026-10-16.md`) measured the live
//! policy on history that was known when it was written. This is the part it
//! could not see: the same replay, frozen before any forward data existed.
//! Nothing here may be tuned after the fact. A changed rule is a NEW forward
//! test with a new start date, never an edit to this one.
//!
//! A port of `crypto.opt.forward` and the `simulate` engine of
//! `crypto.opt.basket` (tag `python-research-final`), with one deliberate
//! difference: each day's regime comes from the bot's own
//! [`regime::evaluate`], so the test judges the code that trades. The tests
//! replay candles captured on 2026-10-18 and must land on the numbers the
//! Python computed from the same candles.
//!
//! Each day from the start:
//! 1. a deposit ($250 on day one, $50 every 7th day after) pays 0.2% to turn
//!    USDC into USD;
//! 2. a coin whose regime (read from the closes before the day) differs from
//!    the one last acted on closes its gap to target at the day's open,
//!    either side;
//! 3. while a deposit is pending, coins already on their regime are topped up
//!    to target, buy-only;
//! 4. the account is marked at the close.
//!
//! Every trade pays Kraken's tier-1 maker 0.40% plus 0.10% slippage. An order
//! under $5 is not placed, a buy spends at most 99% of the cash free that
//! day, and a flip the cash cannot fund is retried on later days.

use std::collections::BTreeMap;

use anyhow::anyhow;
use chrono::{DateTime, NaiveDate, Utc};
use exchange_apiws::KrakenRestClient;

use crate::{paper, regime};

/// The first day the basket study never saw.
pub fn forward_start() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 16).expect("a valid date")
}

pub const INITIAL: f64 = 250.0;
pub const WEEKLY: f64 = 50.0;

/// Kraken tier-1 maker plus slippage, on every coin trade.
const FEE: f64 = 0.0040 + 0.0010;
/// USDC to USD on each deposit, the live account's real cost.
const CONVERSION: f64 = 0.002;
/// Kraken's minimum order is $4-5.5 on every pair.
const MIN_ORDER: f64 = 5.0;
/// A buy spends at most 99% of the free cash, as the bot does.
const FEE_RESERVE: f64 = 0.99;

pub struct Portfolio {
    pub name: &'static str,
    /// Base (bull) weight per Kraken pair, in the order buys are funded.
    pub weights: &'static [(&'static str, f64)],
    /// `false` treats every coin as bull: the same replay without the rule.
    pub trend: bool,
}

const LIVE_V3: &[(&str, f64)] = &[
    ("XBTUSD", 0.44),
    ("ETHUSD", 0.22),
    ("SOLUSD", 0.12),
    ("LINKUSD", 0.05),
    ("XRPUSD", 0.05),
    ("INJUSD", 0.05),
];
const MAJORS_93: &[(&str, f64)] = &[
    ("XBTUSD", 0.44 / 0.78 * 0.93),
    ("ETHUSD", 0.22 / 0.78 * 0.93),
    ("SOLUSD", 0.12 / 0.78 * 0.93),
];
const BTC_ONLY: &[(&str, f64)] = &[("XBTUSD", 1.0)];

/// Frozen 2026-10-16. Do not edit: a changed rule is a new forward test.
pub const PORTFOLIOS: [Portfolio; 5] = [
    Portfolio {
        name: "live v3 (trend)",
        weights: LIVE_V3,
        trend: true,
    },
    Portfolio {
        name: "BTC/ETH/SOL only (trend)",
        weights: MAJORS_93,
        trend: true,
    },
    Portfolio {
        name: "BTC only (trend)",
        weights: BTC_ONLY,
        trend: true,
    },
    Portfolio {
        name: "same weights, no trend",
        weights: LIVE_V3,
        trend: false,
    },
    Portfolio {
        name: "BTC only, no trend",
        weights: BTC_ONLY,
        trend: false,
    },
];

/// One closed daily candle: (open time in unix seconds, open, close).
pub type DailyCandle = (i64, f64, f64);

/// Aligned daily opens and closes for several pairs, and each day's regime.
pub struct Panel {
    pub days: Vec<NaiveDate>,
    pub pairs: Vec<String>,
    /// `[day][pair]`. NaN before a pair's first candle.
    open: Vec<Vec<f64>>,
    close: Vec<Vec<f64>>,
    /// The regime for day `d`, decided from the closes up to `d-1` (the bot
    /// reads it at 00:05 UTC from the last closed candle). `None` until 200
    /// closes exist.
    bull: Vec<Vec<Option<bool>>>,
}

impl Panel {
    /// From each pair's CLOSED daily candles, oldest first. The panel runs
    /// from the latest first day to the earliest last day, and a day a pair
    /// has no candle carries its previous one forward.
    pub fn new(series: &[(String, Vec<DailyCandle>)]) -> Option<Panel> {
        let by_day: Vec<BTreeMap<NaiveDate, (f64, f64)>> = series
            .iter()
            .map(|(_, candles)| {
                candles
                    .iter()
                    .filter_map(|&(t, o, c)| Some((day_of(t)?, (o, c))))
                    .collect()
            })
            .collect();
        let first = by_day
            .iter()
            .map(|m| m.keys().next().copied())
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .max()?;
        let last = by_day
            .iter()
            .map(|m| m.keys().next_back().copied())
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .min()?;
        if last < first {
            return None;
        }
        let days: Vec<NaiveDate> = first.iter_days().take_while(|d| *d <= last).collect();
        let (n, k) = (days.len(), series.len());
        let mut open = vec![vec![f64::NAN; k]; n];
        let mut close = vec![vec![f64::NAN; k]; n];
        for (j, candles) in by_day.iter().enumerate() {
            let mut carried = (f64::NAN, f64::NAN);
            for (i, day) in days.iter().enumerate() {
                if let Some(&oc) = candles.get(day) {
                    carried = oc;
                }
                open[i][j] = carried.0;
                close[i][j] = carried.1;
            }
        }
        let mut bull = vec![vec![None; k]; n];
        for j in 0..k {
            let closes: Vec<f64> = close.iter().map(|row| row[j]).collect();
            // Only leading days can be NaN: a price, once seen, is carried.
            let from = closes.iter().position(|c| !c.is_nan()).unwrap_or(n);
            // The reading on one day's close decides the next day.
            for (offset, reading) in regime::series(&closes[from..]).iter().enumerate() {
                if let Some(row) = bull.get_mut(from + offset + 1) {
                    row[j] = reading.map(|r| r.bull);
                }
            }
        }
        Some(Panel {
            days,
            pairs: series.iter().map(|(p, _)| p.clone()).collect(),
            open,
            close,
            bull,
        })
    }

    /// The regime the bot applies today: read from every closed candle.
    pub fn regimes_today(&self) -> Vec<(String, Option<bool>)> {
        self.pairs
            .iter()
            .enumerate()
            .map(|(j, pair)| {
                let closes: Vec<f64> = self
                    .close
                    .iter()
                    .map(|row| row[j])
                    .filter(|c| !c.is_nan())
                    .collect();
                (pair.clone(), regime::evaluate(&closes).map(|r| r.bull))
            })
            .collect()
    }
}

fn day_of(unix: i64) -> Option<NaiveDate> {
    DateTime::from_timestamp(unix, 0).map(|t| t.date_naive())
}

/// The panel from Kraken's public daily candles for the bot's pairs. No key
/// and no nonce, so it is safe to run beside the trading loop.
pub async fn kraken_panel(client: &KrakenRestClient) -> anyhow::Result<Panel> {
    let now = Utc::now().timestamp();
    let mut series = Vec::new();
    for pair in regime::PAIRS {
        let ohlc = client
            .get_ohlc(pair, 1440)
            .await
            .map_err(|e| anyhow!("daily OHLC {pair}: {e}"))?;
        // Kraken's last daily candle is the day still forming.
        let closed: Vec<DailyCandle> = ohlc
            .candles
            .iter()
            .filter(|c| c.time + 86_400 <= now)
            .map(|c| (c.time, c.open_f64(), c.close_f64()))
            .collect();
        series.push((pair.to_string(), closed));
    }
    Panel::new(&series).ok_or_else(|| anyhow!("Kraken's daily candles do not overlap"))
}

pub enum Deposits {
    /// `initial` on day one and `weekly` every 7th day after.
    Weekly { initial: f64, weekly: f64 },
    /// The real deposit stream, UTC day to USD. A negative amount (a
    /// withdrawal) is not replayed.
    Schedule(BTreeMap<NaiveDate, f64>),
}

pub struct Run {
    pub days: Vec<NaiveDate>,
    pub value: Vec<f64>,
    pub contributed: Vec<f64>,
    /// Time-weighted unit value, 1.0 at the start: the return with the
    /// deposits taken out.
    pub nav: Vec<f64>,
    pub trades: u32,
    pub fees: f64,
}

impl Run {
    /// The worst fall of the unit value from its running peak, e.g. -0.25.
    pub fn max_drawdown(&self) -> f64 {
        let mut peak = f64::MIN;
        let mut worst: f64 = 0.0;
        for &v in &self.nav {
            peak = peak.max(v);
            worst = worst.min(v / peak - 1.0);
        }
        worst
    }
}

/// Mark-to-market of `units` at `px`; a pair with no price counts as 0.
fn worth(units: &[f64], px: &[f64]) -> f64 {
    units
        .iter()
        .zip(px)
        .map(|(u, p)| if p.is_nan() { 0.0 } else { u * p })
        .sum()
}

/// Replay `portfolio` from `start` through `end` (or the panel's last day).
pub fn simulate(
    panel: &Panel,
    portfolio: &Portfolio,
    start: NaiveDate,
    end: Option<NaiveDate>,
    deposits: &Deposits,
) -> Run {
    let i0 = panel.days.partition_point(|d| *d < start);
    let i1 = end.map_or(panel.days.len(), |e| {
        panel.days.partition_point(|d| *d <= e)
    });
    let k = panel.pairs.len();
    // (panel column, base weight), in the order buys are funded.
    let order: Vec<(usize, f64)> = portfolio
        .weights
        .iter()
        .filter_map(|(pair, w)| panel.pairs.iter().position(|p| p == pair).map(|j| (j, *w)))
        .collect();

    let mut units = vec![0.0_f64; k];
    let mut cash = 0.0_f64;
    // The regime last acted on per coin; None = never.
    let mut applied: Vec<Option<bool>> = vec![None; k];
    let mut deposit_pending = false;
    let mut nav_units = 0.0_f64;
    let mut total_in = 0.0_f64;
    let mut run = Run {
        days: Vec::new(),
        value: Vec::new(),
        contributed: Vec::new(),
        nav: Vec::new(),
        trades: 0,
        fees: 0.0,
    };

    for (t, i) in (i0..i1).enumerate() {
        let open = &panel.open[i];
        // 1. The deposit, issuing fund units at the pre-deposit value.
        let amount = match deposits {
            Deposits::Weekly { initial, weekly } => match t {
                0 => *initial,
                t if t % 7 == 0 => *weekly,
                _ => 0.0,
            },
            Deposits::Schedule(s) => s.get(&panel.days[i]).copied().unwrap_or(0.0),
        };
        if amount > 0.0 {
            let pre = cash + worth(&units, open);
            let unit_value = if nav_units > 0.0 {
                pre / nav_units
            } else {
                1.0
            };
            nav_units += amount / unit_value;
            cash += amount * (1.0 - CONVERSION);
            total_in += amount;
            deposit_pending = true;
        }

        let total = cash + worth(&units, open);
        let mut spendable = cash;
        let regime_of = |j: usize| {
            if portfolio.trend {
                panel.bull[i][j]
            } else {
                Some(true)
            }
        };
        let target = |w: f64, bull: bool| {
            let w = if portfolio.trend {
                w * regime::exposure(bull)
            } else {
                w
            };
            w * total
        };

        // 2. Flips, and coins never acted on: close the gap, either side.
        for &(j, w) in &order {
            let price = open[j];
            if price.is_nan() {
                continue;
            }
            let Some(bull) = regime_of(j) else {
                continue; // regime not read yet: the bot leaves the coin alone
            };
            if applied[j] == Some(bull) {
                continue;
            }
            let delta = target(w, bull) - units[j] * price;
            if delta > 0.0 {
                let spend = delta.min(spendable.max(0.0) * FEE_RESERVE);
                if spend < MIN_ORDER {
                    if delta < MIN_ORDER {
                        applied[j] = Some(bull); // already within a minimum order
                    }
                    continue; // under-funded: retried another day
                }
                let fee = spend * FEE;
                units[j] += (spend - fee) / price;
                cash -= spend;
                spendable -= spend;
                run.fees += fee;
                run.trades += 1;
                if delta - spend < MIN_ORDER {
                    applied[j] = Some(bull);
                }
            } else {
                let q = (-delta / price).min(units[j]);
                if q * price < MIN_ORDER {
                    applied[j] = Some(bull);
                    continue;
                }
                let proceeds = q * price;
                let fee = proceeds * FEE;
                units[j] -= q;
                cash += proceeds - fee;
                run.fees += fee;
                run.trades += 1;
                applied[j] = Some(bull);
            }
        }

        // 3. Deposit top-ups: buy-only, into coins already on their regime.
        if deposit_pending {
            let mut wanted_any = false;
            for &(j, w) in &order {
                let price = open[j];
                if price.is_nan() {
                    continue;
                }
                let Some(bull) = regime_of(j) else {
                    continue;
                };
                if applied[j] != Some(bull) {
                    continue;
                }
                let delta = target(w, bull) - units[j] * price;
                if delta <= 0.0 {
                    continue;
                }
                let spend = delta.min(spendable.max(0.0) * FEE_RESERVE);
                if spend < MIN_ORDER {
                    continue;
                }
                let fee = spend * FEE;
                units[j] += (spend - fee) / price;
                cash -= spend;
                spendable -= spend;
                run.fees += fee;
                run.trades += 1;
                wanted_any = true;
            }
            if !wanted_any {
                deposit_pending = false;
            }
        }

        // 4. Mark to the close.
        let v = cash + worth(&units, &panel.close[i]);
        run.days.push(panel.days[i]);
        run.value.push(v);
        run.contributed.push(total_in);
        run.nav
            .push(if nav_units > 0.0 { v / nav_units } else { 1.0 });
    }
    run
}

fn coin(pair: &str) -> &str {
    match pair.trim_end_matches("USD") {
        "XBT" => "BTC",
        c => c,
    }
}

fn pct(x: f64) -> String {
    format!("{:+.2}%", x * 100.0)
}

/// The forward test as text: one row per portfolio, then today's regimes.
/// `None` until the panel reaches `start`.
pub fn report(panel: &Panel, start: NaiveDate) -> Option<String> {
    let last = *panel.days.last()?;
    if last < start {
        return None;
    }
    let days = (last - start).num_days() + 1;
    let mut out = format!(
        "Forward test {start} .. {last} ({days} days), ${INITIAL:.0} + ${WEEKLY:.0}/week\n\
         {:<26} {:>9} {:>9} {:>8} {:>8} {:>8} {:>6}\n",
        "portfolio", "paid in", "value", "gain", "nav", "max dd", "trades"
    );
    let deposits = Deposits::Weekly {
        initial: INITIAL,
        weekly: WEEKLY,
    };
    for p in &PORTFOLIOS {
        let r = simulate(panel, p, start, None, &deposits);
        let (Some(&value), Some(&paid), Some(&nav)) =
            (r.value.last(), r.contributed.last(), r.nav.last())
        else {
            continue;
        };
        out.push_str(&format!(
            "{:<26} {:>9} {:>9} {:>8} {:>8} {:>8} {:>6}\n",
            p.name,
            format!("${paid:.2}"),
            format!("${value:.2}"),
            pct(value / paid - 1.0),
            pct(nav - 1.0),
            pct(r.max_drawdown()),
            r.trades
        ));
    }
    let regimes: Vec<String> = panel
        .regimes_today()
        .iter()
        .map(|(pair, bull)| {
            let state = match bull {
                Some(true) => "bull",
                Some(false) => "bear",
                None => "unknown",
            };
            format!("{} {state}", coin(pair))
        })
        .collect();
    out.push_str(&format!(
        "Regimes today (closes through {last}): {}",
        regimes.join(", ")
    ));
    Some(out)
}

/// The live account against the live policy replayed with the account's own
/// starting value and deposits. What is left between the two is execution
/// (fills, timing, fees), not the deposit stream. `None` without two daily
/// snapshots from `start` to the panel's last day.
pub fn actual(panel: &Panel, state: &paper::State, start: NaiveDate) -> Option<String> {
    let last_day = *panel.days.last()?;
    let history: Vec<(NaiveDate, f64)> = state
        .history
        .iter()
        .filter_map(|s| {
            let day = NaiveDate::parse_from_str(&s.date, "%Y-%m-%d").ok()?;
            (day >= start && day <= last_day).then_some((day, s.total_usd))
        })
        .collect();
    if history.len() < 2 {
        return None;
    }
    let mut deposits: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    for f in &state.flows {
        if let Some(day) = day_of(f.ts) {
            let sign = match f.kind {
                paper::FlowKind::Deposit => 1.0,
                paper::FlowKind::Withdrawal => -1.0,
            };
            *deposits.entry(day).or_insert(0.0) += sign * f.usd_value;
        }
    }
    // Day one deposits the account's real value; later days their real flows.
    let (first_day, first_total) = history[0];
    let mut schedule = BTreeMap::from([(first_day, first_total)]);
    for (day, _) in &history[1..] {
        if let Some(&dep) = deposits.get(day) {
            if dep != 0.0 {
                *schedule.entry(*day).or_insert(0.0) += dep;
            }
        }
    }
    let (end_day, real) = *history.last()?;
    let run = simulate(
        panel,
        &PORTFOLIOS[0],
        first_day,
        Some(end_day),
        &Deposits::Schedule(schedule.clone()),
    );
    let model = *run.value.last()?;
    let paid: f64 = schedule.values().sum();
    Some(format!(
        "Actual account {first_day} .. {end_day}: paid in ${paid:.2}, real ${real:.2} ({}), \
         model ${model:.2} ({}), real minus model ${:+.2}",
        pct(real / paid - 1.0),
        pct(model / paid - 1.0),
        real - model
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alloc;
    use serde_json::Value;

    const FIXTURE: &str = include_str!("../tests/fixtures/forward_kraken_2026-10-18.json");

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn fixture() -> (Value, Panel) {
        let fx: Value = serde_json::from_str(FIXTURE).unwrap();
        let series: Vec<(String, Vec<DailyCandle>)> = fx["pairs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let pair = p.as_str().unwrap().to_string();
                let candles = fx["candles"][&pair]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|c| {
                        (
                            c[0].as_i64().unwrap(),
                            c[1].as_f64().unwrap(),
                            c[2].as_f64().unwrap(),
                        )
                    })
                    .collect();
                (pair, candles)
            })
            .collect();
        let panel = Panel::new(&series).unwrap();
        (fx, panel)
    }

    fn close_to(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * b.abs().max(1.0)
    }

    #[test]
    fn the_regime_each_day_matches_the_python_replay() {
        let (fx, panel) = fixture();
        assert_eq!(
            panel.days.first().unwrap(),
            &day(fx["first_day"].as_str().unwrap())
        );
        assert_eq!(
            panel.days.last().unwrap(),
            &day(fx["last_day"].as_str().unwrap())
        );
        for (j, pair) in panel.pairs.iter().enumerate() {
            let python: Vec<Option<bool>> = fx["bull"][pair]
                .as_array()
                .unwrap()
                .iter()
                .map(Value::as_bool)
                .collect();
            let ours: Vec<Option<bool>> = panel.bull.iter().map(|row| row[j]).collect();
            assert_eq!(ours, python, "{pair}: regime by day");
        }
    }

    #[test]
    fn every_portfolio_lands_on_the_python_numbers() {
        let (fx, panel) = fixture();
        let deposits = Deposits::Weekly {
            initial: INITIAL,
            weekly: WEEKLY,
        };
        let expected = fx["expected"].as_array().unwrap();
        assert_eq!(expected.len(), 15);
        for e in expected {
            let name = e["portfolio"].as_str().unwrap();
            let p = PORTFOLIOS.iter().find(|p| p.name == name).unwrap();
            let start = day(e["start"].as_str().unwrap());
            let r = simulate(&panel, p, start, None, &deposits);
            let what = format!("{name} from {start}");
            assert_eq!(
                r.value.len() as u64,
                e["days"].as_u64().unwrap(),
                "{what}: days"
            );
            assert_eq!(
                u64::from(r.trades),
                e["trades"].as_u64().unwrap(),
                "{what}: trades"
            );
            for (field, ours) in [
                ("paid_in", *r.contributed.last().unwrap()),
                ("value", *r.value.last().unwrap()),
                ("nav", *r.nav.last().unwrap()),
                ("nav_max_dd", r.max_drawdown()),
                ("fees", r.fees),
            ] {
                let python = e[field].as_f64().unwrap();
                assert!(
                    close_to(ours, python),
                    "{what}: {field} {ours} vs Python {python}"
                );
            }
        }
    }

    #[test]
    fn a_real_deposit_schedule_replays_like_the_python() {
        let (fx, panel) = fixture();
        let e = &fx["scheduled"];
        let schedule: BTreeMap<NaiveDate, f64> = e["schedule"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| (day(x[0].as_str().unwrap()), x[1].as_f64().unwrap()))
            .collect();
        let r = simulate(
            &panel,
            &PORTFOLIOS[0],
            day(e["start"].as_str().unwrap()),
            Some(day(e["end"].as_str().unwrap())),
            &Deposits::Schedule(schedule),
        );
        assert_eq!(u64::from(r.trades), e["trades"].as_u64().unwrap());
        assert!(close_to(
            *r.value.last().unwrap(),
            e["value"].as_f64().unwrap()
        ));
        // The withdrawal in the schedule is not replayed as money in.
        assert!(close_to(*r.contributed.last().unwrap(), 440.0));
    }

    #[test]
    fn the_frozen_live_portfolio_is_still_the_live_policy() {
        // If this fails, the live policy changed. Start a NEW forward test with
        // its own start date; never edit the frozen portfolios.
        let live = [
            ("XBTUSD", alloc::BASE_BTC),
            ("ETHUSD", alloc::BASE_ETH),
            ("SOLUSD", alloc::BASE_SOL),
            ("LINKUSD", alloc::BASE_LINK),
            ("XRPUSD", alloc::BASE_XRP),
            ("INJUSD", alloc::BASE_INJ),
        ];
        assert_eq!(LIVE_V3, &live[..]);
        assert_eq!(
            (regime::SMA_DAYS, regime::BAND, regime::CORE),
            (200, 0.05, 0.50)
        );
        assert_eq!(
            regime::PAIRS.to_vec(),
            LIVE_V3.iter().map(|(p, _)| *p).collect::<Vec<_>>()
        );
    }

    /// A two-day, one-coin panel with the regime given directly.
    fn tiny(open: [f64; 2], close: [f64; 2], bull: [Option<bool>; 2]) -> Panel {
        Panel {
            days: vec![day("2026-01-01"), day("2026-01-02")],
            pairs: vec!["XBTUSD".into()],
            open: open.iter().map(|&o| vec![o]).collect(),
            close: close.iter().map(|&c| vec![c]).collect(),
            bull: bull.iter().map(|&b| vec![b]).collect(),
        }
    }

    const BTC: Portfolio = Portfolio {
        name: "btc",
        weights: BTC_ONLY,
        trend: true,
    };

    #[test]
    fn a_bear_flip_sells_down_to_the_core() {
        let panel = tiny([100.0, 100.0], [100.0, 100.0], [Some(true), Some(false)]);
        let schedule = BTreeMap::from([(day("2026-01-01"), 1000.0)]);
        let r = simulate(
            &panel,
            &BTC,
            day("2026-01-01"),
            None,
            &Deposits::Schedule(schedule),
        );
        assert_eq!(r.trades, 2, "the day-one buy, then the bear sale");
        // Day one buys 99% of the cash; the bear sells half the target away.
        let cash = 1000.0 * (1.0 - CONVERSION);
        let bought = cash * FEE_RESERVE;
        let units = bought * (1.0 - FEE) / 100.0;
        let total = cash - bought + units * 100.0;
        let sold = units - total * regime::CORE / 100.0;
        let expect = cash - bought + sold * 100.0 * (1.0 - FEE) + (units - sold) * 100.0;
        assert!(close_to(*r.value.last().unwrap(), expect));
    }

    #[test]
    fn a_gap_under_the_minimum_is_left_alone_but_counts_as_applied() {
        let panel = tiny([100.0, 100.0], [100.0, 100.0], [Some(true), Some(true)]);
        let schedule = BTreeMap::from([(day("2026-01-01"), 4.0)]);
        let r = simulate(
            &panel,
            &BTC,
            day("2026-01-01"),
            None,
            &Deposits::Schedule(schedule),
        );
        assert_eq!(r.trades, 0);
    }

    #[test]
    fn an_unknown_regime_is_never_traded() {
        let panel = tiny([100.0, 100.0], [100.0, 100.0], [None, None]);
        let r = simulate(
            &panel,
            &BTC,
            day("2026-01-01"),
            None,
            &Deposits::Weekly {
                initial: 250.0,
                weekly: 50.0,
            },
        );
        assert_eq!(r.trades, 0);
        assert!(close_to(
            *r.value.last().unwrap(),
            250.0 * (1.0 - CONVERSION)
        ));
    }

    #[test]
    fn the_unit_value_ignores_deposits() {
        // Flat prices and no trades: every deposit loses only its 0.2%.
        let panel = tiny([100.0, 100.0], [100.0, 100.0], [None, None]);
        let schedule = BTreeMap::from([(day("2026-01-01"), 100.0), (day("2026-01-02"), 100.0)]);
        let r = simulate(
            &panel,
            &BTC,
            day("2026-01-01"),
            None,
            &Deposits::Schedule(schedule),
        );
        assert!(close_to(r.nav[0], 1.0 - CONVERSION));
        assert!(close_to(
            *r.value.last().unwrap(),
            200.0 * (1.0 - CONVERSION)
        ));
        assert!(
            *r.nav.last().unwrap() < r.nav[0],
            "the second deposit's cost shows too"
        );
    }

    #[test]
    fn drawdown_is_measured_from_the_running_peak() {
        let r = Run {
            days: vec![],
            value: vec![],
            contributed: vec![],
            nav: vec![1.0, 1.2, 0.9, 1.3, 1.17],
            trades: 0,
            fees: 0.0,
        };
        assert!(close_to(r.max_drawdown(), 0.9 / 1.2 - 1.0));
    }

    #[test]
    fn the_actual_account_replays_its_own_start_and_deposits() {
        let (_, panel) = fixture();
        let mut state = paper::State::default_paper();
        let snap = |date: &str, total_usd: f64| paper::DailySnapshot {
            date: date.into(),
            total_usd,
            qty: BTreeMap::new(),
            mark: BTreeMap::new(),
            net_deposits_usd_to_date: 0.0,
        };
        state.history = vec![
            snap("2026-10-10", 300.0),
            snap("2026-10-16", 330.0),
            snap("2026-10-17", 380.0),
        ];
        state.flows = vec![paper::FlowRecord {
            refid: "L1".into(),
            ts: 1_792_195_200 + 3_600, // 2026-10-17 01:00 UTC
            kind: paper::FlowKind::Deposit,
            asset: "USD".into(),
            amount: 50.0,
            usd_value: 50.0,
        }];
        let line = actual(&panel, &state, forward_start()).unwrap();
        assert!(
            line.starts_with(
                "Actual account 2026-10-16 .. 2026-10-17: paid in $380.00, real $380.00"
            ),
            "{line}"
        );
    }
}
