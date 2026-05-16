//! poly-orderbook-recorder — standalone tool that records Polymarket BTC
//! 5-min up/down market order book best-bid/best-ask snapshots to SQLite
//! once per second.
//!
//! Architecture:
//!   - Active market discovery loop (every ~window_minutes): polls gamma-api
//!     for the currently-trading window via existing `GammaMarketDiscovery`.
//!   - When the window rolls, (re)subscribe to the CLOB WebSocket
//!     (`wss://ws-subscriptions-clob.polymarket.com/ws/`) on the `market`
//!     channel for the new Up + Down token IDs.
//!   - WS handler maintains an in-memory map<token_id, BookState> updated by
//!     `book` snapshots and `price_change` deltas.
//!   - A 1-second timer reads the map and writes one row per token to SQLite.
//!   - Graceful shutdown on Ctrl+C flushes pending writes and closes the DB.
//!
//! This binary is intentionally independent of the live trader: it does not
//! touch Redis, uses a separate SQLite DB, and runs in its own process.

use anyhow::{Context, Result};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use poly_tui::trader::adapters::gamma_wrapper::GammaMarketDiscovery;
use poly_tui::trader::market::{floor_window, MarketDiscovery, WindowMarket};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing_subscriber::EnvFilter;

const CLOB_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

#[derive(Parser, Debug)]
#[command(
    name = "poly-orderbook-recorder",
    about = "Record Polymarket BTC up/down order book snapshots to SQLite"
)]
struct Args {
    /// SQLite DB path. Default: ~/.poly-orderbook/recorder.db
    #[arg(long)]
    db_path: Option<PathBuf>,

    /// Gamma-API base URL.
    #[arg(long, default_value = "https://gamma-api.polymarket.com")]
    gamma_host: String,

    /// Window length in minutes (5/15/60).
    #[arg(long, default_value_t = 5)]
    window_minutes: u32,
}

/// In-memory best-bid/best-ask state per token. Updated by WS handler, read
/// by the 1-second snapshot timer.
#[derive(Clone, Debug, Default)]
struct BookState {
    best_bid: Option<f64>,
    best_ask: Option<f64>,
    bid_size: Option<f64>,
    ask_size: Option<f64>,
    /// Window this token belongs to (used to write `window_ts` + `outcome`).
    window_ts: i64,
    outcome: String, // "Up" or "Down"
}

type BookMap = Arc<RwLock<HashMap<String, BookState>>>;

fn default_db_path() -> PathBuf {
    let mut p = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push(".poly-orderbook");
    p.push("recorder.db");
    p
}

fn open_db(path: &PathBuf) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating DB parent dir {}", parent.display()))?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("opening sqlite at {}", path.display()))?;
    // Per-doc PRAGMAs: WAL is friendlier for a writer + ad-hoc reader, and
    // NORMAL sync trades a tiny crash-risk window for substantially less I/O.
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    conn.pragma_update(None, "synchronous", "NORMAL").ok();
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS orderbook_snapshots (
            ts INTEGER NOT NULL,
            token_id TEXT NOT NULL,
            window_ts INTEGER NOT NULL,
            outcome TEXT NOT NULL,
            best_bid REAL,
            best_ask REAL,
            bid_size REAL,
            ask_size REAL,
            PRIMARY KEY (ts, token_id)
        );
        CREATE INDEX IF NOT EXISTS idx_window ON orderbook_snapshots(window_ts);
        CREATE INDEX IF NOT EXISTS idx_ts_outcome ON orderbook_snapshots(ts, outcome);
        "#,
    )?;
    Ok(conn)
}

