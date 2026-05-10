use crate::trader::errors::ExecError;
use crate::trader::executor::{compute_share_count, FillResult, OrderExecutor};
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

/// Dry-run executor: simulates fills without touching CLOB. Default fill price
/// $0.50 for buys, $0.99 for sells.
pub struct SimulatedExecutor {
    buy_price: Decimal,
    sell_price: Decimal,
    order_counter: AtomicU64,
}

impl Default for SimulatedExecutor {
    fn default() -> Self {
        Self {
            buy_price: Decimal::from_str("0.50").unwrap(),
            sell_price: Decimal::from_str("0.99").unwrap(),
            order_counter: AtomicU64::new(0),
        }
    }
}

impl SimulatedExecutor {
    pub fn new() -> Self { Self::default() }
    pub fn with_prices(buy: Decimal, sell: Decimal) -> Self {
        Self { buy_price: buy, sell_price: sell, order_counter: AtomicU64::new(0) }
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
        _side: crate::trader::executor::OrderSide,
        _price: Decimal,
        _shares: Decimal,
    ) -> Result<crate::trader::executor::OrderId, ExecError> {
        let n = self.order_counter.fetch_add(1, Ordering::SeqCst);
        Ok(crate::trader::executor::OrderId(format!("sim-order-{n}")))
    }

    async fn cancel(
        &self,
        _order_id: &crate::trader::executor::OrderId,
    ) -> Result<(), ExecError> {
        Ok(())
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
