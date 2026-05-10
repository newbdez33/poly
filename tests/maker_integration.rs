#![cfg(test)]

use async_trait::async_trait;
use chrono::Utc;
use poly_tui::trader::adapters::redis_stream_wrapper::RedisTraderStream;
use poly_tui::trader::adapters::simulated_executor::SimulatedExecutor;
use poly_tui::trader::errors::StreamError;
use poly_tui::trader::event::{TraderEvent, TraderEventEmitter, TraderEventKind};
use poly_tui::trader::executor::OrderId;
use poly_tui::trader::order_events::{OrderEvent, OrderEventStream};
use poly_tui::tui::events::TraderEventStream;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;
use tokio::sync::mpsc;

/// Scripted OrderEventStream for integration tests. Mirrors the lib's
/// in-tree test stub but lives here because integration tests are a separate
/// crate and don't see `#[cfg(test)] pub mod tests` items from the lib.
struct ScriptedOrderEvents {
    script: Mutex<HashMap<OrderId, Vec<OrderEvent>>>,
}
impl ScriptedOrderEvents {
    fn new() -> Arc<Self> {
        Arc::new(Self { script: Mutex::new(HashMap::new()) })
    }
    fn add(&self, id: OrderId, events: Vec<OrderEvent>) {
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
                if tx.send(ev).await.is_err() { return; }
                tokio::task::yield_now().await;
            }
            drop(tx);
        });
        Ok(rx)
    }
}

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "tests must NOT bind dev Redis port");
    (node, format!("redis://127.0.0.1:{port}"))
}

#[tokio::test]
#[ignore]
async fn maker_event_sequence_roundtrips_through_redis_stream() {
    // Verifies the four new TraderEventKind variants survive the
    // Redis stream emit->subscribe roundtrip. Standalone — doesn't run
    // run_maker (that's covered by lib unit tests).
    let (_node, url) = start_redis().await;
    let stream_impl = Arc::new(RedisTraderStream::connect(&url).await.unwrap());
    let emitter: Arc<dyn TraderEventEmitter> = stream_impl.clone();
    let session_id = uuid::Uuid::new_v4();
    let ladder = poly_tui::trader::ladder::LadderState::new(
        poly_tui::trader::ladder::Direction::Up,
        Decimal::from(5), 5, Utc::now(),
    );

    let evt_kinds = vec![
        TraderEventKind::BuyLimitPosted {
            order_id: "ord-1".into(),
            price: Decimal::from_str("0.49").unwrap(),
        },
        TraderEventKind::BuyLimitSwept {
            from_price: Decimal::from_str("0.49").unwrap(),
            to_price: Decimal::from_str("0.50").unwrap(),
        },
        TraderEventKind::TpLimitPosted {
            order_id: "ord-2".into(),
            price: Decimal::from_str("0.85").unwrap(),
        },
        TraderEventKind::TpLimitFilled {
            order_id: "ord-2".into(),
            fill_price: Decimal::from_str("0.85").unwrap(),
            shares: Decimal::from(10),
            partial: false,
        },
    ];
    for kind in &evt_kinds {
        let ev = TraderEvent {
            ts: Utc::now(),
            session_id,
            kind: kind.clone(),
            ladder: ladder.clone(),
        };
        emitter.emit(&ev).await.unwrap();
    }

    let stream: Arc<dyn TraderEventStream> = stream_impl;
    let tail = stream.tail(64).await.unwrap();
    let history: Vec<_> = tail.history.iter()
        .filter(|e| e.session_id == session_id)
        .map(|e| e.kind.clone())
        .collect();

    assert_eq!(history.len(), 4);
    assert!(matches!(history[0], TraderEventKind::BuyLimitPosted { .. }));
    assert!(matches!(history[1], TraderEventKind::BuyLimitSwept { .. }));
    assert!(matches!(history[2], TraderEventKind::TpLimitPosted { .. }));
    assert!(matches!(history[3], TraderEventKind::TpLimitFilled { partial: false, .. }));
}

#[tokio::test]
#[ignore]
async fn run_maker_full_window_redis_emits_expected_events() {
    use poly_tui::trader::ladder::{LadderState, Direction, WindowOutcome};
    use poly_tui::trader::maker::{run_maker, MakerDeps};
    use poly_tui::trader::market::WindowMarket;
    use poly_tui::trader::exit_watcher::ExitConfig;
    use poly_tui::trader::price::MidwindowPriceFetcher;
    use poly_tui::trader::errors::PriceError;
    use tokio_util::sync::CancellationToken;

    let (_node, url) = start_redis().await;
    let emitter: Arc<dyn TraderEventEmitter> = Arc::new(
        RedisTraderStream::connect(&url).await.unwrap()
    );

    struct ConstPrice { p: Decimal }
    #[async_trait::async_trait]
    impl MidwindowPriceFetcher for ConstPrice {
        async fn current_bid(&self, _: &str) -> Result<Decimal, PriceError> { Ok(self.p) }
    }

    let executor = Arc::new(SimulatedExecutor::default());
    let events: Arc<dyn OrderEventStream> = {
        let s = ScriptedOrderEvents::new();
        s.add(OrderId("sim-order-0".into()), vec![
            OrderEvent::Filled {
                id: OrderId("sim-order-0".into()),
                fill_price: Decimal::from_str("0.49").unwrap(),
                shares_filled: Decimal::from(10),
                total_shares: Decimal::from(10),
            },
        ]);
        s.add(OrderId("sim-order-1".into()), vec![
            OrderEvent::Filled {
                id: OrderId("sim-order-1".into()),
                fill_price: Decimal::from_str("0.85").unwrap(),
                shares_filled: Decimal::from(10),
                total_shares: Decimal::from(10),
            },
        ]);
        s
    };
    let price: Arc<dyn MidwindowPriceFetcher> = Arc::new(ConstPrice { p: Decimal::from_str("0.55").unwrap() });

    let market = WindowMarket {
        window_ts: chrono::Utc::now().timestamp(),
        slug: "test".into(),
        up_token_id: "tok-up".into(), down_token_id: "tok-down".into(),
        up_ask: Decimal::from_str("0.50").unwrap(),
        down_ask: Decimal::from_str("0.50").unwrap(),
        closed: false, winner: None, price_to_beat: None,
    };
    let ladder = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());

    let outcome = run_maker(
        &MakerDeps { executor, events, price, emitter: emitter.clone() },
        &ladder, &market, "tok-up",
        Decimal::from(5), Decimal::from_str("0.50").unwrap(),
        &ExitConfig {
            tp_price: Decimal::from_str("0.85").unwrap(),
            sl_price: Decimal::from_str("0.45").unwrap(),
            poll: Duration::from_millis(50),
        },
        market.window_ts,
        CancellationToken::new(),
    ).await;

    assert!(matches!(outcome, WindowOutcome::Won { .. }));
}
