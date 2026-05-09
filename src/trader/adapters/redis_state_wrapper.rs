use crate::trader::errors::StateError;
use crate::trader::ladder::LadderState;
use crate::trader::state::{TraderStateStore, LADDER_KEY, LOCK_KEY};
use async_trait::async_trait;
use fred::interfaces::{ClientLike, KeysInterface};
use fred::prelude::{Expiration, RedisClient, RedisConfig, RedisError, SetOptions};
use std::time::Duration;

pub struct RedisTraderState {
    client: RedisClient,
}

impl RedisTraderState {
    pub async fn connect(url: &str) -> Result<Self, StateError> {
        let config = RedisConfig::from_url(url)
            .map_err(|e| StateError::Op(format!("bad redis url: {e}")))?;
        let client = RedisClient::new(config, None, None, None);
        client
            .init()
            .await
            .map_err(|e| StateError::Op(format!("redis init: {e}")))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl TraderStateStore for RedisTraderState {
    async fn load(&self) -> Result<Option<LadderState>, StateError> {
        let raw: Option<String> = self.client.get(LADDER_KEY).await.map_err(map_err)?;
        match raw {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| StateError::Decode(e.to_string())),
        }
    }

    async fn save(&self, state: &LadderState) -> Result<(), StateError> {
        let json =
            serde_json::to_string(state).map_err(|e| StateError::Decode(e.to_string()))?;
        self.client
            .set::<(), _, _>(LADDER_KEY, json, None, None, false)
            .await
            .map_err(map_err)
    }

    async fn clear(&self) -> Result<(), StateError> {
        let _: () = self.client.del(LADDER_KEY).await.map_err(map_err)?;
        Ok(())
    }

    async fn try_lock(&self, owner: &str, ttl: Duration) -> Result<bool, StateError> {
        // SET LOCK_KEY owner EX <ttl> NX
        // Redis returns OK (-> Some("OK")) on acquire, nil (-> None) when key exists.
        let result: Option<String> = self
            .client
            .set(
                LOCK_KEY,
                owner,
                Some(Expiration::EX(ttl.as_secs() as i64)),
                Some(SetOptions::NX),
                false,
            )
            .await
            .map_err(map_err)?;
        Ok(result.is_some())
    }

    async fn refresh_lock(&self, owner: &str, ttl: Duration) -> Result<(), StateError> {
        let current: Option<String> = self.client.get(LOCK_KEY).await.map_err(map_err)?;
        match current {
            Some(ref c) if c == owner => {
                self.client
                    .set::<(), _, _>(
                        LOCK_KEY,
                        owner,
                        Some(Expiration::EX(ttl.as_secs() as i64)),
                        Some(SetOptions::XX),
                        false,
                    )
                    .await
                    .map_err(map_err)
            }
            _ => Err(StateError::LockLost),
        }
    }

    async fn release_lock(&self, owner: &str) -> Result<(), StateError> {
        let current: Option<String> = self.client.get(LOCK_KEY).await.map_err(map_err)?;
        if matches!(current, Some(ref c) if c == owner) {
            let _: () = self.client.del(LOCK_KEY).await.map_err(map_err)?;
        }
        Ok(())
    }
}

fn map_err(e: RedisError) -> StateError {
    StateError::Op(e.to_string())
}
