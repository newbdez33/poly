#![cfg(test)]

use poly_tui::cache::{BalanceCache, RedisBalanceCache, BALANCE_KEY_PROD};
use poly_tui::domain::Balance;
use rust_decimal::Decimal;
use std::str::FromStr;
use chrono::Utc;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "testcontainers must not bind dev port");
    let url = format!("redis://127.0.0.1:{port}");
    (node, url)
}

#[tokio::test]
#[ignore]
async fn redis_set_then_get_roundtrips() {
    let (_node, url) = start_redis().await;
    let cache = RedisBalanceCache::connect(&url).await.unwrap();

    let b = Balance {
        usdc: Decimal::from_str("123.45").unwrap(),
        fetched_at: Utc::now(),
    };
    cache.set(&b).await.unwrap();
    let got = cache.get().await.unwrap().expect("Some");
    assert_eq!(got.usdc, b.usdc);
}

#[tokio::test]
#[ignore]
async fn redis_get_returns_none_when_unset() {
    let (_node, url) = start_redis().await;
    let cache = RedisBalanceCache::connect(&url).await.unwrap();
    let got = cache.get().await.unwrap();
    assert!(got.is_none());
}

#[tokio::test]
#[ignore]
async fn redis_ping_succeeds() {
    let (_node, url) = start_redis().await;
    let cache = RedisBalanceCache::connect(&url).await.unwrap();
    cache.ping().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn redis_uses_prod_namespace_key() {
    let (_node, url) = start_redis().await;
    let _cache = RedisBalanceCache::connect(&url).await.unwrap();
    assert!(BALANCE_KEY_PROD.starts_with("poly:prod:"));
}
