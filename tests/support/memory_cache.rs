use async_trait::async_trait;
use poly_tui::cache::BalanceCache;
use poly_tui::domain::{Balance, CacheError};
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct InMemoryCache {
    state: Mutex<Option<Balance>>,
    pub fail_next_get: Mutex<bool>,
    pub fail_next_set: Mutex<bool>,
    pub fail_next_ping: Mutex<bool>,
}

impl InMemoryCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_balance(b: Balance) -> Self {
        let c = Self::default();
        *c.state.lock().unwrap() = Some(b);
        c
    }
}

#[async_trait]
impl BalanceCache for InMemoryCache {
    async fn get(&self) -> Result<Option<Balance>, CacheError> {
        let mut flag = self.fail_next_get.lock().unwrap();
        if *flag {
            *flag = false;
            return Err(CacheError::Disconnected);
        }
        Ok(self.state.lock().unwrap().clone())
    }

    async fn set(&self, balance: &Balance) -> Result<(), CacheError> {
        let mut flag = self.fail_next_set.lock().unwrap();
        if *flag {
            *flag = false;
            return Err(CacheError::Op("forced".into()));
        }
        *self.state.lock().unwrap() = Some(balance.clone());
        Ok(())
    }

    async fn ping(&self) -> Result<(), CacheError> {
        let mut flag = self.fail_next_ping.lock().unwrap();
        if *flag {
            *flag = false;
            return Err(CacheError::Disconnected);
        }
        Ok(())
    }
}
