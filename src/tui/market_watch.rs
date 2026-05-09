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

    /// current_price - price_to_beat. None if either is missing.
    pub fn diff(&self) -> Option<Decimal> {
        match (self.price_to_beat, self.current_price) {
            (Some(p), Some(c)) => Some(c - p),
            _ => None,
        }
    }

    /// True iff RPC has succeeded within the last 30 seconds.
    pub fn rpc_healthy(&self, now: DateTime<Utc>) -> bool {
        match self.last_rpc_ok_at {
            Some(t) => now.signed_duration_since(t).num_seconds() < 30,
            None => false,
        }
    }

    /// True iff gamma has succeeded within the last 6 minutes.
    pub fn gamma_healthy(&self, now: DateTime<Utc>) -> bool {
        match self.last_gamma_ok_at {
            Some(t) => now.signed_duration_since(t).num_seconds() < 6 * 60,
            None => false,
        }
    }

    /// Seconds remaining until the current 5-minute window closes.
    /// Returns 300 when exactly at a boundary, counting down to 1 one second before.
    pub fn seconds_to_next_boundary(&self, now_ts: i64) -> i64 {
        let r = now_ts.rem_euclid(300);
        if r == 0 { 300 } else { 300 - r }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::str::FromStr;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    fn state_with(price_to_beat: Option<&str>, current_price: Option<&str>) -> MarketState {
        MarketState {
            window_ts: Some(1700000000),
            price_to_beat: price_to_beat.map(|s| Decimal::from_str(s).unwrap()),
            current_price: current_price.map(|s| Decimal::from_str(s).unwrap()),
            last_rpc_ok_at: None,
            last_gamma_ok_at: None,
        }
    }

    #[test]
    fn diff_both_present_positive() {
        let s = state_with(Some("80000"), Some("80050"));
        assert_eq!(s.diff(), Some(Decimal::from(50)));
    }

    #[test]
    fn diff_both_present_negative() {
        let s = state_with(Some("80000"), Some("79950"));
        assert_eq!(s.diff(), Some(Decimal::from(-50)));
    }

    #[test]
    fn diff_both_present_zero() {
        let s = state_with(Some("80000"), Some("80000"));
        assert_eq!(s.diff(), Some(Decimal::ZERO));
    }

    #[test]
    fn diff_missing_to_beat_is_none() {
        let s = state_with(None, Some("80050"));
        assert_eq!(s.diff(), None);
    }

    #[test]
    fn diff_missing_current_is_none() {
        let s = state_with(Some("80000"), None);
        assert_eq!(s.diff(), None);
    }

    #[test]
    fn diff_both_missing_is_none() {
        let s = state_with(None, None);
        assert_eq!(s.diff(), None);
    }

    #[test]
    fn rpc_healthy_within_30s() {
        let mut s = MarketState::empty();
        s.last_rpc_ok_at = Some(ts(1000));
        assert!(s.rpc_healthy(ts(1015)));
        assert!(s.rpc_healthy(ts(1029)));
    }

    #[test]
    fn rpc_unhealthy_past_30s() {
        let mut s = MarketState::empty();
        s.last_rpc_ok_at = Some(ts(1000));
        assert!(!s.rpc_healthy(ts(1030)));
        assert!(!s.rpc_healthy(ts(1100)));
    }

    #[test]
    fn rpc_unhealthy_when_never_ok() {
        let s = MarketState::empty();
        assert!(!s.rpc_healthy(ts(1000)));
    }

    #[test]
    fn gamma_healthy_within_6_min() {
        let mut s = MarketState::empty();
        s.last_gamma_ok_at = Some(ts(1000));
        assert!(s.gamma_healthy(ts(1000 + 5 * 60)));
        assert!(!s.gamma_healthy(ts(1000 + 6 * 60)));
    }

    #[test]
    fn seconds_to_next_boundary_at_open() {
        // 1700000100 % 300 == 0: exactly at a window boundary → 300s remain
        let s = MarketState::empty();
        assert_eq!(s.seconds_to_next_boundary(1700000100), 300);
    }

    #[test]
    fn seconds_to_next_boundary_mid_window() {
        // 1700000200 % 300 == 100: 100s into window → 200s remain
        let s = MarketState::empty();
        assert_eq!(s.seconds_to_next_boundary(1700000200), 200);
    }

    #[test]
    fn seconds_to_next_boundary_at_close() {
        // 1700000400 % 300 == 0: next window boundary → 300s remain
        let s = MarketState::empty();
        assert_eq!(s.seconds_to_next_boundary(1700000400), 300);
    }

    #[test]
    fn seconds_to_next_boundary_one_before_close() {
        // 1700000399 % 300 == 299: one second before boundary → 1s remains
        let s = MarketState::empty();
        assert_eq!(s.seconds_to_next_boundary(1700000399), 1);
    }

    #[test]
    fn empty_state_has_no_data() {
        let s = MarketState::empty();
        assert!(s.window_ts.is_none());
        assert!(s.price_to_beat.is_none());
        assert!(s.current_price.is_none());
        assert!(s.last_rpc_ok_at.is_none());
        assert!(s.last_gamma_ok_at.is_none());
    }
}
