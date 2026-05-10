use crate::trader::ladder::Direction;
use chrono::NaiveDate;
use clap::Parser;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

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
}

#[derive(Clone, Debug)]
pub struct StrategyConfig {
    pub name: String,
    pub direction: Direction,
    pub band_min: Decimal,
    pub band_max: Decimal,
    pub stake: StakeRule,
    pub exit: ExitRule,
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
    fn strategy_set_has_eleven_strategies() {
        let s = strategy_set();
        assert_eq!(s.len(), 11);
        let names: Vec<&str> = s.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"1_hold_martingale"));
        assert!(names.contains(&"6_fixed_stake_baseline"));
        assert!(names.contains(&"11_tp85_sl20"));
    }

    #[test]
    fn strategy_set_uniqueness() {
        let s = strategy_set();
        let mut names: Vec<&String> = s.iter().map(|c| &c.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 11);
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
        assert_eq!(filter_strategies(&s, "all").len(), 11);
        assert_eq!(filter_strategies(&s, "").len(), 11);
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
}
