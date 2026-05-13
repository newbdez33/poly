//! RSI-based direction gate for v1.11 strategy 33.
//!
//! Before each window:
//! 1. Fetch last `period+1` 1-min BTC closes from Binance (ending just before window open).
//! 2. Compute Wilder-smoothed RSI(period) on those closes.
//! 3. Decide:
//!    - RSI < oversold → bet UP (mean reversion: BTC sold off too much)
//!    - RSI > overbought → bet DOWN (BTC ran up too much)
//!    - Otherwise → skip window (no trade)
//!
//! Failure mode: if Binance is unreachable, skip the window (strict — never bet blind).

use crate::trader::ladder::Direction;
use anyhow::{Context, Result};
use async_trait::async_trait;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RsiDecision {
    /// RSI extreme → trade this direction.
    Trade { direction: Direction, rsi: f64 },
    /// RSI in neutral zone → skip.
    SkipNeutral { rsi: f64 },
    /// Binance fetch failed → skip (do not bet blindly).
    FetchFailed,
}

impl RsiDecision {
    pub fn rsi_decimal(&self) -> Option<Decimal> {
        match self {
            RsiDecision::Trade { rsi, .. } | RsiDecision::SkipNeutral { rsi } => {
                Decimal::from_f64(*rsi)
            }
            RsiDecision::FetchFailed => None,
        }
    }
}

/// Abstraction over the Binance fetcher for testability.
#[async_trait]
pub trait BinanceCandleFetcher: Send + Sync {
    /// Fetch the last `count` 1-min candles ending strictly before `window_open_ts`.
    /// Returns Vec sorted ascending by open_ts; close prices only.
    async fn fetch_closes(&self, window_open_ts: i64, count: usize) -> Result<Vec<f64>>;
}

pub struct RsiGate {
    pub period: usize,
    pub oversold: f64,
    pub overbought: f64,
    pub fetcher: Box<dyn BinanceCandleFetcher>,
}

impl RsiGate {
    pub fn new(
        period: usize,
        oversold: f64,
        overbought: f64,
        fetcher: Box<dyn BinanceCandleFetcher>,
    ) -> Self {
        Self { period, oversold, overbought, fetcher }
    }

    pub async fn decide(&self, window_open_ts: i64) -> RsiDecision {
        let needed = self.period + 1;
        let closes = match self.fetcher.fetch_closes(window_open_ts, needed).await {
            Ok(c) if c.len() >= needed => c,
            Ok(c) => {
                tracing::warn!(
                    "rsi-gate: insufficient candles ({} < {}) for window {}",
                    c.len(), needed, window_open_ts
                );
                return RsiDecision::FetchFailed;
            }
            Err(e) => {
                tracing::warn!("rsi-gate: Binance fetch failed for window {}: {}", window_open_ts, e);
                return RsiDecision::FetchFailed;
            }
        };
        let rsi = compute_rsi(&closes, self.period);
        if rsi < self.oversold {
            RsiDecision::Trade { direction: Direction::Up, rsi }
        } else if rsi > self.overbought {
            RsiDecision::Trade { direction: Direction::Down, rsi }
        } else {
            RsiDecision::SkipNeutral { rsi }
        }
    }
}

/// Wilder-smoothed RSI on the most recent `period+1` closes.
/// Matches `backtest::data::binance::BinanceData::rsi_at` so dry-run agrees with backtest.
pub fn compute_rsi(closes: &[f64], period: usize) -> f64 {
    debug_assert!(closes.len() >= period + 1);
    let slice = &closes[closes.len() - (period + 1)..];
    let mut gain_sum = 0.0;
    let mut loss_sum = 0.0;
    for i in 1..slice.len() {
        let diff = slice[i] - slice[i - 1];
        if diff >= 0.0 { gain_sum += diff; } else { loss_sum -= diff; }
    }
    let avg_gain = gain_sum / period as f64;
    let avg_loss = loss_sum / period as f64;
    if avg_loss == 0.0 {
        return if avg_gain == 0.0 { 50.0 } else { 100.0 };
    }
    let rs = avg_gain / avg_loss;
    100.0 - 100.0 / (1.0 + rs)
}

// ────────────────────────────────────────────────────────────────────────
// Live Binance HTTP fetcher
// ────────────────────────────────────────────────────────────────────────

pub struct LiveBinanceFetcher {
    client: reqwest::Client,
}

impl LiveBinanceFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest builds"),
        }
    }
}

impl Default for LiveBinanceFetcher {
    fn default() -> Self { Self::new() }
}

