use crate::trader::event::{TraderEventEmitter, TraderEventKind};
use crate::trader::executor::{OrderExecutor, OrderId, OrderSide};
use crate::trader::exit_watcher::ExitConfig;
use crate::trader::ladder::{LadderState, SkipReason, WindowOutcome};
use crate::trader::market::WindowMarket;
use crate::trader::order_events::{OrderEvent, OrderEventStream};
use crate::trader::price::MidwindowPriceFetcher;
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub struct MakerDeps {
    pub executor: Arc<dyn OrderExecutor>,
    pub events: Arc<dyn OrderEventStream>,
    pub price: Arc<dyn MidwindowPriceFetcher>,
    pub emitter: Arc<dyn TraderEventEmitter>,
}

/// Run a single window in maker mode. Caller has already done band check; we
/// receive the entry `ask` so we can build the sweep ladder.
pub async fn run_maker(
    deps: &MakerDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    dollars: Decimal,    // stake from ladder.current_bet_usd()
    ask: Decimal,        // reference for sweep ladder
    exit_cfg: &ExitConfig,
    window_ts: i64,
    window_seconds: i64, // window length (300/900/3600 for {5,15,60}-min)
    shutdown: CancellationToken,
) -> WindowOutcome {
    // Phase 1: PendingBuy with sweep at t=30/60, give up at t=90.
    let buy_fill = match buy_with_sweep(deps, ladder, token_id, dollars, ask, &shutdown).await {
        BuyOutcome::Filled { shares, dollars_spent, fill_price } => {
            BuyFill { shares, dollars: dollars_spent, fill_price }
        }
        BuyOutcome::Skipped => {
            return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
        }
        BuyOutcome::ShutdownDuringBuy => {
            return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
        }
    };

    // Phase 2: PendingTpSell with SL price-watch + cancel-and-market-sell at
    // window_ts + window_seconds - 30.
    sell_with_tp_sl(deps, ladder, market, token_id, &buy_fill, exit_cfg, window_ts, window_seconds, shutdown).await
}

#[derive(Debug)]
enum BuyOutcome {
    Filled { shares: Decimal, dollars_spent: Decimal, fill_price: Decimal },
    Skipped,
    ShutdownDuringBuy,
}

#[derive(Clone, Debug)]
struct BuyFill {
    shares: Decimal,
    dollars: Decimal,
    #[allow(dead_code)]
    fill_price: Decimal,
}

