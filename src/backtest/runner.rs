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
        DirectionSignal::RsiWithTrendFilter { .. } => (fallback, false), // handled in run_strategy
        DirectionSignal::LateMomentum { .. } => (fallback, false), // handled in run_strategy
        DirectionSignal::IntraWindowMomentum { .. } => (fallback, false), // handled in run_strategy
    }
}

/// v1.14 passive maker: post a limit BID at `entry_price` for direction `dir`.
/// Walks the window second-by-second checking the oracle's ask for that side.
/// On the first second the ask drops to or below `entry_price`, simulate fill
/// at `entry_price`. After fill, walk to either TP trigger or window-close
/// resolution (depending on `exit_rule`). If ask never reaches entry_price by
/// t=240s, skip.
fn simulate_passive_maker(
    window: &crate::backtest::data::gamma_history::WindowMeta,
    oracle: &dyn crate::backtest::oracle::TokenPriceOracle,
    stake: Decimal,
    entry_price: Decimal,
    dir: Direction,
    exit_rule: &crate::backtest::config::ExitRule,
) -> WindowOutcome {
    use crate::backtest::config::ExitRule;
    use rust_decimal_macros::dec;
    const ENTRY_CUTOFF: u32 = 240;
    // Walk t=0..ENTRY_CUTOFF looking for ask <= entry_price.
    let mut entry_t: Option<u32> = None;
    for t in 0..=ENTRY_CUTOFF {
        let (up_bid, up_ask) = oracle.price_at(window, t);
        let side_ask = match dir {
            Direction::Up => up_ask,
            Direction::Down => (Decimal::ONE - up_bid).max(Decimal::ZERO),
        };
        if side_ask > Decimal::ZERO && side_ask <= entry_price {
            entry_t = Some(t);
            break;
        }
    }
    let Some(entry_t) = entry_t else {
        return WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { ask: Decimal::ZERO } };
    };
    let shares = (stake / entry_price).floor();
    if shares < dec!(5) {
        return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
    }
    let dollars_spent = shares * entry_price;
    // Now walk forward looking for TP fill (if exit_rule is TpOnlyOrHold).
    let tp_price = match exit_rule {
        ExitRule::TpOnlyOrHold { tp_price } => Some(*tp_price),
        _ => None,
    };
    if let Some(tp) = tp_price {
        for t in entry_t..=300 {
            let (bid, _ask) = oracle.price_at(window, t);
            let side_bid = match dir {
                Direction::Up => bid,
                Direction::Down => (Decimal::ONE - oracle.price_at(window, t).1).max(Decimal::ZERO),
            };
            if side_bid >= tp {
                let proceeds = shares * side_bid;
                return WindowOutcome::Won { proceeds_usd: proceeds, cost_usd: dollars_spent };
            }
        }
    }
    // Fall through to resolution.
    if window.winner == Some(dir) {
        WindowOutcome::Won {
            proceeds_usd: shares * dec!(0.99),
            cost_usd: dollars_spent,
        }
    } else {
        WindowOutcome::Lost { spent_usd: dollars_spent }
    }
}

/// v1.14 LateMomentum: at `entry_offset_secs` into the window, compare current
/// BTC price (chainlink-equivalent via BinanceData) to `price_to_beat`. If the
/// gap exceeds `threshold_dollars`, BUY in the gap's direction at the oracle's
/// ask at that moment and hold to resolution. Otherwise skip.
fn simulate_late_momentum(
    window: &crate::backtest::data::gamma_history::WindowMeta,
    btc: Option<&crate::backtest::data::binance::BinanceData>,
    oracle: &dyn crate::backtest::oracle::TokenPriceOracle,
    stake: Decimal,
    entry_offset_secs: u32,
    threshold_dollars: f64,
) -> WindowOutcome {
    use rust_decimal_macros::dec;
    use rust_decimal::prelude::ToPrimitive;

    // Need BTC data + price_to_beat from window meta.
    let (Some(btc), Some(ptb)) = (btc, window.price_to_beat.to_f64()) else {
        return WindowOutcome::Skipped { reason: SkipReason::GammaApiUnavailable };
    };
    let entry_ts = window.window_ts + entry_offset_secs as i64;
    let Some(current_btc) = btc.price_at(entry_ts) else {
        return WindowOutcome::Skipped { reason: SkipReason::GammaApiUnavailable };
    };
    let delta = current_btc - ptb;
    if delta.abs() < threshold_dollars {
        return WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { ask: Decimal::ZERO } };
    }
    let direction = if delta > 0.0 { Direction::Up } else { Direction::Down };
    // Oracle returns (bid, ask) for the UP token. For DOWN, we buy the DOWN
    // token whose ask ≈ 1 - up_bid (binary complementary market).
    let (up_bid, up_ask) = oracle.price_at(window, entry_offset_secs);
    let ask = match direction {
        Direction::Up => up_ask,
        Direction::Down => (Decimal::ONE - up_bid).max(Decimal::ZERO),
    };
    if ask <= Decimal::ZERO {
        return WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { ask } };
    }
    let shares = (stake / ask).floor();
    if shares < dec!(5) {
        return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
    }
    let dollars_spent = shares * ask;
    let our_won = window.winner == Some(direction);
    if our_won {
        // Resolution payout: shares × $1.00.
        WindowOutcome::Won {
            proceeds_usd: shares * dec!(1.00),
            cost_usd: dollars_spent,
        }
    } else {
        WindowOutcome::Lost { spent_usd: dollars_spent }
    }
}