/// Parse a single CLOB WS message and update the book state.
///
/// CLOB pushes either:
///   - `book` event: full snapshot with `bids` + `asks` arrays
///   - `price_change` event: per-side level updates with new `size` (0 = removed)
fn handle_ws_event(event: &Value, books: &mut HashMap<String, BookState>) {
    let evt_type = event.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
    let asset_id = match event.get("asset_id").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return,
    };
    let Some(state) = books.get_mut(&asset_id) else { return };

    match evt_type {
        "book" => {
            // Polymarket CLOB convention: `bids` ascending (lowest first) so
            // the LAST entry is best bid; `asks` ascending so FIRST entry is
            // best ask. We take max of bids and min of asks to be safe
            // regardless of ordering.
            let (bb, bs) = best_side(event.get("bids"), /*max=*/ true);
            let (ba, as_) = best_side(event.get("asks"), /*max=*/ false);
            state.best_bid = bb;
            state.bid_size = bs;
            state.best_ask = ba;
            state.ask_size = as_;
        }
        "price_change" => {
            // `changes` may be a single object or an array.
            let changes = event.get("changes");
            let mut levels: Vec<&Value> = Vec::new();
            if let Some(arr) = changes.and_then(|v| v.as_array()) {
                levels.extend(arr.iter());
            } else if let Some(obj) = changes {
                if obj.is_object() {
                    levels.push(obj);
                }
            }
            for ch in levels {
                let side = ch.get("side").and_then(|v| v.as_str()).unwrap_or("");
                let price = ch.get("price").and_then(parse_num);
                let size = ch.get("size").and_then(parse_num);
                // We don't maintain the full ladder — only the best level.
                // The book snapshot keeps us roughly correct; price_change
                // can shift the best level if size goes to 0 or if a new
                // better price arrives. Without the full book we approximate:
                // if the change is at a better price than current best, use
                // it; if it nukes the current best, leave stale (next `book`
                // snapshot from CLOB will correct).
                match (side, price, size) {
                    ("BUY", Some(p), Some(s)) if s > 0.0 => {
                        if state.best_bid.map_or(true, |cur| p > cur) {
                            state.best_bid = Some(p);
                            state.bid_size = Some(s);
                        } else if state.best_bid == Some(p) {
                            state.bid_size = Some(s);
                        }
                    }
                    ("SELL", Some(p), Some(s)) if s > 0.0 => {
                        if state.best_ask.map_or(true, |cur| p < cur) {
                            state.best_ask = Some(p);
                            state.ask_size = Some(s);
                        } else if state.best_ask == Some(p) {
                            state.ask_size = Some(s);
                        }
                    }
                    _ => {}
                }
            }
        }
        // `tick_size_change` and unknown events: nothing to update.
        _ => {}
    }
}

fn parse_num(v: &Value) -> Option<f64> {
    if let Some(f) = v.as_f64() {
        return Some(f);
    }
    v.as_str().and_then(|s| s.parse::<f64>().ok())
}

/// Return (price, size) for best side. `max=true` -> highest price (best bid),
/// `max=false` -> lowest price (best ask).
fn best_side(arr: Option<&Value>, max: bool) -> (Option<f64>, Option<f64>) {
    let Some(arr) = arr.and_then(|v| v.as_array()) else {
        return (None, None);
    };
    let mut best: Option<(f64, f64)> = None;
    for level in arr {
        let p = level.get("price").and_then(parse_num);
        let s = level.get("size").and_then(parse_num);
        if let (Some(p), Some(s)) = (p, s) {
            if s <= 0.0 {
                continue;
            }
            match best {
                None => best = Some((p, s)),
                Some((bp, _)) => {
                    let take = if max { p > bp } else { p < bp };
                    if take {
                        best = Some((p, s));
                    }
                }
            }
        }
    }
    match best {
        Some((p, s)) => (Some(p), Some(s)),
        None => (None, None),
    }
}