/// Phase 1 — sweep BUY with 30s/60s/90s schedule.
async fn buy_with_sweep(
    deps: &MakerDeps,
    ladder: &LadderState,
    token_id: &str,
    dollars: Decimal,
    ask: Decimal,
    shutdown: &CancellationToken,
) -> BuyOutcome {
    // Three price steps: ask-0.01, ask, ask+0.01. Round to 0.01 tick.
    let prices = [
        round_tick(ask - Decimal::new(1, 2)),
        round_tick(ask),
        round_tick(ask + Decimal::new(1, 2)),
    ];
    let step_durations = [Duration::from_secs(30), Duration::from_secs(30), Duration::from_secs(30)];

    let mut current_price: Option<Decimal> = None;
    let mut current_id: Option<OrderId> = None;

    for (step_idx, (&step_price, &step_dur)) in prices.iter().zip(step_durations.iter()).enumerate() {
        // Compute share size: floor(dollars / step_price), require >=5 shares.
        let shares = if step_price > Decimal::ZERO {
            (dollars / step_price).floor()
        } else {
            Decimal::ZERO
        };
        if shares < Decimal::from(5) {
            // Can't post a sub-min order. Skip the whole window.
            return BuyOutcome::Skipped;
        }

        // Cancel previous step's order if any.
        if let (Some(prev_id), Some(prev_price)) = (current_id.take(), current_price) {
            let _ = deps.executor.cancel(&prev_id).await;
            emit(&deps.emitter, ladder, TraderEventKind::BuyLimitSwept {
                from_price: prev_price, to_price: step_price,
            }).await;
        }

        // Post new BUY limit at step_price.
        let new_id = match deps.executor.place_limit(token_id, OrderSide::Buy, step_price, shares).await {
            Ok(id) => id,
            Err(e) => {
                emit(&deps.emitter, ladder, TraderEventKind::OrderRejected {
                    reason: format!("place_limit step {step_idx}: {e}"),
                }).await;
                return BuyOutcome::Skipped;
            }
        };
        emit(&deps.emitter, ladder, TraderEventKind::BuyLimitPosted {
            order_id: new_id.0.clone(), price: step_price,
        }).await;
        current_id = Some(new_id.clone());
        current_price = Some(step_price);

        // Subscribe to fills for this order.
        let mut events_rx = match deps.events.watch(new_id.clone()).await {
            Ok(rx) => rx,
            Err(_) => {
                // Stream subscription failed — bail out, cancel order.
                let _ = deps.executor.cancel(&new_id).await;
                return BuyOutcome::Skipped;
            }
        };

        // Wait for either: fill, sweep-step deadline, or shutdown.
        let deadline = tokio::time::Instant::now() + step_dur;
        let mut total_filled = Decimal::ZERO;
        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => {
                    let _ = deps.executor.cancel(&new_id).await;
                    return BuyOutcome::ShutdownDuringBuy;
                }
                _ = tokio::time::sleep_until(deadline) => {
                    // Step deadline hit. Break out, advance to next step (or terminal).
                    break;
                }
                ev = events_rx.recv() => {
                    match ev {
                        None => break, // channel closed → assume terminal
                        Some(OrderEvent::Filled { shares_filled, total_shares, fill_price, .. }) => {
                            total_filled = shares_filled;
                            if total_filled >= total_shares {
                                // Fully filled.
                                emit(&deps.emitter, ladder, TraderEventKind::OrderFilled {
                                    fill_price,
                                    shares: total_filled,
                                    dollars: total_filled * fill_price,
                                }).await;
                                return BuyOutcome::Filled {
                                    shares: total_filled,
                                    dollars_spent: total_filled * fill_price,
                                    fill_price,
                                };
                            }
                            // Else partial — keep looping until full or deadline.
                        }
                        Some(OrderEvent::Cancelled { .. }) => {
                            // Externally cancelled (rare). Move to next sweep step.
                            break;
                        }
                        Some(OrderEvent::Rejected { reason, .. }) => {
                            emit(&deps.emitter, ladder, TraderEventKind::OrderRejected { reason }).await;
                            // Move to next step (or terminal).
                            break;
                        }
                    }
                }
            }
        }

        // If we got a partial fill on the current step, accept it as the buy.
        // The remaining unfilled portion is dropped (per spec).
        if total_filled >= Decimal::from(5) {
            emit(&deps.emitter, ladder, TraderEventKind::OrderFilled {
                fill_price: step_price,
                shares: total_filled,
                dollars: total_filled * step_price,
            }).await;
            // Cancel the (now partially-filled) resting order before moving on.
            if let Some(id) = current_id.take() {
                let _ = deps.executor.cancel(&id).await;
            }
            return BuyOutcome::Filled {
                shares: total_filled,
                dollars_spent: total_filled * step_price,
                fill_price: step_price,
            };
        }
    }

    // All three steps exhausted without enough fill. Cancel last and skip.
    if let Some(id) = current_id {
        let _ = deps.executor.cancel(&id).await;
    }
    BuyOutcome::Skipped
}

