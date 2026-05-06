#![cfg(test)]

use chrono::Utc;
use poly_tui::{
    app::{self, AppState},
    cache::{BalanceCache, RedisBalanceCache},
    clob::BalanceFetcher,
    domain::{AppEvent, Balance, RefreshStatus},
    refresher::{self, Cmd},
};
use ratatui::{Terminal, backend::TestBackend};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;
use tokio::sync::mpsc;

#[path = "support/mod.rs"]
mod support;
use support::FakeFetcher;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "E2E must NOT bind dev Redis port");
    let url = format!("redis://127.0.0.1:{port}");
    (node, url)
}

fn buffer_string(term: &Terminal<TestBackend>) -> String {
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            // ratatui 0.29 — adjust if compile fails
            let cell = buf.cell((x, y)).unwrap();
            out.push_str(cell.symbol());
        }
        out.push('\n');
    }
    out
}

#[tokio::test]
#[ignore]
async fn e2e_full_path_renders_balance() {
    let (_node, url) = start_redis().await;
    let cache: Arc<dyn BalanceCache> = Arc::new(RedisBalanceCache::connect(&url).await.unwrap());
    let fetcher: Arc<dyn BalanceFetcher> = Arc::new(FakeFetcher::with_usdc("100.00"));
    let (status_tx, mut status_rx) = mpsc::channel::<RefreshStatus>(8);

    refresher::do_fetch(fetcher.as_ref(), cache.as_ref(), &status_tx).await;
    let s = status_rx.try_recv().unwrap();
    assert!(matches!(s, RefreshStatus::Ok { .. }));

    let mut state = AppState::new(Duration::from_secs(30));
    state.last_refresh = Some(s);
    app::tick_once(&mut state, cache.as_ref()).await;
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    term.draw(|f| poly_tui::ui::render(f, &state.ui_state(Utc::now()))).unwrap();

    let buf = buffer_string(&term);
    assert!(buf.contains("USDC: $100.00"), "buffer:\n{buf}");
}

#[tokio::test]
#[ignore]
async fn e2e_clob_down_keeps_cached_value() {
    let (_node, url) = start_redis().await;
    let cache: Arc<dyn BalanceCache> = Arc::new(RedisBalanceCache::connect(&url).await.unwrap());
    let fetcher = Arc::new(FakeFetcher::with_usdc("100.00"));
    let fetcher_dyn: Arc<dyn BalanceFetcher> = fetcher.clone();
    let (status_tx, mut status_rx) = mpsc::channel::<RefreshStatus>(8);

    refresher::do_fetch(fetcher_dyn.as_ref(), cache.as_ref(), &status_tx).await;
    let _ok = status_rx.try_recv().unwrap();

    fetcher.set_fail(true);
    refresher::do_fetch(fetcher_dyn.as_ref(), cache.as_ref(), &status_tx).await;
    let s = status_rx.try_recv().unwrap();
    assert!(matches!(s, RefreshStatus::Failed { .. }));

    let mut state = AppState::new(Duration::from_secs(30));
    state.last_refresh = Some(s);
    app::tick_once(&mut state, cache.as_ref()).await;
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    term.draw(|f| poly_tui::ui::render(f, &state.ui_state(Utc::now()))).unwrap();

    let buf = buffer_string(&term);
    assert!(buf.contains("USDC: $100.00"), "still shows last good value:\n{buf}");
    assert!(buf.contains("last: failed"), "shows failure status:\n{buf}");
}

#[tokio::test]
#[ignore]
async fn e2e_quit_key_terminates_cleanly() {
    let (_node, _url) = start_redis().await;
    let mut state = AppState::new(Duration::from_secs(30));
    let (cmd_tx, _cmd_rx) = mpsc::channel::<Cmd>(8);

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    app::handle_event(
        &mut state,
        AppEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        &cmd_tx,
    );
    assert!(state.should_quit);
}
