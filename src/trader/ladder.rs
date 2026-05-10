use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction { Up, Down }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    CapReached,
    ManualStop,
    FatalError(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipReason {
    PriceOutsideBand { ask: Decimal },
    FillOrKillFailed,
    ResolutionTimeout,
    GammaApiUnavailable,
    MarketNotFound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowOutcome {
    Won { proceeds_usd: Decimal },
    Lost { spent_usd: Decimal },
    Skipped { reason: SkipReason },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderState {
    pub session_id: Uuid,
    pub direction: Direction,
    pub base_usd: Decimal,
    pub max_step: u8,
    pub current_step: u8,
    pub session_started_at: DateTime<Utc>,
    pub realized_pnl_usd: Decimal,
    pub windows_won: u32,
    pub windows_lost: u32,
    pub windows_skipped: u32,
    pub stopped: Option<StopReason>,
    /// Trading window length in minutes. {5, 15, 60}. Pre-v1.7.1 ladder JSON
    /// omits this field; serde(default) restores 5min behavior on legacy state.
    #[serde(default = "default_window_minutes")]
    pub window_minutes: u32,
}

fn default_window_minutes() -> u32 { 5 }

impl LadderState {
    pub fn new(direction: Direction, base_usd: Decimal, max_step: u8, now: DateTime<Utc>) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            direction, base_usd, max_step,
            current_step: 1,
            session_started_at: now,
            realized_pnl_usd: Decimal::ZERO,
            windows_won: 0, windows_lost: 0, windows_skipped: 0,
            stopped: None,
            window_minutes: 5,
        }
    }

    /// Builder-style override for `window_minutes`. Use after `new()`.
    pub fn with_window_minutes(mut self, mins: u32) -> Self {
        self.window_minutes = mins;
        self
    }

    pub fn current_bet_usd(&self) -> Decimal {
        let multiplier = 2_u64.pow((self.current_step - 1) as u32);
        self.base_usd * Decimal::from(multiplier)
    }

    pub fn is_stopped(&self) -> bool { self.stopped.is_some() }
}

/// Pure FSM transition. No I/O. `_now` reserved for future state-time derived fields.
pub fn apply_outcome(
    state: &LadderState,
    outcome: &WindowOutcome,
    _now: DateTime<Utc>,
) -> LadderState {
    let mut next = state.clone();
    match outcome {
        WindowOutcome::Won { proceeds_usd } => {
            let bet = state.current_bet_usd();
            next.realized_pnl_usd += proceeds_usd - bet;
            next.windows_won += 1;
            next.current_step = 1;
        }
        WindowOutcome::Lost { spent_usd } => {
            next.realized_pnl_usd -= spent_usd;
            next.windows_lost += 1;
            if state.current_step >= state.max_step {
                next.stopped = Some(StopReason::CapReached);
            } else {
                next.current_step += 1;
            }
        }
        WindowOutcome::Skipped { .. } => {
            next.windows_skipped += 1;
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::str::FromStr;

    fn ts() -> DateTime<Utc> { Utc.timestamp_opt(1_700_000_000, 0).unwrap() }

    fn fresh(step: u8) -> LadderState {
        LadderState {
            session_id: Uuid::nil(),
            direction: Direction::Up,
            base_usd: Decimal::from(5),
            max_step: 5,
            current_step: step,
            session_started_at: ts(),
            realized_pnl_usd: Decimal::ZERO,
            windows_won: 0, windows_lost: 0, windows_skipped: 0,
            stopped: None,
            window_minutes: 5,
        }
    }

    #[test]
    fn current_bet_doubles_each_step() {
        for (step, expected) in [(1u8, "5"), (2, "10"), (3, "20"), (4, "40"), (5, "80")] {
            assert_eq!(fresh(step).current_bet_usd(), Decimal::from_str(expected).unwrap());
        }
    }

    #[test]
    fn won_resets_step_credits_pnl() {
        let s = fresh(3);
        let bet = s.current_bet_usd();
        let next = apply_outcome(&s,
            &WindowOutcome::Won { proceeds_usd: Decimal::from_str("39.60").unwrap() }, ts());
        assert_eq!(next.current_step, 1);
        assert_eq!(next.windows_won, 1);
        assert_eq!(next.realized_pnl_usd, Decimal::from_str("39.60").unwrap() - bet);
        assert!(next.stopped.is_none());
    }

    #[test]
    fn lost_advances_step_debits_pnl() {
        let s = fresh(2);
        let next = apply_outcome(&s, &WindowOutcome::Lost { spent_usd: Decimal::from(10) }, ts());
        assert_eq!(next.current_step, 3);
        assert_eq!(next.windows_lost, 1);
        assert_eq!(next.realized_pnl_usd, Decimal::from(-10));
        assert!(next.stopped.is_none());
    }

    #[test]
    fn lost_at_max_step_sets_cap_reached() {
        let next = apply_outcome(&fresh(5),
            &WindowOutcome::Lost { spent_usd: Decimal::from(80) }, ts());
        assert_eq!(next.current_step, 5);
        assert_eq!(next.stopped, Some(StopReason::CapReached));
    }

    #[test]
    fn skipped_does_not_change_step_or_pnl() {
        let s = fresh(3);
        let next = apply_outcome(&s,
            &WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed }, ts());
        assert_eq!(next.current_step, 3);
        assert_eq!(next.realized_pnl_usd, s.realized_pnl_usd);
        assert_eq!(next.windows_skipped, 1);
        assert_eq!(next.windows_won, 0);
        assert_eq!(next.windows_lost, 0);
    }

    #[test]
    fn cumulative_loss_to_cap() {
        let mut s = LadderState::new(Direction::Up, Decimal::from(5), 5, ts());
        for _ in 0..5 {
            s = apply_outcome(&s, &WindowOutcome::Lost { spent_usd: s.current_bet_usd() }, ts());
        }
        assert_eq!(s.stopped, Some(StopReason::CapReached));
        assert_eq!(s.realized_pnl_usd, Decimal::from(-155));
        assert_eq!(s.windows_lost, 5);
    }

    #[test]
    fn serde_roundtrip_preserves_all_fields() {
        let s = fresh(4);
        let back: LadderState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn serde_roundtrip_with_stopped() {
        let mut s = fresh(5);
        s.stopped = Some(StopReason::CapReached);
        let back: LadderState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn skip_reasons_serialize_distinctly() {
        let band = WindowOutcome::Skipped {
            reason: SkipReason::PriceOutsideBand { ask: Decimal::from_str("0.62").unwrap() },
        };
        let fok = WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
        assert_ne!(serde_json::to_string(&band).unwrap(),
                   serde_json::to_string(&fok).unwrap());
    }

    #[test]
    fn new_session_starts_at_step_1() {
        let s = LadderState::new(Direction::Down, Decimal::from(5), 5, ts());
        assert_eq!(s.current_step, 1);
        assert_eq!(s.realized_pnl_usd, Decimal::ZERO);
        assert!(s.stopped.is_none());
    }

    #[test]
    fn property_step_within_bounds_for_any_outcome() {
        for start in 1..=5_u8 {
            for outcome in [
                WindowOutcome::Won { proceeds_usd: Decimal::from(10) },
                WindowOutcome::Lost { spent_usd: Decimal::from(5) },
                WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed },
            ] {
                let next = apply_outcome(&fresh(start), &outcome, ts());
                assert!(next.current_step >= 1 && next.current_step <= next.max_step);
            }
        }
    }

    #[test]
    fn ladder_default_window_minutes_is_5() {
        let s = LadderState::new(Direction::Up, Decimal::from(5), 5, ts());
        assert_eq!(s.window_minutes, 5);
    }

    #[test]
    fn ladder_with_window_minutes_builder() {
        let s = LadderState::new(Direction::Up, Decimal::from(5), 5, ts())
            .with_window_minutes(15);
        assert_eq!(s.window_minutes, 15);
    }

    #[test]
    fn ladder_serde_roundtrip_includes_window_minutes() {
        let s = LadderState::new(Direction::Up, Decimal::from(5), 5, ts())
            .with_window_minutes(15);
        let json = serde_json::to_string(&s).unwrap();
        let back: LadderState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.window_minutes, 15);
    }

    #[test]
    fn ladder_legacy_json_without_window_minutes_defaults_to_5() {
        // Pre-v1.7.1 ladder JSON has no window_minutes field.
        let legacy = r#"{
            "session_id": "00000000-0000-0000-0000-000000000000",
            "direction": "up",
            "base_usd": "5",
            "max_step": 5,
            "current_step": 1,
            "session_started_at": "2026-05-10T00:00:00Z",
            "realized_pnl_usd": "0",
            "windows_won": 0,
            "windows_lost": 0,
            "windows_skipped": 0,
            "stopped": null
        }"#;
        let s: LadderState = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.window_minutes, 5);
    }
}
