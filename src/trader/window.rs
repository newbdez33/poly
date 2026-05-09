use crate::trader::errors::{ExecError, MarketError, ResolveError};
use crate::trader::event::{
    EntryDecision, OrderKind, TraderEventEmitter, TraderEventKind, WinLose,
};
use crate::trader::executor::{compute_share_count, meets_minimum, OrderExecutor};
use crate::trader::ladder::{Direction, LadderState, SkipReason, WindowOutcome};
use crate::trader::market::{MarketDiscovery, WindowMarket};
use crate::trader::resolver::WindowResolver;
use rust_decimal::Decimal;
use std::sync::Arc;

pub struct WindowDeps {
    pub market: Arc<dyn MarketDiscovery>,
    pub executor: Arc<dyn OrderExecutor>,
    pub resolver: Arc<dyn WindowResolver>,
    pub emitter: Arc<dyn TraderEventEmitter>,
}

pub struct WindowConfig {
    pub band_min: Decimal,
    pub band_max: Decimal,
}

/// Execute one 5-min window. Returns the WindowOutcome the FSM consumes.
pub async fn run_window(
    deps: &WindowDeps,
    cfg: &WindowConfig,
    ladder: &LadderState,
    window_ts: i64,
) -> WindowOutcome {
    // Step 1: discover market
    let market = match deps.market.find_window(window_ts).await {
        Ok(m) => m,
        Err(MarketError::NotFound { .. }) => {
            emit_kind(deps, ladder, TraderEventKind::EntryDecision {
                decision: EntryDecision::SkipNotFound,
            }).await;
            return WindowOutcome::Skipped { reason: SkipReason::MarketNotFound };
        }
        Err(_) => {
            emit_kind(deps, ladder, TraderEventKind::EntryDecision {
                decision: EntryDecision::SkipNotFound,
            }).await;
            return WindowOutcome::Skipped { reason: SkipReason::GammaApiUnavailable };
        }
    };

    emit_kind(deps, ladder, TraderEventKind::WindowOpening {
        window_ts,
        slug: market.slug.clone(),
    }).await;

    // Step 2: price band check
    let ask = market.ask_for(ladder.direction);
    if ask < cfg.band_min || ask > cfg.band_max {
        emit_kind(deps, ladder, TraderEventKind::EntryDecision {
            decision: EntryDecision::SkipBand { ask },
        }).await;
        return WindowOutcome::Skipped {
            reason: SkipReason::PriceOutsideBand { ask },
        };
    }
    emit_kind(deps, ladder, TraderEventKind::EntryDecision {
        decision: EntryDecision::Enter { ask },
    }).await;

    // Step 3: FoK buy
    let dollars = ladder.current_bet_usd();
    let token_id = market.token_id_for(ladder.direction).to_string();
    let shares_needed = compute_share_count(dollars, ask);
    if !meets_minimum(shares_needed) {
        emit_kind(deps, ladder, TraderEventKind::OrderRejected {
            reason: format!("below 5-share minimum: {shares_needed}"),
        }).await;
        return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
    }
    emit_kind(deps, ladder, TraderEventKind::OrderPlaced {
        kind: OrderKind::Buy,
        dollars,
        token_id: token_id.clone(),
    }).await;

    let buy_fill = match deps.executor.buy_fok(&token_id, dollars).await {
        Ok(f) => f,
        Err(ExecError::FillOrKillFailed) => {
            emit_kind(deps, ladder, TraderEventKind::OrderRejected {
                reason: "FoK rejected".into(),
            }).await;
            return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
        }
        Err(e) => {
            emit_kind(deps, ladder, TraderEventKind::OrderRejected {
                reason: format!("{e}"),
            }).await;
            return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
        }
    };
    emit_kind(deps, ladder, TraderEventKind::OrderFilled {
        fill_price: buy_fill.fill_price,
        shares: buy_fill.shares,
        dollars: buy_fill.dollars,
    }).await;

    // Step 4: await resolution
    let resolution = match deps.resolver.await_resolution(&market).await {
        Ok(r) => r,
        Err(ResolveError::Timeout { .. }) => {
            emit_kind(deps, ladder, TraderEventKind::ResolutionTimeout).await;
            return WindowOutcome::Skipped { reason: SkipReason::ResolutionTimeout };
        }
        Err(e) => {
            emit_kind(deps, ladder, TraderEventKind::Alert {
                message: format!("resolver error: {e}"),
            }).await;
            return WindowOutcome::Skipped { reason: SkipReason::GammaApiUnavailable };
        }
    };

    let our_won = resolution.winner == ladder.direction;
    emit_kind(deps, ladder, TraderEventKind::Resolved {
        winner: resolution.winner,
        our_side: ladder.direction,
        our_outcome: if our_won { WinLose::Win } else { WinLose::Lose },
    }).await;

    if !our_won {
        return WindowOutcome::Lost { spent_usd: buy_fill.dollars };
    }

    // Step 5: sell winning shares
    let sell_fill = match deps.executor.sell_market(&token_id, buy_fill.shares).await {
        Ok(f) => f,
        Err(e) => {
            emit_kind(deps, ladder, TraderEventKind::SellRejected {
                reason: format!("{e}"),
            }).await;
            // Critical: shares stuck. Emit Alert; return Won with proceeds=0
            // (FSM resets ladder; user must clean up manually).
            emit_kind(deps, ladder, TraderEventKind::Alert {
                message: format!("sell failed; shares stuck for token {token_id}"),
            }).await;
            return WindowOutcome::Won { proceeds_usd: Decimal::ZERO };
        }
    };
    emit_kind(deps, ladder, TraderEventKind::SellFilled {
        proceeds_usd: sell_fill.dollars,
    }).await;

    WindowOutcome::Won { proceeds_usd: sell_fill.dollars }
}

