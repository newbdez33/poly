use crate::trader::errors::ExecError;
use async_trait::async_trait;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillResult {
    pub fill_price: Decimal,
    pub shares: Decimal,
    pub dollars: Decimal,
}

#[async_trait]
pub trait OrderExecutor: Send + Sync {
    async fn buy_fok(&self, token_id: &str, dollars: Decimal) -> Result<FillResult, ExecError>;
    async fn sell_market(&self, token_id: &str, shares: Decimal) -> Result<FillResult, ExecError>;

    /// Sell `shares` of `token_id` with a hint of the bid we observed at trigger
    /// time. Real impls (CLOB) ignore the hint and fall through to `sell_market`.
    /// Dry-run impls use the hint as fill price so simulated PnL reflects the
    /// trigger context (e.g. an SL-trigger fill should price at ~SL bid, not $0.99).
    async fn sell_at_bid(
        &self,
        token_id: &str,
        shares: Decimal,
        _bid_hint: Decimal,
    ) -> Result<FillResult, ExecError> {
        self.sell_market(token_id, shares).await
    }
}

/// Number of whole shares to buy with `dollars` at `ask`. Rounds DOWN so we never
/// exceed the budget. Polymarket enforces a 5-share minimum — caller checks.
pub fn compute_share_count(dollars: Decimal, ask: Decimal) -> Decimal {
    if ask <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let raw = dollars / ask;
    raw.floor()
}

/// Polymarket's 5-share minimum order size.
pub const MIN_SHARES: u64 = 5;

pub fn meets_minimum(shares: Decimal) -> bool {
    shares.to_u64().map(|n| n >= MIN_SHARES).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn compute_shares_at_50_cents() {
        let s = compute_share_count(Decimal::from(5), Decimal::from_str("0.50").unwrap());
        assert_eq!(s, Decimal::from(10));
    }

    #[test]
    fn compute_shares_floors_partial() {
        let s = compute_share_count(Decimal::from(5), Decimal::from_str("0.51").unwrap());
        assert_eq!(s, Decimal::from(9));
    }

    #[test]
    fn compute_shares_zero_ask_returns_zero() {
        assert_eq!(compute_share_count(Decimal::from(5), Decimal::ZERO), Decimal::ZERO);
    }

    #[test]
    fn compute_shares_negative_ask_returns_zero() {
        assert_eq!(
            compute_share_count(Decimal::from(5), Decimal::from(-1)),
            Decimal::ZERO
        );
    }

    #[test]
    fn meets_minimum_at_5_shares() {
        assert!(meets_minimum(Decimal::from(5)));
    }

    #[test]
    fn meets_minimum_below_5_shares_is_false() {
        assert!(!meets_minimum(Decimal::from(4)));
    }

    #[test]
    fn meets_minimum_zero_shares_is_false() {
        assert!(!meets_minimum(Decimal::ZERO));
    }

    #[test]
    fn fill_result_serde_roundtrip() {
        let f = FillResult {
            fill_price: Decimal::from_str("0.50").unwrap(),
            shares: Decimal::from(10),
            dollars: Decimal::from(5),
        };
        let back: FillResult = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(f, back);
    }
}
