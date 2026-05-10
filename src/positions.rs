use crate::domain::{CacheError, FetchError};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Production Redis key for the latest positions snapshot.
pub const POSITIONS_KEY: &str = "poly:prod:positions";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side { Up, Down }

impl Side {
    /// Parse from Polymarket's outcome string. Returns None if the outcome is
    /// not a binary BTC up/down market (e.g. presidential markets, etc.) so the
    /// caller can filter those out.
    pub fn parse(s: &str) -> Option<Side> {
        match s.to_ascii_lowercase().as_str() {
            "up" => Some(Side::Up),
            "down" => Some(Side::Down),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub token_id: String,
    pub side: Side,
    pub market_slug: String,
    pub shares: Decimal,
    pub avg_price: Decimal,     // USDC paid per share
    pub current_price: Decimal, // current bid per data-api
}

impl Position {
    pub fn cost_usd(&self) -> Decimal { self.avg_price * self.shares }
    pub fn value_usd(&self) -> Decimal { self.current_price * self.shares }
    /// Percent gain/loss vs cost. Returns 0 if cost is zero (avoid div-by-zero).
    pub fn pnl_pct(&self) -> Decimal {
        let cost = self.cost_usd();
        if cost.is_zero() {
            return Decimal::ZERO;
        }
        (self.value_usd() - cost) / cost * Decimal::from(100)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Positions {
    pub items: Vec<Position>,
    pub fetched_at: DateTime<Utc>,
}

#[async_trait]
pub trait PositionsFetcher: Send + Sync {
    async fn fetch(&self) -> Result<Positions, FetchError>;
}

#[async_trait]
pub trait PositionsCache: Send + Sync {
    async fn get(&self) -> Result<Option<Positions>, CacheError>;
    async fn set(&self, p: &Positions) -> Result<(), CacheError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn pos(avg: &str, cur: &str, shares: &str) -> Position {
        Position {
            token_id: "t".into(),
            side: Side::Up,
            market_slug: "btc-updown-5m-1".into(),
            shares: Decimal::from_str(shares).unwrap(),
            avg_price: Decimal::from_str(avg).unwrap(),
            current_price: Decimal::from_str(cur).unwrap(),
        }
    }

    #[test]
    fn cost_usd_multiplies_avg_by_shares() {
        let p = pos("0.50", "0.485", "10");
        assert_eq!(p.cost_usd(), Decimal::from_str("5.00").unwrap());
    }

    #[test]
    fn value_usd_multiplies_current_by_shares() {
        let p = pos("0.50", "0.485", "10");
        assert_eq!(p.value_usd(), Decimal::from_str("4.85").unwrap());
    }

    #[test]
    fn pnl_pct_negative_when_value_below_cost() {
        let p = pos("0.50", "0.485", "10");
        // (4.85 - 5.00) / 5.00 * 100 = -3.0
        assert_eq!(p.pnl_pct(), Decimal::from_str("-3.0").unwrap());
    }

    #[test]
    fn pnl_pct_positive_when_value_above_cost() {
        let p = pos("0.50", "0.85", "10");
        // (8.50 - 5.00) / 5.00 * 100 = 70.0
        assert_eq!(p.pnl_pct(), Decimal::from_str("70.0").unwrap());
    }

    #[test]
    fn pnl_pct_zero_when_cost_is_zero() {
        let p = pos("0", "0.50", "10");
        assert_eq!(p.pnl_pct(), Decimal::ZERO);
    }

    #[test]
    fn side_parses_case_insensitive() {
        assert_eq!(Side::parse("Up"), Some(Side::Up));
        assert_eq!(Side::parse("up"), Some(Side::Up));
        assert_eq!(Side::parse("DOWN"), Some(Side::Down));
        assert_eq!(Side::parse("Yes"), None);
    }

    #[test]
    fn position_serde_roundtrip() {
        let p = pos("0.50", "0.485", "10");
        let json = serde_json::to_string(&p).unwrap();
        let back: Position = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn positions_serde_roundtrip() {
        let p = Positions {
            items: vec![pos("0.50", "0.485", "10")],
            fetched_at: Utc::now(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Positions = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn positions_key_namespaces_prod() {
        assert!(POSITIONS_KEY.starts_with("poly:prod:"));
    }
}
