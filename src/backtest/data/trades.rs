use anyhow::Result;
use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide { Buy, Sell }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome { Up, Down }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    /// Unix seconds when the trade executed.
    pub timestamp: i64,
    pub side: TradeSide,
    pub price: Decimal,
    pub size: Decimal,
    pub outcome: Outcome,
}

#[async_trait]
pub trait TradeFetcher: Send + Sync {
    /// Fetch all trades for the given market (one Polymarket binary market =
    /// one 5-min window). Paginates internally. Returns sorted by timestamp
    /// ascending. `condition_id` is the hex-prefixed string from gamma.
    async fn fetch_window(
        &self,
        condition_id: &str,
        window_ts: i64,
    ) -> Result<Vec<Trade>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn trade_round_trips_through_json() {
        let t = Trade {
            timestamp: 1778416810,
            side: TradeSide::Buy,
            price: dec!(0.4823),
            size: dec!(100),
            outcome: Outcome::Up,
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: Trade = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn outcome_round_trips_through_json() {
        for o in [Outcome::Up, Outcome::Down] {
            let s = serde_json::to_string(&o).unwrap();
            let back: Outcome = serde_json::from_str(&s).unwrap();
            assert_eq!(o, back);
        }
    }

    #[test]
    fn trade_side_round_trips_through_json() {
        for side in [TradeSide::Buy, TradeSide::Sell] {
            let s = serde_json::to_string(&side).unwrap();
            let back: TradeSide = serde_json::from_str(&s).unwrap();
            assert_eq!(side, back);
        }
    }
}
