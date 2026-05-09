use crate::trader::errors::{ExecError, MarketError, ResolveError};
use crate::trader::event::{
    EntryDecision, OrderKind, TraderEventEmitter, TraderEventKind, WinLose,
};
use crate::trader::executor::{compute_share_count, meets_minimum, FillResult, OrderExecutor};
use crate::trader::exit_watcher::ExitConfig;
use crate::trader::ladder::{Direction, LadderState, SkipReason, WindowOutcome};
use crate::trader::market::{MarketDiscovery, WindowMarket};
use crate::trader::price::MidwindowPriceFetcher;
use crate::trader::resolver::{Resolution, WindowResolver};
use rust_decimal::Decimal;
use std::sync::Arc;

pub struct WindowDeps {
    pub market: Arc<dyn MarketDiscovery>,
    pub executor: Arc<dyn OrderExecutor>,
    pub resolver: Arc<dyn WindowResolver>,
    pub emitter: Arc<dyn TraderEventEmitter>,
    pub price: Arc<dyn MidwindowPriceFetcher>,
}

pub struct WindowConfig {
    pub band_min: Decimal,
    pub band_max: Decimal,
    pub exit: Option<ExitConfig>,
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

    // Step 4: branch on exit rule
    let buy_dollars = buy_fill.dollars;
    match &cfg.exit {
        None => {
            // v1.1 path: hold to resolution, sell winner
            await_resolution_and_sweep(deps, ladder, &market, &token_id, &buy_fill).await
        }
        Some(exit_cfg) => {
            // v1.5 path: race ExitWatcher vs await_resolution
            run_with_tp_sl(
                deps, ladder, &market, &token_id, &buy_fill, exit_cfg, buy_dollars, window_ts,
            ).await
        }
    }
}

/// v1.1 path: existing await_resolution + winner sweep.
async fn await_resolution_and_sweep(
    deps: &WindowDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    buy_fill: &FillResult,
) -> WindowOutcome {
    let r = deps.resolver.await_resolution(market).await;
    winner_sweep(deps, ladder, token_id, buy_fill, r).await
}

