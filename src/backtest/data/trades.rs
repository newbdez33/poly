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
                .build();
            // Retry transient errors (timeouts, rate limits) up to 3 times
            // with exponential backoff. Permanent errors (400 bad request etc.)
            // still bubble immediately.
            let mut attempt = 0u32;
            let page = loop {
                match self.client.trades(&req).await {
                    Ok(p) => break p,
                    Err(e) if attempt < 3 => {
                        let msg = e.to_string();
                        // Retry timeouts and rate limits; bail on 400 / 404.
                        let transient = msg.contains("408") || msg.contains("429")
                            || msg.contains("500") || msg.contains("502")
                            || msg.contains("503") || msg.contains("504")
                            || msg.contains("timed out") || msg.contains("timeout");
                        if !transient {
                            return Err(anyhow::anyhow!("data-api trades error: {e}"));
                        }
                        attempt += 1;
                        let backoff = std::time::Duration::from_millis(500 * (1u64 << attempt));
                        eprintln!(
                            "[trades] window {} offset {} transient error (attempt {}): {} — retrying in {}ms",
                            window_ts, offset, attempt, msg, backoff.as_millis()
                        );
                        tokio::time::sleep(backoff).await;
                    }
                    Err(e) => return Err(anyhow::anyhow!("data-api trades error: {e}")),
                }
            };
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
            // Polymarket's data-api rejects offset > 3000 with "max historical
            // activity offset of 3000 exceeded". Break before we hit that cap
            // (5 pages × 500 = 2500 trades — well above typical 100-300 per
            // 5-min window with taker_only=true).
            if offset > 2500 {
                eprintln!(
                    "[trades] WARNING window {} has > 2500 trades; results truncated",
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

use std::path::{Path, PathBuf};

pub struct CachedTradeStore {
    root: PathBuf,
}

impl CachedTradeStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|e| anyhow::anyhow!("creating cache dir {}: {e}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, window_ts: i64) -> PathBuf {
        self.root.join(format!("{window_ts}.json"))
    }

    pub fn load(&self, window_ts: i64) -> Option<Vec<Trade>> {
        let path = self.path_for(window_ts);
        if !path.exists() { return None; }
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self, window_ts: i64, trades: &[Trade]) -> Result<()> {
        let path = self.path_for(window_ts);
        let bytes = serde_json::to_vec(trades)?;
        std::fs::write(&path, bytes)
            .map_err(|e| anyhow::anyhow!("writing trades cache {}: {e}", path.display()))?;
        Ok(())
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

    use tempfile::TempDir;

    fn fixture_trades() -> Vec<Trade> {
        vec![
            make_trade(1000, TradeSide::Buy, rust_decimal_macros::dec!(0.42)),
            make_trade(1001, TradeSide::Sell, rust_decimal_macros::dec!(0.43)),
        ]
    }

    #[test]
    fn cached_trade_store_save_then_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let store = CachedTradeStore::new(tmp.path()).unwrap();
        let trades = fixture_trades();
        store.save(1700000000, &trades).unwrap();
        let back = store.load(1700000000).unwrap();
        assert_eq!(back, trades);
    }

    #[test]
    fn cached_trade_store_load_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        let store = CachedTradeStore::new(tmp.path()).unwrap();
        assert!(store.load(99999).is_none());
    }

    #[test]
    fn cached_trade_store_save_creates_per_window_file() {
        let tmp = TempDir::new().unwrap();
        let store = CachedTradeStore::new(tmp.path()).unwrap();
        store.save(1700000000, &fixture_trades()).unwrap();
        assert!(tmp.path().join("1700000000.json").exists());
    }
}
