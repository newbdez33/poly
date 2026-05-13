use crate::backtest::config::{DirectionSignal, StakeRule, StrategyConfig};
use crate::backtest::data::binance::BinanceData;
use crate::backtest::data::gamma_history::WindowMeta;
use crate::backtest::exit_rule::simulate_window;
use crate::backtest::oracle::TokenPriceOracle;
use crate::trader::ladder::{apply_outcome, Direction, LadderState, SkipReason, WindowOutcome};
use chrono::Utc;
use rust_decimal::Decimal;

#[derive(Clone, Debug)]
pub struct WindowResult {
    pub window_ts: i64,
    pub stake: Decimal,
    pub outcome: WindowOutcome,
    pub ladder_step_before: u8,
    pub ladder_step_after: u8,
    pub ladder_pnl_after: Decimal,
}

#[derive(Clone, Debug)]
pub struct StrategyRunResult {
    pub name: String,
    pub windows: Vec<WindowResult>,
    pub cap_resets: u32,
    pub final_pnl: Decimal,
}

/// Returns (direction_to_bet, skip_window). When skip_window=true, the runner
/// emits a Skipped outcome and the ladder stays unchanged.
fn pick_direction_with_signal(
    signal: &DirectionSignal,
    rsi: Option<f64>,
    prev_winner: Option<Direction>,
    fallback: Direction,
) -> (Direction, bool) {
    // If RSI is unavailable (e.g. not enough history at window 0), fall back to
    // the strategy's fixed direction and never skip.
    let Some(rsi) = rsi else {
        return (fallback, false);
    };
    match signal {
        DirectionSignal::RsiDirection { oversold, overbought, .. } => {
            if rsi < *oversold { (Direction::Up, false) }
            else if rsi > *overbought { (Direction::Down, false) }
            else { (fallback, false) }
        }
        DirectionSignal::RsiFilterSkipNeutral { oversold, overbought, .. } => {
            if rsi < *oversold { (Direction::Up, false) }
            else if rsi > *overbought { (Direction::Down, false) }
            else { (fallback, true) }  // SKIP neutral zone
        }
        DirectionSignal::RsiPlusAntiFollow { oversold, overbought, .. } => {
            if rsi < *oversold { (Direction::Up, false) }
            else if rsi > *overbought { (Direction::Down, false) }
            else {
                // neutral zone: anti-follow-previous-winner
                let anti = match prev_winner {
                    Some(Direction::Up) => Direction::Down,
                    Some(Direction::Down) => Direction::Up,
                    None => fallback,
                };
                (anti, false)
            }
        }
        DirectionSignal::Random { .. } => (fallback, false), // handled in run_strategy
    }
}

