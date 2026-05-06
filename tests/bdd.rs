use chrono::Utc;
use cucumber::{given, then, when, World};
use poly_tui::{
    app::{self, AppState},
    domain::{AppEvent, Balance, RefreshStatus},
    refresher::{self, Cmd},
};
use ratatui::{Terminal, backend::TestBackend};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[path = "support/mod.rs"]
mod support;
use support::{FakeFetcher, InMemoryCache};

#[derive(Debug, World)]
#[world(init = Self::new)]
struct AppWorld {
    cache: Arc<InMemoryCache>,
    fetcher: Arc<FakeFetcher>,
    state: Option<AppState>,
    terminal: Option<Terminal<TestBackend>>,
    cmd_tx: Option<mpsc::Sender<Cmd>>,
    event_tx: Option<mpsc::Sender<AppEvent>>,
    last_buffer: String,
}

impl AppWorld {
    fn new() -> Self {
        Self {
            cache: Arc::new(InMemoryCache::new()),
            fetcher: Arc::new(FakeFetcher::with_usdc("0")),
            state: None,
            terminal: None,
            cmd_tx: None,
            event_tx: None,
            last_buffer: String::new(),
        }
    }
}

#[given(regex = r#"^Redis 缓存里有余额 "([^"]+)" USDC$"#)]
async fn given_cache_has(world: &mut AppWorld, amount: String) {
    use poly_tui::cache::BalanceCache;
    let b = Balance {
        usdc: Decimal::from_str(&amount).unwrap(),
        fetched_at: Utc::now(),
    };
    world.cache.set(&b).await.unwrap();
}

#[given("Redis 缓存为空")]
async fn given_cache_empty(_world: &mut AppWorld) { /* default */ }

#[given(regex = r#"^CLOB 返回余额 "([^"]+)" USDC$"#)]
async fn given_clob_returns(world: &mut AppWorld, amount: String) {
    world.fetcher.set_balance(&amount);
}

#[given("CLOB 调用会失败")]
async fn given_clob_fails(world: &mut AppWorld) {
    world.fetcher.set_fail(true);
}

#[when("我启动 TUI 主循环")]
async fn when_start_loop(world: &mut AppWorld) {
    let (cmd_tx, _cmd_rx) = mpsc::channel::<Cmd>(8);
    let (event_tx, _event_rx) = mpsc::channel::<AppEvent>(64);

    world.state = Some(AppState::new(Duration::from_secs(30)));
    world.terminal = Some(Terminal::new(TestBackend::new(60, 12)).unwrap());
    world.cmd_tx = Some(cmd_tx);
    world.event_tx = Some(event_tx);
}

#[when("触发一次强制刷新")]
async fn when_force_refresh(world: &mut AppWorld) {
    let (status_tx, mut status_rx) = mpsc::channel::<RefreshStatus>(8);
    refresher::do_fetch(world.fetcher.as_ref(), world.cache.as_ref(), &status_tx).await;
    if let Ok(s) = status_rx.try_recv() {
        if let Some(state) = world.state.as_mut() {
            state.last_refresh = Some(s);
        }
    }
}

#[when(regex = r"^驱动 (\d+) 个 tick$")]
async fn when_drive_ticks(world: &mut AppWorld, n: u32) {
    let state = world.state.as_mut().expect("state initialized");
    let term = world.terminal.as_mut().expect("terminal initialized");
    for _ in 0..n {
        app::tick_once(state, world.cache.as_ref()).await;
        let snap = state.ui_state(Utc::now());
        term.draw(|f| poly_tui::ui::render(f, &snap)).unwrap();
    }
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = buf.cell((x, y)).unwrap();
            out.push_str(cell.symbol());
        }
        out.push('\n');
    }
    world.last_buffer = out;
}

#[when(regex = r#"^按下 "([^"]+)" 键$"#)]
async fn when_press_key(world: &mut AppWorld, key: String) {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let state = world.state.as_mut().expect("state initialized");
    let cmd_tx = world.cmd_tx.as_ref().expect("cmd_tx");
    let code = match key.as_str() {
        "q" => KeyCode::Char('q'),
        "r" => KeyCode::Char('r'),
        _ => panic!("unsupported key in step: {key}"),
    };
    app::handle_event(
        state,
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)),
        cmd_tx,
    );
}

#[then(regex = r#"^屏幕上能看到 "([^"]+)"$"#)]
async fn then_screen_shows(world: &mut AppWorld, expected: String) {
    assert!(
        world.last_buffer.contains(&expected),
        "screen buffer missing {expected:?}; got:\n{}",
        world.last_buffer
    );
}

#[then(regex = r#"^屏幕上仍显示 "([^"]+)"$"#)]
async fn then_screen_still_shows(world: &mut AppWorld, expected: String) {
    assert!(
        world.last_buffer.contains(&expected),
        "screen buffer should still contain {expected:?}; got:\n{}",
        world.last_buffer
    );
}

#[then("应用进入退出状态")]
async fn then_should_quit(world: &mut AppWorld) {
    let state = world.state.as_ref().expect("state initialized");
    assert!(state.should_quit, "expected should_quit=true");
}

#[tokio::main]
async fn main() {
    AppWorld::cucumber()
        .run("tests/features")
        .await;
}
