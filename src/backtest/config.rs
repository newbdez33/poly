use crate::trader::ladder::Direction;
use chrono::NaiveDate;
use clap::{Parser, ValueEnum};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum OracleKind {
    Bs,
    Noisy,
    Real,
}

#[derive(Parser, Debug, Clone)]
#[command(name = "poly-backtest", about = "Backtest strategies on Polymarket BTC 5min history")]
pub struct BacktestArgs {
    /// Start date (UTC, inclusive) — e.g. 2026-04-09
    #[arg(long)]
    pub start: NaiveDate,

    /// End date (UTC, exclusive) — e.g. 2026-05-09
    #[arg(long)]
    pub end: NaiveDate,

    /// Output HTML path
    #[arg(long, default_value = "backtest-report.html")]
    pub output: std::path::PathBuf,

    /// Cache directory (default ~/.poly-backtest-cache/)
    #[arg(long)]
    pub cache_dir: Option<std::path::PathBuf>,

    /// Override sigma (BTC 5-min std dev in dollars). Defaults to estimated from data.
    #[arg(long)]
    pub sigma: Option<f64>,

    /// Friction coefficient (spread + fees). Default 0.015 (1.5%).
    #[arg(long, default_value = "0.015")]
    pub friction: f64,

    /// Strategy filter — comma-separated names, or "all"
    #[arg(long, default_value = "all")]
    pub strategies: String,

    /// Stddev of Gaussian noise added to BS theoretical bid/ask. Range
    /// [0.0, 0.5]. 0.0 = identical to v1.4 baseline. 0.05 ≈ matches
    /// real-money observed gap-down magnitude.
    #[arg(long, default_value = "0.0")]
    pub oracle_noise: f64,

    /// Seed for the noise RNG. Same seed + same sigma = byte-identical run.
    #[arg(long, default_value = "42")]
    pub noise_seed: u64,

    /// Oracle to use for token price simulation.
    /// `bs` = Black-Scholes theoretical (default; v1.4 behavior).
    /// `noisy` = BS + Gaussian noise (v1.7.2; respects --oracle-noise).
    /// `real` = Real Polymarket trade history (v1.7.5; auto-fetches uncached).
    #[arg(long, value_enum, default_value = "bs")]
    pub oracle: OracleKind,
    /// v1.12: when set, the runner exits at the first cap event — mirrors the
    /// live trader's StopReason::CapReached behavior. Use to validate that a
    /// specific live session is reproducible in backtest.
    #[arg(long)]
    pub stop_at_cap: bool,
    /// v1.12: Unix epoch seconds — skip windows whose `window_ts` is before
    /// this cutoff. Useful for replaying a specific live session that started
    /// mid-day (date-level --start would include extra warmup trades).
    #[arg(long)]
    pub start_ts: Option<i64>,
}

#[derive(Clone, Debug)]
pub enum StakeRule {
    Martingale { base: Decimal, max_step: u8 },
    Fixed { stake: Decimal },
}

#[derive(Clone, Debug)]
pub enum ExitRule {
    HoldToResolution,
    TpOnlyOrHold { tp_price: Decimal },
    TpSlOrHold { tp_price: Decimal, sl_price: Decimal },
    FixedTime { seconds: u32 },
    /// v1.7.5: Try TP at `tp_price`; if not filled by `exit_at_secs`,
    /// market-sell at the current bid. Avoids resolution path entirely.
    TpOnlyOrEarlyExit { tp_price: Decimal, exit_at_secs: u32 },
}

#[derive(Clone, Debug)]
pub struct StrategyConfig {
    pub name: String,
    pub direction: Direction,
    pub band_min: Decimal,
    pub band_max: Decimal,
    pub stake: StakeRule,
    pub exit: ExitRule,
    /// v1.10: if true, runner overrides `direction` per window with the
    /// previous window's actual winner (momentum strategy). First window
    /// falls back to the fixed `direction` field.
    pub follow_previous_winner: bool,
    /// v1.11: per-window direction signal. If `None`, the runner uses
    /// `direction` (or `follow_previous_winner` if set). If Some, overrides
    /// both based on RSI or other technical indicators.
    pub direction_signal: Option<DirectionSignal>,
    /// v1.14: passive maker entry — instead of buying at t=0 ask, post a
    /// limit BID at this fixed price and wait for it to fill. If ask never
    /// drops to this level by `entry_cutoff_secs` (240s default), skip.
    /// `None` = original taker-at-t=0 behavior.
    pub passive_entry_price: Option<Decimal>,
}

