#![cfg(test)]

use chrono::NaiveDate;
use poly_tui::backtest::{
    config::strategy_set,
    data::loader::DataLoader,
    oracle::{estimate_sigma, BlackScholesOracle},
    report::{render_html, ReportMeta},
    runner::run_strategy,
    stats::compute_stats,
};
use std::sync::Arc;

/// End-to-end smoke test: runs poly-backtest's full pipeline on a 1-day window
/// against the real gamma-api and Binance APIs. Marked `#[ignore]` so it is
/// only run on demand:
///
/// ```bash
/// cargo test --test backtest_smoke -- --ignored
/// ```
///
/// Asserts:
/// - data load returns >0 resolved 5-min windows + a non-empty BTC series
/// - all 6 strategies run without panicking
/// - the rendered HTML report is non-trivial (>=5KB)
#[tokio::test]
#[ignore]
async fn end_to_end_one_day() {
    let tmp = tempfile::TempDir::new().unwrap();
    let loader = DataLoader::new(tmp.path().to_path_buf()).unwrap();
    let start = NaiveDate::from_ymd_opt(2026, 5, 8).unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 5, 9).unwrap();
    let loaded = loader
        .load(start, end)
        .await
        .expect("data load (requires gamma-api + Binance reachable)");
    assert!(!loaded.windows.is_empty(), "expected resolved windows");
    assert!(!loaded.btc.is_empty(), "expected BTC candles");

    let sigma = estimate_sigma(&loaded.btc);
    let btc = Arc::new(loaded.btc);
    let oracle = BlackScholesOracle::new(btc, sigma, 0.015);

    let strategies = strategy_set();
    let mut all_stats = Vec::new();
    for s in &strategies {
        let result = run_strategy(s, &loaded.windows, &oracle);
        all_stats.push(compute_stats(&result));
    }
    assert_eq!(all_stats.len(), 6);

    let meta = ReportMeta {
        start,
        end,
        total_windows: loaded.windows.len(),
        sigma,
        friction: 0.015,
        generated_at: chrono::Utc::now(),
    };
    let html = render_html(&all_stats, &meta);
    assert!(html.len() >= 5000, "html too small: {}", html.len());
}
