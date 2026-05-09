#![cfg(test)]

use chrono::Utc;
use poly_tui::trader::adapters::redis_state_wrapper::RedisTraderState;
use poly_tui::trader::adapters::redis_stream_wrapper::RedisTraderStream;
use poly_tui::trader::event::{TraderEventEmitter, TraderEventKind};
use poly_tui::trader::ladder::{Direction, LadderState, StopReason, WindowOutcome};
use poly_tui::trader::scheduler::{run, SchedulerConfig, SchedulerDeps, WindowExecutor};
use poly_tui::trader::state::TraderStateStore;
use poly_tui::tui::events::TraderEventStream;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "E2E must NOT bind dev Redis port");
    let url = format!("redis://127.0.0.1:{port}");
    (node, url)
}

struct ScriptedWindowExec {
    outcomes: std::sync::Mutex<Vec<WindowOutcome>>,
}

#[async_trait::async_trait]
impl WindowExecutor for ScriptedWindowExec {
    async fn execute(&self, _l: &LadderState, _ts: i64) -> WindowOutcome {
        let mut q = self.outcomes.lock().unwrap();
        if q.is_empty() {
            WindowOutcome::Won { proceeds_usd: Decimal::from(10) }
        } else {
            q.remove(0)
        }
    }
}

#[tokio::test]
#[ignore]
async fn e2e_full_session_5_wins() {
    let (_node, url) = start_redis().await;
    let store = Arc::new(RedisTraderState::connect(&url).await.unwrap());
    let emitter = Arc::new(RedisTraderStream::connect(&url).await.unwrap());
    tokio::time::pause();
    let outcomes = (0..5)
        .map(|_| WindowOutcome::Won {
            proceeds_usd: Decimal::from_str("9.90").unwrap(),
        })
        .collect();
    let exec = Arc::new(ScriptedWindowExec {
        outcomes: std::sync::Mutex::new(outcomes),
    });
    let ladder = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    let task = tokio::spawn(run(
        ladder,
        SchedulerDeps {
            window_exec: exec,
            state_store: store,
            emitter,
        },
        SchedulerConfig { max_windows: Some(5) },
        CancellationToken::new(),
    ));
    tokio::time::advance(Duration::from_secs(60 * 30)).await;
    let final_state = task.await.unwrap().unwrap();
    assert_eq!(final_state.windows_won, 5);
    assert!(final_state.realized_pnl_usd > Decimal::ZERO);
}

#[tokio::test]
#[ignore]
async fn e2e_cap_reached_stops_session() {
    let (_node, url) = start_redis().await;
    let store = Arc::new(RedisTraderState::connect(&url).await.unwrap());
    let emitter = Arc::new(RedisTraderStream::connect(&url).await.unwrap());
    tokio::time::pause();
    let losses: Vec<_> = (0..5)
        .map(|i| WindowOutcome::Lost {
            spent_usd: Decimal::from(5_u64 << i),
        })
        .collect();
    let exec = Arc::new(ScriptedWindowExec {
        outcomes: std::sync::Mutex::new(losses),
    });
    let ladder = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    let task = tokio::spawn(run(
        ladder,
        SchedulerDeps {
            window_exec: exec,
            state_store: store,
            emitter,
        },
        SchedulerConfig { max_windows: None },
        CancellationToken::new(),
    ));
    tokio::time::advance(Duration::from_secs(60 * 30)).await;
    let final_state = task.await.unwrap().unwrap();
    assert_eq!(final_state.stopped, Some(StopReason::CapReached));
}

#[tokio::test]
#[ignore]
async fn e2e_resume_from_redis() {
    let (_node, url) = start_redis().await;
    let store = Arc::new(RedisTraderState::connect(&url).await.unwrap());
    let mut s = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    s.current_step = 4;
    s.realized_pnl_usd = Decimal::from(-35);
    store.save(&s).await.unwrap();

    let restored = store.load().await.unwrap().expect("Some");
    assert_eq!(restored.current_step, 4);
    assert_eq!(restored.realized_pnl_usd, Decimal::from(-35));
}

#[tokio::test]
#[ignore]
async fn e2e_lock_prevents_double_run() {
    let (_node, url) = start_redis().await;
    let store_a = RedisTraderState::connect(&url).await.unwrap();
    let store_b = RedisTraderState::connect(&url).await.unwrap();
    assert!(store_a.try_lock("a", Duration::from_secs(60)).await.unwrap());
    assert!(!store_b.try_lock("b", Duration::from_secs(60)).await.unwrap());
}

#[tokio::test]
#[ignore]
async fn e2e_tui_subscribes_to_stream() {
    let (_node, url) = start_redis().await;
    let emitter = RedisTraderStream::connect(&url).await.unwrap();
    let stream = RedisTraderStream::connect(&url).await.unwrap();

    let s = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    let ev = poly_tui::trader::event::TraderEvent {
        ts: Utc::now(),
        session_id: Uuid::nil(),
        kind: TraderEventKind::SessionStarted,
        ladder: s,
    };
    emitter.emit(&ev).await.unwrap();

    let tail = stream.tail(10).await.unwrap();
    assert!(!tail.history.is_empty());
}

#[tokio::test]
#[ignore]
async fn e2e_exit_triggered_event_reaches_stream() {
    // Build a TraderEvent with TraderEventKind::ExitTriggered, push it through
    // RedisTraderStream, then read it back via tail() and verify it round-trips
    // with kind/bid/proceeds intact. Validates the new variant is wire-compatible
    // with the existing event log.
    use poly_tui::trader::exit_watcher::ExitKind;

    let (_node, url) = start_redis().await;
    let emitter = RedisTraderStream::connect(&url).await.unwrap();
    let stream = RedisTraderStream::connect(&url).await.unwrap();

    let ladder = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    let session_id = ladder.session_id;
    let event = poly_tui::trader::event::TraderEvent {
        ts: Utc::now(),
        session_id,
        kind: TraderEventKind::ExitTriggered {
            kind: ExitKind::Tp,
            bid: Decimal::from_str("0.86").unwrap(),
            proceeds_usd: Decimal::from_str("8.40").unwrap(),
        },
        ladder,
    };
    emitter.emit(&event).await.unwrap();

    let tail = stream.tail(10).await.unwrap();
    let recv_event = tail
        .history
        .iter()
        .find(|e| matches!(e.kind, TraderEventKind::ExitTriggered { .. }))
        .expect("ExitTriggered event should appear in tail");

    match &recv_event.kind {
        TraderEventKind::ExitTriggered { kind, bid, proceeds_usd } => {
            assert_eq!(*kind, ExitKind::Tp);
            assert_eq!(*bid, Decimal::from_str("0.86").unwrap());
            assert_eq!(*proceeds_usd, Decimal::from_str("8.40").unwrap());
        }
        other => panic!("unexpected event kind: {other:?}"),
    }
}
