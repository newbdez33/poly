use crate::trader::errors::ExecError;
use crate::trader::executor::{compute_share_count, FillResult, OrderExecutor, OrderId, OrderSide};
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-order metadata recorded by `place_limit` and consumed by
/// `SimulatedOrderEvents` to fill at the correct limit price.
#[derive(Clone, Debug)]
pub struct LimitOrderInfo {
    pub side: OrderSide,
    pub price: Decimal,
    pub shares: Decimal,
}

/// Dry-run executor: simulates fills without touching CLOB. Default fill price
/// $0.50 for buys, $0.99 for sells.
///
/// For maker-mode dry-run, `place_limit` records the order's `(side, price,
/// shares)` so a paired `SimulatedOrderEvents` can return Filled events at
/// the actual limit price (not a hardcoded value).
pub struct SimulatedExecutor {
    buy_price: Decimal,
    sell_price: Decimal,
    order_counter: AtomicU64,
    limits: Mutex<HashMap<OrderId, LimitOrderInfo>>,
}

impl Default for SimulatedExecutor {
    fn default() -> Self {
        Self {
            buy_price: Decimal::from_str("0.50").unwrap(),
            sell_price: Decimal::from_str("0.99").unwrap(),
            order_counter: AtomicU64::new(0),
            limits: Mutex::new(HashMap::new()),
        }
    }
}

impl SimulatedExecutor {
    pub fn new() -> Self { Self::default() }
    pub fn with_prices(buy: Decimal, sell: Decimal) -> Self {
        Self {
            buy_price: buy,
            sell_price: sell,
            order_counter: AtomicU64::new(0),
            limits: Mutex::new(HashMap::new()),
        }
    }

    /// Look up the limit order metadata recorded by a previous `place_limit`.
    /// Used by `SimulatedOrderEvents` to fill at the correct price.
    pub fn limit_order_info(&self, id: &OrderId) -> Option<LimitOrderInfo> {
        self.limits.lock().unwrap().get(id).cloned()
    }
}

#[async_trait]
impl OrderExecutor for SimulatedExecutor {
    async fn buy_fok(&self, _token: &str, dollars: Decimal) -> Result<FillResult, ExecError> {
        let shares = compute_share_count(dollars, self.buy_price);
        Ok(FillResult { fill_price: self.buy_price, shares, dollars })
    }
    async fn sell_market(&self, _token: &str, shares: Decimal) -> Result<FillResult, ExecError> {
        let dollars = self.sell_price * shares;
        Ok(FillResult { fill_price: self.sell_price, shares, dollars })
    }

    async fn sell_at_bid(
        &self,
        _token: &str,
        shares: Decimal,
        bid_hint: Decimal,
    ) -> Result<FillResult, ExecError> {
        // Apply a small slippage haircut so dry-run reflects real CLOB fill
        // realism: sell market orders cross the spread; assume 1% under the
        // observed bid. Floor at 0 so a trigger near zero doesn't go negative.
        let slip = Decimal::from_str("0.99").unwrap();
        let fill_price = (bid_hint * slip).max(Decimal::ZERO);
        let dollars = fill_price * shares;
        Ok(FillResult { fill_price, shares, dollars })
    }

    async fn place_limit(
        &self,
        _token_id: &str,
        side: OrderSide,
        price: Decimal,
        shares: Decimal,
    ) -> Result<OrderId, ExecError> {
        let n = self.order_counter.fetch_add(1, Ordering::SeqCst);
        let id = OrderId(format!("sim-order-{n}"));
        self.limits.lock().unwrap().insert(
            id.clone(),
            LimitOrderInfo { side, price, shares },
        );
        Ok(id)
    }

    async fn cancel(
        &self,
        order_id: &OrderId,
    ) -> Result<(), ExecError> {
        self.limits.lock().unwrap().remove(order_id);
        Ok(())
    }
}

/// Pair to `SimulatedExecutor` — fills any watched order at the limit price
/// recorded when `place_limit` was called. Used by dry-run mode to drive
/// `run_maker` through full state transitions with realistic fill prices.
pub struct SimulatedOrderEvents {
    executor: std::sync::Arc<SimulatedExecutor>,
}

impl SimulatedOrderEvents {
    pub fn new(executor: std::sync::Arc<SimulatedExecutor>) -> Self {
        Self { executor }
    }
}

