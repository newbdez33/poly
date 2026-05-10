use poly_tui::backtest::data::trades::{CachedTradeStore, Outcome, Trade, TradeSide};
use poly_tui::backtest::data::gamma_history::WindowMeta;
use poly_tui::backtest::oracle::{RealTradeOracle, TokenPriceOracle};
use poly_tui::trader::ladder::Direction;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tempfile::TempDir;

#[test]
fn real_oracle_pipeline_fixture_round_trip() {
    let tmp = TempDir::new().unwrap();
    let store = CachedTradeStore::new(tmp.path()).unwrap();

    let window_ts: i64 = 1778416800;
    // Hand-crafted realistic trade sequence:
    //   t=10s ask=0.52 (BUY)
    //   t=20s bid=0.48 (SELL)
    //   t=120s bid=0.78 (SELL) — would trigger TP=0.75 in strategy 12
    //   t=250s bid=0.65 (SELL)
    let trades = vec![
        Trade { timestamp: window_ts + 10,  side: TradeSide::Buy,  price: dec!(0.52), size: dec!(50), outcome: Outcome::Up },
        Trade { timestamp: window_ts + 20,  side: TradeSide::Sell, price: dec!(0.48), size: dec!(50), outcome: Outcome::Up },
        Trade { timestamp: window_ts + 120, side: TradeSide::Sell, price: dec!(0.78), size: dec!(50), outcome: Outcome::Up },
        Trade { timestamp: window_ts + 250, side: TradeSide::Sell, price: dec!(0.65), size: dec!(50), outcome: Outcome::Up },
    ];
    store.save(window_ts, &trades).unwrap();

    let loaded = store.load(window_ts).expect("cache hit");
    assert_eq!(loaded, trades);

    let mut by_window = HashMap::new();
    by_window.insert(window_ts, loaded);
    let oracle = RealTradeOracle::new(by_window);

    let window = WindowMeta {
        window_ts,
        price_to_beat: dec!(80000),
        final_price: Some(dec!(80050)),
        winner: Some(Direction::Up),
        condition_id: Some(
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into(),
        ),
    };

    // At t=5s → no trade yet → fallback 0.5 / 0.5
    assert_eq!(oracle.price_at(&window, 5), (dec!(0.5), dec!(0.5)));

    // At t=15s → ask=0.52 (last BUY), bid=0.5 (no SELL yet)
    assert_eq!(oracle.price_at(&window, 15), (dec!(0.5), dec!(0.52)));

    // At t=125s → ask=0.52, bid=0.78 (TP would have triggered at t=120s)
    assert_eq!(oracle.price_at(&window, 125), (dec!(0.78), dec!(0.52)));

    // At t=295s → still ask=0.52, bid=0.65 (most recent SELL ≤ 295)
    assert_eq!(oracle.price_at(&window, 295), (dec!(0.65), dec!(0.52)));
}

#[test]
#[ignore = "writes to temp dir, runs full simulator pipeline"]
fn real_oracle_strategy_12_triggers_tp() {
    use poly_tui::backtest::config::{ExitRule, StakeRule, StrategyConfig};
    use poly_tui::backtest::exit_rule::simulate_window;
    use poly_tui::trader::ladder::WindowOutcome;

    let window_ts: i64 = 1778416800;
    let trades = vec![
        Trade { timestamp: window_ts + 10, side: TradeSide::Buy,  price: dec!(0.52), size: dec!(50), outcome: Outcome::Up },
        Trade { timestamp: window_ts + 20, side: TradeSide::Sell, price: dec!(0.48), size: dec!(50), outcome: Outcome::Up },
        Trade { timestamp: window_ts + 100, side: TradeSide::Sell, price: dec!(0.78), size: dec!(50), outcome: Outcome::Up },
    ];
    let mut by_window = HashMap::new();
    by_window.insert(window_ts, trades);
    let oracle = RealTradeOracle::new(by_window);

    let window = WindowMeta {
        window_ts,
        price_to_beat: dec!(80000),
        final_price: Some(dec!(80050)),
        winner: Some(Direction::Down),  // would lose at resolution but TP triggers first
        condition_id: Some("0x00".into()),
    };

    let cfg = StrategyConfig {
        name: "12_tp75_early_exit_270".into(),
        direction: Direction::Up,
        band_min: dec!(0.45),
        band_max: dec!(0.55),
        stake: StakeRule::Martingale { base: dec!(5), max_step: 5 },
        exit: ExitRule::TpOnlyOrEarlyExit { tp_price: dec!(0.75), exit_at_secs: 270 },
    };

    let outcome = simulate_window(&window, &cfg, &oracle, dec!(5));
    assert!(matches!(outcome, WindowOutcome::Won { .. }),
            "strategy 12 should have hit TP at t=100s; got {outcome:?}");
}
