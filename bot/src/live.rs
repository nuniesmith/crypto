//! Live `rustrade::ExchangeClient` via `exchange-apiws` Kraken private REST.
//! Only constructed when the operator passes the confirm string.

use async_trait::async_trait;
use exchange_apiws::{KrakenCredentials, KrakenPrivateClient};
use rustrade::{Capability, ExchangeClient, Order, Position, Result, Side, Symbol};

pub struct LiveKraken {
    client: KrakenPrivateClient,
}

impl LiveKraken {
    pub fn from_env() -> anyhow::Result<Self> {
        let creds = KrakenCredentials::from_env()
            .map_err(|e| anyhow::anyhow!("Kraken credentials: {e}"))?;
        let client = KrakenPrivateClient::new(creds)
            .map_err(|e| anyhow::anyhow!("Kraken private client: {e}"))?;
        Ok(Self { client })
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
