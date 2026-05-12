use anyhow::Context;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use poly_tui::{
    adapters::polymarket_positions_wrapper::PolymarketPositionsFetcher,
    adapters::redis_positions_wrapper::RedisPositionsCache,
    app, cache::{BalanceCache, RedisBalanceCache},
    clob::{BalanceFetcher, ClobBalanceFetcher},
    config::Config,
    domain::{AppEvent, RefreshStatus},
    input,
    positions::{PositionsCache, PositionsFetcher},
    positioner,
    refresher::{self, Cmd},
    trader::adapters::redis_stream_wrapper::RedisTraderStream,
    trader::adapters::chainlink_btc_wrapper::HttpChainlinkFeed,
    trader::adapters::gamma_wrapper::GammaMarketDiscovery,
    trader::market::MarketDiscovery,
    tui::events::TraderEventStream,
    tui::market_watch::{self, BtcPriceFeed},
};
use alloy::signers::local::LocalSigner;
use polymarket_client_sdk_v2::{derive_proxy_wallet, POLYGON};
use std::str::FromStr;
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

    // Derive proxy address from EOA private key for positions API.
    let positions_user = match derive_user_address(&cfg.polymarket_private_key) {
        Ok(addr) => Some(addr),
        Err(e) => {
            tracing::warn!("proxy address derivation failed: {e} — positions hidden");
            None
        }
    };

    let positions_fetcher: Option<Arc<dyn PositionsFetcher>> = positions_user
        .map(|addr| Arc::new(PolymarketPositionsFetcher::new(addr)) as Arc<dyn PositionsFetcher>);

    let positions_cache: Arc<dyn PositionsCache> = Arc::new(
        RedisPositionsCache::connect(&cfg.redis_url).await
            .context("connecting Redis for positions cache")?,
    );

    let trader_stream: Option<Arc<dyn TraderEventStream>> =
        match RedisTraderStream::connect(&cfg.redis_url).await {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                tracing::warn!("trader stream subscribe failed: {e} — TUI shows 'not started'");
                None
            }
        };

    let gamma_host = std::env::var("GAMMA_HOST")
        .unwrap_or_else(|_| "https://gamma-api.polymarket.com".into());

    let market_for_watch: Option<Arc<dyn MarketDiscovery>> = Some(Arc::new(
        GammaMarketDiscovery::new(gamma_host)
    ));

    let price_feed: Option<Arc<dyn BtcPriceFeed>> =
        match HttpChainlinkFeed::connect(&cfg.polygon_rpc_url).await {
            Ok(f) => Some(Arc::new(f)),
            Err(e) => {
                tracing::warn!("Chainlink RPC connect failed: {e} — BTC strip shows --");
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
            tracing::info!("trader stream tail: {} history events loaded", tail.history.len());
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

    let event_tx_market = event_tx.clone();
    let shutdown_market = shutdown.clone();
    // Channel for app → market_watch window_minutes updates. App detects
    // changes from the trader's event stream and pushes here; market_watch
    // picks up and re-floors its gamma window.
    let (window_minutes_tx, window_minutes_rx) = mpsc::channel::<u32>(8);
    let h_market = match (price_feed, market_for_watch) {
        (Some(feed), Some(market)) => {
            tokio::spawn(market_watch::run(feed, market, event_tx_market, window_minutes_rx, shutdown_market))
        }
        _ => {
            // Drop the rx to avoid leaving an unfilled channel.
            drop(window_minutes_rx);
            tokio::spawn(async move {})
        }
    };

    let event_tx_pos = event_tx.clone();
    let shutdown_pos = shutdown.clone();
    let h_positions = if let Some(fetcher) = positions_fetcher {
        tokio::spawn(positioner::run(
            fetcher,
            positions_cache.clone(),
            event_tx_pos,
            Duration::from_secs(cfg.refresh_interval_secs),
            shutdown_pos,
        ))
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
        Some(window_minutes_tx),
        shutdown.clone(),
    ).await;

    // Tear down terminal
    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    // Cleanup
    shutdown.cancel();
    let _ = tokio::join!(h_refresh, h_input, h_status, h_trader, h_market, h_positions);

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

/// Derive the user's positions-API address from the EOA private key.
/// For Polymarket Magic/email accounts, this is the proxy contract address;
/// for browser-wallet accounts, this is a Gnosis Safe address. We default to
/// proxy because that's what existing trader code assumes (SignatureType::Proxy).
fn derive_user_address(private_key: &str) -> anyhow::Result<alloy::primitives::Address> {
    let signer = LocalSigner::from_str(private_key)
        .map_err(|e| anyhow::anyhow!("invalid private key: {e}"))?;
    let eoa = signer.address();
    derive_proxy_wallet(eoa, POLYGON)
        .ok_or_else(|| anyhow::anyhow!("derive_proxy_wallet returned None for chain {POLYGON}"))
}
