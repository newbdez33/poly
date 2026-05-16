//! Scalp strategy prototype — observation mode.
//!
//! Connects to Polymarket CLOB book WebSocket for current BTC 5min Up/Down
//! tokens, maintains in-memory best bid/ask, and prints scalp opportunities:
//!   ENTRY DETECTED: ask just hit $0.01 → would post limit BUY
//!   BOUNCE: bid reached $0.02+ → if we held a BUY, would profit
//!   MISS: bid dropped back without bouncing → BUY position stuck
//!
//! No real orders placed. Verbose stdout suitable for tmux observation.
//! Independent of RSI trader (no shared Redis lock, no CLOB executor).

use anyhow::{Context, Result};
use clap::Parser;
use poly_tui::trader::adapters::gamma_wrapper::GammaMarketDiscovery;
use poly_tui::trader::market::{next_window_boundary, MarketDiscovery};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

const CLOB_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "https://gamma-api.polymarket.com")]
    gamma_host: String,
    #[arg(long, default_value = "5")]
    window_minutes: u32,
    /// Detect entry when ask <= this value
    #[arg(long, default_value = "0.011")]
    entry_max: f64,
    /// Detect bounce when bid >= this value (higher = more conservative).
    /// At $0.03+ sustained, any taker buying at $0.03 would first sweep the
    /// cheaper $0.02 asks (where our SELL sits) → essentially guaranteed fill.
    #[arg(long, default_value = "0.03")]
    bounce_target: f64,
    /// Ask must stay <= entry_max for this many seconds to count as a
    /// confirmed entry (= our maker BID @ $0.01 reaches front of queue and fills).
    /// 1s is sufficient: any ask resting at $0.01 will trade through our maker
    /// BID on any taker order that comes through.
    #[arg(long, default_value = "1")]
    entry_persist_secs: i64,
    /// Bid must stay >= bounce_target for this many seconds to count as a
    /// confirmed bounce (= our maker SELL @ $0.02 fills because $0.03 takers
    /// sweep cheaper offers first). 1s is sufficient: any single book snapshot
    /// showing bid ≥ $0.03 already implies someone hit $0.03 → our $0.02 was
    /// swept en-route.
    #[arg(long, default_value = "1")]
    bounce_persist_secs: i64,
    /// Don't open new entries after this many seconds into the window.
    /// Default 240s leaves 60s for bid to bounce before window close.
    #[arg(long, default_value = "240")]
    entry_cutoff_secs: i64,
}

#[derive(Clone, Debug, Default)]
struct TokenState {
    label: String,        // "Up" / "Down"
    best_bid: f64,
    best_ask: f64,
    last_print_ask: f64,  // throttle prints
    // Scalp state machine
    in_entry: bool,       // entry confirmed; would have a long position
    entry_ts: i64,        // when entry was confirmed
    last_event_ts: i64,
    /// One entry per window — set after first confirmed entry, reset on window roll.
    entry_done_this_window: bool,
    /// First time ask touched entry_max (continuous since). Reset if ask
    /// climbs above entry_max before persistence threshold.
    entry_persist_start: Option<i64>,
    /// First time bid touched bounce_target (continuous since). Reset if bid
    /// drops back below target.
    bounce_persist_start: Option<i64>,
}

type Tokens = Arc<Mutex<HashMap<String, TokenState>>>;