/// Shared post-resolution path: handle Timeout/error, on win sell market,
/// on lose return Lost. Used by both v1.1 await_resolution_and_sweep and
/// the v1.5 select! resolver branch.
async fn winner_sweep(
    deps: &WindowDeps,
    ladder: &LadderState,
    token_id: &str,
    buy_fill: &FillResult,
    r: Result<Resolution, ResolveError>,
) -> WindowOutcome {
    let resolution = match r {
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

    let sell_fill = match deps.executor.sell_market(token_id, buy_fill.shares).await {
        Ok(f) => f,
        Err(e) => {
            emit_kind(deps, ladder, TraderEventKind::SellRejected { reason: format!("{e}") }).await;
            emit_kind(deps, ladder, TraderEventKind::Alert {
                message: format!("sell failed; shares stuck for token {token_id}"),
            }).await;
            return WindowOutcome::Won { proceeds_usd: Decimal::ZERO };
        }
    };
    emit_kind(deps, ladder, TraderEventKind::SellFilled { proceeds_usd: sell_fill.dollars }).await;
    WindowOutcome::Won { proceeds_usd: sell_fill.dollars }
}

/// v1.5 path: race ExitWatcher against resolver. Earliest finisher wins.
async fn run_with_tp_sl(
    deps: &WindowDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    buy_fill: &FillResult,
    exit_cfg: &crate::trader::exit_watcher::ExitConfig,
    buy_dollars: Decimal,
    window_ts: i64,
) -> WindowOutcome {
    use crate::trader::exit_watcher::ExitWatcher;
    let watcher = ExitWatcher::new(deps.price.clone(), exit_cfg.clone());
    // Watcher should stop polling no later than the actual window close
    // (window_ts + 300s). Anchor deadline to absolute window-close time so
    // it doesn't drift with buy-fill latency.
    let close_unix = window_ts + 300;
    let now_unix = chrono::Utc::now().timestamp();
    let remaining = (close_unix - now_unix).max(0) as u64;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(remaining);

    let trigger: Option<crate::trader::exit_watcher::ExitTrigger> = tokio::select! {
        t = watcher.watch(token_id, deadline) => t,
        r = deps.resolver.await_resolution(market) => {
            return winner_sweep(deps, ladder, token_id, buy_fill, r).await;
        }
    };

    let trig = match trigger {
        Some(t) => t,
        None => {
            // Watcher hit deadline without crossing tp/sl. Fall through to resolver.
            return await_resolution_and_sweep(deps, ladder, market, token_id, buy_fill).await;
        }
    };

    // Emit ExitTriggered BEFORE sell so the trace shows trigger reason
    // regardless of fill success.
    emit_kind(deps, ladder, TraderEventKind::ExitTriggered {
        kind: trig.kind,
        bid: trig.bid,
    }).await;

    // TP or SL fired. Sell now and report outcome based on proceeds vs cost.
    let sell_fill = match deps.executor.sell_market(token_id, buy_fill.shares).await {
        Ok(f) => f,
        Err(e) => {
            emit_kind(deps, ladder, TraderEventKind::SellRejected { reason: format!("{e}") }).await;
            emit_kind(deps, ladder, TraderEventKind::Alert {
                message: format!("tp/sl sell failed; shares stuck for token {token_id}"),
            }).await;
            return WindowOutcome::Won { proceeds_usd: Decimal::ZERO };
        }
    };
    emit_kind(deps, ladder, TraderEventKind::SellFilled { proceeds_usd: sell_fill.dollars }).await;
    if sell_fill.dollars > buy_dollars {
        WindowOutcome::Won { proceeds_usd: sell_fill.dollars }
    } else {
        WindowOutcome::Lost { spent_usd: buy_dollars - sell_fill.dollars }
    }
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
    use crate::trader::exit_watcher::{ExitConfig, ExitKind};
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
            price_to_beat: None,
        }
    }

    fn stub_price(constant: &str) -> Arc<crate::trader::price::tests::StubPriceFetcher> {
        let value = Decimal::from_str(constant).unwrap();
        let mut q = vec![];
        for _ in 0..1000 { q.push(Ok(value)); }
        Arc::new(crate::trader::price::tests::StubPriceFetcher::new(q))
    }

    fn cfg() -> WindowConfig {
        WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: None,
        }
    }

    fn cfg_with_exit(exit: ExitConfig) -> WindowConfig {
        WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: Some(exit),
        }
    }

    #[tokio::test]
    async fn cfg_default_keeps_exit_none() {
        // Smoke: existing tests build WindowConfig without exit; default is None.
        let c = cfg();
        assert!(c.exit.is_none());
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
            price: stub_price("0.50"),
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
            price: stub_price("0.50"),
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
            price: stub_price("0.50"),
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
            price: stub_price("0.50"),
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
            price: stub_price("0.50"),
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
            price: stub_price("0.50"),
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
            price: stub_price("0.50"),
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
            price: stub_price("0.50"),
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
            price: stub_price("0.50"),
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

    fn open_market_with_token_ids() -> WindowMarket {
        WindowMarket {
            window_ts: 1700000300, slug: "btc-updown-5m-1700000300".into(),
            up_token_id: "tok-up".into(), down_token_id: "tok-down".into(),
            up_ask: Decimal::from_str("0.50").unwrap(),
            down_ask: Decimal::from_str("0.50").unwrap(),
            closed: false, winner: None,
            price_to_beat: None,
        }
    }

    /// Stub resolver that never returns until cancelled.
    struct NeverResolver;
    #[async_trait]
    impl WindowResolver for NeverResolver {
        async fn await_resolution(&self, _m: &WindowMarket)
            -> Result<Resolution, ResolveError>
        {
            std::future::pending().await
        }
    }

    fn scripted_price(prices: Vec<&str>) -> Arc<crate::trader::price::tests::StubPriceFetcher> {
        let q: Vec<_> = prices.iter()
            .map(|p| Ok(Decimal::from_str(p).unwrap()))
            .collect();
        Arc::new(crate::trader::price::tests::StubPriceFetcher::new(q))
    }

    #[tokio::test(start_paused = true)]
    async fn tp_sl_path_tp_triggers_returns_won() {
        let market = open_market_with_token_ids();
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
                    fill_price: Decimal::from_str("0.85").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from_str("8.40").unwrap(),
                }),
            ),
            resolver: Arc::new(NeverResolver),
            emitter: emitter.clone(),
            price: scripted_price(vec!["0.55", "0.70", "0.86"]),
        };
        let cfg = cfg_with_exit(ExitConfig {
            tp_price: Decimal::from_str("0.85").unwrap(),
            sl_price: Decimal::from_str("0.45").unwrap(),
            poll: std::time::Duration::from_millis(100),
        });
        // window_ts must be in the wall-clock future so deadline = now + 300s
        // (vs. now + 0s, which would skip watcher polling entirely).
        let future_ts = chrono::Utc::now().timestamp() + 60;
        let outcome = run_window(&deps, &cfg, &fresh_ladder(), future_ts).await;
        assert!(matches!(outcome,
            WindowOutcome::Won { ref proceeds_usd } if *proceeds_usd == Decimal::from_str("8.40").unwrap()));
        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::ExitTriggered { kind: ExitKind::Tp, .. })));
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::SellFilled { .. })));
    }

    #[tokio::test(start_paused = true)]
    async fn tp_sl_path_sl_triggers_returns_lost() {
        let market = open_market_with_token_ids();
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
                    fill_price: Decimal::from_str("0.45").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from_str("4.40").unwrap(),
                }),
            ),
            resolver: Arc::new(NeverResolver),
            emitter: emitter.clone(),
            price: scripted_price(vec!["0.50", "0.45"]),
        };
        let cfg = cfg_with_exit(ExitConfig {
            tp_price: Decimal::from_str("0.85").unwrap(),
            sl_price: Decimal::from_str("0.45").unwrap(),
            poll: std::time::Duration::from_millis(100),
        });
        // window_ts must be in the wall-clock future so deadline = now + 300s
        // (vs. now + 0s, which would skip watcher polling entirely).
        let future_ts = chrono::Utc::now().timestamp() + 60;
        let outcome = run_window(&deps, &cfg, &fresh_ladder(), future_ts).await;
        assert!(matches!(outcome,
            WindowOutcome::Lost { ref spent_usd } if *spent_usd == Decimal::from_str("0.60").unwrap()));
        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::ExitTriggered { kind: ExitKind::Sl, .. })));
    }

    #[tokio::test]
    async fn tp_sl_watcher_skips_when_window_already_closed() {
        // window_ts already 10 min in the past → close_unix < now → deadline = now.
        // Watcher returns None immediately. Fall-through resolver returns Timeout
        // (using StubResolver::timeout) so the outcome is Skipped{ResolutionTimeout}.
        // This proves: (a) watcher saw deadline-already-past and returned None
        // without firing, and (b) the deadline fall-through resolver path was taken.
        let market = open_market_with_token_ids();
        let emitter = CapturingEmitter::new();
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_only(Ok(FillResult {
                fill_price: Decimal::from_str("0.50").unwrap(),
                shares: Decimal::from(10),
                dollars: Decimal::from(5),
            })),
            resolver: StubResolver::timeout(),
            emitter: emitter.clone(),
            // price stub never triggers; should not be polled because deadline is in the past.
            price: stub_price("0.50"),
        };
        let cfg = cfg_with_exit(ExitConfig {
            tp_price: Decimal::from_str("0.85").unwrap(),
            sl_price: Decimal::from_str("0.45").unwrap(),
            poll: std::time::Duration::from_millis(100),
        });
        let past_window_ts = chrono::Utc::now().timestamp() - 600;
        let outcome = run_window(&deps, &cfg, &fresh_ladder(), past_window_ts).await;
        assert!(matches!(outcome,
            WindowOutcome::Skipped { reason: SkipReason::ResolutionTimeout }),
            "past-deadline window must fall through to resolver and hit timeout");
        let kinds = emitter.kinds();
        assert!(!kinds.iter().any(|k| matches!(k, TraderEventKind::ExitTriggered { .. })),
                "no ExitTriggered event when watcher deadline is already past");
    }

    #[tokio::test(start_paused = true)]
    async fn tp_sl_path_no_trigger_falls_through_to_resolver() {
        // Price stays at 0.50 forever; deadline reached without trigger.
        let market = open_market_with_token_ids();
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
            price: stub_price("0.50"),
        };
        let cfg = cfg_with_exit(ExitConfig {
            tp_price: Decimal::from_str("0.85").unwrap(),
            sl_price: Decimal::from_str("0.45").unwrap(),
            poll: std::time::Duration::from_millis(50),
        });
        let outcome = run_window(&deps, &cfg, &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Won { ref proceeds_usd } if *proceeds_usd == Decimal::from_str("9.90").unwrap()));
        let kinds = emitter.kinds();
        assert!(!kinds.iter().any(|k| matches!(k, TraderEventKind::ExitTriggered { .. })),
                "no exit-triggered event when deadline path fires");
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::Resolved { .. })),
                "resolved event when deadline path fires");
    }
}