pub fn run_strategy(
    strategy: &StrategyConfig,
    windows: &[WindowMeta],
    oracle: &dyn TokenPriceOracle,
    btc: Option<&BinanceData>,
) -> StrategyRunResult {
    // LadderState now uses u32 share counts; backtest keeps dollar-based stakes
    // for parity with the v1.4 baseline. We pass a placeholder base_shares=5
    // (the Polymarket minimum) since the actual stake comes from the manual
    // dollar computation below — the ladder's role here is FSM state only.
    let make_ladder = || LadderState::new(
        strategy.direction,
        5,
        match &strategy.stake {
            StakeRule::Martingale { max_step, .. } => *max_step,
            StakeRule::Fixed { .. } => 5,
        },
        Utc::now(),
    );

    let mut ladder = make_ladder();
    let mut session_pnl = Decimal::ZERO;     // per-ladder-session running pnl
    let mut total_pnl = Decimal::ZERO;       // accumulated across cap resets
    let mut cap_resets = 0;
    let mut history = Vec::with_capacity(windows.len());
    let mut prev_winner: Option<crate::trader::ladder::Direction> = None;

    for window in windows {
        if ladder.is_stopped() {
            cap_resets += 1;
            total_pnl += session_pnl;
            session_pnl = Decimal::ZERO;
            ladder = make_ladder();
        }

        let stake = match &strategy.stake {
            StakeRule::Martingale { base, .. } => {
                // step N → base × 2^(N-1). Manual dollar computation; ladder
                // only tracks FSM state, not share counts here.
                let multiplier = 2_u32.pow((ladder.current_step - 1) as u32);
                *base * Decimal::from(multiplier)
            }
            StakeRule::Fixed { stake } => *stake,
        };
        let step_before = ladder.current_step;

        // Per-window direction selection.
        // v1.11 direction_signal takes precedence over v1.10 follow_previous.
        let (effective_dir, skip_neutral) = match &strategy.direction_signal {
            Some(DirectionSignal::Random { seed }) => {
                // Deterministic 50/50 via SplitMix64 finalizer
                // (window_ts is always a multiple of 300, so bit-0 alone is biased — need full mixing).
                let mut x = seed.wrapping_add(window.window_ts as u64)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15);
                x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                x ^= x >> 31;
                let dir = if x & 1 == 0 { Direction::Up } else { Direction::Down };
                (dir, false)
            }
            Some(signal) => {
                let rsi = btc.and_then(|b| match signal {
                    DirectionSignal::RsiDirection { period, .. }
                    | DirectionSignal::RsiFilterSkipNeutral { period, .. }
                    | DirectionSignal::RsiPlusAntiFollow { period, .. } => {
                        b.rsi_at(window.window_ts, *period)
                    }
                    DirectionSignal::Random { .. } => None,
                });
                pick_direction_with_signal(signal, rsi, prev_winner, strategy.direction)
            }
            None => {
                let dir = if strategy.follow_previous_winner {
                    prev_winner.unwrap_or(strategy.direction)
                } else {
                    strategy.direction
                };
                (dir, false)
            }
        };

        let outcome = if skip_neutral {
            WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { ask: Decimal::ZERO } }
        } else {
            let effective_strategy = StrategyConfig {
                direction: effective_dir,
                ..strategy.clone()
            };
            simulate_window(window, &effective_strategy, oracle, stake)
        };
        prev_winner = window.winner;

        // Apply outcome to ladder (Martingale FSM); for Fixed stake, ladder stays at step 1
        // since we override stake on next iter, but apply_outcome still tracks pnl.
        ladder = apply_outcome(&ladder, &outcome, Utc::now());
        session_pnl = ladder.realized_pnl_usd;

        history.push(WindowResult {
            window_ts: window.window_ts,
            stake,
            outcome,
            ladder_step_before: step_before,
            ladder_step_after: ladder.current_step,
            ladder_pnl_after: total_pnl + session_pnl,
        });
    }

    total_pnl += session_pnl;

    StrategyRunResult {
        name: strategy.name.clone(),
        windows: history,
        cap_resets,
        final_pnl: total_pnl,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::config::{ExitRule, StakeRule};
    use crate::backtest::oracle::TokenPriceOracle;
    use crate::trader::ladder::Direction;
    use rust_decimal_macros::dec;

    /// Stub oracle: returns price 0.50 at all times (simulates a flat market).
    struct FlatOracle;
    impl TokenPriceOracle for FlatOracle {
        fn price_at(&self, _window: &WindowMeta, _t_secs: u32) -> (Decimal, Decimal) {
            (dec!(0.50), dec!(0.50))
        }
    }

    fn make_windows(winners: Vec<Direction>) -> Vec<WindowMeta> {
        winners.into_iter().enumerate().map(|(i, w)| WindowMeta {
            window_ts: 1000 + i as i64 * 300,
            price_to_beat: dec!(80000),
            final_price: Some(dec!(80050)),
            winner: Some(w),
            condition_id: None,
        }).collect()
    }

    fn martingale_strategy() -> StrategyConfig {
        StrategyConfig {
            name: "test_mart".into(),
            direction: Direction::Up,
            band_min: dec!(0.45), band_max: dec!(0.55),
            stake: StakeRule::Martingale { base: dec!(5), max_step: 5 },
            exit: ExitRule::HoldToResolution,
            follow_previous_winner: false,
            direction_signal: None,
        }
    }

    fn fixed_strategy() -> StrategyConfig {
        StrategyConfig {
            stake: StakeRule::Fixed { stake: dec!(5) },
            ..martingale_strategy()
        }
    }

    #[test]
    fn martingale_advances_on_loss() {
        let windows = make_windows(vec![Direction::Down, Direction::Down, Direction::Down]);
        let result = run_strategy(&martingale_strategy(), &windows, &FlatOracle, None);
        // After 3 losses: ladder step 1 → 2 → 3 → 4
        assert_eq!(result.windows[0].stake, dec!(5));
        assert_eq!(result.windows[1].stake, dec!(10));
        assert_eq!(result.windows[2].stake, dec!(20));
    }

    #[test]
    fn martingale_resets_on_win() {
        let windows = make_windows(vec![Direction::Down, Direction::Up, Direction::Down]);
        let result = run_strategy(&martingale_strategy(), &windows, &FlatOracle, None);
        assert_eq!(result.windows[0].stake, dec!(5));   // step 1
        assert_eq!(result.windows[1].stake, dec!(10));  // step 2 (after loss)
        assert_eq!(result.windows[2].stake, dec!(5));   // step 1 (after win reset)
    }

    #[test]
    fn martingale_cap_reset_after_5_consecutive_losses() {
        let windows = make_windows(vec![Direction::Down; 6]);
        let result = run_strategy(&martingale_strategy(), &windows, &FlatOracle, None);
        // After 5 losses cap is reached. The 6th window starts a fresh session at step 1.
        assert_eq!(result.cap_resets, 1);
        // The 6th window's stake should be base ($5) again
        assert_eq!(result.windows[5].stake, dec!(5));
    }

    #[test]
    fn fixed_stake_never_advances_ladder() {
        let windows = make_windows(vec![Direction::Down; 5]);
        let result = run_strategy(&fixed_strategy(), &windows, &FlatOracle, None);
        // All stakes are $5; cap_resets = 0 because Fixed stake apply_outcome still moves
        // ladder, but our stake selection ignores ladder
        assert!(result.windows.iter().all(|w| w.stake == dec!(5)));
    }

    #[test]
    fn final_pnl_accumulates_correctly() {
        let windows = make_windows(vec![Direction::Up, Direction::Up]);
        let result = run_strategy(&fixed_strategy(), &windows, &FlatOracle, None);
        // 2 wins × ($4.90 each) = $9.80
        assert_eq!(result.final_pnl, dec!(9.80));
    }
}
