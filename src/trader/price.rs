use crate::trader::errors::PriceError;
use async_trait::async_trait;
use rust_decimal::Decimal;

#[async_trait]
pub trait MidwindowPriceFetcher: Send + Sync {
    /// Fetch the current bid for `token_id`. Returns `Err` on transient failure
    /// (network/decode); caller should log and retry on the next poll tick.
    async fn current_bid(&self, token_id: &str) -> Result<Decimal, PriceError>;
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::str::FromStr;
    use std::sync::Mutex;

    pub struct StubPriceFetcher {
        pub responses: Mutex<Vec<Result<Decimal, PriceError>>>,
    }
    impl StubPriceFetcher {
        pub fn new(responses: Vec<Result<Decimal, PriceError>>) -> Self {
            Self { responses: Mutex::new(responses) }
        }
    }
    #[async_trait]
    impl MidwindowPriceFetcher for StubPriceFetcher {
        async fn current_bid(&self, _token_id: &str) -> Result<Decimal, PriceError> {
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                Err(PriceError::Network("queue empty".into()))
            } else {
                q.remove(0)
            }
        }
    }

    #[tokio::test]
    async fn stub_dispenses_responses_in_order() {
        let f = StubPriceFetcher::new(vec![
            Ok(Decimal::from_str("0.50").unwrap()),
            Err(PriceError::Network("502".into())),
            Ok(Decimal::from_str("0.85").unwrap()),
        ]);
        assert_eq!(f.current_bid("tok").await.unwrap(), Decimal::from_str("0.50").unwrap());
        assert!(matches!(f.current_bid("tok").await, Err(PriceError::Network(_))));
        assert_eq!(f.current_bid("tok").await.unwrap(), Decimal::from_str("0.85").unwrap());
    }

    #[tokio::test]
    async fn stub_returns_err_when_drained() {
        let f = StubPriceFetcher::new(vec![]);
        assert!(matches!(f.current_bid("tok").await, Err(PriceError::Network(_))));
    }
}
