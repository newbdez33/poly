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
pub enum ExitRuleArg { Hold, TpSl }

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
        if self.poll_secs == 0 || self.poll_secs > 30 {
            return Err(ConfigError::InvalidPollSecs);
        }
        if matches!(self.exit_rule, ExitRuleArg::TpSl) {
            let (tp, sl) = match (self.tp_price, self.sl_price) {
                (Some(tp), Some(sl)) => (tp, sl),
                _ => return Err(ConfigError::ExitRuleMissingThresholds),
            };
            if tp <= Decimal::ZERO || tp >= Decimal::ONE
               || sl <= Decimal::ZERO || sl >= Decimal::ONE {
                return Err(ConfigError::ExitRuleInvalidThreshold);
            }
            if tp <= sl {
                return Err(ConfigError::ExitRuleInvertedThresholds);
            }
        }
        if self.maker && !matches!(self.exit_rule, ExitRuleArg::TpSl) {
            return Err(ConfigError::MakerRequiresTpSl);
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
}