/// v1.11: technical-indicator-based per-window direction rules.
#[derive(Clone, Debug)]
pub enum DirectionSignal {
    /// Strategy A (16): RSI<oversold→UP, RSI>overbought→DOWN, else fallback.
    RsiDirection { period: usize, oversold: f64, overbought: f64 },
    /// Strategy B (17): RSI<oversold→UP, RSI>overbought→DOWN, else SKIP window.
    RsiFilterSkipNeutral { period: usize, oversold: f64, overbought: f64 },
    /// Strategy C (18): RSI extreme zone → mean-reversion direction;
    /// neutral zone → anti-follow-previous-winner (bet against last winner).
    RsiPlusAntiFollow { period: usize, oversold: f64, overbought: f64 },
    /// Strategy 20: deterministic 50/50 random direction per window
    /// (seed mixed with window_ts → reproducible runs).
    Random { seed: u64 },
    /// v1.12: RSI mean-reversion + EMA trend filter. RSI extreme triggers a
    /// counter-trend bet, but only when the trend slope agrees (or is mild).
    /// Skip when slope is strongly against the RSI signal — i.e., don't bet
    /// UP into a sustained downtrend, don't bet DOWN into a sustained uptrend.
    RsiWithTrendFilter {
        period: usize,
        oversold: f64,
        overbought: f64,
        ema_period: usize,
        slope_lookback_mins: i64,
        /// Absolute BTC $/min slope above which the trend is "strong";
        /// counter-trend RSI signals at or beyond this slope get skipped.
        slope_threshold: f64,
    },
    /// v1.14: late-window momentum. At t = `entry_offset_secs` into the
    /// window, compare current BTC price to `price_to_beat`. If the gap
    /// exceeds `threshold_dollars`, bet in the gap's direction. Otherwise
    /// skip. Entry happens at the chosen offset (not t=0), so the share
    /// price reflects late-window certainty — higher cost, higher win rate.
    LateMomentum {
        entry_offset_secs: u32,
        threshold_dollars: f64,
    },
}

