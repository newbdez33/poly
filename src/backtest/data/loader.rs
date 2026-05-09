use crate::backtest::data::binance::{BinanceData, BinanceFetcher};
use crate::backtest::data::cache::DiskCache;
use crate::backtest::data::gamma_history::{GammaHistoryFetcher, WindowMeta};
use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};

pub struct LoadedData {
    pub windows: Vec<WindowMeta>,
    pub btc: BinanceData,
}

pub struct DataLoader {
    pub gamma: GammaHistoryFetcher,
    pub binance: BinanceFetcher,
}

impl DataLoader {
    pub fn new(cache_root: std::path::PathBuf) -> Result<Self> {
        let gamma_cache = DiskCache::new(cache_root.join("gamma"))?;
        let binance_cache = DiskCache::new(cache_root.join("binance"))?;
        Ok(Self {
            gamma: GammaHistoryFetcher::new(
                "https://gamma-api.polymarket.com".to_string(),
                gamma_cache,
            ),
            binance: BinanceFetcher::new(binance_cache),
        })
    }

    /// Loads all 5-min windows in [start_date, end_date) plus the BTC 1min series.
    pub async fn load(&self, start: NaiveDate, end: NaiveDate) -> Result<LoadedData> {
        let btc = self.binance.fetch_range(start, end).await?;

        let start_ts = NaiveDateTime::new(start, NaiveTime::MIN).and_utc().timestamp();
        let end_ts = NaiveDateTime::new(end, NaiveTime::MIN).and_utc().timestamp();
        // Round start_ts up to next 5-min boundary
        let mut ts = if start_ts % 300 == 0 { start_ts } else { start_ts + (300 - start_ts % 300) };

        let mut windows = Vec::new();
        let mut total = 0;
        while ts < end_ts {
            total += 1;
            if total % 100 == 0 {
                eprintln!("gamma: {} / {} windows fetched", total, (end_ts - start_ts) / 300);
            }
            match self.gamma.fetch(ts).await {
                Ok(Some(meta)) => windows.push(meta),
                Ok(None) => {} // skip unsettled / nonexistent
                Err(e) => eprintln!("gamma fetch error at ts={ts}: {e}; skipping"),
            }
            ts += 300;
        }
        eprintln!("loaded {} resolved windows out of {} attempted", windows.len(), total);

        Ok(LoadedData { windows, btc })
    }
}

#[cfg(test)]
mod tests {
    // No unit tests for the integration loader — covered by smoke test in tests/backtest_smoke.rs.
    // The components (gamma_history, binance) are individually unit-tested above.
    #[test]
    fn loader_smoke_compiles() {}
}
