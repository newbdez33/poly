use anyhow::{Context, Result};
use clap::Parser;
use poly_tui::config::Config;
use poly_tui::trader::adapters::{
    clob_executor_wrapper::ClobOrderExecutor,
    gamma_wrapper::GammaMarketDiscovery,
    redis_state_wrapper::RedisTraderState,
    redis_stream_wrapper::RedisTraderStream,
    simulated_executor::SimulatedExecutor,
};
use poly_tui::trader::config::TraderArgs;
use poly_tui::trader::errors::StateError;
use poly_tui::trader::event::TraderEventEmitter;
use poly_tui::trader::executor::OrderExecutor;
use poly_tui::trader::ladder::{Direction, LadderState};
use poly_tui::trader::market::MarketDiscovery;
use poly_tui::trader::resolver::{PolymarketResolver, WindowResolver};
use poly_tui::trader::scheduler::{run, SchedulerConfig, SchedulerDeps, WindowExecutor};
use poly_tui::trader::state::TraderStateStore;
use poly_tui::trader::window::{run_window, WindowConfig, WindowDeps};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let args = TraderArgs::parse();
    args.validate().context("invalid CLI arguments")?;

    dotenvy::dotenv().ok();
    let cfg = Config::from_env().context("loading .env")?;
    let gamma_host = std::env::var("GAMMA_HOST")
        .unwrap_or_else(|_| "https://gamma-api.polymarket.com".into());

    // Logging → file
    let appender = tracing_appender::rolling::daily("logs", "trader.log");
    let (nb, _guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt().with_writer(nb)
        .with_env_filter(EnvFilter::new(&cfg.log_level)).init();
    tracing::info!("starting poly-trader");

    // Adapters
    let state_store: Arc<dyn TraderStateStore> =
        Arc::new(RedisTraderState::connect(&cfg.redis_url).await
            .context("connecting Redis (fatal)")?);
    let emitter: Arc<dyn TraderEventEmitter> =
        Arc::new(RedisTraderStream::connect(&cfg.redis_url).await
            .context("connecting Redis stream")?);
    let market: Arc<dyn MarketDiscovery> =
        Arc::new(GammaMarketDiscovery::new(gamma_host));
    let resolver: Arc<dyn WindowResolver> =
        Arc::new(PolymarketResolver::new(market.clone(), Duration::from_secs(60)));

    let executor: Arc<dyn OrderExecutor> = if args.dry_run {
        Arc::new(SimulatedExecutor::default())
    } else {
        Arc::new(ClobOrderExecutor::connect(&cfg.clob_host, &cfg.polymarket_private_key).await
            .context("CLOB auth (fatal)")?)
    };

    // Acquire singleton lock
    let owner = format!(
        "{}:{}",
        hostname::get().map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "unknown".into()),
        std::process::id()
    );
    let acquired = state_store.try_lock(&owner, Duration::from_secs(60)).await?;
    if !acquired {
        anyhow::bail!("another poly-trader is running (lock held)");
    }

    // Restore or init ladder
    let ladder = restore_or_init(state_store.as_ref(), &args).await?;

    // Lock keepalive
    let keepalive_owner = owner.clone();
    let keepalive_store = state_store.clone();
    let shutdown = CancellationToken::new();
    let shutdown_keepalive = shutdown.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = shutdown_keepalive.cancelled() => break,
                _ = interval.tick() => {
                    if let Err(e) = keepalive_store.refresh_lock(&keepalive_owner, Duration::from_secs(60)).await {
                        tracing::error!("lock keepalive failed: {e}");
                        shutdown_keepalive.cancel();
                        break;
                    }
                }
            }
        }
    });

    // Signal handler
    let shutdown_sig = shutdown.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        shutdown_sig.cancel();
    });

    // WindowExecutor adapter (binds run_window over our deps)
    let window_deps = Arc::new(WindowDeps {
        market: market.clone(),
        executor: executor.clone(),
        resolver: resolver.clone(),
        emitter: emitter.clone(),
    });
    let window_cfg = WindowConfig {
        band_min: args.band_min,
        band_max: args.band_max,
    };
    let window_exec: Arc<dyn WindowExecutor> = Arc::new(BoundWindowExec {
        deps: window_deps.clone(),
        cfg: window_cfg,
    });

    let sched_deps = SchedulerDeps {
        window_exec,
        state_store: state_store.clone(),
        emitter: emitter.clone(),
    };
    let sched_cfg = SchedulerConfig { max_windows: args.max_windows };

    let final_state = run(ladder, sched_deps, sched_cfg, shutdown.clone()).await
        .map_err(|e: StateError| anyhow::anyhow!("scheduler error: {e}"))?;
    tracing::info!("session ended: stopped={:?} pnl={}",
        final_state.stopped, final_state.realized_pnl_usd);

    state_store.release_lock(&owner).await.ok();
    Ok(())
}

struct BoundWindowExec {
    deps: Arc<WindowDeps>,
    cfg: WindowConfig,
}

#[async_trait::async_trait]
impl WindowExecutor for BoundWindowExec {
    async fn execute(&self, ladder: &LadderState, window_ts: i64)
        -> poly_tui::trader::ladder::WindowOutcome
    {
        run_window(&self.deps, &self.cfg, ladder, window_ts).await
    }
}

async fn restore_or_init(
    store: &dyn TraderStateStore,
    args: &TraderArgs,
) -> Result<LadderState> {
    let existing = store.load().await?;
    match (existing, args.reset) {
        (Some(s), false) if !s.is_stopped() => {
            tracing::info!("resuming ladder: step={} pnl={}",
                s.current_step, s.realized_pnl_usd);
            Ok(s)
        }
        (Some(s), false) if s.is_stopped() => {
            anyhow::bail!("previous session stopped: {:?}; pass --reset to start fresh", s.stopped)
        }
        _ => {
            store.clear().await?;
            let direction: Direction = args.direction.into();
            Ok(LadderState::new(direction, args.base, args.max_step, chrono::Utc::now()))
        }
    }
}
