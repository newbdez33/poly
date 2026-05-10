use crate::trader::errors::StateError;
use crate::trader::event::{TraderEvent, TraderEventEmitter, TraderEventKind};
use crate::trader::ladder::{apply_outcome, LadderState, StopReason, WindowOutcome};
use crate::trader::market::next_window_boundary;
use crate::trader::state::TraderStateStore;
use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Indirection for testing the scheduler without invoking real `run_window`.
#[async_trait]
pub trait WindowExecutor: Send + Sync {
    async fn execute(&self, ladder: &LadderState, window_ts: i64) -> WindowOutcome;
}

pub struct SchedulerDeps {
    pub window_exec: Arc<dyn WindowExecutor>,
    pub state_store: Arc<dyn TraderStateStore>,
    pub emitter: Arc<dyn TraderEventEmitter>,
}

pub struct SchedulerConfig {
    pub max_windows: Option<u32>,
    /// Window length in seconds (300/900/3600 for {5,15,60}-min windows).
    pub window_seconds: i64,
}

pub async fn run(
    initial: LadderState,
    deps: SchedulerDeps,
    cfg: SchedulerConfig,
    shutdown: CancellationToken,
) -> Result<LadderState, StateError> {
    let mut ladder = initial;
    let mut windows_run: u32 = 0;

    emit(&deps, &ladder, TraderEventKind::SessionStarted).await;

    loop {
        if ladder.is_stopped() { break; }
        if let Some(max) = cfg.max_windows {
            if windows_run >= max { break; }
        }

        // Wait until next window boundary, observing shutdown.
        let now_ts = chrono::Utc::now().timestamp();
        let mins = (cfg.window_seconds / 60) as u32;
        let next_ts = next_window_boundary(now_ts, mins);
        let wait = Duration::from_secs((next_ts - now_ts).max(0) as u64);

        tokio::select! {
            _ = tokio::time::sleep(wait) => {},
            _ = shutdown.cancelled() => {
                ladder.stopped = Some(StopReason::ManualStop);
                break;
            }
        }

        // Execute the window.
        let outcome = deps.window_exec.execute(&ladder, next_ts).await;

        let from_step = ladder.current_step;
        let new_ladder = apply_outcome(&ladder, &outcome, Utc::now());
        deps.state_store.save(&new_ladder).await?;
        emit(&deps, &new_ladder, TraderEventKind::LadderUpdated {
            from_step,
            to_step: new_ladder.current_step,
            outcome: outcome.clone(),
        }).await;

        ladder = new_ladder;
        windows_run += 1;
    }

    let stop_reason = ladder.stopped.clone().unwrap_or(StopReason::ManualStop);
    emit(&deps, &ladder, TraderEventKind::SessionStopped {
        reason: stop_reason,
    }).await;

    Ok(ladder)
}

