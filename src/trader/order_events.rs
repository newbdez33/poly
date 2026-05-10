use crate::trader::errors::StreamError;
use crate::trader::executor::OrderId;
use async_trait::async_trait;
use polymarket_client_sdk_v2::auth::Normal;
use polymarket_client_sdk_v2::auth::state::Authenticated;
use polymarket_client_sdk_v2::clob::Client as ClobClient;
use polymarket_client_sdk_v2::clob::types::OrderStatusType;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Polls SDK `client.order(id)` every 2s until the order reaches a terminal
/// state (Matched/Canceled). Emits `OrderEvent::Filled` on each tick where
/// `size_matched` increased; `partial` flag is implicit (compare to total).
pub struct PolymarketPollOrderEvents {
    client: Arc<ClobClient<Authenticated<Normal>>>,
}

impl PolymarketPollOrderEvents {
    pub fn new(client: Arc<ClobClient<Authenticated<Normal>>>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl OrderEventStream for PolymarketPollOrderEvents {
    async fn watch(&self, id: OrderId) -> Result<mpsc::Receiver<OrderEvent>, StreamError> {
        let (tx, rx) = mpsc::channel(8);
        let client = self.client.clone();
        let id_owned = id.clone();
        tokio::spawn(async move {
            let mut last_matched = Decimal::ZERO;
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let resp = match client.order(&id_owned.0).await {
                    Ok(r) => r,
                    Err(e) => {
                        // Order not found = filled or cancelled. Without further
                        // info we emit Cancelled; caller distinguishes by
                        // checking position state. Acceptable for v1.7.
                        tracing::debug!("order({}) poll error: {e}", id_owned.0);
                        let _ = tx.send(OrderEvent::Cancelled { id: id_owned.clone() }).await;
                        return;
                    }
                };

                let total = resp.original_size;
                let matched = resp.size_matched;

                if matched > last_matched {
                    last_matched = matched;
                    let _ = tx.send(OrderEvent::Filled {
                        id: id_owned.clone(),
                        fill_price: resp.price,
                        shares_filled: matched,
                        total_shares: total,
                    }).await;
                    // continue regardless — partial fills may turn into full
                }

                match resp.status {
                    OrderStatusType::Matched => {
                        // Fully filled; ensure caller sees the final fill event.
                        return;
                    }
                    OrderStatusType::Canceled => {
                        let _ = tx.send(OrderEvent::Cancelled { id: id_owned.clone() }).await;
                        return;
                    }
                    OrderStatusType::Live | OrderStatusType::Delayed | OrderStatusType::Unmatched => {
                        // Keep polling.
                    }
                    _ => {
                        let _ = tx.send(OrderEvent::Rejected {
                            id: id_owned.clone(),
                            reason: format!("unexpected status {:?}", resp.status),
                        }).await;
                        return;
                    }
                }
            }
        });
        Ok(rx)
    }
}

/// One event in the lifecycle of a single order. Emitted by `OrderEventStream`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderEvent {
    /// Partial or full fill. `shares_filled` is cumulative across fills on this
    /// order so far; `total_shares` is the original order size.
    Filled {
        id: OrderId,
        fill_price: Decimal,
        shares_filled: Decimal,
        total_shares: Decimal,
    },
    /// Order cancelled (by us or by the exchange — e.g. market close).
    Cancelled { id: OrderId },
    /// Exchange rejected the order or it expired without fill.
    Rejected { id: OrderId, reason: String },
}

#[async_trait]
pub trait OrderEventStream: Send + Sync {
    /// Subscribe to events for `id`. Returns a channel that fires when the
    /// order reaches a terminal state (Filled-fully, Cancelled, Rejected) or
    /// when partial fills happen. Caller should drop the receiver when done.
    async fn watch(&self, id: OrderId) -> Result<mpsc::Receiver<OrderEvent>, StreamError>;
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Test stub that emits a scripted sequence per order_id. Each entry in
    /// `script` is a list of events for that id, emitted in order.
    pub struct ScriptedOrderEvents {
        pub script: Mutex<std::collections::HashMap<OrderId, Vec<OrderEvent>>>,
    }
    impl ScriptedOrderEvents {
        pub fn new() -> std::sync::Arc<Self> {
            std::sync::Arc::new(Self {
                script: Mutex::new(std::collections::HashMap::new()),
            })
        }
        pub fn add(&self, id: OrderId, events: Vec<OrderEvent>) {
            self.script.lock().unwrap().insert(id, events);
        }
    }
    #[async_trait]
    impl OrderEventStream for ScriptedOrderEvents {
        async fn watch(&self, id: OrderId) -> Result<mpsc::Receiver<OrderEvent>, StreamError> {
            let events = self.script.lock().unwrap().remove(&id).unwrap_or_default();
            let (tx, rx) = mpsc::channel(8);
            tokio::spawn(async move {
                for ev in events {
                    if tx.send(ev).await.is_err() {
                        return;
                    }
                    // Tiny pause so callers using paused-time can interleave.
                    tokio::task::yield_now().await;
                }
                drop(tx);
            });
            Ok(rx)
        }
    }

    #[tokio::test]
    async fn scripted_emits_in_order() {
        let s = ScriptedOrderEvents::new();
        let id = OrderId("o1".into());
        s.add(id.clone(), vec![
            OrderEvent::Filled {
                id: id.clone(),
                fill_price: Decimal::new(85, 2), // 0.85
                shares_filled: Decimal::from(10),
                total_shares: Decimal::from(10),
            },
        ]);
        let mut rx = s.watch(id.clone()).await.unwrap();
        let ev = rx.recv().await.unwrap();
        match ev {
            OrderEvent::Filled { shares_filled, total_shares, .. } => {
                assert_eq!(shares_filled, Decimal::from(10));
                assert_eq!(total_shares, Decimal::from(10));
            }
            _ => panic!("expected Filled"),
        }
    }

    #[tokio::test]
    async fn scripted_returns_empty_for_unknown_id() {
        let s = ScriptedOrderEvents::new();
        let mut rx = s.watch(OrderId("never-added".into())).await.unwrap();
        // Channel closes immediately because no events were scripted.
        let r = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        // Either timeout or None — both fine; channel didn't deliver anything.
        match r {
            Ok(None) => {}        // channel closed
            Err(_) => {}          // timeout
            Ok(Some(_)) => panic!("expected no events"),
        }
    }

    // Smoke only — real polling against CLOB requires authenticated client.
    // Exercised end-to-end in tests/maker_integration.rs.
    #[test]
    fn polymarket_poll_order_events_constructs() {
        // Just verify the type exists with the expected name.
        // We can't construct without a real authenticated SDK Client.
        fn _assert_sized<T: Sized>() {}
        _assert_sized::<super::PolymarketPollOrderEvents>();
    }
}
