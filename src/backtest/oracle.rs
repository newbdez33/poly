use crate::backtest::data::binance::BinanceData;
use crate::backtest::data::gamma_history::WindowMeta;
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand_distr::{Distribution, Normal as NormalDist};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use statrs::distribution::{ContinuousCDF, Normal};
use std::sync::{Arc, Mutex};

pub trait TokenPriceOracle: Send + Sync {
    /// (bid, ask) for the UP token at `t_secs` seconds into the window.
    fn price_at(&self, window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal);
}

pub struct BlackScholesOracle {
    sigma_dollars: f64,    // BTC 5-min standard deviation in dollars
    friction: f64,         // half-spread (e.g., 0.0075 for 1.5% round-trip)
    btc: Arc<BinanceData>,
}

impl BlackScholesOracle {
    pub fn new(btc: Arc<BinanceData>, sigma_dollars: f64, friction: f64) -> Self {
        Self { sigma_dollars, friction: friction / 2.0, btc }
    }

    pub fn sigma(&self) -> f64 { self.sigma_dollars }
    pub fn friction(&self) -> f64 { self.friction * 2.0 }
}

/// Estimate σ (BTC 5-min stddev in dollars) from the BinanceData closes.
pub fn estimate_sigma(btc: &BinanceData) -> f64 {
    let closes = btc.closes();
    if closes.len() < 6 {
        return 80.0; // sensible default
    }
    // 5-min returns: every 5th candle's close, log return
    let mut log_returns = Vec::new();
    for w in closes.windows(6) {
        let r = (w[5] / w[0]).ln();
        log_returns.push(r);
    }
    let n = log_returns.len() as f64;
    let mean = log_returns.iter().sum::<f64>() / n;
    let variance = log_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n;
    let sigma_log = variance.sqrt();
    let avg_btc = closes.iter().sum::<f64>() / closes.len() as f64;
    sigma_log * avg_btc
}

impl TokenPriceOracle for BlackScholesOracle {
    fn price_at(&self, window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal) {
        let normal = Normal::new(0.0, 1.0).expect("standard normal");
        let t_window_open = window.window_ts;
        let t_now = t_window_open + t_secs as i64;
        let btc_now = match self.btc.price_at(t_now) {
            Some(p) => p,
            None => return (Decimal::from_f64(0.5).unwrap(), Decimal::from_f64(0.5).unwrap()),
        };
        let ptb_f64 = window.price_to_beat.to_string().parse::<f64>().unwrap_or(80000.0);
        let time_remaining = (300_i64 - t_secs as i64).max(1) as f64;
        let arg = (btc_now - ptb_f64) / (self.sigma_dollars * (time_remaining / 300.0).sqrt());
        let mid = normal.cdf(arg);
        let bid = (mid - self.friction).max(0.0).min(1.0);
        let ask = (mid + self.friction).max(0.0).min(1.0);
        (
            Decimal::from_f64(bid).unwrap_or(Decimal::ZERO),
            Decimal::from_f64(ask).unwrap_or(Decimal::ONE),
        )
    }
}

/// Wraps `BlackScholesOracle` and adds Gaussian white noise to bid/ask.
/// Same noise sample applies to both quotes (correlated). Clamped to
/// [0.01, 0.99] to keep prices physically valid.
///
/// Determinism: seeded `StdRng` produces a reproducible sequence for the
/// sequential calls of a backtest run.
pub struct NoisyBlackScholesOracle {
    base: BlackScholesOracle,
    sigma: f64,
    rng: Mutex<StdRng>,
    noise_dist: NormalDist<f64>,
}

impl NoisyBlackScholesOracle {
    pub fn new(base: BlackScholesOracle, sigma: f64, seed: u64) -> Self {
        let dist = NormalDist::new(0.0, sigma.max(0.0)).expect("valid normal dist");
        Self {
            base,
            sigma,
            rng: Mutex::new(StdRng::seed_from_u64(seed)),
            noise_dist: dist,
        }
    }
}