async fn emit(deps: &SchedulerDeps, ladder: &LadderState, kind: TraderEventKind) {
    let event = TraderEvent {
        ts: Utc::now(),
        session_id: ladder.session_id,
        kind,
        ladder: ladder.clone(),
    };
    let _ = deps.emitter.emit(&event).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trader::event::TraderEvent;
    use crate::trader::errors::EmitError;
    use crate::trader::ladder::{Direction, SkipReason};
    use rust_decimal::Decimal;
    use std::sync::Mutex;

    #[derive(Default)]
    struct InMemoryStore {
        ladder: Mutex<Option<LadderState>>,
    }
    #[async_trait]
    impl TraderStateStore for InMemoryStore {
        async fn load(&self) -> Result<Option<LadderState>, StateError> {
            Ok(self.ladder.lock().unwrap().clone())
        }
        async fn save(&self, s: &LadderState) -> Result<(), StateError> {
            *self.ladder.lock().unwrap() = Some(s.clone()); Ok(())
        }
        async fn clear(&self) -> Result<(), StateError> {
            *self.ladder.lock().unwrap() = None; Ok(())
        }
        async fn try_lock(&self, _o: &str, _t: Duration) -> Result<bool, StateError> { Ok(true) }
        async fn refresh_lock(&self, _o: &str, _t: Duration) -> Result<(), StateError> { Ok(()) }
        async fn release_lock(&self, _o: &str) -> Result<(), StateError> { Ok(()) }
    }

    #[derive(Default)]
    struct CaptureEmitter {
        events: Mutex<Vec<TraderEvent>>,
    }
    #[async_trait]
    impl TraderEventEmitter for CaptureEmitter {
        async fn emit(&self, ev: &TraderEvent) -> Result<(), EmitError> {
            self.events.lock().unwrap().push(ev.clone());
            Ok(())
        }
    }

    struct ScriptedWindowExec {
        outcomes: Mutex<Vec<WindowOutcome>>,
    }
    #[async_trait]
    impl WindowExecutor for ScriptedWindowExec {
        async fn execute(&self, _l: &LadderState, _ts: i64) -> WindowOutcome {
            let mut q = self.outcomes.lock().unwrap();
            if q.is_empty() {
                return WindowOutcome::Skipped { reason: SkipReason::MarketNotFound };
            }
            q.remove(0)
        }
    }

    fn ladder() -> LadderState {
        LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now())
    }

    #[tokio::test]
    async fn max_windows_terminates_loop() {
        tokio::time::pause();
        let deps = SchedulerDeps {
            window_exec: Arc::new(ScriptedWindowExec {
                outcomes: Mutex::new(vec![
                    WindowOutcome::Won { proceeds_usd: Decimal::from(10) },
                    WindowOutcome::Won { proceeds_usd: Decimal::from(10) },
                    WindowOutcome::Won { proceeds_usd: Decimal::from(10) },
                ]),
            }),
            state_store: Arc::new(InMemoryStore::default()),
            emitter: Arc::new(CaptureEmitter::default()),
        };
        let token = CancellationToken::new();
        let task = tokio::spawn(run(ladder(), deps,
            SchedulerConfig { max_windows: Some(3), window_seconds: 300 }, token));
        tokio::time::advance(Duration::from_secs(60 * 60)).await;
        let final_state = task.await.unwrap().unwrap();
        assert_eq!(final_state.windows_won, 3);
    }

    #[tokio::test]
    async fn shutdown_signal_terminates_loop() {
        tokio::time::pause();
        let deps = SchedulerDeps {
            window_exec: Arc::new(ScriptedWindowExec {
                outcomes: Mutex::new(vec![]),
            }),
            state_store: Arc::new(InMemoryStore::default()),
            emitter: Arc::new(CaptureEmitter::default()),
        };
        let token = CancellationToken::new();
        let token2 = token.clone();
        let task = tokio::spawn(run(ladder(), deps,
            SchedulerConfig { max_windows: None, window_seconds: 300 }, token));
        token2.cancel();
        let final_state = task.await.unwrap().unwrap();
        assert_eq!(final_state.stopped, Some(StopReason::ManualStop));
    }

    #[tokio::test]
    async fn cap_reached_breaks_loop() {
        tokio::time::pause();
        let losses: Vec<WindowOutcome> = (0..5).map(|_|
            WindowOutcome::Lost { spent_usd: Decimal::from(5) }).collect();
        let deps = SchedulerDeps {
            window_exec: Arc::new(ScriptedWindowExec {
                outcomes: Mutex::new(losses),
            }),
            state_store: Arc::new(InMemoryStore::default()),
            emitter: Arc::new(CaptureEmitter::default()),
        };
        let token = CancellationToken::new();
        let task = tokio::spawn(run(ladder(), deps,
            SchedulerConfig { max_windows: None, window_seconds: 300 }, token));
        tokio::time::advance(Duration::from_secs(60 * 60)).await;
        let final_state = task.await.unwrap().unwrap();
        assert_eq!(final_state.stopped, Some(StopReason::CapReached));
    }

    #[test]
    fn scheduler_config_carries_window_seconds() {
        let c = SchedulerConfig { max_windows: None, window_seconds: 900 };
        assert_eq!(c.window_seconds, 900);
    }
}
