//! Paper `rustrade::ExchangeClient` — immediate fills, JSON journal.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use rustrade::{
    Capability, ExchangeClient, Order, Position, Result, Side, Symbol,
};

use crate::paper::{append_journal, load_state, save_state};

pub struct PaperKraken {
    name: String,
    positions: Mutex<HashMap<String, Position>>,
    last_px: Mutex<HashMap<String, f64>>,
    seq: AtomicU64,
}

impl PaperKraken {
    pub fn new() -> Self {
        Self {
            name: "kraken-paper".into(),
            positions: Mutex::new(HashMap::new()),
            last_px: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(1),
        }
    }

    pub fn set_mark(&self, symbol: &str, px: f64) {
        self.last_px.lock().unwrap().insert(symbol.to_string(), px);
    }
}

#[async_trait]
impl ExchangeClient for PaperKraken {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports(&self, c: Capability) -> bool {
        matches!(c, Capability::ReduceOnly)
    }

    async fn place_order(&self, order: &Order) -> Result<String> {
        let px = order
            .limit_price
            .map(|p| p.value())
            .or_else(|| self.last_px.lock().unwrap().get(order.symbol.as_str()).copied())
            .unwrap_or(0.0);
        if px <= 0.0 {
            return Err(rustrade::Error::Exchange("no fill price".into()));
        }
        let qty = order.size.value();
        let signed = match order.side {
            Side::Buy => qty,
            Side::Sell => -qty,
        };
        {
            let mut pos = self.positions.lock().unwrap();
            let cur = pos
                .entry(order.symbol.as_str().to_string())
                .or_insert(Position::FLAT);
            let new_qty = cur.qty + signed;
            if new_qty.abs() < 1e-12 {
                *cur = Position::FLAT;
            } else {
                cur.qty = new_qty;
                cur.entry_price = Some(px);
                cur.unrealised_pnl = 0.0;
            }
        }
        self.set_mark(order.symbol.as_str(), px);
        let id = format!("paper-{}", self.seq.fetch_add(1, Ordering::SeqCst));
        let _ = append_journal(&serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "kind": "paper_fill",
            "order_id": id,
            "symbol": order.symbol.as_str(),
            "side": format!("{:?}", order.side),
            "qty": qty,
            "px": px,
        }));
        // Mirror into the 3-book paper state file for `crypto-bot status`.
        if let Ok(mut st) = load_state() {
            st.mode = "paper".into();
            let _ = save_state(&st);
        }
        tracing::info!(
            id,
            symbol = order.symbol.as_str(),
            ?order.side,
            qty,
            px,
            "paper fill"
        );
        Ok(id)
    }

    async fn cancel_all(&self, _symbol: &Symbol) -> Result<usize> {
        Ok(0)
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

    async fn get_position(&self, symbol: &Symbol) -> Result<Position> {
        Ok(self
            .positions
            .lock()
            .unwrap()
            .get(symbol.as_str())
            .cloned()
            .unwrap_or(Position::FLAT))
    }

    async fn get_balance(&self, _currency: &str) -> Result<f64> {
        Ok(2_000.0)
    }
}
