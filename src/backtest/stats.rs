use crate::backtest::runner::StrategyRunResult;
use crate::trader::ladder::WindowOutcome;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrategyStats {
    pub name: String,
    pub total_windows: u32,
    pub windows_won: u32,
    pub windows_lost: u32,
    pub windows_skipped: u32,
    pub win_rate: f64,
    pub total_pnl_usd: Decimal,
    pub ev_per_round: Decimal,
    pub ev_per_active_round: Decimal,
    pub cap_resets: u32,
    pub max_consecutive_losses: u32,
    pub max_step_reached: u8,
    pub max_drawdown_usd: Decimal,
    pub max_drawdown_window_ts: i64,
    pub equity_curve: Vec<EquityPoint>,
    pub round_pnls: Vec<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EquityPoint {
    pub window_ts: i64,
    pub cumulative_pnl: Decimal,
}

pub fn compute_stats(run: &StrategyRunResult) -> StrategyStats {
    let total_windows = run.windows.len() as u32;
    let mut wins = 0u32;
    let mut losses = 0u32;
    let mut skips = 0u32;
    let mut max_step = 1u8;
    let mut consec_losses = 0u32;
    let mut max_consec_losses = 0u32;
    let mut equity_curve = Vec::with_capacity(run.windows.len());
    let mut round_pnls = Vec::with_capacity(run.windows.len());
    let mut peak_pnl = Decimal::ZERO;
    let mut max_drawdown = Decimal::ZERO;
    let mut max_drawdown_ts = 0i64;

    let mut prev_pnl = Decimal::ZERO;
    for w in &run.windows {
        let round_pnl = w.ladder_pnl_after - prev_pnl;
        round_pnls.push(round_pnl.to_f64().unwrap_or(0.0));
        match &w.outcome {
            WindowOutcome::Won { .. } => { wins += 1; consec_losses = 0; }
            WindowOutcome::Lost { .. } => {
                losses += 1;
                consec_losses += 1;
                max_consec_losses = max_consec_losses.max(consec_losses);
            }
            WindowOutcome::Skipped { .. } => skips += 1,
        }
        max_step = max_step.max(w.ladder_step_after);

        // Drawdown: peak-to-trough
        if w.ladder_pnl_after > peak_pnl {
            peak_pnl = w.ladder_pnl_after;
        }
        let drawdown = peak_pnl - w.ladder_pnl_after;
        if drawdown > max_drawdown {
            max_drawdown = drawdown;
            max_drawdown_ts = w.window_ts;
        }

        equity_curve.push(EquityPoint {
            window_ts: w.window_ts,
            cumulative_pnl: w.ladder_pnl_after,
        });
        prev_pnl = w.ladder_pnl_after;
    }

    let active = wins + losses;
    let win_rate = if active > 0 { wins as f64 / active as f64 } else { 0.0 };
    let total_pnl = run.final_pnl;
    let ev_per_round = if total_windows > 0 {
        total_pnl / Decimal::from(total_windows)
    } else { Decimal::ZERO };
    let ev_per_active_round = if active > 0 {
        total_pnl / Decimal::from(active)
    } else { Decimal::ZERO };

    StrategyStats {
        name: run.name.clone(),
        total_windows,
        windows_won: wins,
        windows_lost: losses,
        windows_skipped: skips,
        win_rate,
        total_pnl_usd: total_pnl,
        ev_per_round,
        ev_per_active_round,
        cap_resets: run.cap_resets,
        max_consecutive_losses: max_consec_losses,
        max_step_reached: max_step,
        max_drawdown_usd: max_drawdown,
        max_drawdown_window_ts: max_drawdown_ts,
        equity_curve,
        round_pnls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::runner::WindowResult;
    use crate::trader::ladder::SkipReason;
    use rust_decimal_macros::dec;

    fn won(ts: i64, pnl: Decimal) -> WindowResult {
        WindowResult {
            window_ts: ts, stake: dec!(5),
            outcome: WindowOutcome::Won { proceeds_usd: dec!(9.90), cost_usd: dec!(5) },
            ladder_step_before: 1, ladder_step_after: 1,
            ladder_pnl_after: pnl,
        }
    }
    fn lost(ts: i64, pnl: Decimal, step_after: u8) -> WindowResult {
        WindowResult {
            window_ts: ts, stake: dec!(5),
            outcome: WindowOutcome::Lost { spent_usd: dec!(5) },
            ladder_step_before: step_after.saturating_sub(1),
            ladder_step_after: step_after,
            ladder_pnl_after: pnl,
        }
    }
    fn skipped(ts: i64, pnl: Decimal) -> WindowResult {
        WindowResult {
            window_ts: ts, stake: dec!(5),
            outcome: WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed },
            ladder_step_before: 1, ladder_step_after: 1,
            ladder_pnl_after: pnl,
        }
    }

    fn run(windows: Vec<WindowResult>, cap_resets: u32, final_pnl: Decimal) -> StrategyRunResult {
        StrategyRunResult { name: "test".into(), windows, cap_resets, final_pnl }
    }

    #[test]
    fn ev_per_round_uses_total_windows() {
        let r = run(vec![won(0, dec!(4.90)), lost(300, dec!(-0.10), 2)], 0, dec!(-0.10));
        let s = compute_stats(&r);
        assert_eq!(s.ev_per_round, dec!(-0.05));
    }

    #[test]
    fn win_rate_excludes_skips() {
        let r = run(vec![won(0, dec!(4.90)), lost(300, dec!(-0.10), 2), skipped(600, dec!(-0.10))], 0, dec!(-0.10));
        let s = compute_stats(&r);
        assert!((s.win_rate - 0.5).abs() < 1e-9);
        assert_eq!(s.windows_skipped, 1);
    }

    #[test]
    fn max_consecutive_losses_tracked() {
        let r = run(vec![lost(0, dec!(-5), 2), lost(300, dec!(-15), 3), lost(600, dec!(-35), 4), won(900, dec!(-15))], 0, dec!(-15));
        let s = compute_stats(&r);
        assert_eq!(s.max_consecutive_losses, 3);
    }

    #[test]
    fn max_drawdown_peak_to_trough() {
        let r = run(vec![
            won(0, dec!(10)),     // peak +10
            lost(300, dec!(0), 2),
            lost(600, dec!(-15), 3),
            lost(900, dec!(-35), 4),  // drawdown = 10 - (-35) = 45
        ], 0, dec!(-35));
        let s = compute_stats(&r);
        assert_eq!(s.max_drawdown_usd, dec!(45));
        assert_eq!(s.max_drawdown_window_ts, 900);
    }

    #[test]
    fn equity_curve_matches_pnl_history() {
        let r = run(vec![won(0, dec!(4.90)), won(300, dec!(9.80))], 0, dec!(9.80));
        let s = compute_stats(&r);
        assert_eq!(s.equity_curve.len(), 2);
        assert_eq!(s.equity_curve[1].cumulative_pnl, dec!(9.80));
    }

    #[test]
    fn cap_resets_passthrough() {
        let r = run(vec![lost(0, dec!(-155), 5)], 7, dec!(-1085));
        let s = compute_stats(&r);
        assert_eq!(s.cap_resets, 7);
    }
}
