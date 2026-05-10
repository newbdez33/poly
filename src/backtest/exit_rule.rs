use crate::backtest::config::{ExitRule, StrategyConfig};
use crate::backtest::data::gamma_history::WindowMeta;
use crate::backtest::oracle::TokenPriceOracle;
use crate::trader::ladder::{SkipReason, WindowOutcome};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub fn simulate_window(
    window: &WindowMeta,
    config: &StrategyConfig,
    oracle: &dyn TokenPriceOracle,
    stake: Decimal,
) -> WindowOutcome {
    // 1. Entry: ask at t=0 must be in band
    let (_, ask) = oracle.price_at(window, 0);
    if ask < config.band_min || ask > config.band_max {
        return WindowOutcome::Skipped {
            reason: SkipReason::PriceOutsideBand { ask },
        };
    }

    // 2. Compute share count, enforce 5-share minimum
    let shares = if ask > Decimal::ZERO { (stake / ask).floor() } else { Decimal::ZERO };
    if shares < dec!(5) {
        return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
    }
    let dollars_spent = shares * ask;

    // 3. Walk seconds 1..=300, check exit rules
    for t in 1..=300u32 {
        let (bid, _) = oracle.price_at(window, t);
        let proceeds = shares * bid;

        match &config.exit {
            ExitRule::HoldToResolution => {
                // do nothing intra-window
            }
            ExitRule::TpOnlyOrHold { tp_price } => {
                if bid >= *tp_price {
                    return WindowOutcome::Won { proceeds_usd: proceeds };
                }
            }
            ExitRule::TpSlOrHold { tp_price, sl_price } => {
                if bid >= *tp_price {
                    return WindowOutcome::Won { proceeds_usd: proceeds };
                }
                if bid <= *sl_price {
                    return if proceeds > dollars_spent {
                        WindowOutcome::Won { proceeds_usd: proceeds }
                    } else {
                        WindowOutcome::Lost { spent_usd: dollars_spent - proceeds }
                    };
                }
            }
            ExitRule::FixedTime { seconds } if t >= *seconds => {
                return if proceeds > dollars_spent {
                    WindowOutcome::Won { proceeds_usd: proceeds }
                } else {
                    WindowOutcome::Lost { spent_usd: dollars_spent - proceeds }
                };
            }
            _ => {}
        }
    }

    // 4. Hold to resolution: use winner from window meta
    let our_won = window.winner == Some(config.direction);
    if our_won {
        WindowOutcome::Won { proceeds_usd: shares * dec!(0.99) }
    } else {
        WindowOutcome::Lost { spent_usd: dollars_spent }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::config::{StakeRule, StrategyConfig};
    use crate::backtest::oracle::TokenPriceOracle;
    use crate::trader::ladder::Direction;
    use rust_decimal_macros::dec;
    use std::str::FromStr;

    /// Test oracle that returns deterministic (bid, ask) per second of the window.
    struct StubOracle {
        prices: Vec<(Decimal, Decimal)>, // index = t_secs
    }
    impl TokenPriceOracle for StubOracle {
        fn price_at(&self, _window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal) {
            self.prices.get(t_secs as usize).copied()
                .unwrap_or_else(|| *self.prices.last().unwrap())
        }
    }

    fn flat_window(price: &str) -> StubOracle {
        let p = Decimal::from_str(price).unwrap();
        StubOracle { prices: vec![(p, p); 301] }
    }

    fn make_window(winner: Direction) -> WindowMeta {
        WindowMeta {
            window_ts: 1000,
            price_to_beat: dec!(80000),
            final_price: Some(dec!(80050)),
            winner: Some(winner),
            condition_id: None,
        }
    }

    fn config_hold_to_resolution() -> StrategyConfig {
        StrategyConfig {
            name: "test".into(),
            direction: Direction::Up,
            band_min: dec!(0.45),
            band_max: dec!(0.55),
            stake: StakeRule::Fixed { stake: dec!(5) },
            exit: ExitRule::HoldToResolution,
        }
    }

    fn config_with_exit(exit: ExitRule) -> StrategyConfig {
        StrategyConfig { exit, ..config_hold_to_resolution() }
    }

    #[test]
    fn skip_when_ask_below_band() {
        let oracle = flat_window("0.30");
        let outcome = simulate_window(&make_window(Direction::Up), &config_hold_to_resolution(), &oracle, dec!(5));
        assert!(matches!(outcome, WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { .. } }));
    }

    #[test]
    fn skip_when_ask_above_band() {
        let oracle = flat_window("0.62");
        let outcome = simulate_window(&make_window(Direction::Up), &config_hold_to_resolution(), &oracle, dec!(5));
        assert!(matches!(outcome, WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { .. } }));
    }

    #[test]
    fn skip_when_under_5_shares_minimum() {
        // ask = 0.50, but stake too small to buy 5 shares: stake $2 / 0.50 = 4 shares
        let oracle = flat_window("0.50");
        let outcome = simulate_window(&make_window(Direction::Up), &config_hold_to_resolution(), &oracle, dec!(2));
        assert!(matches!(outcome, WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed }));
    }

    #[test]
    fn hold_to_resolution_wins_when_we_picked_winner() {
        let oracle = flat_window("0.50");
        let outcome = simulate_window(&make_window(Direction::Up), &config_hold_to_resolution(), &oracle, dec!(5));
        assert!(matches!(outcome, WindowOutcome::Won { .. }));
    }

    #[test]
    fn hold_to_resolution_loses_when_we_picked_loser() {
        let oracle = flat_window("0.50");
        let outcome = simulate_window(&make_window(Direction::Down), &config_hold_to_resolution(), &oracle, dec!(5));
        assert!(matches!(outcome, WindowOutcome::Lost { .. }));
    }

    #[test]
    fn tp_only_triggers_when_bid_reaches_tp() {
        // At t=0 ask=0.50; from t=1 onwards bid=0.80 → trigger TP at 0.75
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        prices.extend(std::iter::repeat((dec!(0.80), dec!(0.80))).take(300));
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Down),  // Down would be loss at hold-to-end, but TP triggers first
            &config_with_exit(ExitRule::TpOnlyOrHold { tp_price: dec!(0.75) }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Won { .. }));
    }

    #[test]
    fn tp_only_holds_when_no_trigger_and_we_lose() {
        let oracle = flat_window("0.50");
        let outcome = simulate_window(
            &make_window(Direction::Down),
            &config_with_exit(ExitRule::TpOnlyOrHold { tp_price: dec!(0.75) }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Lost { .. }));
    }

    #[test]
    fn tp_sl_symmetric_triggers_sl() {
        // ask=0.50 entry, then bid drops to 0.40 → SL at 0.45 triggers
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        prices.extend(std::iter::repeat((dec!(0.40), dec!(0.40))).take(300));
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Up),
            &config_with_exit(ExitRule::TpSlOrHold {
                tp_price: dec!(0.55), sl_price: dec!(0.45)
            }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Lost { .. }));
    }

    #[test]
    fn tp_sl_symmetric_triggers_tp() {
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        prices.extend(std::iter::repeat((dec!(0.60), dec!(0.60))).take(300));
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Down),
            &config_with_exit(ExitRule::TpSlOrHold {
                tp_price: dec!(0.55), sl_price: dec!(0.45)
            }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Won { .. }));
    }

    #[test]
    fn fixed_time_exit_at_60s() {
        // ask=0.50 entry, then immediately bid=0.55. At t=60s, sell at $0.55 → +$0.50/share
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        for _ in 0..300 { prices.push((dec!(0.55), dec!(0.55))); }
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Up),
            &config_with_exit(ExitRule::FixedTime { seconds: 60 }),
            &oracle, dec!(5)
        );
        match outcome {
            WindowOutcome::Won { proceeds_usd } => {
                // 10 shares × 0.55 = 5.50 (vs 10 × 0.50 = 5.00 spent)
                assert!(proceeds_usd >= dec!(5.40) && proceeds_usd <= dec!(5.60));
            }
            _ => panic!("expected Won, got {outcome:?}"),
        }
    }

    #[test]
    fn fixed_time_exit_loss() {
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        for _ in 0..300 { prices.push((dec!(0.45), dec!(0.45))); }
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Up),
            &config_with_exit(ExitRule::FixedTime { seconds: 60 }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Lost { .. }));
    }

    #[test]
    fn tp_only_no_trigger_but_wins_at_resolution() {
        // Constant 0.50 throughout — TP never hits, but we picked winner
        let oracle = flat_window("0.50");
        let outcome = simulate_window(
            &make_window(Direction::Up),
            &config_with_exit(ExitRule::TpOnlyOrHold { tp_price: dec!(0.75) }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Won { .. }));
    }
}