/// Phase 2 — TP limit + SL price watch + cancel-and-market-sell at
/// `window_ts + window_seconds - 30` (e.g. t=270 for 5min, t=870 for 15min).
async fn sell_with_tp_sl(
    deps: &MakerDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    buy_fill: &BuyFill,
    exit_cfg: &ExitConfig,
    window_ts: i64,
    window_seconds: i64,
    shutdown: CancellationToken,
) -> WindowOutcome {
    let _ = market; // keep param for future fields (resolution path); silence warn.

    // Post TP limit @ tp_price for full shares.
    let tp_id = match deps.executor.place_limit(token_id, OrderSide::Sell, exit_cfg.tp_price, buy_fill.shares).await {
        Ok(id) => id,
        Err(e) => {
            // TP placement failed — fall back to market sell at current bid.
            emit(&deps.emitter, ladder, TraderEventKind::OrderRejected {
                reason: format!("tp place_limit: {e}"),
            }).await;
            return market_sell_residual(deps, ladder, token_id, buy_fill.shares, buy_fill.dollars, &exit_cfg.sl_price).await;
        }
    };
    emit(&deps.emitter, ladder, TraderEventKind::TpLimitPosted {
        order_id: tp_id.0.clone(), price: exit_cfg.tp_price,
    }).await;

    let mut tp_events = match deps.events.watch(tp_id.clone()).await {
        Ok(rx) => Some(rx),
        Err(_) => {
            let _ = deps.executor.cancel(&tp_id).await;
            return market_sell_residual(deps, ladder, token_id, buy_fill.shares, buy_fill.dollars, &exit_cfg.sl_price).await;
        }
    };

    // Tp_partial_proceeds tracked across partial fills.
    let mut tp_partial_shares = Decimal::ZERO;
    let mut tp_partial_proceeds = Decimal::ZERO;

    // Cancel-at-(window_ts + window_seconds - 30) absolute deadline.
    let cancel_unix = window_ts + window_seconds - 30;
    let now_unix = chrono::Utc::now().timestamp();
    let cancel_after = (cancel_unix - now_unix).max(0) as u64;
    let cancel_deadline = tokio::time::Instant::now() + Duration::from_secs(cancel_after);

    // SL price watch — poll gamma every poll_secs.
    let mut sl_ticker = tokio::time::interval(exit_cfg.poll);

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                let _ = deps.executor.cancel(&tp_id).await;
                let residual = buy_fill.shares - tp_partial_shares;
                if residual >= Decimal::from(5) {
                    // Best-effort cleanup sell.
                    let bid = exit_cfg.sl_price; // worst-case hint
                    let r = deps.executor.sell_at_bid(token_id, residual, bid).await;
                    if let Ok(f) = r {
                        return final_outcome(buy_fill.dollars, tp_partial_proceeds + f.dollars);
                    }
                }
                return final_outcome(buy_fill.dollars, tp_partial_proceeds);
            }

            _ = tokio::time::sleep_until(cancel_deadline) => {
                // t=270s reached. Cancel TP, market sell residual.
                let _ = deps.executor.cancel(&tp_id).await;
                let residual = buy_fill.shares - tp_partial_shares;
                if residual < Decimal::from(5) {
                    // Nothing to sell (TP took it all or was unfilled with too few shares).
                    return final_outcome(buy_fill.dollars, tp_partial_proceeds);
                }
                let bid = match deps.price.current_bid(token_id).await {
                    Ok(b) => b,
                    Err(_) => exit_cfg.sl_price, // fallback worst-case
                };
                let sell_fill = match deps.executor.sell_at_bid(token_id, residual, bid).await {
                    Ok(f) => f,
                    Err(e) => {
                        emit(&deps.emitter, ladder, TraderEventKind::SellRejected { reason: format!("{e}") }).await;
                        emit(&deps.emitter, ladder, TraderEventKind::Alert {
                            message: format!("end-of-window sell failed; shares stuck for token {token_id}"),
                        }).await;
                        return WindowOutcome::Won { proceeds_usd: tp_partial_proceeds };
                    }
                };
                emit(&deps.emitter, ladder, TraderEventKind::SellFilled {
                    proceeds_usd: sell_fill.dollars,
                }).await;
                return final_outcome(buy_fill.dollars, tp_partial_proceeds + sell_fill.dollars);
            }

            _ = sl_ticker.tick() => {
                // Poll bid; if <= sl_price, trigger SL exit.
                if let Ok(bid) = deps.price.current_bid(token_id).await {
                    if bid <= exit_cfg.sl_price {
                        // Cancel TP, market sell residual.
                        let _ = deps.executor.cancel(&tp_id).await;
                        emit(&deps.emitter, ladder, TraderEventKind::ExitTriggered {
                            kind: crate::trader::exit_watcher::ExitKind::Sl, bid,
                        }).await;
                        let residual = buy_fill.shares - tp_partial_shares;
                        if residual < Decimal::from(5) {
                            return final_outcome(buy_fill.dollars, tp_partial_proceeds);
                        }
                        let sell_fill = match deps.executor.sell_at_bid(token_id, residual, bid).await {
                            Ok(f) => f,
                            Err(e) => {
                                emit(&deps.emitter, ladder, TraderEventKind::SellRejected { reason: format!("{e}") }).await;
                                emit(&deps.emitter, ladder, TraderEventKind::Alert {
                                    message: format!("sl sell failed; shares stuck for token {token_id}"),
                                }).await;
                                return WindowOutcome::Won { proceeds_usd: tp_partial_proceeds };
                            }
                        };
                        emit(&deps.emitter, ladder, TraderEventKind::SellFilled {
                            proceeds_usd: sell_fill.dollars,
                        }).await;
                        return final_outcome(buy_fill.dollars, tp_partial_proceeds + sell_fill.dollars);
                    }
                }
                // else: keep waiting for TP fill or deadline
            }

            ev = async {
                match tp_events.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match ev {
                    None => {
                        // Channel closed — drop receiver so this branch becomes
                        // permanently pending instead of hot-looping.
                        tp_events = None;
                    }
                    Some(OrderEvent::Filled { shares_filled, total_shares, fill_price, .. }) => {
                        let new_filled = shares_filled - tp_partial_shares;
                        if new_filled <= Decimal::ZERO {
                            continue;
                        }
                        let proceeds_delta = new_filled * fill_price;
                        tp_partial_shares = shares_filled;
                        tp_partial_proceeds = tp_partial_proceeds + proceeds_delta;

                        let is_full = shares_filled >= total_shares;
                        emit(&deps.emitter, ladder, TraderEventKind::TpLimitFilled {
                            order_id: tp_id.0.clone(),
                            fill_price,
                            shares: new_filled,
                            partial: !is_full,
                        }).await;
                        if is_full {
                            emit(&deps.emitter, ladder, TraderEventKind::SellFilled {
                                proceeds_usd: tp_partial_proceeds,
                            }).await;
                            return final_outcome(buy_fill.dollars, tp_partial_proceeds);
                        }
                        // else: keep watching for further fills or SL/deadline
                    }
                    Some(OrderEvent::Cancelled { .. }) | Some(OrderEvent::Rejected { .. }) => {
                        // TP no longer resting; drop subscriber, fall through to
                        // SL/deadline waiters.
                        tp_events = None;
                    }
                }
            }
        }
    }
}