pub fn strategy_set() -> Vec<StrategyConfig> {
    let mart = || StakeRule::Martingale { base: dec!(5), max_step: 5 };
    let common = |name: &str, exit: ExitRule, stake: StakeRule| StrategyConfig {
        name: name.to_string(),
        direction: Direction::Up,
        band_min: dec!(0.45),
        band_max: dec!(0.55),
        stake,
        exit,
        follow_previous_winner: false,
        direction_signal: None,
        passive_entry_price: None,
    };
    let follow_prev = |name: &str, exit: ExitRule, stake: StakeRule| StrategyConfig {
        follow_previous_winner: true,
        ..common(name, exit, stake)
    };
    let with_signal = |name: &str, exit: ExitRule, stake: StakeRule, signal: DirectionSignal| StrategyConfig {
        direction_signal: Some(signal),
        ..common(name, exit, stake)
    };
    vec![
        common("1_hold_martingale",       ExitRule::HoldToResolution,                              mart()),
        common("2_tp_only_martingale",    ExitRule::TpOnlyOrHold { tp_price: dec!(0.75) },         mart()),
        common("3_tp_sl_symmetric",       ExitRule::TpSlOrHold { tp_price: dec!(0.55), sl_price: dec!(0.45) }, mart()),
        common("4_tp_sl_asymmetric",      ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.45) }, mart()),
        common("5_time_60s_martingale",   ExitRule::FixedTime { seconds: 60 },                     mart()),
        common("6_fixed_stake_baseline",  ExitRule::HoldToResolution,                              StakeRule::Fixed { stake: dec!(5) }),
        common("7_tp85_sl40",             ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.40) }, mart()),
        common("8_tp85_sl35",             ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.35) }, mart()),
        common("9_tp85_sl30",             ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.30) }, mart()),
        common("10_tp85_sl25",            ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.25) }, mart()),
        common("11_tp85_sl20",            ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.20) }, mart()),
        common("12_tp75_early_exit_270",
            ExitRule::TpOnlyOrEarlyExit { tp_price: dec!(0.75), exit_at_secs: 270 },
            mart()),
        common("13_hold_early_exit_270",
            ExitRule::FixedTime { seconds: 270 },
            mart()),
        // v1.10: follow-previous-winner variants
        follow_prev("14_hold_followprev",
            ExitRule::HoldToResolution,
            mart()),
        follow_prev("15_hold_early_exit_270_followprev",
            ExitRule::FixedTime { seconds: 270 },
            mart()),
        // v1.11: RSI-based direction strategies (A, B, C)
        with_signal("16_hold_rsi_direction",
            ExitRule::HoldToResolution,
            mart(),
            DirectionSignal::RsiDirection { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("17_hold_rsi_filter",
            ExitRule::HoldToResolution,
            mart(),
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("18_hold_rsi_anti_follow",
            ExitRule::HoldToResolution,
            mart(),
            DirectionSignal::RsiPlusAntiFollow { period: 14, oversold: 30.0, overbought: 70.0 }),
        // v1.11.1: symmetric baseline — always DOWN
        StrategyConfig {
            direction: Direction::Down,
            ..common("19_hold_always_down", ExitRule::HoldToResolution, mart())
        },
        // v1.11.2: random-direction baseline (deterministic via seed+window_ts hash)
        with_signal("20_hold_random_direction",
            ExitRule::HoldToResolution,
            mart(),
            DirectionSignal::Random { seed: 42 }),
        // v1.11.3: strategy 17 with max_step=7 (covers max_consec_losses=9 better)
        with_signal("21_hold_rsi_filter_max7",
            ExitRule::HoldToResolution,
            StakeRule::Martingale { base: dec!(5), max_step: 7 },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        // v1.11.4: strategy 17 with fixed $5 (no martingale) — pure RSI alpha
        with_signal("22_hold_rsi_filter_fixed",
            ExitRule::HoldToResolution,
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        // v1.11.5: strategy 22 + TP-only SELL limit order (avoid end-of-window reversal)
        with_signal("23_rsi_fixed_tp65",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.65) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("24_rsi_fixed_tp75",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.75) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("25_rsi_fixed_tp85",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.85) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        // v1.11.6: TP grid search (0.55 → 0.95 step 0.05, completing the sweep)
        with_signal("26_rsi_fixed_tp55",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.55) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("27_rsi_fixed_tp60",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.60) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("28_rsi_fixed_tp70",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.70) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("29_rsi_fixed_tp80",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.80) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("30_rsi_fixed_tp90",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.90) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("31_rsi_fixed_tp95",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.95) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        // v1.11.7: ultra-fine TP grid around the $0.85–$0.90 plateau
        with_signal("32_rsi_fixed_tp83",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.83) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("33_rsi_fixed_tp87",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.87) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("34_rsi_fixed_tp89",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.89) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("35_rsi_fixed_tp91",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.91) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        // v1.11.8: SL grid (TP=0.87 fixed, SL sweep)
        with_signal("36_rsi_tp87_sl20",
            ExitRule::TpSlOrHold { tp_price: dec!(0.87), sl_price: dec!(0.20) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("37_rsi_tp87_sl25",
            ExitRule::TpSlOrHold { tp_price: dec!(0.87), sl_price: dec!(0.25) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("38_rsi_tp87_sl30",
            ExitRule::TpSlOrHold { tp_price: dec!(0.87), sl_price: dec!(0.30) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("39_rsi_tp87_sl35",
            ExitRule::TpSlOrHold { tp_price: dec!(0.87), sl_price: dec!(0.35) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        with_signal("40_rsi_tp87_sl40",
            ExitRule::TpSlOrHold { tp_price: dec!(0.87), sl_price: dec!(0.40) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        // v1.11.9: hybrid — RSI Mart + TP=0.87 (mirrors the dry-run we accidentally ran)
        with_signal("41_rsi_mart_tp87",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.87) },
            StakeRule::Martingale { base: dec!(5), max_step: 5 },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 }),
        // v1.12: RSI Mart + TP=0.87 + EMA trend filter (slope threshold sweep)
        with_signal("42_rsi_mart_tp87_ema_t2",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.87) },
            StakeRule::Martingale { base: dec!(5), max_step: 5 },
            DirectionSignal::RsiWithTrendFilter {
                period: 14, oversold: 30.0, overbought: 70.0,
                ema_period: 50, slope_lookback_mins: 10, slope_threshold: 2.0,
            }),
        with_signal("43_rsi_mart_tp87_ema_t5",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.87) },
            StakeRule::Martingale { base: dec!(5), max_step: 5 },
            DirectionSignal::RsiWithTrendFilter {
                period: 14, oversold: 30.0, overbought: 70.0,
                ema_period: 50, slope_lookback_mins: 10, slope_threshold: 5.0,
            }),
        with_signal("44_rsi_mart_tp87_ema_t10",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.87) },
            StakeRule::Martingale { base: dec!(5), max_step: 5 },
            DirectionSignal::RsiWithTrendFilter {
                period: 14, oversold: 30.0, overbought: 70.0,
                ema_period: 50, slope_lookback_mins: 10, slope_threshold: 10.0,
            }),
        // v1.12.4: stricter RSI thresholds (25/75) — fewer triggers, hopefully
        // higher conviction. Live observed 53.7% win rate at 30/70, vs backtest's
        // 60.6%; tightening filters out the borderline cases that may dilute alpha.
        with_signal("45_rsi_fixed_tp87_2575",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.87) },
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 25.0, overbought: 75.0 }),
        with_signal("46_rsi_mart_tp87_2575",
            ExitRule::TpOnlyOrHold { tp_price: dec!(0.87) },
            StakeRule::Martingale { base: dec!(5), max_step: 5 },
            DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 25.0, overbought: 75.0 }),
        // v1.14: LateMomentum — enter at t=offset, bet direction of |BTC-price_to_beat|.
        // Exit = hold to resolution (no TP, no SL). High entry cost, hopefully high win rate.
        with_signal("47_late_60s_d10",
            ExitRule::HoldToResolution,
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::LateMomentum { entry_offset_secs: 240, threshold_dollars: 10.0 }),
        with_signal("48_late_60s_d30",
            ExitRule::HoldToResolution,
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::LateMomentum { entry_offset_secs: 240, threshold_dollars: 30.0 }),
        with_signal("49_late_30s_d10",
            ExitRule::HoldToResolution,
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::LateMomentum { entry_offset_secs: 270, threshold_dollars: 10.0 }),
        with_signal("50_late_30s_d30",
            ExitRule::HoldToResolution,
            StakeRule::Fixed { stake: dec!(5) },
            DirectionSignal::LateMomentum { entry_offset_secs: 270, threshold_dollars: 30.0 }),
        // v1.14: passive maker — wait for ask to drop to $0.46 (cheap entry)
        // then post-fill BID @ $0.46. Otherwise skip the window. Same RSI +
        // TP=$0.83 as strategy 32, just with a passive limit-BID entry.
        StrategyConfig {
            passive_entry_price: Some(dec!(0.46)),
            band_min: dec!(0.40),  // widen band so $0.46 ask is in range
            band_max: dec!(0.55),
            ..with_signal("51_rsi_fixed_tp83_passive46",
                ExitRule::TpOnlyOrHold { tp_price: dec!(0.83) },
                StakeRule::Fixed { stake: dec!(5) },
                DirectionSignal::RsiFilterSkipNeutral { period: 14, oversold: 30.0, overbought: 70.0 })
        },
    ]
}

