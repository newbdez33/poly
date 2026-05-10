use anyhow::{Context, Result};
use clap::Parser;
use poly_tui::backtest::{
    config::{filter_strategies, strategy_set, BacktestArgs},
    data::{cache::DiskCache, loader::DataLoader},
    oracle::{estimate_sigma, BlackScholesOracle, NoisyBlackScholesOracle, TokenPriceOracle},
    report::{render_html, ReportMeta},
    runner::run_strategy,
    stats::compute_stats,
};
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

    let loader = DataLoader::new(cache_root)?;
    println!("[poly-backtest] loading data {} -> {}...", args.start, args.end);
    let loaded = loader.load(args.start, args.end).await
        .context("loading data")?;
    println!("[poly-backtest] loaded {} resolved windows, {} BTC candles",
        loaded.windows.len(), loaded.btc.len());

    let sigma = args.sigma.unwrap_or_else(|| estimate_sigma(&loaded.btc));
    println!("[poly-backtest] sigma = ${:.2} (friction {:.2}%)", sigma, args.friction * 100.0);
    let btc_arc = Arc::new(loaded.btc);
    let base_oracle = BlackScholesOracle::new(btc_arc.clone(), sigma, args.friction);
    let oracle: Box<dyn TokenPriceOracle> = if args.oracle_noise > 0.0 {
        eprintln!(
            "[poly-backtest] oracle noise σ={:.4} seed={}",
            args.oracle_noise, args.noise_seed
        );
        Box::new(NoisyBlackScholesOracle::new(
            base_oracle,
            args.oracle_noise,
            args.noise_seed,
        ))
    } else {
        Box::new(base_oracle)
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
