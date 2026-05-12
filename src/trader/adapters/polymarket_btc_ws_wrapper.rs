//! Polymarket Real-Time Data WebSocket BTC price feed.
//!
//! Connects to `wss://ws-live-data.polymarket.com` and subscribes to the
//! `crypto_prices_chainlink` topic for BTC/USD. This is **the same source
//! Polymarket uses for 5min BTC up/down resolution** (Chainlink Data Streams
//! exposed via Polymarket's WebSocket relay) — typically sub-second updates.
//!
//! The standard on-chain Chainlink BTC/USD aggregator on Polygon has a 60s
//! heartbeat and no deviation trigger, which is too slow for 5min markets
//! (BTC moves of $10-$40 within a single window are routinely missed).
//!
//! Architecture:
//! - One spawned task maintains the WebSocket: subscribe, recv loop, PING
//!   every 5s, auto-reconnect on disconnect.
//! - Latest price + timestamp shared via `Arc<Mutex<...>>`.
//! - `latest_price()` returns the cached price; errors only if no price has
//!   ever been received OR the cached price is older than `MAX_STALE_SECS`.

use crate::tui::market_watch::{BtcPriceFeed, MarketWatchError};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_URL: &str = "wss://ws-live-data.polymarket.com";
const PING_INTERVAL: Duration = Duration::from_secs(5);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// Reject `latest_price()` if the cached value is older than this. WebSocket
/// is supposed to push updates every <1s; if we haven't seen one in 30s the
/// connection is degraded.
const MAX_STALE_SECS: u64 = 30;

#[derive(Debug, Deserialize)]
struct UpdateMessage {
    topic: String,
    #[serde(rename = "type")]
    msg_type: String,
    payload: UpdatePayload,
}

#[derive(Debug, Deserialize)]
struct UpdatePayload {
    symbol: String,
    #[serde(default)]
    timestamp: i64,
    /// Polymarket sends this as a JSON number (e.g., 67234.50). serde_json
    /// loses precision for very large floats but BTC fits in f64 fine.
    value: f64,
}

struct LatestPrice {
    value: Decimal,
    received_at: Instant,
}

pub struct PolymarketBtcWsFeed {
    latest: Arc<Mutex<Option<LatestPrice>>>,
}

impl PolymarketBtcWsFeed {
    /// Connect and start the background WebSocket task. Returns immediately;
    /// the first price arrives within ~1-2s after connect.
    pub async fn connect() -> Result<Self, MarketWatchError> {
        let latest = Arc::new(Mutex::new(None));
        let latest_for_task = latest.clone();
        tokio::spawn(async move {
            run_ws_loop(latest_for_task).await;
        });
        Ok(Self { latest })
    }
}

/// Top-level WebSocket loop: connect, subscribe, recv + ping, reconnect on error.
async fn run_ws_loop(latest: Arc<Mutex<Option<LatestPrice>>>) {
    loop {
        if let Err(e) = run_ws_session(&latest).await {
            tracing::warn!("polymarket-ws session ended: {e}; reconnecting in {}s",
                RECONNECT_DELAY.as_secs());
        } else {
            tracing::warn!("polymarket-ws session ended cleanly; reconnecting");
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn run_ws_session(latest: &Arc<Mutex<Option<LatestPrice>>>) -> Result<(), String> {
    let (ws, _resp) = connect_async(WS_URL).await
        .map_err(|e| format!("connect_async failed: {e}"))?;
    tracing::info!("polymarket-ws connected: {WS_URL}");
    let (mut write, mut read) = ws.split();

    // Subscribe to BTC/USD via Chainlink topic.
    let subscribe = serde_json::json!({
        "action": "subscribe",
        "subscriptions": [
            {
                "topic": "crypto_prices_chainlink",
                "type": "*",
                "filters": "{\"symbol\":\"btc/usd\"}"
            }
        ]
    });
    write.send(Message::Text(subscribe.to_string())).await
        .map_err(|e| format!("subscribe send: {e}"))?;

    let mut ping_ticker = tokio::time::interval(PING_INTERVAL);
    ping_ticker.tick().await; // skip the immediate first tick

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
                        if let Ok(upd) = serde_json::from_str::<UpdateMessage>(&text) {
                            if upd.topic == "crypto_prices_chainlink"
                                && upd.msg_type == "update"
                                && upd.payload.symbol == "btc/usd"
                            {
                                let price = Decimal::from_str(&upd.payload.value.to_string())
                                    .ok();
                                if let Some(p) = price {
                                    *latest.lock().unwrap() = Some(LatestPrice {
                                        value: p,
                                        received_at: Instant::now(),
                                    });
                                }
                            }
                        }
                        // ignore subscription-ack and other messages
                    }
                    Message::Ping(p) => {
                        let _ = write.send(Message::Pong(p)).await;
                    }
                    Message::Pong(_) | Message::Frame(_) | Message::Binary(_) => {}
                    Message::Close(_) => return Err("close frame".into()),
                }
            }
        }
    }
}

#[async_trait]
impl BtcPriceFeed for PolymarketBtcWsFeed {
    async fn latest_price(&self) -> Result<Decimal, MarketWatchError> {
        let guard = self.latest.lock().unwrap();
        let entry = guard.as_ref()
            .ok_or_else(|| MarketWatchError::Rpc("no price received yet".into()))?;
        if entry.received_at.elapsed().as_secs() > MAX_STALE_SECS {
            return Err(MarketWatchError::Rpc(format!(
                "cached price is {}s stale (max {MAX_STALE_SECS}s)",
                entry.received_at.elapsed().as_secs()
            )));
        }
        Ok(entry.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_message_deserializes() {
        let json = r#"{
            "topic": "crypto_prices_chainlink",
            "type": "update",
            "timestamp": 1753314088421,
            "payload": {
                "symbol": "btc/usd",
                "timestamp": 1753314088395,
                "value": 67234.50
            }
        }"#;
        let m: UpdateMessage = serde_json::from_str(json).unwrap();
        assert_eq!(m.topic, "crypto_prices_chainlink");
        assert_eq!(m.payload.symbol, "btc/usd");
        assert_eq!(m.payload.value, 67234.50);
    }

    #[test]
    fn ignores_other_topics() {
        let json = r#"{
            "topic": "crypto_prices_binance",
            "type": "update",
            "timestamp": 1753314088421,
            "payload": {"symbol": "btc/usd", "timestamp": 0, "value": 67000.0}
        }"#;
        let m: UpdateMessage = serde_json::from_str(json).unwrap();
        // Decoding succeeds; the filtering happens in run_ws_session.
        assert_eq!(m.topic, "crypto_prices_binance");
    }

    #[tokio::test]
    async fn latest_price_returns_err_when_no_price() {
        // Construct directly without spawning the WS task.
        let feed = PolymarketBtcWsFeed {
            latest: Arc::new(Mutex::new(None)),
        };
        let r = feed.latest_price().await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn latest_price_returns_cached_value() {
        let latest = Arc::new(Mutex::new(Some(LatestPrice {
            value: Decimal::from_str("80000.50").unwrap(),
            received_at: Instant::now(),
        })));
        let feed = PolymarketBtcWsFeed { latest };
        let p = feed.latest_price().await.unwrap();
        assert_eq!(p, Decimal::from_str("80000.50").unwrap());
    }
}
