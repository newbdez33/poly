use crate::trader::errors::{ExecError, MarketError, ResolveError};
use crate::trader::event::{
    EntryDecision, OrderKind, TraderEventEmitter, TraderEventKind, WinLose,
};
use crate::trader::executor::{compute_share_count, meets_minimum, FillResult, OrderExecutor};
use crate::trader::exit_watcher::ExitConfig;
use crate::trader::ladder::{Direction, LadderState, SkipReason, WindowOutcome};
use crate::trader::market::{MarketDiscovery, WindowMarket};
use crate::trader::order_events::OrderEventStream;
use crate::trader::price::MidwindowPriceFetcher;
use crate::trader::resolver::{Resolution, WindowResolver};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::sync::Arc;

pub struct WindowDeps {
    pub market: Arc<dyn MarketDiscovery>,
    pub executor: Arc<dyn OrderExecutor>,
    pub resolver: Arc<dyn WindowResolver>,
    pub emitter: Arc<dyn TraderEventEmitter>,
    pub price: Arc<dyn MidwindowPriceFetcher>,
    pub events: Arc<dyn OrderEventStream>,
    /// v1.9: Chainlink BTC/USD oracle on Polygon — used to determine window
    /// outcome at t=window_close-4s without waiting for gamma resolution.
    /// Pairs with Polymarket Auto-Redeem for instant cash credit on wins.
    pub btc_price: Arc<dyn crate::tui::market_watch::BtcPriceFeed>,
}

pub struct WindowConfig {
    pub band_min: Decimal,
    pub band_max: Decimal,
    pub exit: Option<ExitConfig>,
    /// v1.8: seconds into the window at which to market-sell.
    /// Mutually exclusive with `exit` — at most one is `Some`.
    pub exit_at_secs: Option<u32>,
    pub maker: bool,
    /// Window length in seconds (300/900/3600 for {5,15,60}-min). Threaded
    /// into `run_maker` so its cancel deadline scales with window length.
    pub window_seconds: i64,
}

