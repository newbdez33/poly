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
    /// Trading window length in minutes. {5, 15, 60}. Receives updates via
    /// the mpsc channel from app::handle_event when a TraderEvent arrives.
    pub window_minutes: u32,
}

impl MarketState {
    pub fn empty() -> Self {
        Self {
            window_ts: None,
            price_to_beat: None,
            current_price: None,
            last_rpc_ok_at: None,
            last_gamma_ok_at: None,
            window_minutes: 5,
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

    /// Seconds remaining until the current `window_minutes`-minute window closes.
    /// Returns full window length when exactly at a boundary, counting down to 1
    /// one second before next boundary.
    pub fn seconds_to_next_boundary(&self, now_ts: i64, window_minutes: u32) -> i64 {
        let secs = window_minutes as i64 * 60;
        let r = now_ts.rem_euclid(secs);
        if r == 0 { secs } else { secs - r }
    }
}

use crate::domain::AppEvent;
use crate::trader::market::{floor_window, MarketDiscovery};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

async fn emit(tx: &mpsc::Sender<AppEvent>, state: &MarketState) {
    let _ = tx.send(AppEvent::MarketUpdate(state.clone())).await;
}

pub async fn run(
    price_feed: Arc<dyn BtcPriceFeed>,
    market: Arc<dyn MarketDiscovery>,
    event_tx: mpsc::Sender<AppEvent>,
    mut window_minutes_rx: mpsc::Receiver<u32>,
    shutdown: CancellationToken,
) {
    let mut state = MarketState::empty();
    let mut window_minutes: u32 = 5;
    let mut rpc_ticker = tokio::time::interval(Duration::from_secs(5));
    let mut gamma_ticker = tokio::time::interval(Duration::from_secs(15));

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,

            Some(new_mins) = window_minutes_rx.recv() => {
                if window_minutes != new_mins {
                    window_minutes = new_mins;
                    state.window_minutes = new_mins;
                    state.window_ts = None;  // force re-fetch on next gamma tick
                    emit(&event_tx, &state).await;
                }
            }

            _ = rpc_ticker.tick() => {
                if let Ok(p) = price_feed.latest_price().await {
                    state.current_price = Some(p);
                    state.last_rpc_ok_at = Some(chrono::Utc::now());
                    // Window-open snapshot: gamma's `priceToBeat` is only added
                    // after a window CLOSES, so for live windows we have to
                    // freeze the Chainlink price at the boundary as the strike.
                    // This matches Polymarket's actual resolution mechanic
                    // (BTC at open vs close).
                    let now_ts = chrono::Utc::now().timestamp();
                    let current_window = floor_window(now_ts, window_minutes);
                    if state.window_ts != Some(current_window) {
                        state.window_ts = Some(current_window);
                        state.price_to_beat = Some(p);
                    }
                }
                emit(&event_tx, &state).await;
            }

            _ = gamma_ticker.tick() => {
                let now_ts = chrono::Utc::now().timestamp();
                let current_window = floor_window(now_ts, window_minutes);
                // Best-effort: if gamma DOES have an official priceToBeat
                // (only true for windows that have already closed), prefer it
                // over our local snapshot.
                if let Ok(m) = market.find_window(current_window, window_minutes).await {
                    if let Some(p) = m.price_to_beat {
                        state.price_to_beat = Some(p);
                    }
                    state.last_gamma_ok_at = Some(chrono::Utc::now());
                    emit(&event_tx, &state).await;
                }
            }
        }
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
            window_minutes: 5,
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
        assert_eq!(s.seconds_to_next_boundary(1700000100, 5), 300);
    }

    #[test]
    fn seconds_to_next_boundary_mid_window() {
        // 1700000200 % 300 == 100: 100s into window → 200s remain
        let s = MarketState::empty();
        assert_eq!(s.seconds_to_next_boundary(1700000200, 5), 200);
    }