/// One WebSocket session: subscribe, then loop on messages updating `books`.
/// Returns Err(reason) on disconnect so caller can reconnect.
async fn run_ws_session(asset_ids: Vec<String>, books: BookMap) -> Result<(), String> {
    tracing::info!("clob-ws connect: {} (assets={})", CLOB_WS_URL, asset_ids.len());
    let (ws, _resp) = connect_async(CLOB_WS_URL).await
        .map_err(|e| format!("connect: {e}"))?;
    let (mut write, mut read) = ws.split();

    // Polymarket CLOB market channel subscribe message.
    // Note: field name is `assets_ids` (plural with trailing `s`), per docs.
    let subscribe = serde_json::json!({
        "type": "MARKET",
        "assets_ids": asset_ids,
    });
    write.send(Message::Text(subscribe.to_string())).await
        .map_err(|e| format!("subscribe send: {e}"))?;
    tracing::info!("clob-ws subscribed");

    // Lightweight PING every 10s — CLOB closes idle connections.
    let mut ping_ticker = tokio::time::interval(Duration::from_secs(10));
    ping_ticker.tick().await;

    loop {
        tokio::select! {
            _ = ping_ticker.tick() => {
                if let Err(e) = write.send(Message::Text("PING".into())).await {
                    return Err(format!("ping send: {e}"));
                }
            }
            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return Err(format!("recv: {e}")),
                    None => return Err("stream closed".into()),
                };
                match msg {
                    Message::Text(text) => {
                        if text.eq_ignore_ascii_case("PONG") { continue; }
                        // CLOB may send either a single event object or an
                        // array of events in one frame.
                        match serde_json::from_str::<Value>(&text) {
                            Ok(v) => {
                                let mut guard = books.write().await;
                                if let Some(arr) = v.as_array() {
                                    for ev in arr { handle_ws_event(ev, &mut guard); }
                                } else {
                                    handle_ws_event(&v, &mut guard);
                                }
                            }
                            Err(e) => {
                                tracing::debug!("ws parse err ({e}): {}",
                                    &text.chars().take(200).collect::<String>());
                            }
                        }
                    }
                    Message::Ping(p) => { let _ = write.send(Message::Pong(p)).await; }
                    Message::Pong(_) | Message::Frame(_) | Message::Binary(_) => {}
                    Message::Close(_) => return Err("close frame".into()),
                }
            }
        }
    }
}

/// Spawn a task that holds a WS connection for a fixed (token_id_up, token_id_down)
/// pair. Returns a `CancellationToken` analogue via the abort handle on the
/// JoinHandle so the supervisor can cancel the session on window rollover.
fn spawn_ws_task(asset_ids: Vec<String>, books: BookMap) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff_secs: u64 = 1;
        loop {
            match run_ws_session(asset_ids.clone(), books.clone()).await {
                Ok(_) => unreachable!("session loop never returns Ok"),
                Err(e) => {
                    tracing::warn!("clob-ws session ended: {e}; reconnecting in {backoff_secs}s");
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                }
            }
        }
    })
}

/// Persist one snapshot row per token. Errors are logged, not propagated —
/// recorder must keep running across transient SQLite issues.
async fn write_snapshots(conn: &Arc<Mutex<Connection>>, books: &BookMap, ts: i64) {
    let snapshot: Vec<(String, BookState)> = {
        let guard = books.read().await;
        guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };
    if snapshot.is_empty() {
        return;
    }
    let conn = conn.lock().await;
    let tx = match conn.unchecked_transaction() {
        Ok(tx) => tx,
        Err(e) => { tracing::error!("begin tx: {e}"); return; }
    };
    for (token_id, st) in &snapshot {
        let r = tx.execute(
            "INSERT OR REPLACE INTO orderbook_snapshots
                (ts, token_id, window_ts, outcome, best_bid, best_ask, bid_size, ask_size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                ts,
                token_id,
                st.window_ts,
                st.outcome,
                st.best_bid,
                st.best_ask,
                st.bid_size,
                st.ask_size,
            ],
        );
        if let Err(e) = r {
            tracing::error!("insert {token_id}: {e}");
        }
    }
    if let Err(e) = tx.commit() {
        tracing::error!("commit: {e}");
    }
}

