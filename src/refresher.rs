use crate::cache::BalanceCache;
use crate::clob::BalanceFetcher;
use crate::domain::RefreshStatus;
use chrono::Utc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum Cmd {
    ForceRefresh,
}

/// One-shot fetch + cache write + status emit. Used both by the periodic loop
/// and by the synchronous startup pre-warm in main.
pub async fn do_fetch(
    fetcher: &dyn BalanceFetcher,
    cache: &dyn BalanceCache,
    status_tx: &mpsc::Sender<RefreshStatus>,
) {
    match fetcher.fetch().await {
        Ok(b) => {
            if let Err(e) = cache.set(&b).await {
                let _ = status_tx
                    .send(RefreshStatus::Failed { at: Utc::now(), error: format!("cache: {e}") })
                    .await;
                return;
            }
            let _ = status_tx
                .send(RefreshStatus::Ok { at: Utc::now() })
                .await;
        }
        Err(e) => {
            let _ = status_tx
                .send(RefreshStatus::Failed { at: Utc::now(), error: e.to_string() })
                .await;
        }
    }
}

/// Long-running refresh loop. Exits when `shutdown` is cancelled.
pub async fn run(
    fetcher: Arc<dyn BalanceFetcher>,
    cache: Arc<dyn BalanceCache>,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    status_tx: mpsc::Sender<RefreshStatus>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            Some(Cmd::ForceRefresh) = cmd_rx.recv() => {
                do_fetch(fetcher.as_ref(), cache.as_ref(), &status_tx).await;
            }
            _ = tokio::time::sleep(interval) => {
                do_fetch(fetcher.as_ref(), cache.as_ref(), &status_tx).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Balance, FetchError};
    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeFetcher {
        usdc: Mutex<Decimal>,
        fail: Mutex<bool>,
        calls: AtomicUsize,
    }
    impl FakeFetcher {
        fn ok(amount: &str) -> Arc<Self> {
            Arc::new(Self {
                usdc: Mutex::new(Decimal::from_str(amount).unwrap()),
                fail: Mutex::new(false),
                calls: AtomicUsize::new(0),
            })
        }
    }
    #[async_trait]
    impl BalanceFetcher for FakeFetcher {
        async fn fetch(&self) -> Result<Balance, FetchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if *self.fail.lock().unwrap() {
                return Err(FetchError::Network("x".into()));
            }
            Ok(Balance { usdc: *self.usdc.lock().unwrap(), fetched_at: Utc::now() })
        }
    }

    struct MemCache {
        last: Mutex<Option<Balance>>,
    }
    impl MemCache {
        fn new() -> Arc<Self> { Arc::new(Self { last: Mutex::new(None) }) }
        fn snapshot(&self) -> Option<Balance> { self.last.lock().unwrap().clone() }
    }
    #[async_trait]
    impl BalanceCache for MemCache {
        async fn get(&self) -> Result<Option<Balance>, crate::domain::CacheError> {
            Ok(self.last.lock().unwrap().clone())
        }
        async fn set(&self, b: &Balance) -> Result<(), crate::domain::CacheError> {
            *self.last.lock().unwrap() = Some(b.clone()); Ok(())
        }
        async fn ping(&self) -> Result<(), crate::domain::CacheError> { Ok(()) }
    }

    #[tokio::test]
    async fn do_fetch_writes_cache_and_emits_ok() {
        let f = FakeFetcher::ok("100");
        let c = MemCache::new();
        let (tx, mut rx) = mpsc::channel(8);
        do_fetch(f.as_ref(), c.as_ref(), &tx).await;

        let s = rx.recv().await.unwrap();
        assert!(matches!(s, RefreshStatus::Ok { .. }));
        assert_eq!(c.snapshot().unwrap().usdc, Decimal::from_str("100").unwrap());
    }

    #[tokio::test]
    async fn do_fetch_emits_failed_when_fetch_errors() {
        let f = FakeFetcher::ok("100");
        *f.fail.lock().unwrap() = true;
        let c = MemCache::new();
        let (tx, mut rx) = mpsc::channel(8);
        do_fetch(f.as_ref(), c.as_ref(), &tx).await;

        let s = rx.recv().await.unwrap();
        assert!(matches!(s, RefreshStatus::Failed { .. }));
        assert!(c.snapshot().is_none());
    }

    #[tokio::test]
    async fn force_refresh_command_triggers_fetch() {
        tokio::time::pause();

        let f = FakeFetcher::ok("50");
        let c = MemCache::new();
        let (status_tx, mut status_rx) = mpsc::channel(8);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let token = CancellationToken::new();

        let f_arc: Arc<dyn BalanceFetcher> = f.clone();
        let c_arc: Arc<dyn BalanceCache> = c.clone();
        let task = tokio::spawn(run(f_arc, c_arc, cmd_rx, status_tx,
                                    Duration::from_secs(60), token.clone()));

        cmd_tx.send(Cmd::ForceRefresh).await.unwrap();

        let s = tokio::time::timeout(Duration::from_secs(1), status_rx.recv()).await
            .expect("status emitted").expect("status some");
        assert!(matches!(s, RefreshStatus::Ok { .. }));

        token.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn shutdown_token_cancels_loop() {
        tokio::time::pause();
        let f = FakeFetcher::ok("1");
        let c = MemCache::new();
        let (status_tx, _status_rx) = mpsc::channel(8);
        let (_cmd_tx, cmd_rx) = mpsc::channel(8);
        let token = CancellationToken::new();

        let f_arc: Arc<dyn BalanceFetcher> = f.clone();
        let c_arc: Arc<dyn BalanceCache> = c.clone();
        let task = tokio::spawn(run(f_arc, c_arc, cmd_rx, status_tx,
                                    Duration::from_secs(60), token.clone()));

        token.cancel();
        tokio::time::timeout(Duration::from_secs(1), task).await
            .expect("task exits within 1s")
            .expect("no panic");
    }
}