    #[test]
    fn seconds_to_next_boundary_at_close() {
        // 1700000400 % 300 == 0: next window boundary → 300s remain
        let s = MarketState::empty();
        assert_eq!(s.seconds_to_next_boundary(1700000400, 5), 300);
    }

    #[test]
    fn seconds_to_next_boundary_one_before_close() {
        // 1700000399 % 300 == 299: one second before boundary → 1s remains
        let s = MarketState::empty();
        assert_eq!(s.seconds_to_next_boundary(1700000399, 5), 1);
    }

    #[test]
    fn seconds_to_next_boundary_15m() {
        // 1700000600 % 900 = 500 → 400s remaining
        let s = state_with(None, None);
        assert_eq!(s.seconds_to_next_boundary(1700000600, 15), 400);
        // 1700000100 % 900 = 0 → exactly at boundary → full 900s remain
        assert_eq!(s.seconds_to_next_boundary(1700000100, 15), 900);
    }

    #[test]
    fn seconds_to_next_boundary_5m_unchanged() {
        let s = state_with(None, None);
        assert_eq!(s.seconds_to_next_boundary(1700000200, 5), 200);
    }

    #[tokio::test]
    async fn market_state_carries_window_minutes() {
        let s = MarketState {
            window_ts: Some(1700000000),
            price_to_beat: None,
            current_price: None,
            last_rpc_ok_at: None,
            last_gamma_ok_at: None,
            window_minutes: 15,
        };
        assert_eq!(s.window_minutes, 15);
    }

    #[test]
    fn empty_state_has_no_data() {
        let s = MarketState::empty();
        assert!(s.window_ts.is_none());
        assert!(s.price_to_beat.is_none());
        assert!(s.current_price.is_none());
        assert!(s.last_rpc_ok_at.is_none());
        assert!(s.last_gamma_ok_at.is_none());
        assert_eq!(s.window_minutes, 5);
    }

    use crate::trader::errors::MarketError;
    use crate::trader::market::{MarketDiscovery, WindowMarket};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use std::time::Duration;
    use crate::domain::AppEvent;

    struct FakePriceFeed {
        result: Mutex<Result<Decimal, MarketWatchError>>,
    }
    impl FakePriceFeed {
        fn ok(price: &str) -> Arc<Self> {
            Arc::new(Self {
                result: Mutex::new(Ok(Decimal::from_str(price).unwrap())),
            })
        }
        fn fail() -> Arc<Self> {
            Arc::new(Self {
                result: Mutex::new(Err(MarketWatchError::Rpc("forced".into()))),
            })
        }
    }
    #[async_trait]
    impl BtcPriceFeed for FakePriceFeed {
        async fn latest_price(&self) -> Result<Decimal, MarketWatchError> {
            match &*self.result.lock().unwrap() {
                Ok(p) => Ok(*p),
                Err(_) => Err(MarketWatchError::Rpc("forced".into())),
            }
        }
    }