impl TokenPriceOracle for NoisyBlackScholesOracle {
    fn price_at(&self, window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal) {
        let (bid_bs, ask_bs) = self.base.price_at(window, t_secs);
        if self.sigma == 0.0 {
            return (bid_bs, ask_bs);
        }
        // Non-zero noise path implemented in Task 3.
        (bid_bs, ask_bs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::data::binance::BtcCandle;
    use crate::trader::ladder::Direction;
    use rust_decimal_macros::dec;

    fn make_window(price_to_beat: f64) -> WindowMeta {
        WindowMeta {
            window_ts: 1000,
            price_to_beat: Decimal::from_f64(price_to_beat).unwrap(),
            final_price: None,
            winner: Some(Direction::Up),
        }
    }

    fn make_btc_constant(price: f64) -> Arc<BinanceData> {
        // 6 candles spanning T=1000 → T=1300, all at constant `price`
        let mut candles = Vec::new();
        for i in 0..6 {
            candles.push(BtcCandle {
                open_ts: 1000 + i * 60,
                open: price, high: price, low: price, close: price,
            });
        }
        Arc::new(BinanceData::new(candles))
    }

    fn make_btc_rising(start: f64, end: f64) -> Arc<BinanceData> {
        let mut candles = Vec::new();
        for i in 0..6 {
            let p = start + (end - start) * (i as f64 / 5.0);
            candles.push(BtcCandle {
                open_ts: 1000 + i * 60,
                open: p, high: p, low: p, close: p,
            });
        }
        Arc::new(BinanceData::new(candles))
    }

    #[test]
    fn at_open_btc_equals_ptb_yields_half() {
        let btc = make_btc_constant(80000.0);
        let oracle = BlackScholesOracle::new(btc, 80.0, 0.0);
        let (bid, ask) = oracle.price_at(&make_window(80000.0), 0);
        let mid = (bid + ask) / Decimal::from(2);
        // At t=0 with BTC = priceToBeat, p ≈ 0.5
        assert!((mid - dec!(0.5)).abs() < dec!(0.01));
    }

    #[test]
    fn near_close_btc_high_yields_near_one() {
        let btc = make_btc_rising(80000.0, 80300.0);
        let oracle = BlackScholesOracle::new(btc, 80.0, 0.0);
        let (bid, _) = oracle.price_at(&make_window(80000.0), 290);
        // 290 sec in, BTC much higher, time nearly zero → p → 1
        assert!(bid >= dec!(0.95), "got bid={bid}");
    }

    #[test]
    fn near_close_btc_low_yields_near_zero() {
        let btc = make_btc_rising(80000.0, 79700.0);
        let oracle = BlackScholesOracle::new(btc, 80.0, 0.0);
        let (_, ask) = oracle.price_at(&make_window(80000.0), 290);
        assert!(ask <= dec!(0.05), "got ask={ask}");
    }

    #[test]
    fn friction_widens_spread() {
        let btc = make_btc_constant(80000.0);
        let oracle = BlackScholesOracle::new(btc, 80.0, 0.02);
        let (bid, ask) = oracle.price_at(&make_window(80000.0), 0);
        let spread = ask - bid;
        assert!(spread >= dec!(0.018), "got spread={spread}");
        assert!(spread <= dec!(0.022), "got spread={spread}");
    }

    #[test]
    fn estimate_sigma_returns_default_when_data_too_short() {
        let btc = BinanceData::new(vec![]);
        assert_eq!(estimate_sigma(&btc), 80.0);
    }

    #[test]
    fn estimate_sigma_increases_with_volatility() {
        // Build two synthetic series, one volatile, one calm
        let calm = (0..100).map(|i| BtcCandle {
            open_ts: i * 60, open: 80000.0, high: 80000.0, low: 80000.0, close: 80000.0,
        }).collect();
        let calm_data = BinanceData::new(calm);
        let calm_sigma = estimate_sigma(&calm_data);

        let volatile = (0..100).map(|i| {
            let noise = if i % 2 == 0 { 50.0 } else { -50.0 };
            BtcCandle { open_ts: i * 60, open: 80000.0 + noise, high: 80000.0, low: 80000.0, close: 80000.0 + noise }
        }).collect();
        let vol_data = BinanceData::new(volatile);
        let vol_sigma = estimate_sigma(&vol_data);

        assert!(vol_sigma > calm_sigma, "volatile σ ({}) should exceed calm σ ({})", vol_sigma, calm_sigma);
    }

    #[test]
    fn noisy_oracle_with_sigma_zero_matches_base() {
        let btc = make_btc_constant(80000.0);
        let base = BlackScholesOracle::new(btc.clone(), 80.0, 0.0);
        let base2 = BlackScholesOracle::new(btc, 80.0, 0.0);
        let noisy = NoisyBlackScholesOracle::new(base2, 0.0, 42);

        let window = make_window(80000.0);
        for t in [0u32, 60, 120, 180, 240, 290] {
            let (bid_b, ask_b) = base.price_at(&window, t);
            let (bid_n, ask_n) = noisy.price_at(&window, t);
            assert_eq!(bid_b, bid_n, "bid mismatch at t={t}");
            assert_eq!(ask_b, ask_n, "ask mismatch at t={t}");
        }
    }
}