/// Execute one 5-min window. Returns the WindowOutcome the FSM consumes.
pub async fn run_window(
    deps: &WindowDeps,
    cfg: &WindowConfig,
    ladder: &LadderState,
    window_ts: i64,
) -> WindowOutcome {
    // Step 1: discover market
    let mins = (cfg.window_seconds / 60) as u32;
    let market = match deps.market.find_window(window_ts, mins).await {
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

    // Step 3: Buy. Maker mode places its own limit buy inside run_maker; only
    // the taker path does the FoK here.
    let dollars = ladder.current_bet_dollars(ask);
    let token_id = market.token_id_for(ladder.direction).to_string();

    if cfg.maker && cfg.exit.is_some() {
        // Maker path takes over from here — no FoK.
        let exit_cfg = cfg.exit.as_ref().unwrap();
        let maker_deps = crate::trader::maker::MakerDeps {
            executor: deps.executor.clone(),
            events: deps.events.clone(),
            price: deps.price.clone(),
            emitter: deps.emitter.clone(),
        };
        return crate::trader::maker::run_maker(
            &maker_deps, ladder, &market, &token_id, dollars, ask, exit_cfg,
            window_ts, cfg.window_seconds,
            tokio_util::sync::CancellationToken::new(),
        ).await;
    }

    // Taker path (existing v1.5/v1.6 behaviour) — unchanged below.
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
    if let Some(exit_at_secs) = cfg.exit_at_secs {
        return run_hold_early_exit(
            deps, ladder, &market, &token_id, &buy_fill,
            exit_at_secs, window_ts, cfg.window_seconds,
        ).await;
    }
    match &cfg.exit {
        None => {
            // v1.9 path: hold + chainlink-determined outcome at t=close-4s.
            // Auto-Redeem credits winning shares to USDC; trader skips sell.
            determine_outcome_pre_close(
                deps, ladder, &market, &buy_fill, window_ts, cfg.window_seconds,
            ).await
        }
        Some(exit_cfg) => {
            // v1.5 path: race ExitWatcher vs await_resolution
            run_with_tp_sl(
                deps, ladder, &market, &token_id, &buy_fill, exit_cfg, window_ts,
            ).await
        }
    }
}

/// v1.9 hold mode: at t=window_close-OUTCOME_LEAD_SECS query Chainlink BTC,
/// compare to price_to_beat, emit Won/Lost. No sell — Polymarket Auto-Redeem
/// handles cash crediting for winning shares.
///
/// Trade-offs vs old gamma-resolver approach:
/// - Fast: no UMA resolution lag (gamma typically ~3-10s post-close).
/// - Scheduler doesn't miss next window (returns ~4s before window close).
/// - Risk: BTC could move in the final 4s and flip the outcome (~1% windows
///   on borderline cases). Auto-Redeem pays the correct cash regardless;
///   only the ladder's instantaneous accounting may briefly disagree, then
///   self-corrects on next window.
const OUTCOME_LEAD_SECS: i64 = 4;

async fn determine_outcome_pre_close(
    deps: &WindowDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    buy_fill: &FillResult,
    window_ts: i64,
    window_seconds: i64,
) -> WindowOutcome {
    // Sleep until t=window_close - 4s.
    let determine_at = window_ts + window_seconds - OUTCOME_LEAD_SECS;
    let now = chrono::Utc::now().timestamp();
    let wait = (determine_at - now).max(0) as u64;
    if wait > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
    }

    // Determine winner from Chainlink BTC vs price_to_beat.
    // Gamma populates `priceToBeat` async ~5-30s after window open. The
    // WindowMarket captured at BUY time often has price_to_beat=None.
    // Re-fetch here — by t=window_close-4s, gamma has definitely populated it.
    let mins = (window_seconds / 60) as u32;
    let fresh_market = match deps.market.find_window(window_ts, mins).await {
        Ok(m) => m,
        Err(_) => {
            // Stale data is acceptable here — fall back to the original market.
            market.clone()
        }
    };
    let price_to_beat = match fresh_market.price_to_beat.or(market.price_to_beat) {
        Some(p) => p,
        None => {
            // Still no reference price — fall back to gamma resolver.
            emit_kind(deps, ladder, TraderEventKind::Alert {
                message: "price_to_beat missing; falling back to gamma resolver".into(),
            }).await;
            let r = deps.resolver.await_resolution(market).await;
            return winner_sweep_no_sell(deps, ladder, buy_fill, r).await;
        }
    };
    let btc_price = match deps.btc_price.latest_price().await {
        Ok(p) => p,
        Err(e) => {
            // Chainlink down — fall back to gamma resolver. This may miss
            // the next window boundary but we get the correct outcome.
            emit_kind(deps, ladder, TraderEventKind::Alert {
                message: format!("chainlink fetch failed ({e}); falling back to gamma resolver"),
            }).await;
            let r = deps.resolver.await_resolution(market).await;
            return winner_sweep_no_sell(deps, ladder, buy_fill, r).await;
        }
    };

    let up_won = btc_price > price_to_beat;
    let our_won = match ladder.direction {
        Direction::Up => up_won,
        Direction::Down => !up_won,
    };

    emit_kind(deps, ladder, TraderEventKind::Resolved {
        winner: if up_won { Direction::Up } else { Direction::Down },
        our_side: ladder.direction,
        our_outcome: if our_won { WinLose::Win } else { WinLose::Lose },
    }).await;

    if our_won {
        // Auto-Redeem will credit shares × $1.00 to USDC at gamma's resolution.
        // proceeds_usd = expected redeem payout for ladder accounting.
        WindowOutcome::Won {
            proceeds_usd: buy_fill.shares,  // shares × $1.00 = shares (Decimal)
            cost_usd: buy_fill.dollars,
        }
    } else {
        WindowOutcome::Lost { spent_usd: buy_fill.dollars }
    }
}

/// Fallback for chainlink failures: gamma-resolution path without sell_market.
/// We KNOW the winner from gamma; Auto-Redeem will pay the cash. Emit the
/// outcome and let the FSM advance the ladder.
async fn winner_sweep_no_sell(
    deps: &WindowDeps,
    ladder: &LadderState,
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

    if our_won {
        WindowOutcome::Won {
            proceeds_usd: buy_fill.shares,
            cost_usd: buy_fill.dollars,
        }
    } else {
        WindowOutcome::Lost { spent_usd: buy_fill.dollars }
    }
}

