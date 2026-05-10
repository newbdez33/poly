use anyhow::{Context, Result};
use clap::Parser;
use poly_tui::backtest::{
    config::{filter_strategies, strategy_set, BacktestArgs, OracleKind},
    data::{
        cache::DiskCache,
        loader::DataLoader,
        trades::{CachedTradeStore, PolymarketTradeFetcher, Trade, TradeFetcher},
    },
    oracle::{estimate_sigma, BlackScholesOracle, NoisyBlackScholesOracle, RealTradeOracle, TokenPriceOracle},
    report::{render_html, ReportMeta},
    runner::run_strategy,
    stats::compute_stats,
};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let args = BacktestArgs::parse();

    if args.oracle_noise < 0.0 || args.oracle_noise > 0.5 {
        anyhow::bail!(
            "oracle-noise must be in [0.0, 0.5], got {}",
            args.oracle_noise
        );
    }

    let cache_root = args.cache_dir.clone()
        .unwrap_or_else(|| DiskCache::default_root(""));
    println!("[poly-backtest] cache root: {}", cache_root.display());

    let loader = DataLoader::new(cache_root.clone())?;
    println!("[poly-backtest] loading data {} -> {}...", args.start, args.end);
    let loaded = loader.load(args.start, args.end).await
        .context("loading data")?;
    println!("[poly-backtest] loaded {} resolved windows, {} BTC candles",
        loaded.windows.len(), loaded.btc.len());

    let sigma = args.sigma.unwrap_or_else(|| estimate_sigma(&loaded.btc));
    println!("[poly-backtest] sigma = ${:.2} (friction {:.2}%)", sigma, args.friction * 100.0);
    let btc_arc = Arc::new(loaded.btc);
    let oracle: Box<dyn TokenPriceOracle> = match args.oracle {
        OracleKind::Bs => Box::new(BlackScholesOracle::new(btc_arc.clone(), sigma, args.friction)),
        OracleKind::Noisy => {
            eprintln!(
                "[poly-backtest] oracle noise σ={:.4} seed={}",
                args.oracle_noise, args.noise_seed
            );
            Box::new(NoisyBlackScholesOracle::new(
                BlackScholesOracle::new(btc_arc.clone(), sigma, args.friction),
                args.oracle_noise,
                args.noise_seed,
            ))
        }
        OracleKind::Real => {
            eprintln!(
                "[poly-backtest] loading real trade history (auto-fetching uncached, parallel=4)..."
            );
            let trades_dir = cache_root.join("trades");
            let store = std::sync::Arc::new(CachedTradeStore::new(trades_dir)?);
            let fetcher = std::sync::Arc::new(PolymarketTradeFetcher::new(150)); // 150ms intra-page throttle
            let mut all_trades: HashMap<i64, Vec<Trade>> = HashMap::new();
            let mut cached = 0usize;
            let mut skipped = 0usize;
            let total = loaded.windows.len();

            use futures::stream::{StreamExt, FuturesUnordered};

            let mut to_fetch: Vec<(i64, String)> = Vec::new();
            for w in loaded.windows.iter() {
                let cid = match &w.condition_id {
                    Some(c) => c.clone(),
                    None => { skipped += 1; continue; }
                };
                if let Some(t) = store.load(w.window_ts) {
                    cached += 1;
                    all_trades.insert(w.window_ts, t);
                } else {
                    to_fetch.push((w.window_ts, cid));
                }
            }
            eprintln!(
                "[poly-backtest] trades: {} cached, {} to fetch, {} skipped (no condition_id)",
                cached, to_fetch.len(), skipped
            );

            let mut pending = FuturesUnordered::new();
            let mut iter = to_fetch.into_iter();
            const PARALLEL: usize = 4;
            for _ in 0..PARALLEL {
                if let Some((ts, cid)) = iter.next() {
                    let f = fetcher.clone();
                    let s = store.clone();
                    pending.push(tokio::spawn(async move {
                        let t = f.fetch_window(&cid, ts).await
                            .with_context(|| format!("fetching trades for window {} ({})", ts, cid))?;
                        s.save(ts, &t)?;
                        anyhow::Ok((ts, t))
                    }));
                }
            }

            let mut fetched = 0usize;
            while let Some(joined) = pending.next().await {
                let (ts, trades) = joined??;
                all_trades.insert(ts, trades);
                fetched += 1;
                if fetched % 50 == 0 {
                    eprintln!(
                        "[poly-backtest]   trades: {} fetched ({}/{} total windows)",
                        fetched, cached + fetched, total - skipped
                    );
                }
                if let Some((ts, cid)) = iter.next() {
                    let f = fetcher.clone();
                    let s = store.clone();
                    pending.push(tokio::spawn(async move {
                        let t = f.fetch_window(&cid, ts).await
                            .with_context(|| format!("fetching trades for window {} ({})", ts, cid))?;
                        s.save(ts, &t)?;
                        anyhow::Ok((ts, t))
                    }));
                }
            }

            eprintln!(
                "[poly-backtest] trades load complete: {} cached, {} fetched, {} skipped",
                cached, fetched, skipped
            );
            Box::new(RealTradeOracle::new(all_trades))
        }
    };

    let all = strategy_set();
    let strategies = filter_strategies(&all, &args.strategies);
    println!("[poly-backtest] running {} strategies on {} windows",
        strategies.len(), loaded.windows.len());

    let mut all_stats = Vec::new();
    for strategy in &strategies {
        println!("[poly-backtest]   running {}...", strategy.name);
        let result = run_strategy(strategy, &loaded.windows, oracle.as_ref());
        let stats = compute_stats(&result);
        println!("[poly-backtest]     PnL=${:.2}  win_rate={:.1}%  cap_resets={}",
            stats.total_pnl_usd, stats.win_rate * 100.0, stats.cap_resets);
        all_stats.push(stats);
    }

    let meta = ReportMeta {
        start: args.start,
        end: args.end,
        total_windows: loaded.windows.len(),
        sigma,
        friction: args.friction,
        generated_at: chrono::Utc::now(),
    };

    let html = render_html(&all_stats, &meta);
    std::fs::write(&args.output, &html)
        .with_context(|| format!("writing {}", args.output.display()))?;
    println!("[poly-backtest] report: {} ({} bytes)",
        args.output.display(), html.len());

    Ok(())
}
