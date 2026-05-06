use crate::cache::BalanceCache;
use crate::domain::{AppEvent, Balance, HealthLed, RefreshStatus};
use crate::refresher::Cmd;
use crate::ui::{self, UiState};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::Backend};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct AppState {
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub redis_ok: bool,
    pub refresh_interval: Duration,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(refresh_interval: Duration) -> Self {
        Self {
            balance: None,
            last_refresh: None,
            redis_ok: false,
            refresh_interval,
            should_quit: false,
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
    }
}

/// One tick of the main loop. Reads cache, updates state, returns the new state.
/// Exposed for tests to drive the loop deterministically.
pub async fn tick_once(state: &mut AppState, cache: &dyn BalanceCache) {
    match cache.get().await {
        Ok(Some(b)) => { state.balance = Some(b); state.redis_ok = true; }
        Ok(None) => { state.redis_ok = true; /* keep last balance */ }
        Err(_) => { state.redis_ok = false; }
    }
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
        async fn ping(&self) -> Result<(), crate::domain::CacheError> { Ok(()) }
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
}
