use crate::trader::errors::{MarketError, ResolveError};
use crate::trader::ladder::Direction;
use crate::trader::market::{MarketDiscovery, WindowMarket};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub winner: Direction,
}

#[async_trait]
pub trait WindowResolver: Send + Sync {
    async fn await_resolution(&self, market: &WindowMarket) -> Result<Resolution, ResolveError>;
}

/// Production resolver: polls MarketDiscovery every `tick` until `closed=true`
/// or `timeout` elapses.
pub struct PolymarketResolver {
    market: Arc<dyn MarketDiscovery>,
    tick: Duration,
    timeout: Duration,
}

impl PolymarketResolver {
    pub fn new(market: Arc<dyn MarketDiscovery>, timeout: Duration) -> Self {
        Self { market, tick: Duration::from_secs(2), timeout }
    }
    /// Smaller tick for tests under `tokio::time::pause()`.
    pub fn with_tick(market: Arc<dyn MarketDiscovery>, tick: Duration, timeout: Duration) -> Self {
        Self { market, tick, timeout }
    }
}

#[async_trait]
impl WindowResolver for PolymarketResolver {
    async fn await_resolution(&self, market: &WindowMarket) -> Result<Resolution, ResolveError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            match self.market.find_window(market.window_ts).await {
                Ok(latest) if latest.closed => {
                    let winner = latest.winner.ok_or_else(|| {
                        ResolveError::Market(MarketError::Decode(
                            "closed but no winner".into(),
                        ))
                    })?;
                    return Ok(Resolution { winner });
                }
                Ok(_) | Err(MarketError::NotFound { .. }) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(ResolveError::Timeout {
                            seconds: self.timeout.as_secs(),
                        });
                    }
                    tokio::time::sleep(self.tick).await;
                }
                Err(e) => return Err(ResolveError::Market(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trader::market::WindowMarket;
    use rust_decimal::Decimal;
    use std::sync::Mutex;

    /// Fake that returns a queue of pre-programmed responses.
    struct ScriptedDiscovery {
        responses: Mutex<Vec<Result<WindowMarket, MarketError>>>,
    }
    impl ScriptedDiscovery {
        fn new(rs: Vec<Result<WindowMarket, MarketError>>) -> Arc<Self> {
            Arc::new(Self { responses: Mutex::new(rs) })
        }
    }
    #[async_trait]
    impl MarketDiscovery for ScriptedDiscovery {
        async fn find_window(&self, _ts: i64) -> Result<WindowMarket, MarketError> {
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(MarketError::NotFound { window_ts: 0 });
            }
            q.remove(0)
        }
    }

    fn open_market() -> WindowMarket {
        WindowMarket {
            window_ts: 1700000300, slug: "x".into(),
            up_token_id: "u".into(), down_token_id: "d".into(),
            up_ask: Decimal::ONE_HUNDRED, down_ask: Decimal::ONE_HUNDRED,
            closed: false, winner: None,
        }
    }
    fn closed_market(winner: Direction) -> WindowMarket {
        WindowMarket {
            window_ts: 1700000300, slug: "x".into(),
            up_token_id: "u".into(), down_token_id: "d".into(),
            up_ask: Decimal::ONE_HUNDRED, down_ask: Decimal::ONE_HUNDRED,
            closed: true, winner: Some(winner),
        }
    }

    #[tokio::test]
    async fn resolves_immediately_when_already_closed() {
        let disc = ScriptedDiscovery::new(vec![Ok(closed_market(Direction::Up))]);
        let resolver = PolymarketResolver::with_tick(
            disc, Duration::from_millis(1), Duration::from_secs(60));
        let r = resolver.await_resolution(&open_market()).await.unwrap();
        assert_eq!(r.winner, Direction::Up);
    }

    #[tokio::test]
    async fn polls_until_closed() {
        tokio::time::pause();
        let disc = ScriptedDiscovery::new(vec![
            Ok(open_market()),
            Ok(open_market()),
            Ok(closed_market(Direction::Down)),
        ]);
        let resolver = PolymarketResolver::with_tick(
            disc, Duration::from_secs(2), Duration::from_secs(60));
        let task = tokio::spawn(async move {
            resolver.await_resolution(&open_market()).await
        });
        tokio::time::advance(Duration::from_secs(5)).await;
        let r = task.await.unwrap().unwrap();
        assert_eq!(r.winner, Direction::Down);
    }

    #[tokio::test]
    async fn times_out_when_never_closes() {
        tokio::time::pause();
        let many_open: Vec<_> = std::iter::repeat_with(|| Ok(open_market())).take(40).collect();
        let disc = ScriptedDiscovery::new(many_open);
        let resolver = PolymarketResolver::with_tick(
            disc, Duration::from_secs(2), Duration::from_secs(60));
        let task = tokio::spawn(async move {
            resolver.await_resolution(&open_market()).await
        });
        tokio::time::advance(Duration::from_secs(61)).await;
        let r = task.await.unwrap();
        assert!(matches!(r, Err(ResolveError::Timeout { seconds: 60 })));
    }

    #[tokio::test]
    async fn not_found_during_poll_is_retried() {
        tokio::time::pause();
        let disc = ScriptedDiscovery::new(vec![
            Err(MarketError::NotFound { window_ts: 0 }),
            Ok(closed_market(Direction::Up)),
        ]);
        let resolver = PolymarketResolver::with_tick(
            disc, Duration::from_secs(2), Duration::from_secs(60));
        let task = tokio::spawn(async move {
            resolver.await_resolution(&open_market()).await
        });
        tokio::time::advance(Duration::from_secs(3)).await;
        let r = task.await.unwrap().unwrap();
        assert_eq!(r.winner, Direction::Up);
    }

    #[tokio::test]
    async fn network_error_returns_market_err() {
        let disc = ScriptedDiscovery::new(vec![Err(MarketError::Network("boom".into()))]);
        let resolver = PolymarketResolver::with_tick(
            disc, Duration::from_millis(1), Duration::from_secs(60));
        let r = resolver.await_resolution(&open_market()).await;
        assert!(matches!(r, Err(ResolveError::Market(MarketError::Network(_)))));
    }
}