async fn emit_kind(
    deps: &WindowDeps,
    ladder: &LadderState,
    kind: TraderEventKind,
) {
    use crate::trader::event::TraderEvent;
    use chrono::Utc;
    let event = TraderEvent {
        ts: Utc::now(),
        session_id: ladder.session_id,
        kind,
        ladder: ladder.clone(),
    };
    let _ = deps.emitter.emit(&event).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trader::errors::EmitError;
    use crate::trader::event::TraderEvent;
    use crate::trader::executor::FillResult;
    use crate::trader::resolver::Resolution;
    use async_trait::async_trait;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use std::sync::Mutex;

    struct StubMarket {
        result: Mutex<Option<Result<WindowMarket, MarketError>>>,
    }
    impl StubMarket {
        fn ok(m: WindowMarket) -> Arc<Self> {
            Arc::new(Self { result: Mutex::new(Some(Ok(m))) })
        }
        fn err(e: MarketError) -> Arc<Self> {
            Arc::new(Self { result: Mutex::new(Some(Err(e))) })
        }
    }
    #[async_trait]
    impl MarketDiscovery for StubMarket {
        async fn find_window(&self, _ts: i64) -> Result<WindowMarket, MarketError> {
            self.result.lock().unwrap().take()
                .unwrap_or_else(|| Err(MarketError::NotFound { window_ts: 0 }))
        }
    }

    struct StubExec {
        buy: Mutex<Option<Result<FillResult, ExecError>>>,
        sell: Mutex<Option<Result<FillResult, ExecError>>>,
    }
    impl StubExec {
        fn buy_only(buy: Result<FillResult, ExecError>) -> Arc<Self> {
            Arc::new(Self { buy: Mutex::new(Some(buy)), sell: Mutex::new(None) })
        }
        fn buy_then_sell(
            buy: Result<FillResult, ExecError>,
            sell: Result<FillResult, ExecError>,
        ) -> Arc<Self> {
            Arc::new(Self {
                buy: Mutex::new(Some(buy)),
                sell: Mutex::new(Some(sell)),
            })
        }
    }
    #[async_trait]
    impl OrderExecutor for StubExec {
        async fn buy_fok(&self, _t: &str, _d: Decimal) -> Result<FillResult, ExecError> {
            self.buy.lock().unwrap().take()
                .unwrap_or(Err(ExecError::FillOrKillFailed))
        }
        async fn sell_market(&self, _t: &str, _s: Decimal) -> Result<FillResult, ExecError> {
            self.sell.lock().unwrap().take()
                .unwrap_or(Err(ExecError::FillOrKillFailed))
        }
    }

    struct StubResolver {
        result: Mutex<Option<Result<Resolution, ResolveError>>>,
    }
    impl StubResolver {
        fn won(side: Direction) -> Arc<Self> {
            Arc::new(Self {
                result: Mutex::new(Some(Ok(Resolution { winner: side }))),
            })
        }
        fn timeout() -> Arc<Self> {
            Arc::new(Self {
                result: Mutex::new(Some(Err(ResolveError::Timeout { seconds: 60 }))),
            })
        }
    }
    #[async_trait]
    impl WindowResolver for StubResolver {
        async fn await_resolution(&self, _m: &WindowMarket)
            -> Result<Resolution, ResolveError>
        {
            self.result.lock().unwrap().take()
                .unwrap_or(Err(ResolveError::Timeout { seconds: 60 }))
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
    #[async_trait]
    impl TraderEventEmitter for CapturingEmitter {
        async fn emit(&self, ev: &TraderEvent) -> Result<(), EmitError> {
            self.events.lock().unwrap().push(ev.clone());
            Ok(())
        }
    }

    fn fresh_ladder() -> LadderState {
        LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now())
    }

    fn open_market_at(up_ask: &str, down_ask: &str) -> WindowMarket {
        WindowMarket {
            window_ts: 1700000300, slug: "btc-updown-5m-1700000300".into(),
            up_token_id: "tok-up".into(), down_token_id: "tok-down".into(),
            up_ask: Decimal::from_str(up_ask).unwrap(),
            down_ask: Decimal::from_str(down_ask).unwrap(),
            closed: false, winner: None,
        }
    }

    fn cfg() -> WindowConfig {
        WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
        }
    }

    #[tokio::test]
    async fn happy_path_won() {
        let market = open_market_at("0.50", "0.50");
        let emitter = CapturingEmitter::new();
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_then_sell(
                Ok(FillResult {
                    fill_price: Decimal::from_str("0.50").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from(5),
                }),
                Ok(FillResult {
                    fill_price: Decimal::from_str("0.99").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from_str("9.90").unwrap(),
                }),
            ),
            resolver: StubResolver::won(Direction::Up),
            emitter: emitter.clone(),
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Won { ref proceeds_usd } if *proceeds_usd == Decimal::from_str("9.90").unwrap()
        ));
    }

    #[tokio::test]
    async fn happy_path_lost() {
        let market = open_market_at("0.50", "0.50");
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_only(Ok(FillResult {
                fill_price: Decimal::from_str("0.50").unwrap(),
                shares: Decimal::from(10),
                dollars: Decimal::from(5),
            })),
            resolver: StubResolver::won(Direction::Down),
            emitter: CapturingEmitter::new(),
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Lost { ref spent_usd } if *spent_usd == Decimal::from(5)
        ));
    }

    #[tokio::test]
    async fn skip_market_not_found() {
        let deps = WindowDeps {
            market: StubMarket::err(MarketError::NotFound { window_ts: 1700000300 }),
            executor: StubExec::buy_only(Err(ExecError::FillOrKillFailed)),
            resolver: StubResolver::timeout(),
            emitter: CapturingEmitter::new(),
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome, WindowOutcome::Skipped { reason: SkipReason::MarketNotFound }));
    }

    #[tokio::test]
    async fn skip_gamma_api_error() {
        let deps = WindowDeps {
            market: StubMarket::err(MarketError::Network("502".into())),
            executor: StubExec::buy_only(Err(ExecError::FillOrKillFailed)),
            resolver: StubResolver::timeout(),
            emitter: CapturingEmitter::new(),
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Skipped { reason: SkipReason::GammaApiUnavailable }
        ));
    }

    #[tokio::test]
    async fn skip_price_outside_band() {
        let market = open_market_at("0.62", "0.38");
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_only(Err(ExecError::FillOrKillFailed)),
            resolver: StubResolver::timeout(),
            emitter: CapturingEmitter::new(),
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { .. } }
        ));
    }

    #[tokio::test]
    async fn skip_fok_failed() {
        let market = open_market_at("0.50", "0.50");
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_only(Err(ExecError::FillOrKillFailed)),
            resolver: StubResolver::timeout(),
            emitter: CapturingEmitter::new(),
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed }
        ));
    }

    #[tokio::test]
    async fn skip_resolution_timeout() {
        let market = open_market_at("0.50", "0.50");
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_only(Ok(FillResult {
                fill_price: Decimal::from_str("0.50").unwrap(),
                shares: Decimal::from(10),
                dollars: Decimal::from(5),
            })),
            resolver: StubResolver::timeout(),
            emitter: CapturingEmitter::new(),
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Skipped { reason: SkipReason::ResolutionTimeout }
        ));
    }

    #[tokio::test]
    async fn won_but_sell_failed_emits_alert_and_returns_zero_proceeds() {
        let market = open_market_at("0.50", "0.50");
        let emitter = CapturingEmitter::new();
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_then_sell(
                Ok(FillResult {
                    fill_price: Decimal::from_str("0.50").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from(5),
                }),
                Err(ExecError::Network("boom".into())),
            ),
            resolver: StubResolver::won(Direction::Up),
            emitter: emitter.clone(),
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome, WindowOutcome::Won { ref proceeds_usd } if *proceeds_usd == Decimal::ZERO));

        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::SellRejected { .. })));
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::Alert { .. })));
    }

    #[tokio::test]
    async fn happy_path_emits_expected_event_sequence() {
        let market = open_market_at("0.50", "0.50");
        let emitter = CapturingEmitter::new();
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_then_sell(
                Ok(FillResult {
                    fill_price: Decimal::from_str("0.50").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from(5),
                }),
                Ok(FillResult {
                    fill_price: Decimal::from_str("0.99").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from_str("9.90").unwrap(),
                }),
            ),
            resolver: StubResolver::won(Direction::Up),
            emitter: emitter.clone(),
        };
        run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        let kinds = emitter.kinds();
        let names: Vec<_> = kinds.iter().map(|k| match k {
            TraderEventKind::WindowOpening { .. } => "WindowOpening",
            TraderEventKind::EntryDecision { .. } => "EntryDecision",
            TraderEventKind::OrderPlaced { .. } => "OrderPlaced",
            TraderEventKind::OrderFilled { .. } => "OrderFilled",
            TraderEventKind::Resolved { .. } => "Resolved",
            TraderEventKind::SellFilled { .. } => "SellFilled",
            other => panic!("unexpected: {other:?}"),
        }).collect();
        assert_eq!(names, [
            "WindowOpening", "EntryDecision",
            "OrderPlaced", "OrderFilled",
            "Resolved", "SellFilled",
        ]);
    }
}
