use crate::trader::errors::StreamError;
use crate::trader::executor::OrderId;
use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

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
}
