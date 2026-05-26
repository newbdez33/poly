use anyhow::{Context, Result};
use clap::Parser;
use poly_tui::config::Config;
use poly_tui::trader::adapters::{
    chainlink_btc_wrapper::HttpChainlinkFeed,
    polymarket_btc_ws_wrapper::PolymarketBtcWsFeed,
    clob_executor_wrapper::ClobOrderExecutor,
    gamma_price_wrapper::GammaPriceFetcher,
    gamma_wrapper::GammaMarketDiscovery,
    redis_state_wrapper::RedisTraderState,
    redis_stream_wrapper::RedisTraderStream,
    simulated_executor::SimulatedExecutor,
};
use poly_tui::tui::market_watch::BtcPriceFeed;
use poly_tui::trader::config::{ExitRuleArg, TraderArgs};
use poly_tui::trader::errors::StateError;
use poly_tui::trader::event::TraderEventEmitter;
use poly_tui::trader::executor::OrderExecutor;
use poly_tui::trader::exit_watcher::ExitConfig;
use poly_tui::trader::ladder::{Direction, LadderState, SkipReason, WindowOutcome};
use poly_tui::trader::market::MarketDiscovery;
use poly_tui::trader::order_events::{OrderEventStream, PolymarketPollOrderEvents};
use poly_tui::trader::price::MidwindowPriceFetcher;
use poly_tui::trader::resolver::{PolymarketResolver, WindowResolver};
use poly_tui::trader::rsi_gate::{LiveBinanceFetcher, RsiDecision, RsiGate};
use poly_tui::trader::momentum_gate::{MomentumGate, MomentumGateConfig, MomentumDecision};
use poly_tui::trader::scheduler::{run, SchedulerConfig, SchedulerDeps, WindowExecutor};
use poly_tui::trader::state::TraderStateStore;
use poly_tui::trader::window::{run_window, WindowConfig, WindowDeps};
use rust_decimal_macros::dec;
use rust_decimal::prelude::ToPrimitive;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let args = TraderArgs::parse();
    args.validate().context("invalid CLI arguments")?;

    let window_seconds = poly_tui::trader::market::window_seconds(args.window_minutes);

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
        Arc::new(GammaMarketDiscovery::new(gamma_host.clone()));
    // window_seconds + 300s post-close grace. Gamma-api caching is defeated
    // by the wrapper's cache-bust param, but the wider window absorbs CDN tail.
    let resolver: Arc<dyn WindowResolver> =
        Arc::new(PolymarketResolver::new(
            market.clone(),
            Duration::from_secs((window_seconds + 300) as u64),
            args.window_minutes,
        ));

    let (executor, events): (Arc<dyn OrderExecutor>, Arc<dyn OrderEventStream>) = if args.dry_run {
        // Pair SimulatedExecutor + SimulatedOrderEvents: the events stream
        // reads the executor's recorded limit orders and fills at their actual
        // limit price (not a hardcoded 0.50).
        let sim = Arc::new(SimulatedExecutor::default());
        let sim_events = Arc::new(
            poly_tui::trader::adapters::simulated_executor::SimulatedOrderEvents::new(sim.clone())
        );
        (sim as Arc<dyn OrderExecutor>, sim_events as Arc<dyn OrderEventStream>)
    } else {
        // Real CLOB: keep the executor's auth'd client and reuse it for the
        // OrderEventStream poller. v1.7 simplification — v1.7.1 may refactor
        // to a single ownership chain; this avoids re-running the auth flow.
        let clob = ClobOrderExecutor::connect(&cfg.clob_host, &cfg.polymarket_private_key).await
            .context("CLOB auth (fatal)")?;
        let poll_client = clob.inner_client();
        (
            Arc::new(clob),
            Arc::new(PolymarketPollOrderEvents::new(poll_client)),
        )
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

    let price: Arc<dyn MidwindowPriceFetcher> = Arc::new(
        GammaPriceFetcher::new(gamma_host.clone()),
    );

    // v1.9.1: Polymarket Real-Time Data WebSocket (crypto_prices_chainlink).
    // Same source Polymarket uses to resolve 5min BTC up/down markets — sub-
    // second updates vs the 60s heartbeat of on-chain Chainlink aggregator.
    let _ = HttpChainlinkFeed::connect; // keep import referenced for tests
    let btc_price: Arc<dyn BtcPriceFeed> = Arc::new(
        PolymarketBtcWsFeed::connect().await
            .context("connecting Polymarket BTC WebSocket feed")?,
    );
    // Give the WebSocket a moment to receive the first price before windows start.
    tracing::info!("waiting for initial BTC price from Polymarket WebSocket...");
    for _ in 0..10 {
        if btc_price.latest_price().await.is_ok() {
            tracing::info!("initial BTC price received");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    // WindowExecutor adapter (binds run_window over our deps)
    let window_deps = Arc::new(WindowDeps {
        market: market.clone(),
        executor: executor.clone(),
        resolver: resolver.clone(),
        emitter: emitter.clone(),
        price: price.clone(),
        events: events.clone(),
        btc_price: btc_price.clone(),
    });
    let exit_cfg = match args.exit_rule {
        ExitRuleArg::Hold => None,
        ExitRuleArg::HoldEarlyExit => None,
        ExitRuleArg::TpSl => Some(ExitConfig {
            tp_price: args.tp_price.expect("validated: --tp-price required"),
            // v1.11: SL is optional — when omitted, use $0.001 floor (never triggers)
            // for TP-only behavior.
            sl_price: args.sl_price.unwrap_or_else(|| dec!(0.001)),
            poll: std::time::Duration::from_secs(args.poll_secs as u64),
        }),
    };
    let window_cfg = WindowConfig {
        band_min: args.band_min,
        band_max: args.band_max,
        exit: exit_cfg,
        exit_at_secs: args.exit_at_secs,
        maker: args.maker,
        tp_limit_sell: args.tp_limit_sell,
        window_seconds,
    };
    let base_exec: Arc<dyn WindowExecutor> = Arc::new(BoundWindowExec {
        deps: window_deps.clone(),
        cfg: window_cfg,
    });
    let window_exec: Arc<dyn WindowExecutor> = if args.rsi_filter {
        tracing::info!(
            "RSI filter enabled: period={} oversold={} overbought={}",
            args.rsi_period, args.rsi_oversold, args.rsi_overbought,
        );
        let gate = RsiGate::new(
            args.rsi_period,
            args.rsi_oversold,
            args.rsi_overbought,
            Box::new(LiveBinanceFetcher::new()),
        );
        Arc::new(RsiGatedExec {
            inner: base_exec,
            gate: Arc::new(gate),
        })
    } else if args.intra_momentum {
        tracing::info!(
            "IntraWindowMomentum gate enabled: scan {}..{}s, bp [{}, {}]",
            args.intra_scan_start_secs, args.intra_scan_end_secs,
            args.intra_bp_min, args.intra_bp_max,
        );
        let gate = MomentumGate::new(
            MomentumGateConfig {
                scan_start_secs: args.intra_scan_start_secs,
                scan_end_secs: args.intra_scan_end_secs,
                bp_min: args.intra_bp_min,
                bp_max: args.intra_bp_max,
            },
            btc_price.clone(),
        );
        Arc::new(MomentumGatedExec {
            inner: base_exec,
            gate: Arc::new(gate),
        })
    } else {
        base_exec
    };

    let sched_deps = SchedulerDeps {
        window_exec,
        state_store: state_store.clone(),
        emitter: emitter.clone(),
    };
    let sched_cfg = SchedulerConfig { max_windows: args.max_windows, window_seconds };

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

/// v1.11: RSI-gating wrapper for WindowExecutor. Before each window, fetches
/// Binance candles + computes RSI(period). If RSI is in the neutral zone the
/// window is skipped without trading; otherwise the inner executor is invoked
/// with the ladder's direction overridden to UP (oversold) or DOWN (overbought).
struct RsiGatedExec {
    inner: Arc<dyn WindowExecutor>,
    gate: Arc<RsiGate>,
}

#[async_trait::async_trait]
impl WindowExecutor for RsiGatedExec {
    async fn execute(&self, ladder: &LadderState, window_ts: i64) -> WindowOutcome {
        let decision = self.gate.decide(window_ts).await;
        match decision {
            RsiDecision::SkipNeutral { rsi } => {
                tracing::info!("rsi-gate: skip neutral RSI={:.2} window={}", rsi, window_ts);
                let rsi_dec = decision.rsi_decimal().unwrap_or_default();
                WindowOutcome::Skipped {
                    reason: SkipReason::RsiNeutralFilter { rsi: rsi_dec },
                }
            }
            RsiDecision::FetchFailed => {
                tracing::warn!("rsi-gate: skip (fetch failed) window={}", window_ts);
                WindowOutcome::Skipped { reason: SkipReason::RsiFetchFailed }
            }
            RsiDecision::Trade { direction, rsi } => {
                tracing::info!(
                    "rsi-gate: trade {:?} RSI={:.2} window={}",
                    direction, rsi, window_ts,
                );
                if direction == ladder.direction {
                    self.inner.execute(ladder, window_ts).await
                } else {
                    let mut overridden = ladder.clone();
                    overridden.direction = direction;
                    self.inner.execute(&overridden, window_ts).await
                }
            }
        }
    }
}

/// v1.16: IntraWindowMomentum-gating wrapper. Waits inside the window until
/// BTC has moved into the configured bp range, then triggers entry with
/// direction matching the move.
struct MomentumGatedExec {
    inner: Arc<dyn WindowExecutor>,
    gate: Arc<MomentumGate>,
}

#[async_trait::async_trait]
impl WindowExecutor for MomentumGatedExec {
    async fn execute(&self, ladder: &LadderState, window_ts: i64) -> WindowOutcome {
        let decision = self.gate.decide(window_ts).await;
        match decision {
            MomentumDecision::Skip => {
                tracing::info!("momentum-gate: skip (no trigger) window={}", window_ts);
                WindowOutcome::Skipped {
                    reason: poly_tui::trader::ladder::SkipReason::PriceOutsideBand {
                        ask: rust_decimal::Decimal::ZERO,
                    },
                }
            }
            MomentumDecision::FetchFailed => {
                tracing::warn!("momentum-gate: skip (btc feed failed) window={}", window_ts);
                WindowOutcome::Skipped {
                    reason: poly_tui::trader::ladder::SkipReason::RsiFetchFailed,
                }
            }
            MomentumDecision::Trade { direction, bp, t_offset } => {
                tracing::info!(
                    "momentum-gate: trade {:?} bp={:.2} t+{}s window={}",
                    direction, bp, t_offset, window_ts,
                );
                let mut overridden = ladder.clone();
                overridden.direction = direction;
                self.inner.execute(&overridden, window_ts).await
            }
        }
    }
}


async fn restore_or_init(
    store: &dyn TraderStateStore,
    args: &TraderArgs,
) -> Result<LadderState> {
    let existing = store.load().await?;
    match (existing, args.reset) {
        (Some(s), false) if !s.is_stopped() => {
            // Detect mid-session window-length switch — refuse, instruct --reset.
            if s.window_minutes != args.window_minutes {
                anyhow::bail!(
                    "saved ladder is for {}min windows; trader configured for {}min. \
                     Pass --reset to start a fresh session.",
                    s.window_minutes, args.window_minutes
                );
            }
            // v1.11.10: refuse to silently switch between dry-run and live mode
            // mid-session — that masks accidental real-money trading.
            if s.dry_run != args.dry_run {
                anyhow::bail!(
                    "saved ladder is dry_run={}; trader invoked with dry_run={}. \
                     Pass --reset to start a fresh session in the new mode.",
                    s.dry_run, args.dry_run
                );
            }
            // v1.12: same protection for fixed-stake — mode mismatch silently
            // changes risk profile (Martingale vs Fixed). Force --reset.
            if s.fixed_stake != args.fixed_stake {
                anyhow::bail!(
                    "saved ladder is fixed_stake={}; trader invoked with fixed_stake={}. \
                     Pass --reset to start a fresh session in the new mode.",
                    s.fixed_stake, args.fixed_stake
                );
            }
            tracing::info!("resuming ladder: step={} pnl={} window_minutes={}",
                s.current_step, s.realized_pnl_usd, s.window_minutes);
            Ok(s)
        }
        (Some(s), false) if s.is_stopped() => {
            anyhow::bail!("previous session stopped: {:?}; pass --reset to start fresh", s.stopped)
        }
        _ => {
            store.clear().await?;
            let direction: Direction = args.direction.into();
            // TraderArgs::base is still Decimal for backward-compat with the
            // --base flag; convert to u32 shares for the LadderState API.
            // User-facing rename to --base-shares is a follow-up task.
            let base_shares = args.base.to_u32().unwrap_or(5);
            Ok(LadderState::new(direction, base_shares, args.max_step, chrono::Utc::now())
                .with_window_minutes(args.window_minutes)
                .with_dry_run(args.dry_run)
                .with_fixed_stake(args.fixed_stake))
        }
    }
}
