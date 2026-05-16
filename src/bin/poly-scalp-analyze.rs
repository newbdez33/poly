//! Standalone scalp-opportunity analyzer.
//!
//! Reads cached Polymarket 5min BTC trade history (no Redis, no live trader
//! interaction) and computes how often a $0.01 ask got bounced to $0.02 or
//! higher within the same window — the precondition for the 1c→2c scalp idea.
//!
//! Usage:
//!   poly-scalp-analyze [--cache-dir <path>] [--days <N>]
//!
//! Outputs per-window stats + aggregate frequency of bounce events.

use anyhow::{Context, Result};
use clap::Parser;
use rust_decimal::prelude::ToPrimitive;
use poly_tui::backtest::data::trades::{Outcome, Trade};
use std::path::PathBuf;

#[derive(Parser, Debug)]
struct Args {
    /// Cache root (default: ~/.poly-backtest-cache)
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Look back this many days from "now" (default 2)
    #[arg(long, default_value = "2")]
    days: i64,
    /// Bounce target price (default 0.02 = 2 cents)
    #[arg(long, default_value = "0.02")]
    bounce_to: f64,
    /// Min entry price (default 0.01 = 1 cent)
    #[arg(long, default_value = "0.011")]
    entry_max: f64,
}

#[derive(Debug, Default)]
struct WindowStats {
    window_ts: i64,
    side: &'static str,
    trade_count: usize,
    min_price: f64,
    max_price: f64,
    /// True if a trade at <= entry_max was followed (later in same window)
    /// by a trade at >= bounce_to.
    has_bounce: bool,
    bounce_secs: Option<i64>, // seconds from entry trade to bounce trade
    bounce_magnitude: f64,    // bounce_price - entry_price
}

/// One bounce event: entry at low price → later trade at bounce target.
#[derive(Debug, Clone)]
struct BounceEvent {
    entry_ts: i64,
    entry_price: f64,
    bounce_ts: i64,
    bounce_price: f64,
    max_price_in_bounce: f64, // max price reached after entry, before reset
    window_progress_pct: f64,  // 0-100, when in window did entry happen
}

fn analyze_side(window_ts: i64, side: &'static str, trades: &[&Trade], args: &Args)
    -> (WindowStats, Vec<BounceEvent>)
{
    let mut stats = WindowStats {
        window_ts, side, trade_count: trades.len(),
        min_price: f64::MAX, max_price: f64::MIN,
        has_bounce: false, bounce_secs: None, bounce_magnitude: 0.0,
    };
    let mut events: Vec<BounceEvent> = Vec::new();
    if trades.is_empty() { return (stats, events); }

    // Walk trades; whenever price drops to <= entry_max, mark entry; track
    // subsequent prices for bounce. Multiple bounces per window are recorded.
    let mut entry: Option<(i64, f64)> = None;
    let mut bounce_pending: Option<BounceEvent> = None;

    for t in trades {
        let p = t.price.to_f64().unwrap_or(0.0);
        stats.min_price = stats.min_price.min(p);
        stats.max_price = stats.max_price.max(p);

        // New entry when price drops back into range
        if entry.is_none() && p <= args.entry_max {
            entry = Some((t.timestamp, p));
        }

        if let Some((entry_ts, entry_p)) = entry {
            if p >= args.bounce_to {
                // Found a bounce. Track max price in this bounce.
                match bounce_pending.as_mut() {
                    Some(b) if p > b.max_price_in_bounce => b.max_price_in_bounce = p,
                    Some(_) => {}
                    None => {
                        let window_secs = (t.timestamp - window_ts).clamp(0, 300) as f64;
                        bounce_pending = Some(BounceEvent {
                            entry_ts, entry_price: entry_p,
                            bounce_ts: t.timestamp, bounce_price: p,
                            max_price_in_bounce: p,
                            window_progress_pct: 100.0 * window_secs / 300.0,
                        });
                    }
                }
            } else if p <= args.entry_max && bounce_pending.is_some() {
                // Price dropped back into entry range while we were tracking a bounce.
                // Close out this bounce event and start fresh.
                if let Some(b) = bounce_pending.take() { events.push(b); }
                entry = Some((t.timestamp, p));
            }
        }
    }
    // Flush any pending bounce.
    if let Some(b) = bounce_pending { events.push(b); }

    if let Some(first) = events.first() {
        stats.has_bounce = true;
        stats.bounce_secs = Some(first.bounce_ts - first.entry_ts);
        stats.bounce_magnitude = first.bounce_price - first.entry_price;
    }
    (stats, events)
}

fn load_trades(path: &PathBuf) -> Result<Vec<Trade>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {}", path.display()))
}

