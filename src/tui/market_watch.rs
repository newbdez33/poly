use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MarketWatchError {
    #[error("Polygon RPC connection failed: {0}")]
    Connect(String),
    #[error("RPC call failed: {0}")]
    Rpc(String),
    #[error("response decode failed: {0}")]
    Decode(String),
}

#[async_trait]
pub trait BtcPriceFeed: Send + Sync {
    async fn latest_price(&self) -> Result<Decimal, MarketWatchError>;
}

/// Live state of the BTC market strip. Updated by the market_watch task,
/// emitted via AppEvent::MarketUpdate, rendered by ui::render_market_strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketState {
    pub window_ts: Option<i64>,
    pub price_to_beat: Option<Decimal>,
    pub current_price: Option<Decimal>,
    pub last_rpc_ok_at: Option<DateTime<Utc>>,
    pub last_gamma_ok_at: Option<DateTime<Utc>>,
}

impl MarketState {
    pub fn empty() -> Self {
        Self {
            window_ts: None,
            price_to_beat: None,
            current_price: None,
            last_rpc_ok_at: None,
            last_gamma_ok_at: None,
        }
    }
}