/// v1.16 IntraWindowMomentum: scan 1Hz from `scan_start_secs` to
/// `scan_end_secs` looking for BTC to deviate `[bp_min, bp_max]` basis points
/// from `price_to_beat`. On trigger, enter the SAME side as the move
/// (momentum). Apply the configured exit rule.
fn simulate_intra_window_reversion(
    window: &crate::backtest::data::gamma_history::WindowMeta,
    btc: Option<&crate::backtest::data::binance::BinanceData>,
    oracle: &dyn crate::backtest::oracle::TokenPriceOracle,
    stake: Decimal,
    scan_start_secs: u32,
    scan_end_secs: u32,
    bp_min: i32,
    bp_max: i32,
    exit_rule: &crate::backtest::config::ExitRule,
) -> WindowOutcome {
    use rust_decimal_macros::dec;
    use rust_decimal::prelude::ToPrimitive;

    let (Some(btc), Some(ptb)) = (btc, window.price_to_beat.to_f64()) else {
        return WindowOutcome::Skipped { reason: SkipReason::GammaApiUnavailable };
    };
    if ptb <= 0.0 {
        return WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { ask: Decimal::ZERO } };
    }

    let mut trigger: Option<(u32, Direction)> = None;
    for t in scan_start_secs..=scan_end_secs {
        let ts = window.window_ts + t as i64;
        let Some(current_btc) = btc.price_at(ts) else { continue };
        let bp = ((current_btc - ptb) / ptb) * 10_000.0;
        let abs_bp = bp.abs();
        if abs_bp < bp_min as f64 || abs_bp > bp_max as f64 {
            continue;
        }
        // Bet WITH the direction BTC has moved (momentum).
        let dir = if bp > 0.0 { Direction::Up } else { Direction::Down };
        trigger = Some((t, dir));
        break;
    }
    let Some((entry_offset, direction)) = trigger else {
        return WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { ask: Decimal::ZERO } };
    };

    let (up_bid, up_ask) = oracle.price_at(window, entry_offset);
    let ask = match direction {
        Direction::Up => up_ask,
        Direction::Down => (Decimal::ONE - up_bid).max(Decimal::ZERO),
    };
    if ask <= Decimal::ZERO {
        return WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { ask } };
    }
    let shares = (stake / ask).floor();
    if shares < dec!(5) {
        return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
    }
    let dollars_spent = shares * ask;

    use crate::backtest::config::ExitRule;
    let resolve_outcome = || -> WindowOutcome {
        let our_won = window.winner == Some(direction);
        if our_won {
            WindowOutcome::Won { proceeds_usd: shares * dec!(1.00), cost_usd: dollars_spent }
        } else {
            WindowOutcome::Lost { spent_usd: dollars_spent }
        }
    };

    match exit_rule {
        ExitRule::HoldToResolution => resolve_outcome(),
        ExitRule::TpOnlyOrHold { tp_price } => {
            // Backtest only handles 5-min windows; hardcode 300s.
            let window_end_t: u32 = 300;
            for t in (entry_offset + 1)..=window_end_t {
                let (up_bid_t, _up_ask_t) = oracle.price_at(window, t);
                let our_bid = match direction {
                    Direction::Up => up_bid_t,
                    Direction::Down => (Decimal::ONE - up_bid_t).max(Decimal::ZERO),
                };
                if our_bid >= *tp_price {
                    let proceeds = shares * *tp_price;
                    if proceeds > dollars_spent {
                        return WindowOutcome::Won { proceeds_usd: proceeds, cost_usd: dollars_spent };
                    } else {
                        return WindowOutcome::Lost { spent_usd: dollars_spent - proceeds };
                    }
                }
            }
            resolve_outcome()
        }
        _ => resolve_outcome(),
    }
}

pub fn run_strategy(
    strategy: &StrategyConfig,
    windows: &[WindowMeta],
    oracle: &dyn TokenPriceOracle,
    btc: Option<&BinanceData>,
) -> StrategyRunResult {
    run_strategy_with_opts(strategy, windows, oracle, btc, false)
}