/// v1.8 path: hold, then market-sell at t = exit_at_secs into the window.
/// No resolver wait, no redemption — avoids the MATIC redeem blocker.
async fn run_hold_early_exit(
    deps: &WindowDeps,
    ladder: &LadderState,
    _market: &WindowMarket,
    token_id: &str,
    buy_fill: &FillResult,
    exit_at_secs: u32,
    window_ts: i64,
    window_seconds: i64,
) -> WindowOutcome {
    let now = chrono::Utc::now().timestamp();
    let deadline = window_ts + exit_at_secs as i64;
    let wait_secs = (deadline - now).max(0) as u64;

    // Hard cap: don't sleep past window close. Defensive — validate() should
    // reject exit_at_secs > window_seconds - 30 already.
    let cap = (window_seconds - 30).max(0) as u64;
    let wait_secs = wait_secs.min(cap);

    if wait_secs > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
    }

    // Fetch the current bid as a fill-price hint. Real CLOB ignores the hint;
    // dry-run simulator uses it so PnL reflects the actual mid-window price
    // (not the hardcoded $0.99 winning-payout default of `sell_market`).
    let bid = match deps.price.current_bid(token_id).await {
        Ok(b) => b,
        Err(e) => {
            emit_kind(deps, ladder, TraderEventKind::Alert {
                message: format!("hold-early-exit: bid fetch failed ({e}); selling without hint"),
            }).await;
            Decimal::ZERO
        }
    };

    // Market-sell the entire position at the current bid.
    let sell_fill = match deps.executor.sell_at_bid(token_id, buy_fill.shares, bid).await {
        Ok(f) => f,
        Err(e) => {
            emit_kind(deps, ladder, TraderEventKind::SellRejected {
                reason: format!("{e}"),
            }).await;
            emit_kind(deps, ladder, TraderEventKind::Alert {
                message: format!("hold-early-exit sell failed; shares stuck for token {token_id}"),
            }).await;
            // FAK SELL failed → shares stuck awaiting resolution. Use the
            // current bid as the outcome signal:
            //   - bid > 0.5 → window is leaning UP (we picked UP). Will likely
            //     resolve UP-winner; Auto-Redeem credits ~shares × $1.00.
            //     Emit Won so Martingale ladder does NOT escalate.
            //   - bid ≤ 0.5 → window is leaning DOWN. Will likely resolve $0.
            //     Emit Lost so Martingale escalates stake on the next window.
            //
            // The estimated proceeds for Won (shares × bid) is a best-guess
            // for `realized_pnl_usd` accounting — Auto-Redeem actually pays
            // shares × $1.00, so this slightly under-counts wins. The ladder
            // FSM only cares about Won-vs-Lost branching, not the magnitude.
            return if bid > dec!(0.5) {
                WindowOutcome::Won {
                    proceeds_usd: buy_fill.shares * bid,
                    cost_usd: buy_fill.dollars,
                }
            } else {
                WindowOutcome::Lost { spent_usd: buy_fill.dollars }
            };
        }
    };
    emit_kind(deps, ladder, TraderEventKind::SellFilled {
        proceeds_usd: sell_fill.dollars,
    }).await;

    if sell_fill.dollars > buy_fill.dollars {
        WindowOutcome::Won { proceeds_usd: sell_fill.dollars, cost_usd: buy_fill.dollars }
    } else {
        WindowOutcome::Lost { spent_usd: buy_fill.dollars - sell_fill.dollars }
    }
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
            return WindowOutcome::Won { proceeds_usd: Decimal::ZERO, cost_usd: buy_fill.dollars };
        }
    };
    emit_kind(deps, ladder, TraderEventKind::SellFilled { proceeds_usd: sell_fill.dollars }).await;
    WindowOutcome::Won { proceeds_usd: sell_fill.dollars, cost_usd: buy_fill.dollars }
}

