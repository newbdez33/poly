use anyhow::Result;
use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide { Buy, Sell }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome { Up, Down }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    /// Unix seconds when the trade executed.
    pub timestamp: i64,
    pub side: TradeSide,
    pub price: Decimal,
    pub size: Decimal,
    pub outcome: Outcome,
}

#[async_trait]
pub trait TradeFetcher: Send + Sync {
    /// Fetch all trades for the given market (one Polymarket binary market =
    /// one 5-min window). Paginates internally. Returns sorted by timestamp
    /// ascending. `condition_id` is the hex-prefixed string from gamma.
    async fn fetch_window(
        &self,
        condition_id: &str,
        window_ts: i64,
    ) -> Result<Vec<Trade>>;
}

use polymarket_client_sdk_v2::data::Client as SdkClient;
use polymarket_client_sdk_v2::data::types::request::TradesRequest;
use polymarket_client_sdk_v2::data::types::{MarketFilter, Side};
use polymarket_client_sdk_v2::types::B256;
use std::str::FromStr;
use std::time::Duration;

const PAGE_LIMIT: i32 = 500;

pub struct PolymarketTradeFetcher {
    client: SdkClient,
    throttle: Duration,
}

impl PolymarketTradeFetcher {
    pub fn new(throttle_ms: u64) -> Self {
        Self {
            client: SdkClient::default(),
            throttle: Duration::from_millis(throttle_ms),
        }
    }
}

#[async_trait]
impl TradeFetcher for PolymarketTradeFetcher {
    async fn fetch_window(
        &self,
        condition_id: &str,
        window_ts: i64,
    ) -> Result<Vec<Trade>> {
        let cid = B256::from_str(condition_id)
            .map_err(|e| anyhow::anyhow!("invalid condition_id {}: {}", condition_id, e))?;
        let mut all = Vec::new();
        let mut offset: i32 = 0;

        loop {
            let req = TradesRequest::builder()
                .filter(MarketFilter::markets([cid]))
                .limit(PAGE_LIMIT)
                .map_err(|e| anyhow::anyhow!("limit out of range: {e}"))?
                .offset(offset)
                .map_err(|e| anyhow::anyhow!("offset out of range: {e}"))?
                .taker_only(false)
                .build();
            let page = self.client.trades(&req).await
                .map_err(|e| anyhow::anyhow!("data-api trades error: {e}"))?;
            let n = page.len();

            for sdk in page.into_iter() {
                let side = match sdk.side {
                    Side::Buy => TradeSide::Buy,
                    Side::Sell => TradeSide::Sell,
                    _ => continue,
                };
                let outcome = match sdk.outcome.to_ascii_lowercase().as_str() {
                    "up" => Outcome::Up,
                    "down" => Outcome::Down,
                    _ => continue,
                };
                all.push(Trade {
                    timestamp: sdk.timestamp,
                    side,
                    price: sdk.price,
                    size: sdk.size,
                    outcome,
                });
            }

            if n < PAGE_LIMIT as usize { break; }
            offset += PAGE_LIMIT;
            // Defensive: SDK enforces offset ≤ 10000; abort cleanly if a single
            // window has more than that (shouldn't happen for 5-min markets).
            if offset >= 10000 {
                eprintln!(
                    "[trades] WARNING window {} hit 10k offset cap; results truncated",
                    window_ts
                );
                break;
            }
            tokio::time::sleep(self.throttle).await;
        }

        all.sort_by_key(|t| t.timestamp);
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use std::sync::{Arc, Mutex};

    /// Mock that hands back canned page sequences. Each call to `fetch_window`
    /// drains pages from `pages` in order. The mock is one-shot per
    /// (condition_id, window_ts) combo; pages-drained semantics test pagination.
    struct MockFetcher {
        pages: Mutex<Vec<Vec<Trade>>>,
        calls: Arc<Mutex<u32>>,
    }
    impl MockFetcher {
        fn new(pages: Vec<Vec<Trade>>) -> (Self, Arc<Mutex<u32>>) {
            let calls = Arc::new(Mutex::new(0));
            (Self { pages: Mutex::new(pages), calls: calls.clone() }, calls)
        }
    }
    #[async_trait]
    impl TradeFetcher for MockFetcher {
        async fn fetch_window(&self, _cid: &str, _ts: i64) -> Result<Vec<Trade>> {
            *self.calls.lock().unwrap() += 1;
            let mut all = Vec::new();
            let mut pages = self.pages.lock().unwrap();
            while let Some(p) = pages.first() {
                let n = p.len();
                all.extend(pages.remove(0));
                if n < 500 { break; }
            }
            all.sort_by_key(|t: &Trade| t.timestamp);
            Ok(all)
        }
    }

    fn make_trade(ts: i64, side: TradeSide, price: rust_decimal::Decimal) -> Trade {
        Trade { timestamp: ts, side, price, size: rust_decimal_macros::dec!(10), outcome: Outcome::Up }
    }

    #[tokio::test]
    async fn mock_fetcher_concatenates_pages() {
        // Page 1 = 500 entries (full page), Page 2 = 12 entries (partial → stop).
        let p1: Vec<Trade> = (0..500)
            .map(|i| make_trade(1000 + i as i64, TradeSide::Buy, rust_decimal_macros::dec!(0.50)))
            .collect();
        let p2: Vec<Trade> = (0..12)
            .map(|i| make_trade(1500 + i as i64, TradeSide::Sell, rust_decimal_macros::dec!(0.51)))
            .collect();
        let (mock, calls) = MockFetcher::new(vec![p1, p2]);
        let out = mock.fetch_window("0xdeadbeef", 1000).await.unwrap();
        assert_eq!(out.len(), 512);
        assert!(out.windows(2).all(|w| w[0].timestamp <= w[1].timestamp), "sorted ascending");
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn trade_round_trips_through_json() {
        let t = Trade {
            timestamp: 1778416810,
            side: TradeSide::Buy,
            price: dec!(0.4823),
            size: dec!(100),
            outcome: Outcome::Up,
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: Trade = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn outcome_round_trips_through_json() {
        for o in [Outcome::Up, Outcome::Down] {
            let s = serde_json::to_string(&o).unwrap();
            let back: Outcome = serde_json::from_str(&s).unwrap();
            assert_eq!(o, back);
        }
    }

    #[test]
    fn trade_side_round_trips_through_json() {
        for side in [TradeSide::Buy, TradeSide::Sell] {
            let s = serde_json::to_string(&side).unwrap();
            let back: TradeSide = serde_json::from_str(&s).unwrap();
            assert_eq!(side, back);
        }
    }
}