/// Filter `all_strategies` by the comma-separated `filter` string. "all" returns all.
pub fn filter_strategies(all: &[StrategyConfig], filter: &str) -> Vec<StrategyConfig> {
    if filter == "all" || filter.is_empty() {
        return all.to_vec();
    }
    let names: Vec<&str> = filter.split(',').map(|s| s.trim()).collect();
    all.iter().filter(|s| names.contains(&s.name.as_str())).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> BacktestArgs {
        let mut full = vec!["poly-backtest"];
        full.extend(args);
        BacktestArgs::parse_from(full)
    }

    #[test]
    fn parses_minimal_args() {
        let a = parse(&["--start", "2026-04-09", "--end", "2026-05-09"]);
        assert_eq!(a.start, NaiveDate::from_ymd_opt(2026, 4, 9).unwrap());
        assert_eq!(a.end, NaiveDate::from_ymd_opt(2026, 5, 9).unwrap());
        assert_eq!(a.friction, 0.015);
        assert_eq!(a.strategies, "all");
    }

    #[test]
    fn strategy_set_uniqueness() {
        let s = strategy_set();
        let mut names: Vec<&String> = s.iter().map(|c| &c.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 51);
    }

    #[test]
    fn strategy_1_is_hold_to_resolution_martingale() {
        let s = strategy_set();
        let s1 = s.iter().find(|c| c.name == "1_hold_martingale").unwrap();
        assert!(matches!(s1.exit, ExitRule::HoldToResolution));
        assert!(matches!(s1.stake, StakeRule::Martingale { .. }));
    }

    #[test]
    fn strategy_6_is_fixed_stake_no_martingale() {
        let s = strategy_set();
        let s6 = s.iter().find(|c| c.name == "6_fixed_stake_baseline").unwrap();
        assert!(matches!(s6.stake, StakeRule::Fixed { stake } if stake == dec!(5)));
    }

    #[test]
    fn filter_all_returns_everything() {
        let s = strategy_set();
        assert_eq!(filter_strategies(&s, "all").len(), 51);
        assert_eq!(filter_strategies(&s, "").len(), 51);
    }

    #[test]
    fn strategy_set_includes_sl_sweep_variants() {
        let s = strategy_set();
        let sweep_names = ["7_tp85_sl40", "8_tp85_sl35", "9_tp85_sl30", "10_tp85_sl25", "11_tp85_sl20"];
        for name in sweep_names {
            let entry = s.iter().find(|c| c.name == name)
                .unwrap_or_else(|| panic!("strategy '{name}' missing"));
            match &entry.exit {
                ExitRule::TpSlOrHold { tp_price, sl_price } => {
                    assert_eq!(*tp_price, dec!(0.85), "{name} TP wrong");
                    let expected_sl = match name {
                        "7_tp85_sl40" => dec!(0.40),
                        "8_tp85_sl35" => dec!(0.35),
                        "9_tp85_sl30" => dec!(0.30),
                        "10_tp85_sl25" => dec!(0.25),
                        "11_tp85_sl20" => dec!(0.20),
                        _ => unreachable!(),
                    };
                    assert_eq!(*sl_price, expected_sl, "{name} SL wrong");
                }
                _ => panic!("{name} should be TpSlOrHold"),
            }
        }
    }

    #[test]
    fn filter_specific_names() {
        let s = strategy_set();
        let f = filter_strategies(&s, "1_hold_martingale,4_tp_sl_asymmetric");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].name, "1_hold_martingale");
        assert_eq!(f[1].name, "4_tp_sl_asymmetric");
    }

    #[test]
    fn filter_unknown_name_returns_empty() {
        let s = strategy_set();
        assert_eq!(filter_strategies(&s, "nonexistent").len(), 0);
    }

    #[test]
    fn parses_oracle_noise_default_zero() {
        let a = parse(&["--start", "2026-04-09", "--end", "2026-05-09"]);
        assert_eq!(a.oracle_noise, 0.0);
    }

    #[test]
    fn parses_oracle_noise_005() {
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--oracle-noise", "0.05",
        ]);
        assert_eq!(a.oracle_noise, 0.05);
    }

    #[test]
    fn parses_noise_seed_default_42() {
        let a = parse(&["--start", "2026-04-09", "--end", "2026-05-09"]);
        assert_eq!(a.noise_seed, 42);
    }

    #[test]
    fn parses_noise_seed_custom() {
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--noise-seed", "12345",
        ]);
        assert_eq!(a.noise_seed, 12345);
    }

    #[test]
    fn parses_oracle_noise_negative_value() {
        // Clap accepts the value at parse time; runtime validation in main()
        // rejects. This test just documents that clap doesn't reject negatives
        // at parse — they must be caught downstream. Use `--flag=val` form
        // so clap doesn't mistake `-0.1` for a short flag.
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--oracle-noise=-0.1",
        ]);
        assert_eq!(a.oracle_noise, -0.1);
    }

    #[test]
    fn parses_oracle_noise_above_half() {
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--oracle-noise", "0.6",
        ]);
        assert_eq!(a.oracle_noise, 0.6);
    }

    #[test]
    fn parses_oracle_default_bs() {
        let a = parse(&["--start", "2026-04-09", "--end", "2026-05-09"]);
        assert_eq!(a.oracle, OracleKind::Bs);
    }

    #[test]
    fn parses_oracle_real() {
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--oracle", "real",
        ]);
        assert_eq!(a.oracle, OracleKind::Real);
    }

    #[test]
    fn parses_oracle_noisy() {
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--oracle", "noisy",
        ]);
        assert_eq!(a.oracle, OracleKind::Noisy);
    }

    #[test]
    fn strategy_set_has_fiftyone_strategies() {
        let s = strategy_set();
        assert_eq!(s.len(), 51);
    }

    #[test]
    fn strategy_12_is_tp75_early_exit_270() {
        let s = strategy_set();
        let s12 = s.iter().find(|c| c.name == "12_tp75_early_exit_270")
            .expect("strategy 12 missing");
        match &s12.exit {
            ExitRule::TpOnlyOrEarlyExit { tp_price, exit_at_secs } => {
                assert_eq!(*tp_price, dec!(0.75));
                assert_eq!(*exit_at_secs, 270);
            }
            _ => panic!("strategy 12 should be TpOnlyOrEarlyExit"),
        }
        assert!(matches!(s12.stake, StakeRule::Martingale { .. }));
    }

    #[test]
    fn strategy_13_is_hold_early_exit_270() {
        let s = strategy_set();
        let s13 = s.iter().find(|c| c.name == "13_hold_early_exit_270")
            .expect("strategy 13 missing");
        match &s13.exit {
            ExitRule::FixedTime { seconds } => {
                assert_eq!(*seconds, 270);
            }
            _ => panic!("strategy 13 should be FixedTime {{ seconds: 270 }}"),
        }
        assert!(matches!(s13.stake, StakeRule::Martingale { .. }));
    }
}
