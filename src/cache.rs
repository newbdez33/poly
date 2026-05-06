use crate::domain::{Balance, CacheError};
use async_trait::async_trait;

/// Production Redis key for the latest balance. Namespaced so test data can never
/// collide with prod data even if someone connects the wrong client.
pub const BALANCE_KEY_PROD: &str = "poly:prod:balance:latest";

#[async_trait]
pub trait BalanceCache: Send + Sync {
    async fn get(&self) -> Result<Option<Balance>, CacheError>;
    async fn set(&self, balance: &Balance) -> Result<(), CacheError>;
    async fn ping(&self) -> Result<(), CacheError>;
}