#[derive(Debug, Default, Clone)]
struct Stats {
    entries_detected: u64,    // times we crossed into entry zone
    bounces_hit: u64,         // ask was at entry, then bid hit bounce target
    bounces_missed: u64,      // entry detected, no bounce by window close
    max_ask_seen: f64,
    /// Min ask observed during 0-240s window phase (across both sides).
    /// Tracks if asks are getting close to $0.01 during the trading window.
    min_ask_during_trading: f64,
    /// Count of book updates where any token's ask was <= $0.05 during trading.
    /// "Close to triggerable" indicator.
    near_entry_observations: u64,
    last_window_ts: i64,
    window_entries: u64,
    window_bounces: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    println!("=== poly-scalp prototype (observation mode) ===");
    println!("Gamma: {} | Window: {}min", args.gamma_host, args.window_minutes);
    println!("Dual-leg maker scalp simulation:");
    println!("  Window open: place BID @ $0.01 on BOTH Up and Down (maker)");
    println!("  Phase 1 (entry):  ask ≤ ${:.2} sustained ≥ {}s on either side",
        args.entry_max, args.entry_persist_secs);
    println!("                    → our $0.01 BID reaches front of queue and fills");
    println!("  Phase 2 (bounce): bid ≥ ${:.2} sustained ≥ {}s on entered side",
        args.bounce_target, args.bounce_persist_secs);
    println!("                    → maker ASK @ $0.02 fills (any $0.03 taker sweeps cheaper first)");
    println!("  Per-window: at most one entry per side; cutoff at {}s",
        args.entry_cutoff_secs);
    println!();

    // rustls setup
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let market: Arc<dyn MarketDiscovery> =
        Arc::new(GammaMarketDiscovery::new(args.gamma_host.clone()));
    let stats = Arc::new(Mutex::new(Stats::default()));

    // Supervisor loop: every 30s check for new window, manage WS task.
    let mut ws_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut current_window: i64 = 0;
    let tokens: Tokens = Arc::new(Mutex::new(HashMap::new()));