/// v1.5 path: race ExitWatcher against resolver. Earliest finisher wins.
async fn run_with_tp_sl(
    deps: &WindowDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    buy_fill: &FillResult,
    exit_cfg: &crate::trader::exit_watcher::ExitConfig,
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
            // Watcher hit deadline without crossing tp/sl. Fall through to
            // resolver + winner-sweep (legacy v1.5 TP/SL path uses sell_market).
            let r = deps.resolver.await_resolution(market).await;
            return winner_sweep(deps, ladder, token_id, buy_fill, r).await;
        }
    };

    // Emit ExitTriggered BEFORE sell so the trace shows trigger reason
    // regardless of fill success.
    emit_kind(deps, ladder, TraderEventKind::ExitTriggered {
        kind: trig.kind,
        bid: trig.bid,
    }).await;

    // TP or SL fired. Sell with the trigger bid as a fill-price hint so dry-run
    // simulation reflects trigger context (real CLOB ignores the hint).
    let sell_fill = match deps.executor.sell_at_bid(token_id, buy_fill.shares, trig.bid).await {
        Ok(f) => f,
        Err(e) => {
            emit_kind(deps, ladder, TraderEventKind::SellRejected { reason: format!("{e}") }).await;
            emit_kind(deps, ladder, TraderEventKind::Alert {
                message: format!("tp/sl sell failed; shares stuck for token {token_id}"),
            }).await;
            return WindowOutcome::Won { proceeds_usd: Decimal::ZERO, cost_usd: buy_fill.dollars };
        }
    };
    emit_kind(deps, ladder, TraderEventKind::SellFilled { proceeds_usd: sell_fill.dollars }).await;
    if sell_fill.dollars > buy_fill.dollars {
        WindowOutcome::Won { proceeds_usd: sell_fill.dollars, cost_usd: buy_fill.dollars }
    } else {
        WindowOutcome::Lost { spent_usd: buy_fill.dollars - sell_fill.dollars }
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
    use crate::trader::executor::{FillResult, OrderId};
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
        async fn find_window(&self, _ts: i64, _mins: u32)
            -> Result<WindowMarket, MarketError>
        {
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

    /// v1.9: Stub BTC price feed. Most window tests don't exercise the
    /// Chainlink outcome path, so a constant price is sufficient.
    struct StubBtcPrice {
        result: Result<Decimal, ()>,
    }
    impl StubBtcPrice {
        fn at(price: &str) -> Arc<Self> {
            Arc::new(Self { result: Ok(Decimal::from_str(price).unwrap()) })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self { result: Err(()) })
        }
    }
    #[async_trait]
    impl crate::tui::market_watch::BtcPriceFeed for StubBtcPrice {
        async fn latest_price(&self)
            -> Result<Decimal, crate::tui::market_watch::MarketWatchError>
        {
            self.result.clone().map_err(|_| {
                crate::tui::market_watch::MarketWatchError::Rpc("stub error".into())
            })
        }
    }

    fn fresh_ladder() -> LadderState {
        LadderState::new(Direction::Up, 5, 5, Utc::now())
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

    fn stub_events() -> Arc<crate::trader::order_events::tests::ScriptedOrderEvents> {
        crate::trader::order_events::tests::ScriptedOrderEvents::new()
    }

    fn cfg() -> WindowConfig {
        WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: None,
            exit_at_secs: None,
            maker: false,
            window_seconds: 300,
        }
    }

    fn cfg_with_exit(exit: ExitConfig) -> WindowConfig {
        WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: Some(exit),
            exit_at_secs: None,
            maker: false,
            window_seconds: 300,
        }
    }

    #[tokio::test]
    async fn cfg_default_keeps_exit_none() {
        // Smoke: existing tests build WindowConfig without exit; default is None.
        let c = cfg();
        assert!(c.exit.is_none());
    }

    #[test]
    fn window_config_carries_window_seconds() {
        let c = WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: None,
            exit_at_secs: None,
            maker: false,
            window_seconds: 900,
        };
        assert_eq!(c.window_seconds, 900);
    }

    /// Market with price_to_beat set, for v1.9 chainlink-based outcome tests.
    fn open_market_with_ptb(up_ask: &str, down_ask: &str, ptb: &str) -> WindowMarket {
        let mut m = open_market_at(up_ask, down_ask);
        m.price_to_beat = Some(Decimal::from_str(ptb).unwrap());
        m
    }

    #[tokio::test]
    async fn happy_path_won() {
        // BTC > price_to_beat → UP wins; we picked UP → our_won = true.
        // Auto-Redeem path emits Won { proceeds = shares × $1.00 = $10, cost = $5 }.
        let market = open_market_with_ptb("0.50", "0.50", "80000");
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_only(Ok(FillResult {
                fill_price: Decimal::from_str("0.50").unwrap(),
                shares: Decimal::from(10),
                dollars: Decimal::from(5),
            })),
            resolver: StubResolver::timeout(),  // chainlink path doesn't use resolver
            emitter: CapturingEmitter::new(),
            price: stub_price("0.50"),
            events: stub_events(),
            btc_price: StubBtcPrice::at("80100"),  // > price_to_beat → UP wins
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        // proceeds_usd = shares × $1.00 = 10
        assert!(matches!(outcome,
            WindowOutcome::Won { ref proceeds_usd, .. } if *proceeds_usd == Decimal::from(10)
        ), "got {outcome:?}");
    }

    #[tokio::test]
    async fn happy_path_lost() {
        // BTC < price_to_beat → DOWN wins; we picked UP → our_won = false.
        let market = open_market_with_ptb("0.50", "0.50", "80000");
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
            events: stub_events(),
            btc_price: StubBtcPrice::at("79900"),  // < price_to_beat → UP loses
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Lost { ref spent_usd } if *spent_usd == Decimal::from(5)
        ), "got {outcome:?}");
    }

    #[tokio::test]
    async fn skip_market_not_found() {
        let deps = WindowDeps {
            market: StubMarket::err(MarketError::NotFound { window_ts: 1700000300 }),
            executor: StubExec::buy_only(Err(ExecError::FillOrKillFailed)),
            resolver: StubResolver::timeout(),
            emitter: CapturingEmitter::new(),
            price: stub_price("0.50"),
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
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
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
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
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
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
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
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
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Skipped { reason: SkipReason::ResolutionTimeout }
        ));
    }

    #[tokio::test]
    async fn chainlink_failure_falls_back_to_gamma_resolver() {
        // v1.9: if Chainlink fetch fails, fall back to gamma resolver.
        // Auto-Redeem still handles the winner cash — no sell call.
        let market = open_market_with_ptb("0.50", "0.50", "80000");
        let emitter = CapturingEmitter::new();
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_only(Ok(FillResult {
                fill_price: Decimal::from_str("0.50").unwrap(),
                shares: Decimal::from(10),
                dollars: Decimal::from(5),
            })),
            resolver: StubResolver::won(Direction::Up),
            emitter: emitter.clone(),
            price: stub_price("0.50"),
            events: stub_events(),
            btc_price: StubBtcPrice::failing(),  // chainlink RPC errors
        };
        let outcome = run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        // Falls back via gamma → resolver says UP won → Won
        assert!(matches!(outcome,
            WindowOutcome::Won { ref proceeds_usd, .. } if *proceeds_usd == Decimal::from(10)
        ), "got {outcome:?}");

        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::Alert { .. })),
                "expected fallback Alert");
    }

    #[tokio::test]
    async fn happy_path_emits_expected_event_sequence() {
        // v1.9 event sequence: no SellFilled (Auto-Redeem handles winning shares).
        let market = open_market_with_ptb("0.50", "0.50", "80000");
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
            price: stub_price("0.50"),
            events: stub_events(),
            btc_price: StubBtcPrice::at("80100"),  // UP wins
        };
        run_window(&deps, &cfg(), &fresh_ladder(), 1700000300).await;
        let kinds = emitter.kinds();
        let names: Vec<_> = kinds.iter().map(|k| match k {
            TraderEventKind::WindowOpening { .. } => "WindowOpening",
            TraderEventKind::EntryDecision { .. } => "EntryDecision",
            TraderEventKind::OrderPlaced { .. } => "OrderPlaced",
            TraderEventKind::OrderFilled { .. } => "OrderFilled",
            TraderEventKind::Resolved { .. } => "Resolved",
            other => panic!("unexpected: {other:?}"),
        }).collect();
        assert_eq!(names, [
            "WindowOpening", "EntryDecision",
            "OrderPlaced", "OrderFilled",
            "Resolved",
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
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
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
            WindowOutcome::Won { ref proceeds_usd, .. } if *proceeds_usd == Decimal::from_str("8.40").unwrap()));
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
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
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
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
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
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
        };
        let cfg = cfg_with_exit(ExitConfig {
            tp_price: Decimal::from_str("0.85").unwrap(),
            sl_price: Decimal::from_str("0.45").unwrap(),
            poll: std::time::Duration::from_millis(50),
        });
        let outcome = run_window(&deps, &cfg, &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Won { ref proceeds_usd, .. } if *proceeds_usd == Decimal::from_str("9.90").unwrap()));
        let kinds = emitter.kinds();
        assert!(!kinds.iter().any(|k| matches!(k, TraderEventKind::ExitTriggered { .. })),
                "no exit-triggered event when deadline path fires");
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::Resolved { .. })),
                "resolved event when deadline path fires");
    }

    #[tokio::test(start_paused = true)]
    async fn maker_flag_routes_to_run_maker() {
        // Smoke: with cfg.maker=true and a stub fill, run_window dispatches
        // to maker.rs which posts a BuyLimitPosted event (taker path doesn't).
        let market = open_market_at("0.50", "0.50");
        let emitter = CapturingEmitter::new();
        let exec = crate::trader::adapters::simulated_executor::SimulatedExecutor::default();

        // Build a price stub that returns >SL forever (no SL trigger).
        let price = stub_price("0.50");

        let events = crate::trader::order_events::tests::ScriptedOrderEvents::new();
        // Pre-script: buy "sim-order-0" fills at 0.49, tp "sim-order-1" fills at 0.85.
        events.add(OrderId("sim-order-0".into()), vec![
            crate::trader::order_events::OrderEvent::Filled {
                id: OrderId("sim-order-0".into()),
                fill_price: Decimal::from_str("0.49").unwrap(),
                shares_filled: Decimal::from(10),
                total_shares: Decimal::from(10),
            },
        ]);
        events.add(OrderId("sim-order-1".into()), vec![
            crate::trader::order_events::OrderEvent::Filled {
                id: OrderId("sim-order-1".into()),
                fill_price: Decimal::from_str("0.85").unwrap(),
                shares_filled: Decimal::from(10),
                total_shares: Decimal::from(10),
            },
        ]);

        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: Arc::new(exec),
            resolver: StubResolver::won(Direction::Up),
            emitter: emitter.clone(),
            price,
            events: events as Arc<dyn crate::trader::order_events::OrderEventStream>,
            btc_price: StubBtcPrice::at("80000"),
        };
        let cfg = WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: Some(ExitConfig {
                tp_price: Decimal::from_str("0.85").unwrap(),
                sl_price: Decimal::from_str("0.45").unwrap(),
                poll: std::time::Duration::from_millis(50),
            }),
            exit_at_secs: None,
            maker: true,
            window_seconds: 300,
        };
        let _outcome = run_window(&deps, &cfg, &fresh_ladder(), chrono::Utc::now().timestamp()).await;

        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::BuyLimitPosted { .. })),
                "maker route must emit BuyLimitPosted; events: {kinds:?}");
    }

    fn cfg_with_early_exit(exit_at_secs: u32) -> WindowConfig {
        WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: None,
            exit_at_secs: Some(exit_at_secs),
            maker: false,
            window_seconds: 300,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn hold_early_exit_sells_at_deadline_with_profit() {
        // Use window_ts = now so wait = exit_at_secs seconds of tokio time.
        let window_ts = chrono::Utc::now().timestamp();
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
                    fill_price: Decimal::from_str("0.55").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from_str("5.50").unwrap(),
                }),
            ),
            // Resolver is never invoked on this path — give it timeout to surface
            // any accidental call as a failure.
            resolver: StubResolver::timeout(),
            emitter: emitter.clone(),
            price: stub_price("0.55"),
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
        };

        let cfg = cfg_with_early_exit(2); // 2-second deadline (small for tests)

        let task = tokio::spawn(async move {
            run_window(&deps, &cfg, &fresh_ladder(), window_ts).await
        });

        // Advance tokio's mocked clock past the 2s deadline.
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        let outcome = task.await.unwrap();

        assert!(matches!(outcome,
            WindowOutcome::Won { ref proceeds_usd, .. } if *proceeds_usd == Decimal::from_str("5.50").unwrap()
        ), "got {outcome:?}");

        // Verify event sequence ended with SellFilled, never Resolved.
        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::SellFilled { .. })),
                "missing SellFilled event");
        assert!(!kinds.iter().any(|k| matches!(k, TraderEventKind::Resolved { .. })),
                "should NOT have Resolved event on early-exit path");
    }

    #[tokio::test(start_paused = true)]
    async fn hold_early_exit_sells_at_deadline_with_loss() {
        let window_ts = chrono::Utc::now().timestamp();
        let market = open_market_at("0.50", "0.50");
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_then_sell(
                Ok(FillResult {
                    fill_price: Decimal::from_str("0.50").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from(5),
                }),
                Ok(FillResult {
                    fill_price: Decimal::from_str("0.40").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from_str("4.00").unwrap(),
                }),
            ),
            resolver: StubResolver::timeout(),
            emitter: CapturingEmitter::new(),
            price: stub_price("0.40"),
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
        };

        let cfg = cfg_with_early_exit(2);
        let task = tokio::spawn(async move {
            run_window(&deps, &cfg, &fresh_ladder(), window_ts).await
        });
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        let outcome = task.await.unwrap();

        match outcome {
            WindowOutcome::Lost { spent_usd } => {
                // 5.00 buy - 4.00 sell = 1.00 net loss
                assert_eq!(spent_usd, Decimal::from(1), "got {spent_usd}");
            }
            _ => panic!("expected Lost, got {outcome:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn hold_early_exit_sell_failure_low_bid_emits_lost_for_martingale() {
        // Bid low (= UP losing) + FAK fail → emit Lost so Martingale escalates.
        // Auto-Redeem won't pay anything on a loser.
        let window_ts = chrono::Utc::now().timestamp();
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
                Err(ExecError::FillOrKillFailed),
            ),
            resolver: StubResolver::timeout(),
            emitter: emitter.clone(),
            price: stub_price("0.10"),  // low bid → likely losing
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
        };

        let cfg = cfg_with_early_exit(2);
        let task = tokio::spawn(async move {
            run_window(&deps, &cfg, &fresh_ladder(), window_ts).await
        });
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        let outcome = task.await.unwrap();

        // bid 0.10 ≤ 0.5 → Lost { spent_usd: $5 } so Martingale escalates next.
        match outcome {
            WindowOutcome::Lost { spent_usd } => {
                assert_eq!(spent_usd, Decimal::from(5));
            }
            _ => panic!("expected Lost (low bid signals loss), got {outcome:?}"),
        }

        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::SellRejected { .. })),
                "missing SellRejected");
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::Alert { .. })),
                "missing Alert");
    }

    #[tokio::test(start_paused = true)]
    async fn hold_early_exit_sell_failure_high_bid_emits_won_for_no_escalation() {
        // Bid high (= UP winning) + FAK fail → emit Won so Martingale stays
        // at step 1. Auto-Redeem will collect ~shares × $1.00 at resolution.
        let window_ts = chrono::Utc::now().timestamp();
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
                Err(ExecError::FillOrKillFailed),
            ),
            resolver: StubResolver::timeout(),
            emitter: emitter.clone(),
            price: stub_price("0.99"),  // high bid → likely winning
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
        };

        let cfg = cfg_with_early_exit(2);
        let task = tokio::spawn(async move {
            run_window(&deps, &cfg, &fresh_ladder(), window_ts).await
        });
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        let outcome = task.await.unwrap();

        // bid 0.99 > 0.5 → Won with proceeds = shares × bid = 10 × 0.99 = $9.90
        match outcome {
            WindowOutcome::Won { proceeds_usd, .. } => {
                assert_eq!(proceeds_usd, Decimal::from_str("9.90").unwrap());
            }
            _ => panic!("expected Won (high bid signals win), got {outcome:?}"),
        }

        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::SellRejected { .. })),
                "missing SellRejected");
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::Alert { .. })),
                "missing Alert");
    }

    #[tokio::test(start_paused = true)]
    async fn hold_early_exit_does_not_invoke_resolver() {
        // If resolver is called, StubResolver::timeout would make the test
        // pass with Skipped { ResolutionTimeout } — that's a bug.
        // We assert the outcome is Won (not Skipped) to detect any
        // accidental fallthrough to await_resolution_and_sweep.
        let window_ts = chrono::Utc::now().timestamp();
        let market = open_market_at("0.50", "0.50");
        let deps = WindowDeps {
            market: StubMarket::ok(market),
            executor: StubExec::buy_then_sell(
                Ok(FillResult {
                    fill_price: Decimal::from_str("0.50").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from(5),
                }),
                Ok(FillResult {
                    fill_price: Decimal::from_str("0.50").unwrap(),
                    shares: Decimal::from(10),
                    dollars: Decimal::from(5),
                }),
            ),
            resolver: StubResolver::timeout(),  // would cause Skipped if invoked
            emitter: CapturingEmitter::new(),
            price: stub_price("0.50"),
            events: stub_events(),
            btc_price: StubBtcPrice::at("80000"),
        };

        let cfg = cfg_with_early_exit(2);
        let task = tokio::spawn(async move {
            run_window(&deps, &cfg, &fresh_ladder(), window_ts).await
        });
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        let outcome = task.await.unwrap();

        // Break-even: 5.00 buy at 0.50, 5.00 sell at 0.50 → Lost { 0 }.
        match outcome {
            WindowOutcome::Lost { spent_usd } => {
                assert_eq!(spent_usd, Decimal::ZERO);
            }
            _ => panic!("expected Lost (break-even), got {outcome:?}"),
        }
    }
}