fn main() -> Result<()> {
    let args = Args::parse();
    let cache_root = args.cache_dir.clone()
        .or_else(|| dirs::home_dir().map(|h| h.join(".poly-backtest-cache")))
        .context("can't determine cache dir")?;
    let trades_dir = cache_root.join("trades");

    let now = chrono::Utc::now().timestamp();
    let cutoff = now - args.days * 24 * 3600;

    let mut entries: Vec<_> = std::fs::read_dir(&trades_dir)
        .with_context(|| format!("read {}", trades_dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            let ts: i64 = p.file_stem()?.to_str()?.parse().ok()?;
            if ts >= cutoff { Some((ts, p)) } else { None }
        })
        .collect();
    entries.sort_by_key(|(ts, _)| *ts);

    println!("Analyzing {} windows (last {} days, since {})",
        entries.len(), args.days,
        chrono::DateTime::<chrono::Utc>::from_timestamp(cutoff, 0).unwrap());
    println!("Entry: trade at <= ${:.3}, bounce target: trade at >= ${:.3}",
        args.entry_max, args.bounce_to);
    println!();

    let mut total_up_bounces = 0;
    let mut total_down_bounces = 0;
    let mut total_up_entries = 0;
    let mut total_down_entries = 0;
    let mut all_events: Vec<BounceEvent> = Vec::new();
    // Histograms
    let mut mag_buckets = vec![0usize; 10]; // $0.01–0.10 in 1c steps
    let mut time_buckets = vec![0usize; 10]; // 0-100% window progress
    let mut bounces_per_window = std::collections::BTreeMap::<usize, usize>::new();

    for (ts, path) in &entries {
        let trades = match load_trades(path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let mut up: Vec<&Trade> = trades.iter().filter(|t| t.outcome == Outcome::Up).collect();
        let mut down: Vec<&Trade> = trades.iter().filter(|t| t.outcome == Outcome::Down).collect();
        up.sort_by_key(|t| t.timestamp);
        down.sort_by_key(|t| t.timestamp);

        let (up_stats, up_evts) = analyze_side(*ts, "UP", &up, &args);
        let (down_stats, dn_evts) = analyze_side(*ts, "DOWN", &down, &args);

        if up_stats.min_price <= args.entry_max { total_up_entries += 1; }
        if down_stats.min_price <= args.entry_max { total_down_entries += 1; }
        if up_stats.has_bounce { total_up_bounces += 1; }
        if down_stats.has_bounce { total_down_bounces += 1; }

        let window_total = up_evts.len() + dn_evts.len();
        *bounces_per_window.entry(window_total).or_insert(0) += 1;

        for e in up_evts.into_iter().chain(dn_evts.into_iter()) {
            // Magnitude bucket: floor((max - entry) / 0.01), cap at 9 ($0.10+)
            let mag_idx = (((e.max_price_in_bounce - e.entry_price) * 100.0).floor() as i64)
                .clamp(1, 9) as usize - 1;
            mag_buckets[mag_idx] += 1;
            // Time bucket: 10 deciles of window progress
            let t_idx = ((e.window_progress_pct / 10.0).floor() as i64).clamp(0, 9) as usize;
            time_buckets[t_idx] += 1;
            all_events.push(e);
        }
    }
    let bounce_count = all_events.len();
    let bounce_magnitude_sum: f64 = all_events.iter().map(|e| e.bounce_price - e.entry_price).sum();
    let max_bounce_sum: f64 = all_events.iter().map(|e| e.max_price_in_bounce - e.entry_price).sum();
    let bounce_secs_sum: i64 = all_events.iter().map(|e| e.bounce_ts - e.entry_ts).sum();

    let total_windows = entries.len();
    println!("=== Aggregate ({} windows analyzed) ===", total_windows);
    println!("UP   side: {}/{} windows hit entry, {}/{} ({:.1}%) had a bounce",
        total_up_entries, total_windows, total_up_bounces, total_up_entries,
        if total_up_entries > 0 { 100.0 * total_up_bounces as f64 / total_up_entries as f64 } else { 0.0 });
    println!("DOWN side: {}/{} windows hit entry, {}/{} ({:.1}%) had a bounce",
        total_down_entries, total_windows, total_down_bounces, total_down_entries,
        if total_down_entries > 0 { 100.0 * total_down_bounces as f64 / total_down_entries as f64 } else { 0.0 });
    println!();
    if bounce_count > 0 {
        println!("Bounce stats:");
        println!("  Total bounces: {}", bounce_count);
        println!("  Avg time entry→bounce: {} seconds", bounce_secs_sum / bounce_count as i64);
        println!("  Avg bounce magnitude (first cross): ${:.3}", bounce_magnitude_sum / bounce_count as f64);
        println!("  Avg max bounce magnitude:           ${:.3}", max_bounce_sum / bounce_count as f64);
        println!();

        println!("=== Bounce magnitude histogram (max price reached) ===");
        let mag_total: usize = mag_buckets.iter().sum();
        for (i, &n) in mag_buckets.iter().enumerate() {
            let lo = (i + 1) as f64 / 100.0;
            let hi = (i + 2) as f64 / 100.0;
            let pct = if mag_total > 0 { 100.0 * n as f64 / mag_total as f64 } else { 0.0 };
            let bar = "▇".repeat((pct / 2.0) as usize);
            println!("  +${:.2}-${:.2}: {:4} ({:.1}%) {}", lo, hi, n, pct, bar);
        }

        println!();
        println!("=== Bounce timing within window (entry trade time) ===");
        let t_total: usize = time_buckets.iter().sum();
        for (i, &n) in time_buckets.iter().enumerate() {
            let lo = i * 10;
            let hi = (i + 1) * 10;
            let pct = if t_total > 0 { 100.0 * n as f64 / t_total as f64 } else { 0.0 };
            let bar = "▇".repeat((pct / 2.0) as usize);
            println!("  {:3}%-{:3}% window: {:4} ({:.1}%) {}", lo, hi, n, pct, bar);
        }

        println!();
        println!("=== Bounces per window distribution ===");
        for (count, n_windows) in &bounces_per_window {
            println!("  {} bounces: {} windows", count, n_windows);
        }

        println!();
        println!("=== Naive PnL simulation (per bounce captured) ===");
        let entry_avg = 0.01;
        let sell_target = args.bounce_to;
        let shares_per_trade = 100.0; // assume $1 min order
        let per_bounce_profit = (sell_target - entry_avg) * shares_per_trade;
        let per_bounce_loss = entry_avg * shares_per_trade; // worst case: BUY filled, SELL didn't, lost at resolution
        println!("  Assume $1 capital per bounce: {} shares × ${}", shares_per_trade, entry_avg);
        println!("  Best case (TP fills): +${:.2} per bounce", per_bounce_profit);
        println!("  Worst case (no SELL, position loses): -${:.2}", per_bounce_loss);

        for &fill_rate in &[0.10, 0.25, 0.50, 0.75, 1.00] {
            let captured = bounce_count as f64 * fill_rate;
            let est_pnl = captured * per_bounce_profit - (bounce_count as f64 - captured) * 0.0; // skipped = no harm
            println!("  Fill rate {:>4.0}% → captured {:.0}/{} bounces → ${:.2}",
                fill_rate * 100.0, captured, bounce_count, est_pnl);
        }
    }

    // Per-day breakdown
    println!();
    println!("=== Per-day breakdown ===");
    let mut by_day: std::collections::BTreeMap<String, (usize, usize, usize)> = std::collections::BTreeMap::new();
    for (ts, _) in &entries {
        let date = chrono::DateTime::<chrono::Utc>::from_timestamp(*ts, 0).unwrap()
            .format("%Y-%m-%d").to_string();
        by_day.entry(date.clone()).or_insert((0, 0, 0));
    }
    // Re-scan: count bounces per day
    for (ts, path) in &entries {
        let date = chrono::DateTime::<chrono::Utc>::from_timestamp(*ts, 0).unwrap()
            .format("%Y-%m-%d").to_string();
        let trades = match load_trades(path) { Ok(t) => t, Err(_) => continue };
        let mut up: Vec<&Trade> = trades.iter().filter(|t| t.outcome == Outcome::Up).collect();
        let mut down: Vec<&Trade> = trades.iter().filter(|t| t.outcome == Outcome::Down).collect();
        up.sort_by_key(|t| t.timestamp);
        down.sort_by_key(|t| t.timestamp);
        let (u, _) = analyze_side(*ts, "UP", &up, &args);
        let (d, _) = analyze_side(*ts, "DOWN", &down, &args);
        let entry = by_day.get_mut(&date).unwrap();
        entry.0 += 1; // total windows
        if u.has_bounce { entry.1 += 1; }
        if d.has_bounce { entry.2 += 1; }
    }
    for (date, (w, up_b, dn_b)) in &by_day {
        println!("  {}: {} windows | UP bounces: {} | DOWN bounces: {} | Total: {} ({:.1}% of windows)",
            date, w, up_b, dn_b, up_b + dn_b,
            100.0 * (up_b + dn_b) as f64 / *w as f64);
    }

    Ok(())
}
