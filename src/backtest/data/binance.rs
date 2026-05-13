use crate::backtest::data::cache::DiskCache;
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// One BTC 1-minute candle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BtcCandle {
    pub open_ts: i64,    // candle open epoch seconds
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

/// Loaded BTC 1-min series for the requested period.
pub struct BinanceData {
    candles: Vec<BtcCandle>, // sorted by open_ts ascending
}

impl BinanceData {
    pub fn new(mut candles: Vec<BtcCandle>) -> Self {
        candles.sort_by_key(|c| c.open_ts);
        Self { candles }
    }

    /// Linear interpolation: BTC price at arbitrary epoch second `t`.
    /// Uses surrounding 1-min candles' close prices.
    pub fn price_at(&self, t_secs: i64) -> Option<f64> {
        if self.candles.is_empty() {
            return None;
        }
        // Find the candle whose open_ts <= t_secs < open_ts + 60
        let idx = match self.candles.binary_search_by_key(&t_secs, |c| c.open_ts) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let candle = &self.candles[idx];
        // Linear interp within the candle: open at start, close at end
        let elapsed = (t_secs - candle.open_ts).clamp(0, 60) as f64;
        let frac = elapsed / 60.0;
        Some(candle.open + (candle.close - candle.open) * frac)
    }

    /// All candle close prices, used for σ estimation.
    pub fn closes(&self) -> Vec<f64> {
        self.candles.iter().map(|c| c.close).collect()
    }

    pub fn is_empty(&self) -> bool { self.candles.is_empty() }
    pub fn len(&self) -> usize { self.candles.len() }

    /// Wilder-style RSI(period) computed on the `n` most-recent closes whose
    /// open_ts < `ts`. Returns None if fewer than `period + 1` such candles
    /// exist. Returns 0..100.
    ///
    /// For BTC 5min markets, querying RSI at the window's `open_ts` uses the
    /// preceding 14 minutes of 1-min closes — capturing the latest momentum
    /// without leaking forward-look data.
    pub fn rsi_at(&self, ts: i64, period: usize) -> Option<f64> {
        let needed = period + 1;
        // Find index of first candle with open_ts >= ts (strictly before that is allowed).
        let end_idx = match self.candles.binary_search_by_key(&ts, |c| c.open_ts) {
            Ok(i) => i,    // candle AT ts: exclude (window-open candle hasn't closed)
            Err(i) => i,   // first candle after ts: same exclusion
        };
        if end_idx < needed { return None; }
        let start = end_idx - needed;
        let slice = &self.candles[start..end_idx];

        // Wilder's smoothing: classic RSI.
        let mut gain_sum = 0.0;
        let mut loss_sum = 0.0;
        for i in 1..slice.len() {
            let diff = slice[i].close - slice[i - 1].close;
            if diff >= 0.0 { gain_sum += diff; } else { loss_sum -= diff; }
        }
        let avg_gain = gain_sum / period as f64;
        let avg_loss = loss_sum / period as f64;
        if avg_loss == 0.0 {
            return Some(if avg_gain == 0.0 { 50.0 } else { 100.0 });
        }
        let rs = avg_gain / avg_loss;
        Some(100.0 - 100.0 / (1.0 + rs))
    }
}

pub struct BinanceFetcher {
    client: reqwest::Client,
    cache: DiskCache,
}

impl BinanceFetcher {
    pub fn new(cache: DiskCache) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("reqwest builds"),
            cache,
        }
    }

    /// Fetch 1-minute BTC candles for [start, end), one cache file per UTC day.
    pub async fn fetch_range(&self, start: NaiveDate, end: NaiveDate) -> Result<BinanceData> {
        let mut all = Vec::new();
        let mut day = start;
        while day < end {
            let day_candles = self.fetch_day(day).await
                .with_context(|| format!("fetching {day}"))?;
            all.extend(day_candles);
            day = day.succ_opt().expect("date succ");
        }
        Ok(BinanceData::new(all))
    }

    async fn fetch_day(&self, day: NaiveDate) -> Result<Vec<BtcCandle>> {
        let key = day.format("%Y-%m-%d").to_string();
        if self.cache.exists(&key) {
            return self.cache.read::<Vec<BtcCandle>>(&key);
        }
        let start = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let end = day.succ_opt().unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc();
        let candles = self.fetch_klines(start, end).await?;
        self.cache.write(&key, &candles)?;
        Ok(candles)
    }

    async fn fetch_klines(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<BtcCandle>> {
        let mut out = Vec::new();
        let mut cursor = start;
        while cursor < end {
            let url = format!(
                "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&startTime={}&endTime={}&limit=1000",
                cursor.timestamp_millis(), end.timestamp_millis()
            );
            let resp = self.client.get(&url).send().await?;
            if !resp.status().is_success() {
                anyhow::bail!("binance HTTP {}", resp.status());
            }
            let raw: Vec<Vec<serde_json::Value>> = resp.json().await?;
            if raw.is_empty() { break; }
            for row in &raw {
                let open_ts = row[0].as_i64().unwrap_or(0) / 1000;
                let open = row[1].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                let high = row[2].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                let low  = row[3].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                let close = row[4].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                out.push(BtcCandle { open_ts, open, high, low, close });
            }
            let last_ts = out.last().map(|c| c.open_ts).unwrap_or(0);
            cursor = DateTime::from_timestamp(last_ts + 60, 0).unwrap_or(end);
            if raw.len() < 1000 { break; }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(open_ts: i64, open: f64, close: f64) -> BtcCandle {
        BtcCandle { open_ts, open, high: open.max(close), low: open.min(close), close }
    }

    #[test]
    fn price_at_exact_open_returns_open() {
        let d = BinanceData::new(vec![c(1000, 80000.0, 80100.0)]);
        let p = d.price_at(1000).unwrap();
        assert!((p - 80000.0).abs() < 1e-6);
    }

    #[test]
    fn price_at_mid_candle_interpolates() {
        let d = BinanceData::new(vec![c(1000, 80000.0, 80100.0)]);
        let p = d.price_at(1030).unwrap();
        // 30 sec into a 60 sec candle: (30/60) * (80100 - 80000) + 80000 = 80050
        assert!((p - 80050.0).abs() < 0.01);
    }

    #[test]
    fn price_at_after_last_candle_returns_close() {
        let d = BinanceData::new(vec![c(1000, 80000.0, 80100.0)]);
        let p = d.price_at(2000).unwrap();
        // far past last candle: clamps to close
        assert!((p - 80100.0).abs() < 0.01);
    }

    #[test]
    fn price_at_empty_data_returns_none() {
        let d = BinanceData::new(vec![]);
        assert!(d.price_at(1000).is_none());
    }

    #[test]
    fn closes_returns_close_prices() {
        let d = BinanceData::new(vec![c(1000, 80000.0, 80100.0), c(1060, 80100.0, 80200.0)]);
        assert_eq!(d.closes(), vec![80100.0, 80200.0]);
    }

    #[test]
    fn candles_sorted_by_open_ts() {
        let d = BinanceData::new(vec![c(1060, 1.0, 2.0), c(1000, 3.0, 4.0)]);
        // Verify sorting: index 0 should be 1000, index 1 should be 1060
        let p_at_1000 = d.price_at(1000).unwrap();
        assert!((p_at_1000 - 3.0).abs() < 0.01);
    }
}