    struct FakeMarket {
        responses: Mutex<Vec<Result<WindowMarket, MarketError>>>,
    }
    impl FakeMarket {
        fn with_price_to_beat(p: &str) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(vec![Ok(WindowMarket {
                    window_ts: 0,
                    slug: "test".into(),
                    up_token_id: "u".into(),
                    down_token_id: "d".into(),
                    up_ask: Decimal::ZERO,
                    down_ask: Decimal::ZERO,
                    closed: false,
                    winner: None,
                    price_to_beat: Some(Decimal::from_str(p).unwrap()),
                })]),
            })
        }
        fn always_fail() -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(vec![]),
            })
        }
    }
    #[async_trait]
    impl MarketDiscovery for FakeMarket {
        async fn find_window(&self, _ts: i64, _mins: u32)
            -> Result<WindowMarket, MarketError>
        {
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(MarketError::NotFound { window_ts: 0 });
            }
            q[0].clone()
        }
    }

    #[tokio::test]
    async fn run_emits_after_first_rpc_tick() {
        tokio::time::pause();
        let feed = FakePriceFeed::ok("80000");
        let market = FakeMarket::with_price_to_beat("80100");
        let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
        let (_mins_tx, mins_rx) = mpsc::channel::<u32>(8);
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(run(feed, market, tx, mins_rx, shutdown.clone()));
        // Yield once so the spawned task can register its intervals before we advance.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(6)).await;
        tokio::task::yield_now().await;

        let mut got_market = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::MarketUpdate(s) = ev {
                if s.current_price == Some(Decimal::from(80000)) {
                    got_market = true;
                    break;
                }
            }
        }
        assert!(got_market, "expected MarketUpdate with current_price 80000");

        shutdown.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn run_emits_price_to_beat_at_gamma_tick() {
        tokio::time::pause();
        let feed = FakePriceFeed::ok("80000");
        let market = FakeMarket::with_price_to_beat("80100");
        let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
        let (_mins_tx, mins_rx) = mpsc::channel::<u32>(8);
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(run(feed, market, tx, mins_rx, shutdown.clone()));
        // Yield once so the spawned task can register its intervals before we advance.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(20)).await;
        tokio::task::yield_now().await;

        let mut latest_state: Option<MarketState> = None;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::MarketUpdate(s) = ev {
                latest_state = Some(s);
            }
        }
        let s = latest_state.expect("at least one MarketUpdate");
        assert_eq!(s.price_to_beat, Some(Decimal::from(80100)));

        shutdown.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn snapshots_chainlink_at_window_open_when_gamma_lacks_price_to_beat() {
        // Gamma returns no priceToBeat (live un-resolved window) — the rpc tick
        // must snapshot the chainlink price as the local price-to-beat.
        tokio::time::pause();
        let feed = FakePriceFeed::ok("80000");
        let market = Arc::new(FakeMarket {
            responses: Mutex::new(vec![Ok(WindowMarket {
                window_ts: 0,
                slug: "test".into(),
                up_token_id: "u".into(),
                down_token_id: "d".into(),
                up_ask: Decimal::ZERO,
                down_ask: Decimal::ZERO,
                closed: false,
                winner: None,
                price_to_beat: None, // live window has no strike yet
            })]),
        });
        let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
        let (_mins_tx, mins_rx) = mpsc::channel::<u32>(8);
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(run(feed, market, tx, mins_rx, shutdown.clone()));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(6)).await;
        tokio::task::yield_now().await;

        let mut latest_state: Option<MarketState> = None;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::MarketUpdate(s) = ev {
                latest_state = Some(s);
            }
        }
        let s = latest_state.expect("at least one MarketUpdate");
        // Snapshot kicked in: price_to_beat == current_price after the boundary.
        assert_eq!(s.price_to_beat, Some(Decimal::from(80000)));
        assert_eq!(s.current_price, Some(Decimal::from(80000)));

        shutdown.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn run_keeps_emitting_when_rpc_fails() {
        tokio::time::pause();
        let feed = FakePriceFeed::fail();
        let market = FakeMarket::always_fail();
        let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
        let (_mins_tx, mins_rx) = mpsc::channel::<u32>(8);
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(run(feed, market, tx, mins_rx, shutdown.clone()));
        // Yield once so the spawned task can register its intervals before we advance.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(11)).await;
        tokio::task::yield_now().await;

        let mut count = 0;
        while let Ok(_) = rx.try_recv() { count += 1; }
        assert!(count > 0, "expected at least one MarketUpdate even on RPC failure");

        shutdown.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn run_exits_on_shutdown() {
        tokio::time::pause();
        let feed = FakePriceFeed::ok("80000");
        let market = FakeMarket::always_fail();
        let (tx, _rx) = mpsc::channel::<AppEvent>(64);
        let (_mins_tx, mins_rx) = mpsc::channel::<u32>(8);
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(run(feed, market, tx, mins_rx, shutdown.clone()));
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), task).await
            .expect("task exits within 1s")
            .expect("no panic");
    }
}
