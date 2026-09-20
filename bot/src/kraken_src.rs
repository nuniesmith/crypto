//! `rustrade::CandleSource` backed by `exchange-apiws` Kraken public REST.
#![allow(dead_code)]

use std::time::Duration;

use async_trait::async_trait;
use exchange_apiws::KrakenRestClient;
use rustrade::{Candle, CandleSource, Result, Symbol};

pub struct KrakenHourly {
    client: KrakenRestClient,
}

impl KrakenHourly {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: KrakenRestClient::new().map_err(|e| anyhow::anyhow!("{e}"))?,
        })
    }

    pub fn rest(&self) -> &KrakenRestClient {
        &self.client
    }
}

#[async_trait]
impl CandleSource for KrakenHourly {
    fn name(&self) -> &str {
        "kraken"
    }

    async fn poll(&self, symbol: &Symbol, interval: Duration, limit: usize) -> Result<Vec<Candle>> {
        let mins = (interval.as_secs() / 60).max(1) as u32;
        let ohlc = self
            .client
            .get_ohlc(symbol.as_str(), mins)
            .await
            .map_err(|e| rustrade::Error::Exchange(e.to_string()))?;
        let now = chrono::Utc::now().timestamp();
        let step = i64::from(mins) * 60;
        let mut out: Vec<Candle> = ohlc
            .candles
            .iter()
            .filter(|c| c.time + step <= now)
            .map(|c| Candle {
                time: c.time * 1000,
                open: c.open_f64(),
                high: c.high_f64(),
                low: c.low_f64(),
                close: c.close_f64(),
                volume: c.volume_f64(),
            })
            .collect();
        if out.len() > limit {
            let skip = out.len() - limit;
            out.drain(0..skip);
        }
        Ok(out)
    }
}
