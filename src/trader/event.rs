use crate::trader::errors::EmitError;
use crate::trader::exit_watcher::ExitKind;
use crate::trader::ladder::{Direction, LadderState, StopReason, WindowOutcome};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderKind { Buy, Sell }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WinLose { Win, Lose }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryDecision {
    Enter { ask: Decimal },
    SkipBand { ask: Decimal },
    SkipNotFound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraderEventKind {
    SessionStarted,
    SessionStopped { reason: StopReason },
    WindowOpening { window_ts: i64, slug: String },
    EntryDecision { decision: EntryDecision },
    OrderPlaced { kind: OrderKind, dollars: Decimal, token_id: String },
    OrderFilled { fill_price: Decimal, shares: Decimal, dollars: Decimal },
    OrderRejected { reason: String },
    Resolved { winner: Direction, our_side: Direction, our_outcome: WinLose },
    ResolutionTimeout,
    ExitTriggered {
        kind: ExitKind,
        bid: Decimal,
    },
    SellFilled { proceeds_usd: Decimal },
    SellRejected { reason: String },
    LadderUpdated { from_step: u8, to_step: u8, outcome: WindowOutcome },
    Alert { message: String },
    BuyLimitPosted { order_id: String, price: Decimal },
    BuyLimitSwept { from_price: Decimal, to_price: Decimal },
    TpLimitPosted { order_id: String, price: Decimal },
    TpLimitFilled {
        order_id: String,
        fill_price: Decimal,
        shares: Decimal,
        partial: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraderEvent {
    pub ts: DateTime<Utc>,
    pub session_id: Uuid,
    pub kind: TraderEventKind,
    pub ladder: LadderState,
}

#[async_trait]
pub trait TraderEventEmitter: Send + Sync {
    async fn emit(&self, event: &TraderEvent) -> Result<(), EmitError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::str::FromStr;

    fn fake_ladder() -> LadderState {
        LadderState::new(Direction::Up, Decimal::from(5), 5,
                         Utc.timestamp_opt(1700000000, 0).unwrap())
    }

    fn fake_event(kind: TraderEventKind) -> TraderEvent {
        TraderEvent {
            ts: Utc.timestamp_opt(1700000100, 0).unwrap(),
            session_id: Uuid::nil(),
            kind,
            ladder: fake_ladder(),
        }
    }

    #[test]
    fn session_started_roundtrip() {
        let e = fake_event(TraderEventKind::SessionStarted);
        let back: TraderEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn order_filled_roundtrip() {
        let e = fake_event(TraderEventKind::OrderFilled {
            fill_price: Decimal::from_str("0.50").unwrap(),
            shares: Decimal::from(10),
            dollars: Decimal::from(5),
        });
        let back: TraderEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn entry_decisions_serialize_distinctly() {
        let enter = EntryDecision::Enter { ask: Decimal::from_str("0.50").unwrap() };
        let skip_band = EntryDecision::SkipBand { ask: Decimal::from_str("0.62").unwrap() };
        let skip_nf = EntryDecision::SkipNotFound;
        assert_ne!(serde_json::to_string(&enter).unwrap(),
                   serde_json::to_string(&skip_band).unwrap());
        assert_ne!(serde_json::to_string(&skip_band).unwrap(),
                   serde_json::to_string(&skip_nf).unwrap());
    }

    #[test]
    fn resolved_roundtrip() {
        let e = fake_event(TraderEventKind::Resolved {
            winner: Direction::Up,
            our_side: Direction::Up,
            our_outcome: WinLose::Win,
        });
        let back: TraderEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn ladder_updated_with_outcome() {
        use crate::trader::ladder::{SkipReason, WindowOutcome};
        let e = fake_event(TraderEventKind::LadderUpdated {
            from_step: 2,
            to_step: 1,
            outcome: WindowOutcome::Won { proceeds_usd: Decimal::from(10) },
        });
        let back: TraderEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);

        let skip = fake_event(TraderEventKind::LadderUpdated {
            from_step: 2,
            to_step: 2,
            outcome: WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed },
        });
        let back2: TraderEvent = serde_json::from_str(&serde_json::to_string(&skip).unwrap()).unwrap();
        assert_eq!(skip, back2);
    }

    #[test]
    fn alert_message_preserved() {
        let e = fake_event(TraderEventKind::Alert { message: "stuck shares".into() });
        let back: TraderEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn exit_triggered_roundtrip() {
        use crate::trader::exit_watcher::ExitKind;
        let e = fake_event(TraderEventKind::ExitTriggered {
            kind: ExitKind::Tp,
            bid: Decimal::from_str("0.86").unwrap(),
        });
        let back: TraderEvent =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn exit_triggered_tp_and_sl_serialize_distinctly() {
        use crate::trader::exit_watcher::ExitKind;
        let tp = TraderEventKind::ExitTriggered {
            kind: ExitKind::Tp,
            bid: Decimal::from_str("0.85").unwrap(),
        };
        let sl = TraderEventKind::ExitTriggered {
            kind: ExitKind::Sl,
            bid: Decimal::from_str("0.45").unwrap(),
        };
        assert_ne!(serde_json::to_string(&tp).unwrap(),
                   serde_json::to_string(&sl).unwrap());
    }

    #[test]
    fn buy_limit_posted_serde_roundtrip() {
        let e = fake_event(TraderEventKind::BuyLimitPosted {
            order_id: "ord-1".into(),
            price: Decimal::from_str("0.49").unwrap(),
        });
        let back: TraderEvent =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn buy_limit_swept_serde_roundtrip() {
        let e = fake_event(TraderEventKind::BuyLimitSwept {
            from_price: Decimal::from_str("0.49").unwrap(),
            to_price: Decimal::from_str("0.50").unwrap(),
        });
        let back: TraderEvent =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn tp_limit_posted_serde_roundtrip() {
        let e = fake_event(TraderEventKind::TpLimitPosted {
            order_id: "ord-2".into(),
            price: Decimal::from_str("0.85").unwrap(),
        });
        let back: TraderEvent =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn tp_limit_filled_partial_serde_roundtrip() {
        let e = fake_event(TraderEventKind::TpLimitFilled {
            order_id: "ord-2".into(),
            fill_price: Decimal::from_str("0.85").unwrap(),
            shares: Decimal::from(6),
            partial: true,
        });
        let back: TraderEvent =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }
}
