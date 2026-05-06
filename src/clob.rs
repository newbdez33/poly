use crate::domain::{Balance, FetchError};
use async_trait::async_trait;

#[async_trait]
pub trait BalanceFetcher: Send + Sync {
    async fn fetch(&self) -> Result<Balance, FetchError>;
}
