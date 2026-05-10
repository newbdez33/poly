use crate::domain::AppEvent;
use crate::positions::{PositionsCache, PositionsFetcher};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// One-shot fetch + cache write + event emit.
pub async fn do_fetch(
    fetcher: &dyn PositionsFetcher,
    cache: &dyn PositionsCache,
    event_tx: &mpsc::Sender<AppEvent>,
) {
    match fetcher.fetch().await {
        Ok(p) => {
            if let Err(e) = cache.set(&p).await {
                tracing::warn!("positions cache write failed: {e}");
                // Still emit so UI gets fresh data even if cache is broken.
            }
            let _ = event_tx.send(AppEvent::PositionsUpdate(p)).await;
        }
        Err(e) => {
            tracing::warn!("positions fetch failed: {e}");
            // Don't emit on failure — App keeps last known positions.
        }
    }
}

/// Long-running positions poll loop. First fetch happens immediately, then
/// every `interval`. Exits when `shutdown` is cancelled.
pub async fn run(
    fetcher: Arc<dyn PositionsFetcher>,
    cache: Arc<dyn PositionsCache>,
    event_tx: mpsc::Sender<AppEvent>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    // Immediate first fetch so the UI doesn't wait `interval` seconds on launch.
    do_fetch(fetcher.as_ref(), cache.as_ref(), &event_tx).await;

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(interval) => {
                do_fetch(fetcher.as_ref(), cache.as_ref(), &event_tx).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CacheError, FetchError};
    use crate::positions::{Position, Positions, Side};
    use async_trait::async_trait;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeFetcher {
        items: Mutex<Vec<Position>>,
        fail: Mutex<bool>,
        calls: AtomicUsize,
    }
    impl FakeFetcher {
        fn ok(items: Vec<Position>) -> Arc<Self> {
            Arc::new(Self { items: Mutex::new(items), fail: Mutex::new(false), calls: AtomicUsize::new(0) })
        }
        fn fail() -> Arc<Self> {
            Arc::new(Self { items: Mutex::new(vec![]), fail: Mutex::new(true), calls: AtomicUsize::new(0) })
        }
    }
    #[async_trait]
    impl PositionsFetcher for FakeFetcher {
        async fn fetch(&self) -> Result<Positions, FetchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if *self.fail.lock().unwrap() {
                return Err(FetchError::Network("x".into()));
            }
            Ok(Positions { items: self.items.lock().unwrap().clone(), fetched_at: Utc::now() })
        }
    }

    struct MemCache { last: Mutex<Option<Positions>> }
    impl MemCache {
        fn new() -> Arc<Self> { Arc::new(Self { last: Mutex::new(None) }) }
        fn snapshot(&self) -> Option<Positions> { self.last.lock().unwrap().clone() }
    }
    #[async_trait]
    impl PositionsCache for MemCache {
        async fn get(&self) -> Result<Option<Positions>, CacheError> {
            Ok(self.last.lock().unwrap().clone())
        }
        async fn set(&self, p: &Positions) -> Result<(), CacheError> {
            *self.last.lock().unwrap() = Some(p.clone()); Ok(())
        }
    }

    fn p(slug: &str) -> Position {
        Position {
            token_id: "1".into(),
            side: Side::Up,
            market_slug: slug.into(),
            shares: Decimal::from(10),
            avg_price: Decimal::from_str("0.50").unwrap(),
            current_price: Decimal::from_str("0.485").unwrap(),
        }
    }

    #[tokio::test]
    async fn do_fetch_writes_cache_and_emits_event() {
        let f = FakeFetcher::ok(vec![p("m1")]);
        let c = MemCache::new();
        let (tx, mut rx) = mpsc::channel::<AppEvent>(8);
        do_fetch(f.as_ref(), c.as_ref(), &tx).await;
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, AppEvent::PositionsUpdate(_)));
        assert_eq!(c.snapshot().unwrap().items.len(), 1);
    }

    #[tokio::test]
    async fn do_fetch_silent_on_fetch_error() {
        let f = FakeFetcher::fail();
        let c = MemCache::new();
        let (tx, mut rx) = mpsc::channel::<AppEvent>(8);
        do_fetch(f.as_ref(), c.as_ref(), &tx).await;
        // No event when fetch fails
        assert!(rx.try_recv().is_err());
        // Cache still empty
        assert!(c.snapshot().is_none());
    }

    #[tokio::test]
    async fn run_first_fetch_happens_immediately() {
        tokio::time::pause();
        let f = FakeFetcher::ok(vec![p("m1")]);
        let c = MemCache::new();
        let (tx, mut rx) = mpsc::channel::<AppEvent>(8);
        let shutdown = CancellationToken::new();

        let f_arc: Arc<dyn PositionsFetcher> = f.clone();
        let c_arc: Arc<dyn PositionsCache> = c.clone();
        let task = tokio::spawn(run(f_arc, c_arc, tx, Duration::from_secs(60), shutdown.clone()));

        // We expect an immediate fetch — should arrive without advancing time.
        let ev = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await
            .expect("event arrives without time advance").unwrap();
        assert!(matches!(ev, AppEvent::PositionsUpdate(_)));

        shutdown.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn shutdown_token_cancels_loop() {
        tokio::time::pause();
        let f = FakeFetcher::ok(vec![]);
        let c = MemCache::new();
        let (tx, _rx) = mpsc::channel::<AppEvent>(8);
        let shutdown = CancellationToken::new();

        let f_arc: Arc<dyn PositionsFetcher> = f.clone();
        let c_arc: Arc<dyn PositionsCache> = c.clone();
        let task = tokio::spawn(run(f_arc, c_arc, tx, Duration::from_secs(60), shutdown.clone()));

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), task).await
            .expect("task exits within 1s")
            .expect("no panic");
    }
}
