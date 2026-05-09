use crate::cache::BalanceCache;
use crate::domain::{AppEvent, Balance, HealthLed, RefreshStatus};
use crate::refresher::Cmd;
use crate::trader::event::TraderEvent;
use crate::ui::{self, UiState};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::Backend};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraderHealth {
    NotStarted,
    Healthy,
    Lagging,
    Stale,
    Stopped,
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub redis_ok: bool,
    pub refresh_interval: Duration,
    pub should_quit: bool,
    pub trader_log: VecDeque<TraderEvent>,
    pub trader_latest: Option<TraderEvent>,
    pub trader_health: TraderHealth,
}

impl AppState {
    pub fn new(refresh_interval: Duration) -> Self {
        Self {
            balance: None,
            last_refresh: None,
            redis_ok: false,
            refresh_interval,
            should_quit: false,
            trader_log: VecDeque::with_capacity(64),
            trader_latest: None,
            trader_health: TraderHealth::NotStarted,
        }
    }

    pub fn ui_state(&self, now: DateTime<Utc>) -> UiState {
        let clob_health = HealthLed::from_clob_age(self.last_refresh.as_ref(), self.refresh_interval, now);
        let redis_health = if self.redis_ok { HealthLed::Green } else { HealthLed::Red };
        UiState {
            balance: self.balance.clone(),
            last_refresh: self.last_refresh.clone(),
            clob_health,
            redis_health,
            refresh_interval: self.refresh_interval,
            now,
            trader_log: self.trader_log.iter().cloned().collect(),
            trader_latest: self.trader_latest.clone(),
            trader_health: self.trader_health,
        }
    }
}

pub fn handle_event(state: &mut AppState, ev: AppEvent, cmd_tx: &mpsc::Sender<Cmd>) {
    match ev {
        AppEvent::Tick => {}
        AppEvent::Shutdown => state.should_quit = true,
        AppEvent::Refresh(s) => state.last_refresh = Some(s),
        AppEvent::Key(k) => match (k.code, k.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => state.should_quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => state.should_quit = true,
            (KeyCode::Char('r'), _) => { let _ = cmd_tx.try_send(Cmd::ForceRefresh); }
            _ => {}
        },
        AppEvent::TraderEvent(ev) => {
            if state.trader_log.len() >= 64 {
                state.trader_log.pop_front();
            }
            state.trader_log.push_back(ev.clone());
            state.trader_latest = Some(ev);
        }
        AppEvent::MarketUpdate(_) => {}
    }
}

/// One tick of the main loop. Reads cache, updates state, returns the new state.
/// Exposed for tests to drive the loop deterministically.
pub async fn tick_once(state: &mut AppState, cache: &dyn BalanceCache) {
    state.redis_ok = cache.ping().await.is_ok();

    if let Ok(Some(b)) = cache.get().await {
        state.balance = Some(b);
    }
    // Ok(None) and Err(_) both leave the last known balance untouched.

    state.trader_health = compute_trader_health(&state.trader_latest, chrono::Utc::now());
}

pub fn compute_trader_health(latest: &Option<TraderEvent>, now: chrono::DateTime<chrono::Utc>) -> TraderHealth {
    use crate::trader::event::TraderEventKind;
    let Some(ev) = latest else { return TraderHealth::NotStarted; };

    if matches!(ev.kind, TraderEventKind::SessionStopped { .. }) {
        return TraderHealth::Stopped;
    }
    let age = now.signed_duration_since(ev.ts).num_seconds().max(0) as u64;
    if age < 6 * 60 { TraderHealth::Healthy }
    else if age < 12 * 60 { TraderHealth::Lagging }
    else { TraderHealth::Stale }
}

pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    cache: Arc<dyn BalanceCache>,
    cmd_tx: mpsc::Sender<Cmd>,
    mut events: mpsc::Receiver<AppEvent>,
    refresh_interval: Duration,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let mut state = AppState::new(refresh_interval);
    let mut render_ticker = tokio::time::interval(Duration::from_millis(250));

    loop {
        if state.should_quit { break; }

        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            Some(ev) = events.recv() => handle_event(&mut state, ev, &cmd_tx),
            _ = render_ticker.tick() => {
                tick_once(&mut state, cache.as_ref()).await;
                let now = Utc::now();
                let snap = state.ui_state(now);
                terminal.draw(|f| ui::render(f, &snap))?;
            }
        }
    }
    shutdown.cancel();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Balance;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use std::sync::Mutex;

    struct MemCache { state: Mutex<Option<Balance>>, fail: Mutex<bool> }
    impl MemCache {
        fn new() -> Arc<Self> { Arc::new(Self { state: Mutex::new(None), fail: Mutex::new(false) }) }
        fn with(b: Balance) -> Arc<Self> {
            Arc::new(Self { state: Mutex::new(Some(b)), fail: Mutex::new(false) })
        }
    }
    #[async_trait]
    impl BalanceCache for MemCache {
        async fn get(&self) -> Result<Option<Balance>, crate::domain::CacheError> {
            if *self.fail.lock().unwrap() { return Err(crate::domain::CacheError::Disconnected); }
            Ok(self.state.lock().unwrap().clone())
        }
        async fn set(&self, b: &Balance) -> Result<(), crate::domain::CacheError> {
            *self.state.lock().unwrap() = Some(b.clone()); Ok(())
        }
        async fn ping(&self) -> Result<(), crate::domain::CacheError> {
            if *self.fail.lock().unwrap() { return Err(crate::domain::CacheError::Disconnected); }
            Ok(())
        }
    }

    fn key(c: char) -> AppEvent {
        AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    #[tokio::test]
    async fn quit_key_sets_should_quit() {
        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, _rx) = mpsc::channel(1);
        handle_event(&mut s, key('q'), &tx);
        assert!(s.should_quit);
    }

    #[tokio::test]
    async fn ctrl_c_sets_should_quit() {
        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, _rx) = mpsc::channel(1);
        let ev = AppEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        handle_event(&mut s, ev, &tx);
        assert!(s.should_quit);
    }

    #[tokio::test]
    async fn refresh_key_sends_cmd() {
        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, mut rx) = mpsc::channel(1);
        handle_event(&mut s, key('r'), &tx);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::ForceRefresh));
    }

    #[tokio::test]
    async fn refresh_status_updates_last_refresh() {
        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, _rx) = mpsc::channel(1);
        let now = Utc.timestamp_opt(1000, 0).unwrap();
        handle_event(&mut s, AppEvent::Refresh(RefreshStatus::Ok { at: now }), &tx);
        assert!(matches!(s.last_refresh, Some(RefreshStatus::Ok { .. })));
    }

    #[tokio::test]
    async fn tick_once_reads_cache() {
        let b = Balance { usdc: Decimal::from_str("42").unwrap(), fetched_at: Utc::now() };
        let cache = MemCache::with(b.clone());
        let mut s = AppState::new(Duration::from_secs(30));
        tick_once(&mut s, cache.as_ref()).await;
        assert_eq!(s.balance.unwrap().usdc, b.usdc);
        assert!(s.redis_ok);
    }

    #[tokio::test]
    async fn tick_once_keeps_balance_on_cache_error() {
        let b = Balance { usdc: Decimal::from_str("99").unwrap(), fetched_at: Utc::now() };
        let cache = MemCache::with(b.clone());
        let mut s = AppState::new(Duration::from_secs(30));
        tick_once(&mut s, cache.as_ref()).await;          // populate
        *cache.fail.lock().unwrap() = true;
        tick_once(&mut s, cache.as_ref()).await;          // now errors
        assert_eq!(s.balance.unwrap().usdc, b.usdc);      // still there
        assert!(!s.redis_ok);                             // but flagged
    }

    #[tokio::test]
    async fn redis_ok_reflects_ping_not_get_none() {
        let cache = MemCache::new();           // empty cache
        let mut s = AppState::new(Duration::from_secs(30));
        tick_once(&mut s, cache.as_ref()).await;
        assert!(s.redis_ok, "ping ok with empty cache should be green");

        *cache.fail.lock().unwrap() = true;
        tick_once(&mut s, cache.as_ref()).await;
        assert!(!s.redis_ok, "ping fail should be red");
    }

    #[tokio::test]
    async fn trader_event_appended_to_log() {
        use crate::trader::event::{TraderEvent, TraderEventKind};
        use crate::trader::ladder::{Direction, LadderState};
        use rust_decimal::Decimal;
        use uuid::Uuid;

        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, _rx) = mpsc::channel(1);
        let ev = TraderEvent {
            ts: Utc::now(),
            session_id: Uuid::nil(),
            kind: TraderEventKind::SessionStarted,
            ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now()),
        };
        handle_event(&mut s, AppEvent::TraderEvent(ev.clone()), &tx);
        assert_eq!(s.trader_log.len(), 1);
        assert_eq!(s.trader_latest.as_ref().unwrap().session_id, ev.session_id);
    }

    #[tokio::test]
    async fn trader_log_caps_at_64() {
        use crate::trader::event::{TraderEvent, TraderEventKind};
        use crate::trader::ladder::{Direction, LadderState};
        use rust_decimal::Decimal;
        use uuid::Uuid;

        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, _rx) = mpsc::channel(1);
        for _ in 0..70 {
            let ev = TraderEvent {
                ts: Utc::now(), session_id: Uuid::nil(),
                kind: TraderEventKind::SessionStarted,
                ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now()),
            };
            handle_event(&mut s, AppEvent::TraderEvent(ev), &tx);
        }
        assert_eq!(s.trader_log.len(), 64);
    }

    #[test]
    fn trader_health_not_started_when_no_events() {
        use chrono::TimeZone;
        let now = Utc.timestamp_opt(1700000000, 0).unwrap();
        assert_eq!(compute_trader_health(&None, now), TraderHealth::NotStarted);
    }

    #[test]
    fn trader_health_healthy_under_6_min() {
        use crate::trader::event::{TraderEvent, TraderEventKind};
        use crate::trader::ladder::{Direction, LadderState};
        use chrono::{Duration as Cd, TimeZone};
        use rust_decimal::Decimal;
        use uuid::Uuid;

        let now = Utc.timestamp_opt(1700001000, 0).unwrap();
        let ev = TraderEvent {
            ts: now - Cd::seconds(120), session_id: Uuid::nil(),
            kind: TraderEventKind::SessionStarted,
            ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, now),
        };
        assert_eq!(compute_trader_health(&Some(ev), now), TraderHealth::Healthy);
    }

    #[test]
    fn trader_health_lagging_between_6_and_12_min() {
        use crate::trader::event::{TraderEvent, TraderEventKind};
        use crate::trader::ladder::{Direction, LadderState};
        use chrono::{Duration as Cd, TimeZone};
        use rust_decimal::Decimal;
        use uuid::Uuid;

        let now = Utc.timestamp_opt(1700001000, 0).unwrap();
        let ev = TraderEvent {
            ts: now - Cd::seconds(8 * 60), session_id: Uuid::nil(),
            kind: TraderEventKind::SessionStarted,
            ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, now),
        };
        assert_eq!(compute_trader_health(&Some(ev), now), TraderHealth::Lagging);
    }

    #[test]
    fn trader_health_stale_over_12_min() {
        use crate::trader::event::{TraderEvent, TraderEventKind};
        use crate::trader::ladder::{Direction, LadderState};
        use chrono::{Duration as Cd, TimeZone};
        use rust_decimal::Decimal;
        use uuid::Uuid;

        let now = Utc.timestamp_opt(1700001000, 0).unwrap();
        let ev = TraderEvent {
            ts: now - Cd::seconds(15 * 60), session_id: Uuid::nil(),
            kind: TraderEventKind::SessionStarted,
            ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, now),
        };
        assert_eq!(compute_trader_health(&Some(ev), now), TraderHealth::Stale);
    }

    #[test]
    fn trader_health_stopped_takes_priority() {
        use crate::trader::event::{TraderEvent, TraderEventKind};
        use crate::trader::ladder::{Direction, LadderState, StopReason};
        use chrono::{Duration as Cd, TimeZone};
        use rust_decimal::Decimal;
        use uuid::Uuid;

        let now = Utc.timestamp_opt(1700001000, 0).unwrap();
        let ev = TraderEvent {
            ts: now - Cd::seconds(30), session_id: Uuid::nil(),
            kind: TraderEventKind::SessionStopped { reason: StopReason::CapReached },
            ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, now),
        };
        assert_eq!(compute_trader_health(&Some(ev), now), TraderHealth::Stopped);
    }
}