/// Helper for the rare path where we couldn't even post the TP — straight to
/// market sell, treat as one-shot exit.
async fn market_sell_residual(
    deps: &MakerDeps,
    ladder: &LadderState,
    token_id: &str,
    shares: Decimal,
    cost: Decimal,
    fallback_bid: &Decimal,
) -> WindowOutcome {
    let bid = match deps.price.current_bid(token_id).await {
        Ok(b) => b,
        Err(_) => *fallback_bid,
    };
    let sell_fill = match deps.executor.sell_at_bid(token_id, shares, bid).await {
        Ok(f) => f,
        Err(e) => {
            emit(&deps.emitter, ladder, TraderEventKind::SellRejected { reason: format!("{e}") }).await;
            emit(&deps.emitter, ladder, TraderEventKind::Alert {
                message: format!("market sell failed; shares stuck for token {token_id}"),
            }).await;
            return WindowOutcome::Won { proceeds_usd: Decimal::ZERO };
        }
    };
    emit(&deps.emitter, ladder, TraderEventKind::SellFilled { proceeds_usd: sell_fill.dollars }).await;
    final_outcome(cost, sell_fill.dollars)
}

fn final_outcome(buy_dollars: Decimal, total_proceeds: Decimal) -> WindowOutcome {
    if total_proceeds > buy_dollars {
        WindowOutcome::Won { proceeds_usd: total_proceeds }
    } else {
        WindowOutcome::Lost { spent_usd: buy_dollars - total_proceeds }
    }
}

/// Round a Decimal to the nearest 0.01 tick. Polymarket BTC market tick=0.01.
fn round_tick(p: Decimal) -> Decimal {
    p.round_dp(2)
}