#[async_trait]
impl BinanceCandleFetcher for LiveBinanceFetcher {
    async fn fetch_closes(&self, window_open_ts: i64, count: usize) -> Result<Vec<f64>> {
        // Fetch candles with open_ts ∈ [window_open_ts - count*60, window_open_ts).
        // Binance endTime is exclusive of candles whose open is AT endTime, so
        // passing window_open_ts gives us the last `count` 1-min candles closed
        // strictly before window open.
        let start_ms = (window_open_ts - (count as i64 + 2) * 60) * 1000;
        let end_ms = window_open_ts * 1000;
        let url = format!(
            "https://api.binance.com/api/v3/klines\
             ?symbol=BTCUSDT&interval=1m&startTime={}&endTime={}&limit={}",
            start_ms, end_ms, count + 2
        );
        let resp = self.client.get(&url).send().await
            .context("Binance HTTP GET")?;
        if !resp.status().is_success() {
            anyhow::bail!("Binance returned HTTP {}", resp.status());
        }
        let raw: Vec<Vec<serde_json::Value>> = resp.json().await
            .context("Binance JSON decode")?;
        let mut closes: Vec<(i64, f64)> = raw.iter().filter_map(|row| {
            let open_ts = row.get(0)?.as_i64()? / 1000;
            let close = row.get(4)?.as_str()?.parse::<f64>().ok()?;
            // Exclude candles at or after window_open_ts (still open / forward-look).
            if open_ts < window_open_ts { Some((open_ts, close)) } else { None }
        }).collect();
        closes.sort_by_key(|(ts, _)| *ts);
        // Keep the last `count` candles.
        let result: Vec<f64> = closes.iter().rev().take(count).rev().map(|(_, c)| *c).collect();
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake fetcher that returns a hardcoded close sequence.
    struct StubFetcher {
        closes: Vec<f64>,
    }
    #[async_trait]
    impl BinanceCandleFetcher for StubFetcher {
        async fn fetch_closes(&self, _ts: i64, _count: usize) -> Result<Vec<f64>> {
            Ok(self.closes.clone())
        }
    }

    /// Fetcher that always errors.
    struct FailFetcher;
    #[async_trait]
    impl BinanceCandleFetcher for FailFetcher {
        async fn fetch_closes(&self, _ts: i64, _count: usize) -> Result<Vec<f64>> {
            anyhow::bail!("simulated fetch failure")
        }
    }

    fn rising_closes(n: usize) -> Vec<f64> {
        (0..n).map(|i| 80000.0 + i as f64 * 100.0).collect()
    }

    fn falling_closes(n: usize) -> Vec<f64> {
        (0..n).map(|i| 81500.0 - i as f64 * 100.0).collect()
    }

    fn flat_closes(n: usize) -> Vec<f64> {
        vec![80000.0; n]
    }

    #[test]
    fn rsi_all_gains_returns_100() {
        let r = compute_rsi(&rising_closes(15), 14);
        assert!((r - 100.0).abs() < 1e-6);
    }

    #[test]
    fn rsi_all_losses_returns_0() {
        let r = compute_rsi(&falling_closes(15), 14);
        assert!(r < 1e-6);
    }

    #[test]
    fn rsi_flat_returns_50() {
        let r = compute_rsi(&flat_closes(15), 14);
        assert!((r - 50.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn falling_closes_trigger_up_bet() {
        let gate = RsiGate::new(14, 30.0, 70.0, Box::new(StubFetcher { closes: falling_closes(15) }));
        let d = gate.decide(0).await;
        assert!(matches!(d, RsiDecision::Trade { direction: Direction::Up, .. }));
    }

    #[tokio::test]
    async fn rising_closes_trigger_down_bet() {
        let gate = RsiGate::new(14, 30.0, 70.0, Box::new(StubFetcher { closes: rising_closes(15) }));
        let d = gate.decide(0).await;
        assert!(matches!(d, RsiDecision::Trade { direction: Direction::Down, .. }));
    }

    #[tokio::test]
    async fn flat_closes_skip_neutral() {
        let gate = RsiGate::new(14, 30.0, 70.0, Box::new(StubFetcher { closes: flat_closes(15) }));
        let d = gate.decide(0).await;
        assert!(matches!(d, RsiDecision::SkipNeutral { .. }));
    }

    #[tokio::test]
    async fn fetch_failure_returns_fetch_failed() {
        let gate = RsiGate::new(14, 30.0, 70.0, Box::new(FailFetcher));
        let d = gate.decide(0).await;
        assert_eq!(d, RsiDecision::FetchFailed);
    }

    #[tokio::test]
    async fn insufficient_candles_returns_fetch_failed() {
        let gate = RsiGate::new(14, 30.0, 70.0, Box::new(StubFetcher { closes: vec![80000.0; 5] }));
        let d = gate.decide(0).await;
        assert_eq!(d, RsiDecision::FetchFailed);
    }
}
