#![cfg(test)]

use chrono::Utc;
use poly_tui::trader::adapters::redis_state_wrapper::RedisTraderState;
use poly_tui::trader::ladder::{Direction, LadderState};
use poly_tui::trader::state::TraderStateStore;
use rust_decimal::Decimal;
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "integration must NOT bind dev Redis port");
    let url = format!("redis://127.0.0.1:{port}");
    (node, url)
}

#[tokio::test]
#[ignore]
async fn save_load_clear_roundtrip() {
    let (_node, url) = start_redis().await;
    let store = RedisTraderState::connect(&url).await.unwrap();
    let s = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());

    assert!(store.load().await.unwrap().is_none());
    store.save(&s).await.unwrap();
    let back = store.load().await.unwrap().expect("Some");
    assert_eq!(back.session_id, s.session_id);

    store.clear().await.unwrap();
    assert!(store.load().await.unwrap().is_none());
}

#[tokio::test]
#[ignore]
async fn lock_acquire_then_contention() {
    let (_node, url) = start_redis().await;
    let store_a = RedisTraderState::connect(&url).await.unwrap();
    let store_b = RedisTraderState::connect(&url).await.unwrap();

    let acquired = store_a
        .try_lock("owner-a", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(acquired);

    let denied = store_b
        .try_lock("owner-b", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(!denied);
}

#[tokio::test]
#[ignore]
async fn lock_release_allows_reacquire() {
    let (_node, url) = start_redis().await;
    let store = RedisTraderState::connect(&url).await.unwrap();

    assert!(store
        .try_lock("a", Duration::from_secs(60))
        .await
        .unwrap());
    store.release_lock("a").await.unwrap();
    assert!(store
        .try_lock("b", Duration::from_secs(60))
        .await
        .unwrap());
}

#[tokio::test]
#[ignore]
async fn refresh_lock_succeeds_when_owner_matches() {
    let (_node, url) = start_redis().await;
    let store = RedisTraderState::connect(&url).await.unwrap();

    store
        .try_lock("a", Duration::from_secs(60))
        .await
        .unwrap();
    store
        .refresh_lock("a", Duration::from_secs(60))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn refresh_lock_fails_when_owner_mismatches() {
    let (_node, url) = start_redis().await;
    let store = RedisTraderState::connect(&url).await.unwrap();

    store
        .try_lock("a", Duration::from_secs(60))
        .await
        .unwrap();
    let r = store
        .refresh_lock("b", Duration::from_secs(60))
        .await;
    assert!(matches!(r, Err(_)));
}