async fn emit(
    emitter: &Arc<dyn TraderEventEmitter>,
    ladder: &LadderState,
    kind: TraderEventKind,
) {
    use crate::trader::event::TraderEvent;
    let event = TraderEvent {
        ts: chrono::Utc::now(),
        session_id: ladder.session_id,
        kind,
        ladder: ladder.clone(),
    };
    let _ = emitter.emit(&event).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trader::errors::{EmitError, PriceError};
    use crate::trader::event::TraderEvent;
    use crate::trader::executor::FillResult;
    use crate::trader::ladder::Direction;
    use crate::trader::order_events::tests::ScriptedOrderEvents;
    use chrono::Utc;
    use std::str::FromStr;
    use std::sync::Mutex;

    // -----------------------------------------------------------------------
    // Stubs
    // -----------------------------------------------------------------------
    struct StubExec {
        place_calls: Mutex<Vec<(OrderSide, Decimal, Decimal)>>, // (side, price, shares)
        cancel_calls: Mutex<Vec<OrderId>>,
        sell_calls: Mutex<Vec<(Decimal, Decimal)>>, // (shares, bid)
        sell_response: Mutex<Result<FillResult, crate::trader::errors::ExecError>>,
        order_counter: std::sync::atomic::AtomicU64,
    }
    impl StubExec {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                place_calls: Mutex::new(vec![]),
                cancel_calls: Mutex::new(vec![]),
                sell_calls: Mutex::new(vec![]),
                sell_response: Mutex::new(Ok(FillResult {
                    fill_price: Decimal::from_str("0.5").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from(5),
                })),
                order_counter: std::sync::atomic::AtomicU64::new(0),
            })
        }
        fn next_id(&self) -> OrderId {
            let n = self.order_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            OrderId(format!("stub-{n}"))
        }
    }
    #[async_trait::async_trait]
    impl OrderExecutor for StubExec {
        async fn buy_fok(&self, _t: &str, _d: Decimal) -> Result<FillResult, crate::trader::errors::ExecError> {
            unimplemented!()
        }
        async fn sell_market(&self, _t: &str, _s: Decimal) -> Result<FillResult, crate::trader::errors::ExecError> {
            unimplemented!()
        }
        async fn sell_at_bid(&self, _t: &str, shares: Decimal, bid: Decimal)
            -> Result<FillResult, crate::trader::errors::ExecError>
        {
            self.sell_calls.lock().unwrap().push((shares, bid));
            self.sell_response.lock().unwrap().clone()
                .map(|f| FillResult { shares, dollars: shares * bid, fill_price: bid, ..f })
        }
        async fn place_limit(&self, _t: &str, side: OrderSide, price: Decimal, shares: Decimal)
            -> Result<OrderId, crate::trader::errors::ExecError>
        {
            self.place_calls.lock().unwrap().push((side, price, shares));
            Ok(self.next_id())
        }
        async fn cancel(&self, id: &OrderId) -> Result<(), crate::trader::errors::ExecError> {
            self.cancel_calls.lock().unwrap().push(id.clone());
            Ok(())
        }
    }

    impl Clone for crate::trader::errors::ExecError {
        fn clone(&self) -> Self {
            // Simple clone via thiserror-friendly variants.
            match self {
                crate::trader::errors::ExecError::FillOrKillFailed => Self::FillOrKillFailed,
                crate::trader::errors::ExecError::Network(s) => Self::Network(s.clone()),
                crate::trader::errors::ExecError::Decode(s) => Self::Decode(s.clone()),
                crate::trader::errors::ExecError::InsufficientFunds => Self::InsufficientFunds,
                crate::trader::errors::ExecError::NotSupported => Self::NotSupported,
            }
        }
    }

    struct StubPrice {
        bids: Mutex<Vec<Result<Decimal, PriceError>>>,
    }
    impl StubPrice {
        fn const_bid(b: &str) -> Arc<Self> {
            Arc::new(Self {
                bids: Mutex::new(vec![Ok(Decimal::from_str(b).unwrap()); 1000]),
            })
        }
    }
    #[async_trait::async_trait]
    impl MidwindowPriceFetcher for StubPrice {
        async fn current_bid(&self, _: &str) -> Result<Decimal, PriceError> {
            let mut q = self.bids.lock().unwrap();
            if q.is_empty() {
                return Err(PriceError::Network("drained".into()));
            }
            q.remove(0)
        }
    }

    #[derive(Default)]
    struct CapturingEmitter {
        events: Mutex<Vec<TraderEvent>>,
    }
    impl CapturingEmitter {
        fn new() -> Arc<Self> { Arc::new(Self::default()) }
        fn kinds(&self) -> Vec<TraderEventKind> {
            self.events.lock().unwrap().iter().map(|e| e.kind.clone()).collect()
        }
    }
    #[async_trait::async_trait]
    impl TraderEventEmitter for CapturingEmitter {
        async fn emit(&self, ev: &TraderEvent) -> Result<(), EmitError> {
            self.events.lock().unwrap().push(ev.clone());
            Ok(())
        }
    }

    fn fresh_ladder() -> LadderState {
        LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now())
    }

    fn cfg() -> ExitConfig {
        ExitConfig {
            tp_price: Decimal::from_str("0.85").unwrap(),
            sl_price: Decimal::from_str("0.45").unwrap(),
            poll: Duration::from_millis(50),
        }
    }

    fn fake_market() -> WindowMarket {
        WindowMarket {
            window_ts: 1700000300, slug: "btc-updown-5m-1700000300".into(),
            up_token_id: "tok-up".into(), down_token_id: "tok-down".into(),
            up_ask: Decimal::from_str("0.50").unwrap(),
            down_ask: Decimal::from_str("0.50").unwrap(),
            closed: false, winner: None, price_to_beat: None,
        }
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------
    #[tokio::test(start_paused = true)]
    async fn buy_fills_immediately_then_tp_fills_returns_won() {
        let exec = StubExec::new();
        let events = ScriptedOrderEvents::new();
        // Pre-script: buy order id "stub-0" gets full fill at 0.49 (10 sh).
        events.add(OrderId("stub-0".into()), vec![
            OrderEvent::Filled {
                id: OrderId("stub-0".into()),
                fill_price: Decimal::from_str("0.49").unwrap(),
                shares_filled: Decimal::from(10),
                total_shares: Decimal::from(10),
            },
        ]);
        // TP order id "stub-1" gets full fill at 0.85.
        events.add(OrderId("stub-1".into()), vec![
            OrderEvent::Filled {
                id: OrderId("stub-1".into()),
                fill_price: Decimal::from_str("0.85").unwrap(),
                shares_filled: Decimal::from(10),
                total_shares: Decimal::from(10),
            },
        ]);
        let price = StubPrice::const_bid("0.55");
        let emitter = CapturingEmitter::new();
        let deps = MakerDeps {
            executor: exec.clone(), events: events.clone(), price: price.clone(), emitter: emitter.clone(),
        };

        let outcome = run_maker(
            &deps, &fresh_ladder(), &fake_market(), "tok-up",
            Decimal::from(5), Decimal::from_str("0.50").unwrap(),
            &cfg(),
            chrono::Utc::now().timestamp(), // now -> cancel deadline ~270s in future
            300,
            CancellationToken::new(),
        ).await;

        let proceeds = match outcome {
            WindowOutcome::Won { proceeds_usd } => proceeds_usd,
            other => panic!("expected Won, got {other:?}"),
        };
        // 10 sh x 0.85 = 8.50 proceeds
        assert!(proceeds >= Decimal::from_str("8.40").unwrap());

        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::BuyLimitPosted { .. })));
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::OrderFilled { .. })));
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::TpLimitPosted { .. })));
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::TpLimitFilled { partial: false, .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn buy_never_fills_three_steps_then_skipped() {
        let exec = StubExec::new();
        let events = ScriptedOrderEvents::new(); // no scripted fills -> all 3 buys time out
        let price = StubPrice::const_bid("0.55");
        let emitter = CapturingEmitter::new();
        let deps = MakerDeps {
            executor: exec.clone(), events: events.clone(), price: price.clone(), emitter: emitter.clone(),
        };

        let outcome = run_maker(
            &deps, &fresh_ladder(), &fake_market(), "tok-up",
            Decimal::from(5), Decimal::from_str("0.50").unwrap(),
            &cfg(),
            chrono::Utc::now().timestamp(),
            300,
            CancellationToken::new(),
        ).await;

        assert!(matches!(outcome, WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed }));
        // 3 place_limit calls, 3 cancels (each step + final).
        assert_eq!(exec.place_calls.lock().unwrap().len(), 3);
        // BuyLimitSwept emitted twice (between steps).
        let kinds = emitter.kinds();
        let swept_count = kinds.iter().filter(|k| matches!(k, TraderEventKind::BuyLimitSwept { .. })).count();
        assert_eq!(swept_count, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn sl_triggers_during_hold_phase() {
        let exec = StubExec::new();
        let events = ScriptedOrderEvents::new();
        events.add(OrderId("stub-0".into()), vec![
            OrderEvent::Filled {
                id: OrderId("stub-0".into()),
                fill_price: Decimal::from_str("0.49").unwrap(),
                shares_filled: Decimal::from(10),
                total_shares: Decimal::from(10),
            },
        ]);
        // TP never fills.
        // Price drops to 0.40 (<= sl_price 0.45) on second poll.
        let price = Arc::new(StubPrice {
            bids: Mutex::new(vec![
                Ok(Decimal::from_str("0.50").unwrap()),
                Ok(Decimal::from_str("0.40").unwrap()),
            ]),
        });
        let emitter = CapturingEmitter::new();
        let deps = MakerDeps {
            executor: exec.clone(), events: events.clone(),
            price: price as Arc<dyn MidwindowPriceFetcher>,
            emitter: emitter.clone(),
        };

        let outcome = run_maker(
            &deps, &fresh_ladder(), &fake_market(), "tok-up",
            Decimal::from(5), Decimal::from_str("0.50").unwrap(),
            &cfg(),
            chrono::Utc::now().timestamp(),
            300,
            CancellationToken::new(),
        ).await;

        // Sold 10 shares at 0.40 -> $4.00 proceeds vs $4.90 buy -> Lost $0.90.
        assert!(matches!(outcome, WindowOutcome::Lost { .. }));
        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k,
            TraderEventKind::ExitTriggered { kind: crate::trader::exit_watcher::ExitKind::Sl, .. }
        )));
    }

    /// Always-on bid stub. Unlike `StubPrice::const_bid` (1000-entry vec) this
    /// never drains, which matters when the test runs to a 270s+ deadline with
    /// a 50ms poll (5400+ ticks).
    struct InfBid(Decimal);
    #[async_trait::async_trait]
    impl MidwindowPriceFetcher for InfBid {
        async fn current_bid(&self, _: &str) -> Result<Decimal, PriceError> { Ok(self.0) }
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_deadline_scales_with_window_seconds_15m() {
        // 15min window: cancel deadline is window_ts + 900 - 30 = window_ts + 870.
        // For a window 600s in the past, deadline = 270s in the future, which lets
        // the test run cleanly under tokio::time::pause().
        let exec = StubExec::new();
        let events = ScriptedOrderEvents::new();
        events.add(OrderId("stub-0".into()), vec![
            OrderEvent::Filled {
                id: OrderId("stub-0".into()),
                fill_price: Decimal::from_str("0.49").unwrap(),
                shares_filled: Decimal::from(10),
                total_shares: Decimal::from(10),
            },
        ]);
        // TP never fills; SL never fires (price stays > 0.45). InfBid avoids
        // draining the bid queue under sub-second polling for 270+s deadline.
        let price: Arc<dyn MidwindowPriceFetcher> = Arc::new(InfBid(Decimal::from_str("0.55").unwrap()));
        let emitter = CapturingEmitter::new();
        let deps = MakerDeps {
            executor: exec.clone(), events: events.clone(),
            price, emitter: emitter.clone(),
        };

        // 600s ago window_ts: with window_seconds=900, cancel = window_ts + 870
        // ≈ now + 270s. Final residual market sell @ stub bid 0.55.
        let window_ts = chrono::Utc::now().timestamp() - 600;
        let outcome = run_maker(
            &deps, &fresh_ladder(), &fake_market(), "tok-up",
            Decimal::from(5), Decimal::from_str("0.50").unwrap(),
            &cfg(), window_ts,
            900,  // window_seconds = 15min
            CancellationToken::new(),
        ).await;
        // Buy: 10 sh @ 0.49 = $4.90. Sell: 10 sh @ 0.55 = $5.50. Won $0.60.
        assert!(matches!(outcome, WindowOutcome::Won { .. }),
                "outcome was: {outcome:?}");
    }
}
