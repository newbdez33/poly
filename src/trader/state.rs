use crate::trader::errors::StateError;
use crate::trader::ladder::LadderState;
use async_trait::async_trait;
use std::time::Duration;

#[async_trait]
pub trait TraderStateStore: Send + Sync {
    async fn load(&self) -> Result<Option<LadderState>, StateError>;
    async fn save(&self, state: &LadderState) -> Result<(), StateError>;
    async fn clear(&self) -> Result<(), StateError>;

    /// Try to acquire the singleton trader lock. Returns true if acquired.
    async fn try_lock(&self, owner: &str, ttl: Duration) -> Result<bool, StateError>;

    /// Refresh the lock TTL. Returns Err(LockLost) if the lock is no longer ours.
    async fn refresh_lock(&self, owner: &str, ttl: Duration) -> Result<(), StateError>;

    /// Best-effort release. Errors are logged but not fatal.
    async fn release_lock(&self, owner: &str) -> Result<(), StateError>;
}

/// Production Redis keys.
pub const LADDER_KEY: &str = "poly:prod:trader:ladder";
pub const LOCK_KEY: &str = "poly:prod:trader:lock";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_namespace_is_prod() {
        assert!(LADDER_KEY.starts_with("poly:prod:trader:"));
        assert!(LOCK_KEY.starts_with("poly:prod:trader:"));
    }
}