    // Stats reporter
    let stats_clone = stats.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.tick().await; // skip first
        loop {
            interval.tick().await;
            let s = stats_clone.lock().await.clone();
            println!();
            println!("─────────── STATS (60s) ───────────");
            println!("  Entries CONFIRMED (ask≤$0.01 sustained):  {}", s.entries_detected);
            println!("  Bounces CONFIRMED (bid≥$0.03 sustained):  {}", s.bounces_hit);
            println!("  Misses (entry, no confirmed bounce):   {}", s.bounces_missed);
            if s.entries_detected > 0 {
                println!("  Confirmed hit rate (≈ real fill):      {:.1}%",
                    100.0 * s.bounces_hit as f64 / s.entries_detected as f64);
            }
            println!("  Min ask during 0-240s trading:         ${:.3}", s.min_ask_during_trading);
            println!("  Near-entry observations (ask ≤ $0.05): {}", s.near_entry_observations);
            println!("  Max ask seen:                          ${:.3}", s.max_ask_seen);
            println!("───────────────────────────────────");
            println!();
        }
    });

    loop {
        let now_ts = chrono::Utc::now().timestamp();
        let win_ts = next_window_boundary(now_ts, args.window_minutes) - (args.window_minutes as i64 * 60);
        if win_ts != current_window {
            if let Some(h) = ws_task.take() { h.abort(); }
            match market.find_window(win_ts, args.window_minutes).await {
                Ok(m) => {
                    current_window = win_ts;
                    println!("🪟 Window roll: {} ({})", win_ts,
                        chrono::DateTime::<chrono::Utc>::from_timestamp(win_ts, 0).unwrap().format("%H:%M:%S UTC"));
                    println!("   UP:   {}", &m.up_token_id[..16]);
                    println!("   DOWN: {}", &m.down_token_id[..16]);
                    let up_id = m.up_token_id.clone();
                    let down_id = m.down_token_id.clone();
                    {
                        let mut t = tokens.lock().await;
                        t.clear();
                        t.insert(up_id.clone(), TokenState { label: "UP".into(), ..Default::default() });
                        t.insert(down_id.clone(), TokenState { label: "DOWN".into(), ..Default::default() });
                    }
                    let tokens_c = tokens.clone();
                    let stats_c = stats.clone();
                    let entry_max = args.entry_max;
                    let bounce_target = args.bounce_target;
                    let entry_persist = args.entry_persist_secs;
                    let bounce_persist = args.bounce_persist_secs;
                    let entry_cutoff = args.entry_cutoff_secs;
                    let win_open = win_ts;
                    ws_task = Some(tokio::spawn(async move {
                        let _ = run_ws(vec![up_id, down_id], tokens_c, stats_c,
                                      win_open, entry_max, bounce_target, entry_persist, bounce_persist, entry_cutoff).await;
                    }));
                }
                Err(e) => println!("   gamma find_window failed: {e}; retrying"),
            }
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

async fn run_ws(
    asset_ids: Vec<String>,
    tokens: Tokens,
    stats: Arc<Mutex<Stats>>,
    window_open_ts: i64,
    entry_max: f64,
    bounce_target: f64,
    entry_persist_secs: i64,
    bounce_persist_secs: i64,
    entry_cutoff_secs: i64,
) -> Result<()> {
    use futures::{SinkExt, StreamExt};
    let mut backoff = Duration::from_secs(1);
    loop {
        match tokio_tungstenite::connect_async(CLOB_WS_URL).await {
            Ok((mut ws, _)) => {
                let sub = serde_json::json!({"type":"MARKET","assets_ids": asset_ids});
                ws.send(Message::Text(sub.to_string().into())).await
                    .context("subscribe")?;
                backoff = Duration::from_secs(1);
                while let Some(msg) = ws.next().await {
                    let txt = match msg {
                        Ok(Message::Text(t)) => t.to_string(),
                        Ok(Message::Close(_)) => break,
                        Ok(_) => continue,
                        Err(_) => break,
                    };
                    handle_message(&txt, &tokens, &stats, window_open_ts, entry_max, bounce_target, entry_persist_secs, bounce_persist_secs, entry_cutoff_secs).await;
                }
            }
            Err(_) => {}
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

async fn handle_message(
    raw: &str,
    tokens: &Tokens,
    stats: &Arc<Mutex<Stats>>,
    window_open_ts: i64,
    entry_max: f64,
    bounce_target: f64,
    entry_persist_secs: i64,
    bounce_persist_secs: i64,
    entry_cutoff_secs: i64,
) {
    // Polymarket book messages are arrays of events.
    let json: Value = match serde_json::from_str(raw) { Ok(v) => v, _ => return };
    let arr = if json.is_array() { json.as_array().unwrap().clone() } else { vec![json] };
    for event in arr {
        let event_type = event.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
        let asset_id = event.get("asset_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if asset_id.is_empty() { continue; }
        let mut t = tokens.lock().await;
        let state = match t.get_mut(&asset_id) { Some(s) => s, None => continue };
        match event_type {
            "book" => {
                // Full snapshot: take best bid (highest) and best ask (lowest).
                if let Some(bids) = event.get("bids").and_then(|v| v.as_array()) {
                    let mut best = 0.0_f64;
                    for b in bids {
                        if let Some(p) = b.get("price").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
                            if p > best { best = p; }
                        }
                    }
                    state.best_bid = best;
                }
                if let Some(asks) = event.get("asks").and_then(|v| v.as_array()) {
                    let mut best = f64::MAX;
                    for a in asks {
                        if let Some(p) = a.get("price").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()) {
                            if p < best { best = p; }
                        }
                    }
                    if best == f64::MAX { state.best_ask = 0.0; } else { state.best_ask = best; }
                }
            }
            // price_change events convey delta updates to the order book, but
            // without the full ladder we can't correctly track when the best
            // level disappears. Rely solely on periodic `book` snapshots.
            "price_change" => {}
            _ => continue,
        }

        // Now evaluate scalp state machine
        let now = chrono::Utc::now().timestamp();
        let window_secs = (now - window_open_ts).clamp(0, 300);
        let ask = state.best_ask;
        let bid = state.best_bid;

        // Throttle ask-update prints (only when ask changes by >= $0.005)
        if (ask - state.last_print_ask).abs() >= 0.005 {
            // skip — too verbose for casual obs
            state.last_print_ask = ask;
        }

        // Stats update
        {
            let mut s = stats.lock().await;
            if ask > 0.0 && ask > s.max_ask_seen { s.max_ask_seen = ask; }
            if ask > 0.0 && window_secs <= entry_cutoff_secs {
                if s.min_ask_during_trading == 0.0 || ask < s.min_ask_during_trading {
                    s.min_ask_during_trading = ask;
                }
                if ask <= 0.05 { s.near_entry_observations += 1; }
            }
        }

        // State machine.
        // Phase 1 (entry confirmation): ask <= entry_max sustained for entry_persist_secs.
        if !state.in_entry && !state.entry_done_this_window
            && window_secs <= entry_cutoff_secs
        {
            if ask > 0.0 && ask <= entry_max {
                match state.entry_persist_start {
                    None => {
                        state.entry_persist_start = Some(now);
                        println!("👀 [{:3}s] {} entry candidate: ask=${:.3} (need ${:.2} sustained {}s)",
                            window_secs, state.label, ask, entry_max, entry_persist_secs);
                    }
                    Some(start) if now - start >= entry_persist_secs => {
                        state.in_entry = true;
                        state.entry_done_this_window = true;
                        state.entry_ts = now;
                        state.entry_persist_start = None;
                        println!("🎯 [{:3}s] {} CONFIRMED ENTRY: ask≤${:.2} for ≥{}s → maker BUY @ $0.01 filled",
                            window_secs, state.label, entry_max, entry_persist_secs);
                        let mut s = stats.lock().await;
                        s.entries_detected += 1;
                        s.window_entries += 1;
                    }
                    _ => {}
                }
            } else if state.entry_persist_start.is_some() {
                // Ask moved away before persistence threshold — reset.
                println!("⚡ [{:3}s] {} entry flicker: ask jumped to ${:.3} before sustaining ${:.2}",
                    window_secs, state.label, ask, entry_max);
                state.entry_persist_start = None;
            }
        }

        // Phase 2 (bounce confirmation): bid >= bounce_target sustained for bounce_persist_secs.
        if state.in_entry && bid >= bounce_target {
            // Bounce started — track persistence.
            match state.bounce_persist_start {
                None => {
                    state.bounce_persist_start = Some(now);
                }
                Some(start_ts) => {
                    if now - start_ts >= bounce_persist_secs {
                        // Sustained bounce — count as guaranteed hit.
                        state.in_entry = false;
                        state.bounce_persist_start = None;
                        let dt = now - state.entry_ts;
                        println!("✅ [{:3}s] {} CONFIRMED BOUNCE after {}s (bid ≥ ${:.2} for ≥ {}s): would SELL @ $0.02, profit ~${:.3}/share",
                            window_secs, state.label, dt, bounce_target, bounce_persist_secs, 0.01);
                        let mut s = stats.lock().await;
                        s.bounces_hit += 1;
                        s.window_bounces += 1;
                    }
                }
            }
        } else if state.in_entry && state.bounce_persist_start.is_some() && bid < bounce_target {
            // Bid dropped back below target before persist threshold — reset.
            println!("⚡ [{:3}s] {} BOUNCE-FLICKER: bid dropped back to ${:.3} before sustaining ${:.2}",
                window_secs, state.label, bid, bounce_target);
            state.bounce_persist_start = None;
        } else if state.in_entry && ask > entry_max + 0.005 {
            // Ask moved away from entry without bid hitting target.
            // Position would still be sitting in book waiting.
        }

        // Check for window close — if still in_entry, count as missed
        if state.in_entry && window_secs >= 290 {
            state.in_entry = false;
            println!("⏰ [{:3}s] {} MISS: window ending, ask was at entry but no bounce", window_secs, state.label);
            let mut s = stats.lock().await;
            s.bounces_missed += 1;
        }

        state.last_event_ts = now;
    }
}
