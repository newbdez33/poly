#![cfg(test)]

use chrono::Utc;
use poly_tui::adapters::redis_positions_wrapper::RedisPositionsCache;
use poly_tui::domain::{AppEvent, FetchError};
use poly_tui::positioner;
use poly_tui::positions::{Position, Positions, PositionsCache, PositionsFetcher, Side};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "tests must NOT bind dev Redis port");
    let url = format!("redis://127.0.0.1:{port}");
    (node, url)
}

struct StubFetcher {
    items: Mutex<Vec<Position>>,
}
impl StubFetcher {
    fn new(items: Vec<Position>) -> Arc<Self> {
        Arc::new(Self { items: Mutex::new(items) })
    }
}
#[async_trait::async_trait]
impl PositionsFetcher for StubFetcher {
    async fn fetch(&self) -> Result<Positions, FetchError> {
        Ok(Positions {
            items: self.items.lock().unwrap().clone(),
            fetched_at: Utc::now(),
        })
    }
}

fn pos(slug: &str) -> Position {
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
#[ignore]
async fn positioner_writes_redis_and_emits_event() {
    let (_node, url) = start_redis().await;
    let cache: Arc<dyn PositionsCache> = Arc::new(
        RedisPositionsCache::connect(&url).await.unwrap(),
    );
    let fetcher: Arc<dyn PositionsFetcher> = StubFetcher::new(vec![pos("btc-updown-5m-1")]);
    let (event_tx, mut event_rx) = mpsc::channel::<AppEvent>(8);
    let shutdown = CancellationToken::new();

    let task = tokio::spawn(positioner::run(
        fetcher,
        cache.clone(),
        event_tx,
        Duration::from_secs(60),
        shutdown.clone(),
    ));

    // Immediate first fetch should fire within 1s of spawn.
    let ev = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
        .await.expect("event arrives").expect("Some");
    let p = match ev {
        AppEvent::PositionsUpdate(p) => p,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].market_slug, "btc-updown-5m-1");

    // Cache should also contain it.
    let cached = cache.get().await.unwrap().expect("cached value");
    assert_eq!(cached.items.len(), 1);

    shutdown.cancel();
    let _ = task.await;
}
