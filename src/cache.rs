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

use fred::interfaces::ClientLike;
use fred::prelude::{KeysInterface, RedisClient, RedisConfig};

pub struct RedisBalanceCache {
    client: RedisClient,
}

impl RedisBalanceCache {
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        let config = RedisConfig::from_url(url)
            .map_err(|e| CacheError::Op(format!("bad redis url: {e}")))?;
        let client = RedisClient::new(config, None, None, None);
        client
            .init()
            .await
            .map_err(|e| CacheError::Op(format!("redis init: {e}")))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl BalanceCache for RedisBalanceCache {
    async fn get(&self) -> Result<Option<Balance>, CacheError> {
        let raw: Option<String> = self
            .client
            .get(BALANCE_KEY_PROD)
            .await
            .map_err(map_err)?;
        match raw {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| CacheError::Decode(e.to_string())),
        }
    }

    async fn set(&self, balance: &Balance) -> Result<(), CacheError> {
        let json = serde_json::to_string(balance)
            .map_err(|e| CacheError::Decode(e.to_string()))?;
        self.client
            .set::<(), _, _>(BALANCE_KEY_PROD, json, None, None, false)
            .await
            .map_err(map_err)
    }

    async fn ping(&self) -> Result<(), CacheError> {
        let _: () = self.client.ping().await.map_err(map_err)?;
        Ok(())
    }
}

fn map_err(e: fred::error::RedisError) -> CacheError {
    use fred::error::RedisErrorKind;
    if matches!(e.kind(), RedisErrorKind::IO | RedisErrorKind::Canceled) {
        CacheError::Disconnected
    } else {
        CacheError::Op(e.to_string())
    }
}