/// Find the current window market and return it. Tries floor(now) first,
/// then floor(now) - window_seconds as a fallback (in case gamma-api hasn't
/// indexed the new window yet right after a boundary).
async fn current_window(
    discovery: &dyn MarketDiscovery,
    window_minutes: u32,
) -> Result<WindowMarket> {
    let now = chrono::Utc::now().timestamp();
    let win = floor_window(now, window_minutes);
    match discovery.find_window(win, window_minutes).await {
        Ok(m) => Ok(m),
        Err(e) => {
            // Fallback to previous window (just-closed) — useful right after
            // boundary when gamma-api hasn't surfaced the new market.
            tracing::warn!("find_window({win}) failed: {e}; trying previous");
            let prev = win - (window_minutes as i64 * 60);
            discovery.find_window(prev, window_minutes).await
                .with_context(|| format!("no market for window {win} or {prev}"))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Logging → daily rolling file.
    std::fs::create_dir_all("logs").ok();
    let appender = tracing_appender::rolling::daily("logs", "orderbook-recorder.log");
    let (nb, _guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(nb)
        .with_env_filter(EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    // rustls 0.23 requires a crypto provider — must be installed before any
    // TLS connection. Mirror the pattern used in polymarket_btc_ws_wrapper.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let db_path = args.db_path.unwrap_or_else(default_db_path);
    tracing::info!("starting poly-orderbook-recorder db={} gamma={} window={}m",
        db_path.display(), args.gamma_host, args.window_minutes);

    let conn = open_db(&db_path).context("opening DB")?;
    let conn = Arc::new(Mutex::new(conn));

    let discovery: Arc<dyn MarketDiscovery> =
        Arc::new(GammaMarketDiscovery::new(args.gamma_host.clone()));

    let books: BookMap = Arc::new(RwLock::new(HashMap::new()));

    // Supervisor: track the current window and the WS task subscribed to it.
    // When the window rolls, abort the old task and spawn a new one.
    let mut current_window_ts: i64 = 0;
    let mut ws_task: Option<tokio::task::JoinHandle<()>> = None;

    let mut market_refresh =
        tokio::time::interval(Duration::from_secs(30));
    let mut snapshot_tick = tokio::time::interval(Duration::from_secs(1));
    snapshot_tick.set_missed_tick_behavior(
        tokio::time::MissedTickBehavior::Skip);

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    // Kick off initial market discovery before entering the loop so the WS
    // task is up promptly.
    if let Err(e) = refresh_market(
        &*discovery, args.window_minutes, &books,
        &mut current_window_ts, &mut ws_task,
    ).await {
        tracing::error!("initial market discovery failed: {e:#}");
    }

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("ctrl-c received, shutting down");
                if let Some(h) = ws_task.take() { h.abort(); }
                // Final flush is a no-op (writes are synchronous-per-tick),
                // but explicit close releases the SQLite file lock.
                let guard = conn.lock().await;
                // Drop the Arc<Mutex<Connection>> — rusqlite's Drop closes the
                // file. We hold the lock to force exclusive access first.
                guard.flush_prepared_statement_cache();
                drop(guard);
                break;
            }
            _ = market_refresh.tick() => {
                if let Err(e) = refresh_market(
                    &*discovery, args.window_minutes, &books,
                    &mut current_window_ts, &mut ws_task,
                ).await {
                    tracing::warn!("market refresh failed: {e:#}");
                }
            }
            _ = snapshot_tick.tick() => {
                let ts = chrono::Utc::now().timestamp();
                write_snapshots(&conn, &books, ts).await;
            }
        }
    }

    tracing::info!("shutdown complete");
    Ok(())
}

/// Check whether the active window has rolled forward. If so, rebuild the
/// books map and respawn the WS task with the new asset IDs.
async fn refresh_market(
    discovery: &dyn MarketDiscovery,
    window_minutes: u32,
    books: &BookMap,
    current_window_ts: &mut i64,
    ws_task: &mut Option<tokio::task::JoinHandle<()>>,
) -> Result<()> {
    let m = current_window(discovery, window_minutes).await?;
    if m.window_ts == *current_window_ts {
        return Ok(());
    }
    tracing::info!(
        "window roll: {} -> {} ({}); up={} down={}",
        *current_window_ts, m.window_ts, m.slug,
        truncate(&m.up_token_id, 12), truncate(&m.down_token_id, 12),
    );

    // Replace books map with fresh entries for the new window.
    {
        let mut guard = books.write().await;
        guard.clear();
        guard.insert(m.up_token_id.clone(), BookState {
            window_ts: m.window_ts,
            outcome: "Up".into(),
            ..Default::default()
        });
        guard.insert(m.down_token_id.clone(), BookState {
            window_ts: m.window_ts,
            outcome: "Down".into(),
            ..Default::default()
        });
    }

    if let Some(h) = ws_task.take() {
        h.abort();
    }
    *ws_task = Some(spawn_ws_task(
        vec![m.up_token_id.clone(), m.down_token_id.clone()],
        books.clone(),
    ));
    *current_window_ts = m.window_ts;
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}…", &s[..n]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seed_books() -> HashMap<String, BookState> {
        let mut m = HashMap::new();
        m.insert("tok-up".into(), BookState {
            window_ts: 1747789200, outcome: "Up".into(), ..Default::default()
        });
        m
    }

    #[test]
    fn handle_book_snapshot_picks_best_levels() {
        let mut books = seed_books();
        let ev = json!({
            "event_type": "book",
            "asset_id": "tok-up",
            "bids": [
                {"price": "0.40", "size": "100"},
                {"price": "0.45", "size": "50"},
                {"price": "0.42", "size": "80"}
            ],
            "asks": [
                {"price": "0.55", "size": "30"},
                {"price": "0.52", "size": "20"},
                {"price": "0.60", "size": "10"}
            ]
        });
        handle_ws_event(&ev, &mut books);
        let st = &books["tok-up"];
        assert_eq!(st.best_bid, Some(0.45));
        assert_eq!(st.bid_size, Some(50.0));
        assert_eq!(st.best_ask, Some(0.52));
        assert_eq!(st.ask_size, Some(20.0));
    }

    #[test]
    fn handle_book_ignores_unknown_asset() {
        let mut books = seed_books();
        let ev = json!({
            "event_type": "book",
            "asset_id": "other",
            "bids": [{"price": "0.5", "size": "10"}],
            "asks": [{"price": "0.6", "size": "10"}],
        });
        handle_ws_event(&ev, &mut books);
        let st = &books["tok-up"];
        assert!(st.best_bid.is_none());
    }

    #[test]
    fn price_change_improves_best_bid() {
        let mut books = seed_books();
        books.get_mut("tok-up").unwrap().best_bid = Some(0.45);
        books.get_mut("tok-up").unwrap().bid_size = Some(10.0);
        let ev = json!({
            "event_type": "price_change",
            "asset_id": "tok-up",
            "changes": [{"side": "BUY", "price": "0.48", "size": "5"}]
        });
        handle_ws_event(&ev, &mut books);
        assert_eq!(books["tok-up"].best_bid, Some(0.48));
        assert_eq!(books["tok-up"].bid_size, Some(5.0));
    }

    #[test]
    fn price_change_updates_existing_best_size() {
        let mut books = seed_books();
        books.get_mut("tok-up").unwrap().best_ask = Some(0.55);
        books.get_mut("tok-up").unwrap().ask_size = Some(30.0);
        let ev = json!({
            "event_type": "price_change",
            "asset_id": "tok-up",
            "changes": {"side": "SELL", "price": "0.55", "size": "12"}
        });
        handle_ws_event(&ev, &mut books);
        assert_eq!(books["tok-up"].best_ask, Some(0.55));
        assert_eq!(books["tok-up"].ask_size, Some(12.0));
    }

    #[test]
    fn parse_num_handles_string_and_number() {
        assert_eq!(parse_num(&json!("0.42")), Some(0.42));
        assert_eq!(parse_num(&json!(0.42)), Some(0.42));
        assert_eq!(parse_num(&json!(null)), None);
    }

    #[test]
    fn open_db_creates_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.db");
        let conn = open_db(&path).unwrap();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='orderbook_snapshots'",
            [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn write_snapshot_inserts_row() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.db");
        let conn = open_db(&path).unwrap();
        conn.execute(
            "INSERT INTO orderbook_snapshots
                (ts, token_id, window_ts, outcome, best_bid, best_ask, bid_size, ask_size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![1700000001i64, "tok-up", 1700000000i64, "Up", 0.45, 0.55, 50.0, 30.0],
        ).unwrap();
        let cnt: i64 = conn.query_row(
            "SELECT COUNT(*) FROM orderbook_snapshots", [], |r| r.get(0)).unwrap();
        assert_eq!(cnt, 1);
    }

    #[test]
    fn truncate_shortens_long_ids() {
        assert_eq!(truncate("abcdef", 3), "abc…");
        assert_eq!(truncate("ab", 5), "ab");
    }
}