/// v1.12: same as `run_strategy` but with `stop_at_cap` — when true, the
/// runner exits at the first cap event (mirrors the live trader's
/// `StopReason::CapReached` behavior). Used to validate that backtest
/// reproduces a specific live session's outcome.
pub fn run_strategy_with_opts(
    strategy: &StrategyConfig,
    windows: &[WindowMeta],
    oracle: &dyn TokenPriceOracle,
    btc: Option<&BinanceData>,
    stop_at_cap: bool,
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
            if stop_at_cap {
                // Mirror the live trader's exit-on-cap behavior for validation runs.
                break;
            }
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

        // v1.14: passive maker entry — wait for ask to drop to fixed bid
        // price, enter there. Requires RSI filter or direction to pick a side.
        if let Some(entry_price) = strategy.passive_entry_price {
            // First, determine direction (use existing RSI signal if any).
            let dir = match &strategy.direction_signal {
                Some(DirectionSignal::RsiFilterSkipNeutral { period, oversold, overbought, .. }) => {
                    let rsi = btc.and_then(|b| b.rsi_at(window.window_ts, *period));
                    match rsi {
                        Some(r) if r < *oversold => Some(Direction::Up),
                        Some(r) if r > *overbought => Some(Direction::Down),
                        _ => None,
                    }
                }
                _ => Some(strategy.direction),
            };
            let outcome = match dir {
                None => WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { ask: Decimal::ZERO } },
                Some(d) => simulate_passive_maker(window, oracle, stake, entry_price, d, &strategy.exit),
            };
            prev_winner = window.winner;
            ladder = apply_outcome(&ladder, &outcome, Utc::now());
            session_pnl = ladder.realized_pnl_usd;
            history.push(WindowResult {
                window_ts: window.window_ts, stake, outcome,
                ladder_step_before: step_before,
                ladder_step_after: ladder.current_step,
                ladder_pnl_after: total_pnl + session_pnl,
            });
            continue;
        }

        // v1.14: LateMomentum is handled completely separately — it enters
        // mid-window (not t=0) and uses chainlink-based outcome rather than
        // walking the oracle. Short-circuit here before the t=0 entry path.
        if let Some(DirectionSignal::IntraWindowMomentum { scan_start_secs, scan_end_secs, bp_min, bp_max }) = &strategy.direction_signal {
            let outcome = simulate_intra_window_reversion(
                window, btc, oracle, stake,
                *scan_start_secs, *scan_end_secs, *bp_min, *bp_max,
                &strategy.exit,
            );
            prev_winner = window.winner;
            ladder = apply_outcome(&ladder, &outcome, Utc::now());
            session_pnl = ladder.realized_pnl_usd;
            history.push(WindowResult {
                window_ts: window.window_ts, stake, outcome,
                ladder_step_before: step_before,
                ladder_step_after: ladder.current_step,
                ladder_pnl_after: total_pnl + session_pnl,
            });
            continue;
        }

        if let Some(DirectionSignal::LateMomentum { entry_offset_secs, threshold_dollars }) = &strategy.direction_signal {
            let outcome = simulate_late_momentum(
                window, btc, oracle, stake,
                *entry_offset_secs, *threshold_dollars,
            );
            prev_winner = window.winner;
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
            continue;
        }

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
            Some(DirectionSignal::RsiWithTrendFilter {
                period, oversold, overbought,
                ema_period, slope_lookback_mins, slope_threshold,
            }) => {
                let rsi = btc.and_then(|b| b.rsi_at(window.window_ts, *period));
                let slope = btc.and_then(|b| b.ema_slope_at(window.window_ts, *ema_period, *slope_lookback_mins));
                match (rsi, slope) {
                    (Some(rsi), Some(slope)) => {
                        if rsi < *oversold {
                            // Want to bet UP (mean-revert from oversold).
                            // If trend is strongly DOWN, skip — don't fight it.
                            if slope < -slope_threshold { (Direction::Up, true) }
                            else { (Direction::Up, false) }
                        } else if rsi > *overbought {
                            // Want to bet DOWN. Skip if strong UPtrend.
                            if slope > *slope_threshold { (Direction::Down, true) }
                            else { (Direction::Down, false) }
                        } else {
                            (strategy.direction, true) // RSI neutral → skip
                        }
                    }
                    // Missing data → fall back to RSI-only behavior.
                    _ => {
                        let dummy_signal = DirectionSignal::RsiFilterSkipNeutral {
                            period: *period, oversold: *oversold, overbought: *overbought,
                        };
                        pick_direction_with_signal(&dummy_signal, rsi, prev_winner, strategy.direction)
                    }
                }
            }
            Some(signal) => {
                let rsi = btc.and_then(|b| match signal {
                    DirectionSignal::RsiDirection { period, .. }
                    | DirectionSignal::RsiFilterSkipNeutral { period, .. }
                    | DirectionSignal::RsiPlusAntiFollow { period, .. } => {
                        b.rsi_at(window.window_ts, *period)
                    }
                    DirectionSignal::Random { .. }
                    | DirectionSignal::RsiWithTrendFilter { .. }
                    | DirectionSignal::LateMomentum { .. }
                    | DirectionSignal::IntraWindowMomentum { .. } => None,
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
            passive_entry_price: None,
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
