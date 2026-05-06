use async_trait::async_trait;
use chrono::Utc;
use poly_tui::clob::BalanceFetcher;
use poly_tui::domain::{Balance, FetchError};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};

pub struct FakeFetcher {
    balance: Mutex<Decimal>,
    fail: Mutex<bool>,
    pub call_count: AtomicUsize,
}

impl FakeFetcher {
    pub fn with_usdc(amount: &str) -> Self {
        Self {
            balance: Mutex::new(Decimal::from_str(amount).unwrap()),
            fail: Mutex::new(false),
            call_count: AtomicUsize::new(0),
        }
    }

    pub fn set_balance(&self, amount: &str) {
        *self.balance.lock().unwrap() = Decimal::from_str(amount).unwrap();
    }

    pub fn set_fail(&self, fail: bool) {
        *self.fail.lock().unwrap() = fail;
    }

    pub fn calls(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BalanceFetcher for FakeFetcher {
    async fn fetch(&self) -> Result<Balance, FetchError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        if *self.fail.lock().unwrap() {
            return Err(FetchError::Network("forced fake fail".into()));
        }
        Ok(Balance {
            usdc: *self.balance.lock().unwrap(),
            fetched_at: Utc::now(),
        })
    }
}
