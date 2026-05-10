use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;

use crate::positions::Positions;
use crate::trader::event::TraderEvent;
use crate::tui::market_watch::MarketState;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    pub usdc: Decimal,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefreshStatus {
    Ok { at: DateTime<Utc> },
    Failed { at: DateTime<Utc>, error: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthLed {
    Green,
    Yellow,
    Red,
}

impl HealthLed {
    /// Derive a CLOB health LED from the time since the last successful refresh.
    pub fn from_clob_age(last_status: Option<&RefreshStatus>, interval: Duration, now: DateTime<Utc>) -> HealthLed {
        match last_status {
            None => HealthLed::Red,
            Some(RefreshStatus::Failed { .. }) => HealthLed::Red,
            Some(RefreshStatus::Ok { at }) => {
                let age = now.signed_duration_since(*at).to_std().unwrap_or(Duration::ZERO);
                let i = interval.as_secs_f64();
                let a = age.as_secs_f64();
                if a < 1.5 * i {
                    HealthLed::Green
                } else if a < 3.0 * i {
                    HealthLed::Yellow
                } else {
                    HealthLed::Red
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum AppEvent {
    Tick,
    Key(crossterm::event::KeyEvent),
    Refresh(RefreshStatus),
    Shutdown,
    TraderEvent(TraderEvent),
    MarketUpdate(MarketState),
    PositionsUpdate(Positions),
}

#[derive(Error, Debug)]
pub enum FetchError {
    #[error("CLOB request failed: {0}")]
    Network(String),
    #[error("CLOB returned invalid data: {0}")]
    Decode(String),
    #[error("authentication failed")]
    Auth,
}

#[derive(Error, Debug)]
pub enum CacheError {
    #[error("redis connection lost")]
    Disconnected,
    #[error("redis op failed: {0}")]
    Op(String),
    #[error("cache value malformed: {0}")]
    Decode(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn balance_serde_roundtrip() {
        let b = Balance {
            usdc: Decimal::from_str("123.45").unwrap(),
            fetched_at: ts(1_700_000_000),
        };
        let json = serde_json::to_string(&b).unwrap();
        let back: Balance = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn health_red_when_no_status() {
        let led = HealthLed::from_clob_age(None, Duration::from_secs(30), ts(1000));
        assert_eq!(led, HealthLed::Red);
    }

    #[test]
    fn health_red_when_last_failed() {
        let s = RefreshStatus::Failed { at: ts(1000), error: "x".into() };
        let led = HealthLed::from_clob_age(Some(&s), Duration::from_secs(30), ts(1001));
        assert_eq!(led, HealthLed::Red);
    }

    #[test]
    fn health_green_within_1_5x() {
        let s = RefreshStatus::Ok { at: ts(1000) };
        let led = HealthLed::from_clob_age(Some(&s), Duration::from_secs(30), ts(1040));
        assert_eq!(led, HealthLed::Green);
    }

    #[test]
    fn health_yellow_between_1_5x_and_3x() {
        let s = RefreshStatus::Ok { at: ts(1000) };
        // 60s after at, interval 30s → 2x → yellow
        let led = HealthLed::from_clob_age(Some(&s), Duration::from_secs(30), ts(1060));
        assert_eq!(led, HealthLed::Yellow);
    }

    #[test]
    fn health_red_beyond_3x() {
        let s = RefreshStatus::Ok { at: ts(1000) };
        // 100s after, interval 30s → > 3x → red
        let led = HealthLed::from_clob_age(Some(&s), Duration::from_secs(30), ts(1100));
        assert_eq!(led, HealthLed::Red);
    }

    #[test]
    fn app_event_can_carry_positions_update() {
        // Compile-only sanity: the variant exists and accepts a Positions.
        use crate::positions::Positions;
        let p = Positions { items: vec![], fetched_at: ts(1_700_000_000) };
        let _ev = AppEvent::PositionsUpdate(p);
    }
}
