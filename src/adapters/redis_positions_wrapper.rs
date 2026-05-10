use crate::domain::CacheError;
use crate::positions::{Positions, PositionsCache, POSITIONS_KEY};
use async_trait::async_trait;
use fred::interfaces::ClientLike;
use fred::prelude::{KeysInterface, RedisClient, RedisConfig};

pub struct RedisPositionsCache {
    client: RedisClient,
}

impl RedisPositionsCache {
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
impl PositionsCache for RedisPositionsCache {
    async fn get(&self) -> Result<Option<Positions>, CacheError> {
        let raw: Option<String> = self
            .client
            .get(POSITIONS_KEY)
            .await
            .map_err(map_err)?;
        match raw {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| CacheError::Decode(e.to_string())),
        }
    }

    async fn set(&self, p: &Positions) -> Result<(), CacheError> {
        let json = serde_json::to_string(p)
            .map_err(|e| CacheError::Decode(e.to_string()))?;
        self.client
            .set::<(), _, _>(POSITIONS_KEY, json, None, None, false)
            .await
            .map_err(map_err)
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

#[cfg(test)]
mod tests {
    use super::*;
    // Functional integration tests live in tests/positions_integration.rs
    // (need a real Redis container). This unit test just confirms the type
    // builds and the bad-URL path returns CacheError.
    #[tokio::test]
    async fn connect_rejects_invalid_url() {
        let r = RedisPositionsCache::connect("not a url").await;
        assert!(matches!(r, Err(CacheError::Op(_))));
    }
}
