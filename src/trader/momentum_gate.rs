//! v1.16 IntraWindowMomentum live gate.
//!
//! Wraps a `WindowExecutor` and delays entry until BTC has moved
//! `[bp_min, bp_max]` basis points from the window-open price. Bets in
//! the SAME direction as the move (momentum). If no trigger by
//! `scan_end_secs`, skips the window.
//!
//! Mirrors the backtest implementation in `src/backtest/runner.rs`
//! (simulate_intra_window_momentum).

use crate::trader::ladder::{Direction, LadderState, SkipReason, WindowOutcome};
use crate::tui::market_watch::BtcPriceFeed;
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct MomentumGateConfig {
    pub scan_start_secs: u32,
    pub scan_end_secs: u32,
    pub bp_min: i32,
    pub bp_max: i32,
}

#[derive(Debug)]
pub enum MomentumDecision {
    /// Triggered; enter with this direction.
    Trade { direction: Direction, bp: f64, t_offset: u32 },
    /// Scan window expired with no trigger.
    Skip,
    /// BTC feed unavailable / stale.
    FetchFailed,
}

pub struct MomentumGate {
    pub cfg: MomentumGateConfig,
    pub btc: Arc<dyn BtcPriceFeed>,
}

impl MomentumGate {
    pub fn new(cfg: MomentumGateConfig, btc: Arc<dyn BtcPriceFeed>) -> Self {
        Self { cfg, btc }
    }

    /// Block-wait for the window's BTC price to deviate into the trigger
    /// range. Sleeps 1s between samples. Returns once a decision is reached.
    ///
    /// `window_start` is the wall-clock unix-seconds when the trading
    /// window opened; the gate waits until t = scan_start_secs to start
    /// sampling (so the caller can invoke this immediately at window
    /// open without busy-waiting).
    pub async fn decide(&self, window_start: i64) -> MomentumDecision {
        let now_secs = || chrono::Utc::now().timestamp();

        // First, capture the reference price at t=0 of the window. If we
        // were called late, we still sample as soon as possible.
        let start_price = match self.btc.latest_price().await {
            Ok(p) => p,
            Err(_) => return MomentumDecision::FetchFailed,
        };
        let start_f = match start_price.to_string().parse::<f64>() {
            Ok(v) if v > 0.0 => v,
            _ => return MomentumDecision::FetchFailed,
        };

        let scan_start_ts = window_start + self.cfg.scan_start_secs as i64;
        let scan_end_ts = window_start + self.cfg.scan_end_secs as i64;

        // Sleep until scan window opens.
        let delay_until_scan = scan_start_ts.saturating_sub(now_secs());
        if delay_until_scan > 0 {
            tokio::time::sleep(Duration::from_secs(delay_until_scan as u64)).await;
        }

        // 1Hz scan loop.
        while now_secs() < scan_end_ts {
            match self.btc.latest_price().await {
                Ok(p) => {
                    let cur = p.to_string().parse::<f64>().unwrap_or(0.0);
                    if cur > 0.0 {
                        let bp = ((cur - start_f) / start_f) * 10_000.0;
                        let abs_bp = bp.abs();
                        if abs_bp >= self.cfg.bp_min as f64
                            && abs_bp <= self.cfg.bp_max as f64
                        {
                            let direction = if bp > 0.0 { Direction::Up } else { Direction::Down };
                            let t_offset = (now_secs() - window_start).max(0) as u32;
                            return MomentumDecision::Trade { direction, bp, t_offset };
                        }
                    }
                }
                Err(_) => {
                    // Transient feed failure — keep trying.
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        MomentumDecision::Skip
    }
}

/// Map a MomentumDecision to a WindowOutcome::Skipped if needed.
pub fn skip_outcome(decision: &MomentumDecision) -> Option<WindowOutcome> {
    match decision {
        MomentumDecision::Skip => Some(WindowOutcome::Skipped {
            reason: SkipReason::PriceOutsideBand { ask: Decimal::ZERO },
        }),
        MomentumDecision::FetchFailed => Some(WindowOutcome::Skipped {
            reason: SkipReason::RsiFetchFailed,
        }),
        MomentumDecision::Trade { .. } => None,
    }
}

/// Helper: build the gate config from CLI args.
impl MomentumGateConfig {
    pub fn from_args(
        scan_start_secs: u32,
        scan_end_secs: u32,
        bp_min: i32,
        bp_max: i32,
    ) -> Self {
        Self { scan_start_secs, scan_end_secs, bp_min, bp_max }
    }
}

// Suppress an unused field warning when running tests without LadderState use.
#[allow(dead_code)]
const _: fn() = || {
    let _: Option<LadderState> = None;
};

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crate::tui::market_watch::MarketWatchError;
    use std::str::FromStr;
    use std::sync::Mutex;

    struct StubFeed(Mutex<Vec<f64>>);
    #[async_trait]
    impl BtcPriceFeed for StubFeed {
        async fn latest_price(&self) -> Result<Decimal, MarketWatchError> {
            let mut q = self.0.lock().unwrap();
            if q.is_empty() {
                return Err(MarketWatchError::Rpc("stub empty".into()));
            }
            let v = q.remove(0);
            Ok(Decimal::from_str(&format!("{:.4}", v)).unwrap())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn triggers_when_bp_in_range_up() {
        // Window opens at 1000. Start price 100000.0. At t=30 BTC = 100050 (+5 bp).
        let feed = Arc::new(StubFeed(Mutex::new(vec![
            100000.0,   // t=0 reference
            100050.0,   // first scan tick
        ])));
        let gate = MomentumGate::new(
            MomentumGateConfig { scan_start_secs: 0, scan_end_secs: 240, bp_min: 3, bp_max: 10 },
            feed,
        );
        let decision = gate.decide(chrono::Utc::now().timestamp()).await;
        match decision {
            MomentumDecision::Trade { direction, .. } => assert_eq!(direction, Direction::Up),
            other => panic!("expected Trade Up, got {:?}", other),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn triggers_down_when_bp_negative() {
        let feed = Arc::new(StubFeed(Mutex::new(vec![
            100000.0,
            99940.0,    // -6 bp
        ])));
        let gate = MomentumGate::new(
            MomentumGateConfig { scan_start_secs: 0, scan_end_secs: 240, bp_min: 3, bp_max: 10 },
            feed,
        );
        let decision = gate.decide(chrono::Utc::now().timestamp()).await;
        match decision {
            MomentumDecision::Trade { direction, .. } => assert_eq!(direction, Direction::Down),
            other => panic!("expected Trade Down, got {:?}", other),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn skips_when_bp_too_small() {
        // 100 ticks of barely-moving prices, all within ±1 bp.
        let mut prices = vec![100000.0];
        prices.extend(std::iter::repeat(100002.0).take(250));
        let feed = Arc::new(StubFeed(Mutex::new(prices)));
        let gate = MomentumGate::new(
            MomentumGateConfig { scan_start_secs: 0, scan_end_secs: 5, bp_min: 3, bp_max: 10 },
            feed,
        );
        let decision = gate.decide(chrono::Utc::now().timestamp()).await;
        assert!(matches!(decision, MomentumDecision::Skip));
    }
}
