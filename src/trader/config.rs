use crate::trader::ladder::Direction;
use clap::{Parser, ValueEnum};
use rust_decimal::Decimal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum DirectionArg { Up, Down }

impl From<DirectionArg> for Direction {
    fn from(d: DirectionArg) -> Self {
        match d {
            DirectionArg::Up => Direction::Up,
            DirectionArg::Down => Direction::Down,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ExitRuleArg {
    Hold,
    TpSl,
    /// v1.8: Hold position, then market-sell at `--exit-at-secs`. Avoids
    /// resolution path entirely (no on-chain redeem; no MATIC needed).
    HoldEarlyExit,
}

#[derive(Parser, Debug, Clone)]
#[command(name = "poly-trader",
          about = "Polymarket BTC 5min Martingale trader",
          version)]
pub struct TraderArgs {
    #[arg(long, value_enum)]
    pub direction: DirectionArg,
    #[arg(long, default_value = "5")]
    pub base: Decimal,
    #[arg(long, default_value = "5")]
    pub max_step: u8,
    #[arg(long, default_value = "0.45")]
    pub band_min: Decimal,
    #[arg(long, default_value = "0.55")]
    pub band_max: Decimal,
    #[arg(long, value_enum, default_value = "hold")]
    pub exit_rule: ExitRuleArg,
    #[arg(long)]
    pub tp_price: Option<Decimal>,
    #[arg(long)]
    pub sl_price: Option<Decimal>,
    /// Seconds into the window at which to market-sell. Required when
    /// --exit-rule is hold-early-exit. Rejected for other exit rules.
    /// Range: 1..=(window_seconds - 30) to ensure the orderbook is still
    /// active. Backtest-validated default: 270 (for 5-min windows).
    #[arg(long)]
    pub exit_at_secs: Option<u32>,
    #[arg(long, default_value = "5")]
    pub poll_secs: u32,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub reset: bool,
    #[arg(long)]
    pub max_windows: Option<u32>,
    /// Use limit orders for BUY entry + TP exit. Saves taker fees but may
    /// skip windows when liquidity is thin. Only valid with --exit-rule tp-sl.
    #[arg(long)]
    pub maker: bool,
    /// v1.12: hybrid mode — taker FoK BUY (guaranteed entry) + limit SELL
    /// posted at --tp-price (no missed brief touches). Falls back to market
    /// sell at t=window_close-30s if TP limit not filled. Only valid with
    /// --exit-rule tp-sl. Mutually exclusive with --maker.
    #[arg(long)]
    pub tp_limit_sell: bool,
    /// v1.12: Fixed-stake mode — every BUY is `--base` shares regardless of
    /// outcome. No Martingale doubling, no cap. Each loss is bounded by
    /// `base × ask`. Matches backtest StakeRule::Fixed (strategies 6, 22-40).
    #[arg(long)]
    pub fixed_stake: bool,
    /// Trading window length in minutes. {5, 15, 60}. 5 has full backtest
    /// coverage; 15 has observed deeper liquidity but is unvalidated; 60 is
    /// unvalidated. Default 5.
    #[arg(long, default_value = "5")]
    pub window_minutes: u32,
    /// v1.11: enable RSI(period) direction filter (strategy 33).
    /// Before each window, fetch Binance 1-min closes, compute RSI:
    ///   RSI < oversold → force UP, RSI > overbought → force DOWN,
    ///   otherwise → skip window. Overrides --direction per window.
    #[arg(long)]
    pub rsi_filter: bool,
    #[arg(long, default_value = "14")]
    pub rsi_period: usize,
    #[arg(long, default_value = "30.0")]
    pub rsi_oversold: f64,
    #[arg(long, default_value = "70.0")]
    pub rsi_overbought: f64,
}

impl TraderArgs {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.base <= Decimal::ZERO { return Err(ConfigError::InvalidBase); }
        if self.max_step < 1 || self.max_step > 10 { return Err(ConfigError::InvalidMaxStep); }
        if self.band_min >= self.band_max
           || self.band_min < Decimal::ZERO
           || self.band_max > Decimal::ONE {
            return Err(ConfigError::InvalidBand);
        }
        if !matches!(self.window_minutes, 5 | 15 | 60) {
            return Err(ConfigError::InvalidWindowMinutes);
        }
        if self.poll_secs == 0 || self.poll_secs > 30 {
            return Err(ConfigError::InvalidPollSecs);
        }
        if matches!(self.exit_rule, ExitRuleArg::TpSl) {
            // v1.11: SL is optional — when omitted, an effective floor of $0.001
            // is used (never triggers), giving us TP-only behavior.
            let tp = self.tp_price.ok_or(ConfigError::ExitRuleMissingThresholds)?;
            if tp <= Decimal::ZERO || tp >= Decimal::ONE {
                return Err(ConfigError::ExitRuleInvalidThreshold);
            }
            if let Some(sl) = self.sl_price {
                if sl <= Decimal::ZERO || sl >= Decimal::ONE {
                    return Err(ConfigError::ExitRuleInvalidThreshold);
                }
                if tp <= sl {
                    return Err(ConfigError::ExitRuleInvertedThresholds);
                }
            }
        }
        if self.rsi_filter {
            if self.rsi_period < 2 || self.rsi_period > 60 {
                return Err(ConfigError::RsiPeriodOutOfRange);
            }
            if self.rsi_oversold <= 0.0 || self.rsi_oversold >= self.rsi_overbought
               || self.rsi_overbought >= 100.0 {
                return Err(ConfigError::RsiThresholdsInvalid);
            }
        }
        let window_seconds = (self.window_minutes as u32) * 60;
        match self.exit_rule {
            ExitRuleArg::HoldEarlyExit => {
                let secs = self.exit_at_secs.ok_or(ConfigError::ExitAtSecsRequired)?;
                if secs == 0 || secs > window_seconds.saturating_sub(30) {
                    return Err(ConfigError::ExitAtSecsOutOfRange);
                }
            }
            _ => {
                if self.exit_at_secs.is_some() {
                    return Err(ConfigError::ExitAtSecsWrongMode);
                }
            }
        }
        if self.maker && !matches!(self.exit_rule, ExitRuleArg::TpSl) {
            return Err(ConfigError::MakerRequiresTpSl);
        }
        if self.tp_limit_sell && !matches!(self.exit_rule, ExitRuleArg::TpSl) {
            return Err(ConfigError::TpLimitSellRequiresTpSl);
        }
        if self.tp_limit_sell && self.maker {
            return Err(ConfigError::TpLimitSellConflictsWithMaker);
        }
        Ok(())
    }
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum ConfigError {
    #[error("base must be > 0")]
    InvalidBase,
    #[error("max_step must be in 1..=10")]
    InvalidMaxStep,
    #[error("band: must satisfy 0 <= band_min < band_max <= 1")]
    InvalidBand,
    #[error("poll-secs must be in 1..=30")]
    InvalidPollSecs,
    #[error("--exit-rule tp-sl requires --tp-price and --sl-price")]
    ExitRuleMissingThresholds,
    #[error("tp-price and sl-price must each be in (0, 1)")]
    ExitRuleInvalidThreshold,
    #[error("tp-price must be greater than sl-price")]
    ExitRuleInvertedThresholds,
    #[error("--maker requires --exit-rule tp-sl")]
    MakerRequiresTpSl,
    #[error("window-minutes must be 5, 15, or 60")]
    InvalidWindowMinutes,
    #[error("--exit-rule hold-early-exit requires --exit-at-secs")]
    ExitAtSecsRequired,
    #[error("--exit-at-secs only valid with --exit-rule hold-early-exit")]
    ExitAtSecsWrongMode,
    #[error("--exit-at-secs must be in 1..=(window-seconds - 30)")]
    ExitAtSecsOutOfRange,
    #[error("--rsi-period must be in 2..=60")]
    RsiPeriodOutOfRange,
    #[error("--rsi-oversold and --rsi-overbought must satisfy 0 < oversold < overbought < 100")]
    RsiThresholdsInvalid,
    #[error("--tp-limit-sell requires --exit-rule tp-sl")]
    TpLimitSellRequiresTpSl,
    #[error("--tp-limit-sell and --maker are mutually exclusive")]
    TpLimitSellConflictsWithMaker,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn parse(args: &[&str]) -> TraderArgs {
        let mut full = vec!["poly-trader"];
        full.extend(args);
        TraderArgs::parse_from(full)
    }

    #[test]
    fn parses_minimal_args() {
        let a = parse(&["--direction", "up"]);
        assert_eq!(a.direction, DirectionArg::Up);
        assert_eq!(a.base, Decimal::from(5));
        assert_eq!(a.max_step, 5);
        assert_eq!(a.band_min, Decimal::from_str("0.45").unwrap());
        assert_eq!(a.band_max, Decimal::from_str("0.55").unwrap());
        assert!(!a.dry_run && !a.reset && a.max_windows.is_none());
    }

    #[test]
    fn parses_full_args() {
        let a = parse(&["--direction", "down", "--base", "10", "--max-step", "4",
                        "--band-min", "0.4", "--band-max", "0.6",
                        "--dry-run", "--reset", "--max-windows", "12"]);
        assert_eq!(a.direction, DirectionArg::Down);
        assert_eq!(a.base, Decimal::from(10));
        assert_eq!(a.max_step, 4);
        assert!(a.dry_run && a.reset);
        assert_eq!(a.max_windows, Some(12));
    }

    #[test]
    fn validate_rejects_negative_base() {
        let mut a = parse(&["--direction", "up"]);
        a.base = Decimal::from(-1);
        assert_eq!(a.validate(), Err(ConfigError::InvalidBase));
    }

    #[test]
    fn validate_rejects_zero_max_step() {
        let mut a = parse(&["--direction", "up"]);
        a.max_step = 0;
        assert_eq!(a.validate(), Err(ConfigError::InvalidMaxStep));
    }

    #[test]
    fn validate_rejects_excessive_max_step() {
        let mut a = parse(&["--direction", "up"]);
        a.max_step = 11;
        assert_eq!(a.validate(), Err(ConfigError::InvalidMaxStep));
    }

    #[test]
    fn validate_rejects_inverted_band() {
        let mut a = parse(&["--direction", "up"]);
        a.band_min = Decimal::from_str("0.6").unwrap();
        a.band_max = Decimal::from_str("0.4").unwrap();
        assert_eq!(a.validate(), Err(ConfigError::InvalidBand));
    }

    #[test]
    fn validate_rejects_out_of_range_band() {
        let mut a = parse(&["--direction", "up"]);
        a.band_max = Decimal::from_str("1.5").unwrap();
        assert_eq!(a.validate(), Err(ConfigError::InvalidBand));
    }

    #[test]
    fn validate_accepts_default() {
        assert!(parse(&["--direction", "up"]).validate().is_ok());
    }

    #[test]
    fn direction_arg_to_domain() {
        assert_eq!(Direction::from(DirectionArg::Up), Direction::Up);
        assert_eq!(Direction::from(DirectionArg::Down), Direction::Down);
    }

    #[test]
    fn parses_exit_rule_hold_default() {
        let a = parse(&["--direction", "up"]);
        assert_eq!(a.exit_rule, ExitRuleArg::Hold);
        assert_eq!(a.tp_price, None);
        assert_eq!(a.sl_price, None);
        assert_eq!(a.poll_secs, 5);
    }

    #[test]
    fn parses_exit_rule_tp_sl_with_thresholds() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "tp-sl",
            "--tp-price", "0.85",
            "--sl-price", "0.45",
        ]);
        assert_eq!(a.exit_rule, ExitRuleArg::TpSl);
        assert_eq!(a.tp_price, Some(Decimal::from_str("0.85").unwrap()));
        assert_eq!(a.sl_price, Some(Decimal::from_str("0.45").unwrap()));
    }

    #[test]
    fn validate_rejects_tp_sl_without_thresholds() {
        let mut a = parse(&["--direction", "up", "--exit-rule", "tp-sl"]);
        a.tp_price = None;
        a.sl_price = Some(Decimal::from_str("0.45").unwrap());
        assert_eq!(a.validate(), Err(ConfigError::ExitRuleMissingThresholds));
    }

    #[test]
    fn validate_rejects_tp_le_sl() {
        let a = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                        "--tp-price", "0.50", "--sl-price", "0.50"]);
        assert_eq!(a.validate(), Err(ConfigError::ExitRuleInvertedThresholds));
    }

    #[test]
    fn validate_rejects_thresholds_out_of_range() {
        let a = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                        "--tp-price", "1.0", "--sl-price", "0.45"]);
        assert_eq!(a.validate(), Err(ConfigError::ExitRuleInvalidThreshold));
        let b = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                        "--tp-price", "0.85", "--sl-price", "0.0"]);
        assert_eq!(b.validate(), Err(ConfigError::ExitRuleInvalidThreshold));
    }

    #[test]
    fn validate_rejects_poll_secs_zero_or_huge() {
        let mut a = parse(&["--direction", "up"]);
        a.poll_secs = 0;
        assert_eq!(a.validate(), Err(ConfigError::InvalidPollSecs));
        a.poll_secs = 31;
        assert_eq!(a.validate(), Err(ConfigError::InvalidPollSecs));
    }

    #[test]
    fn validate_accepts_tp_sl_full() {
        let a = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                        "--tp-price", "0.85", "--sl-price", "0.45"]);
        assert!(a.validate().is_ok());
    }

    #[test]
    fn parses_maker_flag_off_by_default() {
        let a = parse(&["--direction", "up"]);
        assert!(!a.maker);
    }

    #[test]
    fn parses_maker_flag_on() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "tp-sl",
            "--tp-price", "0.85",
            "--sl-price", "0.45",
            "--maker",
        ]);
        assert!(a.maker);
    }

    #[test]
    fn validate_rejects_maker_without_tp_sl() {
        let mut a = parse(&["--direction", "up"]);
        a.maker = true;
        // exit_rule is Hold by default
        assert_eq!(a.validate(), Err(ConfigError::MakerRequiresTpSl));
    }

    #[test]
    fn validate_accepts_maker_with_tp_sl() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "tp-sl",
            "--tp-price", "0.85",
            "--sl-price", "0.45",
            "--maker",
        ]);
        assert!(a.validate().is_ok());
    }

    #[test]
    fn parses_window_minutes_default_5() {
        let a = parse(&["--direction", "up"]);
        assert_eq!(a.window_minutes, 5);
    }

    #[test]
    fn parses_window_minutes_15() {
        let a = parse(&["--direction", "up", "--window-minutes", "15"]);
        assert_eq!(a.window_minutes, 15);
    }

    #[test]
    fn parses_window_minutes_60() {
        let a = parse(&["--direction", "up", "--window-minutes", "60"]);
        assert_eq!(a.window_minutes, 60);
    }

    #[test]
    fn validate_rejects_window_minutes_7() {
        let mut a = parse(&["--direction", "up"]);
        a.window_minutes = 7;
        assert_eq!(a.validate(), Err(ConfigError::InvalidWindowMinutes));
    }

    #[test]
    fn validate_rejects_window_minutes_0() {
        let mut a = parse(&["--direction", "up"]);
        a.window_minutes = 0;
        assert_eq!(a.validate(), Err(ConfigError::InvalidWindowMinutes));
    }

    #[test]
    fn validate_accepts_window_minutes_15() {
        let a = parse(&["--direction", "up", "--window-minutes", "15"]);
        assert!(a.validate().is_ok());
    }

    #[test]
    fn parses_exit_rule_hold_early_exit_with_secs() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "hold-early-exit",
            "--exit-at-secs", "270",
        ]);
        assert_eq!(a.exit_rule, ExitRuleArg::HoldEarlyExit);
        assert_eq!(a.exit_at_secs, Some(270));
        assert!(a.validate().is_ok());
    }

    #[test]
    fn parses_exit_at_secs_default_is_none() {
        let a = parse(&["--direction", "up"]);
        assert_eq!(a.exit_at_secs, None);
    }

    #[test]
    fn validate_rejects_hold_early_exit_without_secs() {
        let mut a = parse(&["--direction", "up", "--exit-rule", "hold-early-exit"]);
        a.exit_at_secs = None;
        assert_eq!(a.validate(), Err(ConfigError::ExitAtSecsRequired));
    }

    #[test]
    fn validate_rejects_exit_at_secs_zero() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "hold-early-exit",
            "--exit-at-secs", "0",
        ]);
        assert_eq!(a.validate(), Err(ConfigError::ExitAtSecsOutOfRange));
    }

    #[test]
    fn validate_rejects_exit_at_secs_too_close_to_close() {
        // For 5-min window (300s), exit-at-secs must be <= 270 (300 - 30).
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "hold-early-exit",
            "--exit-at-secs", "290",
        ]);
        assert_eq!(a.validate(), Err(ConfigError::ExitAtSecsOutOfRange));
    }

    #[test]
    fn validate_rejects_exit_at_secs_for_non_hold_early_exit() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "hold",
            "--exit-at-secs", "200",
        ]);
        assert_eq!(a.validate(), Err(ConfigError::ExitAtSecsWrongMode));
    }

    #[test]
    fn validate_rejects_maker_with_hold_early_exit() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "hold-early-exit",
            "--exit-at-secs", "270",
            "--maker",
        ]);
        assert_eq!(a.validate(), Err(ConfigError::MakerRequiresTpSl));
    }

    #[test]
    fn validate_hold_early_exit_with_15min_window() {
        // For 15-min window (900s), exit-at-secs must be <= 870 (900 - 30).
        let a = parse(&[
            "--direction", "up",
            "--window-minutes", "15",
            "--exit-rule", "hold-early-exit",
            "--exit-at-secs", "870",
        ]);
        assert!(a.validate().is_ok());
    }
}