#[async_trait]
impl crate::trader::order_events::OrderEventStream for SimulatedOrderEvents {
    async fn watch(
        &self,
        id: OrderId,
    ) -> Result<tokio::sync::mpsc::Receiver<crate::trader::order_events::OrderEvent>,
                crate::trader::errors::StreamError>
    {
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let info = self.executor.limit_order_info(&id);
        tokio::spawn(async move {
            // Yield once so the caller can subscribe before we fill.
            tokio::task::yield_now().await;
            match info {
                Some(info) => {
                    let _ = tx.send(crate::trader::order_events::OrderEvent::Filled {
                        id: id.clone(),
                        fill_price: info.price,
                        shares_filled: info.shares,
                        total_shares: info.shares,
                    }).await;
                }
                None => {
                    // Order ID not recognized — emit Cancelled so the watcher
                    // breaks out of its select! arm cleanly.
                    let _ = tx.send(crate::trader::order_events::OrderEvent::Cancelled {
                        id: id.clone(),
                    }).await;
                }
            }
        });
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn buy_returns_synthetic_fill() {
        let ex = SimulatedExecutor::default();
        let f = ex.buy_fok("any", Decimal::from(5)).await.unwrap();
        assert_eq!(f.shares, Decimal::from(10));
        assert_eq!(f.dollars, Decimal::from(5));
    }
    #[tokio::test]
    async fn sell_returns_synthetic_proceeds() {
        let ex = SimulatedExecutor::default();
        let f = ex.sell_market("any", Decimal::from(10)).await.unwrap();
        assert_eq!(f.dollars, Decimal::from_str("9.90").unwrap());
    }

    #[tokio::test]
    async fn sell_at_bid_uses_hint_with_slippage() {
        // SL trigger at bid=0.45, 10 shares → fill at 0.45 * 0.99 = 0.4455 → $4.455
        let ex = SimulatedExecutor::default();
        let f = ex.sell_at_bid("any", Decimal::from(10), Decimal::from_str("0.45").unwrap())
            .await.unwrap();
        assert_eq!(f.fill_price, Decimal::from_str("0.4455").unwrap());
        assert_eq!(f.dollars, Decimal::from_str("4.455").unwrap());
    }

    #[tokio::test]
    async fn sell_at_bid_with_tp_trigger_returns_winning_proceeds() {
        // TP trigger at bid=0.85, 10 shares → fill at 0.85 * 0.99 = 0.8415 → $8.415
        let ex = SimulatedExecutor::default();
        let f = ex.sell_at_bid("any", Decimal::from(10), Decimal::from_str("0.85").unwrap())
            .await.unwrap();
        assert!(f.dollars > Decimal::from(5), "TP should still beat $5 cost");
        assert_eq!(f.dollars, Decimal::from_str("8.415").unwrap());
    }

    #[tokio::test]
    async fn sell_at_bid_floors_at_zero() {
        let ex = SimulatedExecutor::default();
        let f = ex.sell_at_bid("any", Decimal::from(10), Decimal::ZERO).await.unwrap();
        assert_eq!(f.dollars, Decimal::ZERO);
    }

    use crate::trader::executor::{OrderId, OrderSide};

    #[tokio::test]
    async fn place_limit_returns_synthetic_order_id() {
        let ex = SimulatedExecutor::default();
        let id = ex.place_limit("tok-1", OrderSide::Buy,
            Decimal::from_str("0.49").unwrap(), Decimal::from(10)).await.unwrap();
        // Synthetic id is deterministic and non-empty.
        assert!(!id.0.is_empty());
    }

    #[tokio::test]
    async fn place_limit_returns_unique_ids_for_consecutive_calls() {
        let ex = SimulatedExecutor::default();
        let id1 = ex.place_limit("tok", OrderSide::Buy, Decimal::from_str("0.49").unwrap(), Decimal::from(10)).await.unwrap();
        let id2 = ex.place_limit("tok", OrderSide::Buy, Decimal::from_str("0.50").unwrap(), Decimal::from(10)).await.unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn cancel_succeeds_for_any_order_id() {
        let ex = SimulatedExecutor::default();
        let r = ex.cancel(&OrderId("anything".into())).await;
        assert!(r.is_ok());
    }
}
