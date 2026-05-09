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
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub reset: bool,
    #[arg(long)]
    pub max_windows: Option<u32>,
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
}
