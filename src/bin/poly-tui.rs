use anyhow::Context;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use poly_tui::{
    app, cache::{BalanceCache, RedisBalanceCache},
    clob::{BalanceFetcher, ClobBalanceFetcher},
    config::Config,
    domain::{AppEvent, RefreshStatus},
    input, refresher::{self, Cmd},
    trader::adapters::redis_stream_wrapper::RedisTraderStream,
    tui::events::TraderEventStream,
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{io, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let cfg = Config::from_env().context("loading .env / environment")?;

    // Logging → file only; never stdout while TUI is up
    let file_appender = tracing_appender::rolling::daily("logs", "poly.log");
    let (nb, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(nb)
        .with_env_filter(EnvFilter::new(&cfg.log_level))
        .init();

    tracing::info!("starting poly-tui");

    // Build adapters
    let cache: Arc<dyn BalanceCache> = Arc::new(
        RedisBalanceCache::connect(&cfg.redis_url).await
            .context("connecting Redis (fatal: cache architecture requires it)")?
    );

    let fetcher: Arc<dyn BalanceFetcher> = match
        ClobBalanceFetcher::connect(&cfg.clob_host, &cfg.polymarket_private_key).await
    {
        Ok(f) => Arc::new(f),
        Err(e) => {
            tracing::warn!("CLOB connect failed at startup: {e} — TUI will start with red CLOB led");
            Arc::new(AlwaysFails)
        }
    };

    let trader_stream: Option<Arc<dyn TraderEventStream>> =
        match RedisTraderStream::connect(&cfg.redis_url).await {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                tracing::warn!("trader stream subscribe failed: {e} — TUI shows 'not started'");
                None
            }
        };

    // Channels
    let (status_tx, mut status_rx) = mpsc::channel::<RefreshStatus>(64);
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(8);
    let (event_tx, event_rx) = mpsc::channel::<AppEvent>(64);
    let shutdown = CancellationToken::new();

    // Synchronous pre-warm (5s timeout, ignore failure)
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        refresher::do_fetch(fetcher.as_ref(), cache.as_ref(), &status_tx),
    ).await;

    // Forward Refresher status into app event channel
    let event_tx_status = event_tx.clone();
    let shutdown_status = shutdown.clone();
    let h_status = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_status.cancelled() => break,
                Some(s) = status_rx.recv() => {
                    if event_tx_status.send(AppEvent::Refresh(s)).await.is_err() { break; }
                }
            }
        }
    });

    // Spawn refresher
    let h_refresh = tokio::spawn(refresher::run(
        fetcher.clone(),
        cache.clone(),
        cmd_rx,
        status_tx.clone(),
        Duration::from_secs(cfg.refresh_interval_secs),
        shutdown.clone(),
    ));

    // Spawn input
    let h_input = tokio::spawn(input::run(event_tx.clone(), shutdown.clone()));

    // Spawn trader event forwarder
    let event_tx_trader = event_tx.clone();
    let shutdown_trader = shutdown.clone();
    let h_trader = if let Some(stream) = trader_stream {
        tokio::spawn(async move {
            let tail = match stream.tail(64).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("tail() failed: {e}");
                    return;
                }
            };
            for ev in tail.history {
                if event_tx_trader.send(AppEvent::TraderEvent(ev)).await.is_err() { return; }
            }
            let mut live = tail.live;
            loop {
                tokio::select! {
                    _ = shutdown_trader.cancelled() => break,
                    Some(ev) = futures::StreamExt::next(&mut live) => {
                        if event_tx_trader.send(AppEvent::TraderEvent(ev)).await.is_err() { break; }
                    }
                }
            }
        })
    } else {
        tokio::spawn(async move {})
    };

    // Set up terminal
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("init terminal")?;

    // Run app loop
    let app_result = app::run(
        &mut terminal,
        cache.clone(),
        cmd_tx,
        event_rx,
        Duration::from_secs(cfg.refresh_interval_secs),
        shutdown.clone(),
    ).await;

    // Tear down terminal
    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    // Cleanup
    shutdown.cancel();
    let _ = tokio::join!(h_refresh, h_input, h_status, h_trader);

    app_result
}

// Fallback fetcher used when initial CLOB auth fails — keeps the TUI alive
// so you can see the red LED instead of the binary refusing to start.
struct AlwaysFails;
#[async_trait::async_trait]
impl BalanceFetcher for AlwaysFails {
    async fn fetch(&self) -> Result<poly_tui::domain::Balance, poly_tui::domain::FetchError> {
        Err(poly_tui::domain::FetchError::Auth)
    }
}
