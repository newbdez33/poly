# v1.7 — Trader limit-order maker mode

**Goal:** Add a `--maker` flag that switches the trader's BUY entry and TP exit from market orders to limit orders, capturing maker rebates and avoiding cross-spread slippage. SL stays as market sell (a limit SL would not protect against fast price drops). End-of-window: cancel resting TP at t=270s and market-sell the residual at the current bid — no resolution-redemption needed.

**Non-goal:** Pre-window order pre-staging, multi-market parallel orders, volume-aware sweep heuristics, websocket-only reliance, mid-window restart recovery of orphaned orders.

## Context

v1.5 trader uses market orders throughout: FoK BUY at window open, market SELL on TP/SL trigger or winner-sweep. This pays Polymarket's taker fee (~1%) on every trade and crosses the spread on entry (real fills observed at 0.54–0.58 vs band 0.50). On a profitable strategy 4, those costs eat 5–15% of expected EV.

The recent share-precision incident (`c94c189`) plus the manual-redemption gas pain motivated avoiding the resolution path entirely when possible.

This spec adds a hybrid maker-first mode behind a single CLI flag. v1.5 default behavior is preserved bit-for-bit when `--maker` is absent.

## Architecture

`run_with_tp_sl` becomes a two-phase state machine driven by `tokio::select!` over four event sources:

1. **Order fills** — primary via SDK WebSocket user channel; 2-second polling fallback if WS drops.
2. **Price ticks** — existing `MidwindowPriceFetcher` (gamma 5s polling) for SL trigger detection.
3. **Sweep/cancel timers** — `tokio::time::sleep_until` at fixed seconds-since-window-open.
4. **Shutdown** — existing `CancellationToken`.

The hold-mode path (`--exit-rule hold`) is untouched; same goes for tp-sl without `--maker`.

```
                          run_window (v1.7 with --maker)
       ┌───────────────────────────────────────────────────────┐
       │                                                       │
       │  t=0   discover market, band check                    │
       │        place LIMIT BUY @ ask-0.01                     │
       │        emit BuyLimitPosted{id, price}                 │
       │                                                       │
       │  ┌── PendingBuy ─────────────────────────────────┐    │
       │  │ select!:                                       │    │
       │  │   buy fills    → BuyFilled, advance to        │    │
       │  │                  PendingTpSell                 │    │
       │  │   t=30s tick  → cancel + re-post @ ask         │    │
       │  │                  emit BuyLimitSwept            │    │
       │  │   t=60s tick  → cancel + re-post @ ask+0.01    │    │
       │  │   t=90s tick  → cancel + emit Skipped          │    │
       │  │   shutdown    → cancel + return                │    │
       │  └────────────────────────────────────────────────┘    │
       │                              ↓                         │
       │  ┌── PendingTpSell ──────────────────────────────┐    │
       │  │ place LIMIT SELL @ tp_price (e.g. 0.85)       │    │
       │  │ emit TpLimitPosted{id, price}                  │    │
       │  │ select!:                                       │    │
       │  │   TP fully fills    → TpLimitFilled{full}     │    │
       │  │                       → return Won             │    │
       │  │   TP partially fills → TpLimitFilled{partial} │    │
       │  │                       → keep resting           │    │
       │  │   SL price ≤ sl     → cancel TP                │    │
       │  │                       → market sell residual   │    │
       │  │   t=270s tick       → cancel TP                │    │
       │  │                       → market sell at bid     │    │
       │  │   shutdown          → cancel + return          │    │
       │  └────────────────────────────────────────────────┘    │
       │                                                       │
       │  Returns WindowOutcome (Won / Lost / Skipped)         │
       └───────────────────────────────────────────────────────┘

       OrderEventStream:
         polling-only for v1.7: poll /data/orders every 2s (≤2s latency)
         (WebSocket integration deferred to v1.7.1 if poll latency hurts)
```

Polling at 2s adds at most 2s latency to fill detection — acceptable for 5-min windows. WebSocket would push fills in ~100ms, but the SDK's WS lifecycle (reconnect, heartbeat, drop handling) adds ~150 LOC for marginal benefit. Ship polling first; revisit if real-money latency proves to matter.

## CLI surface

| Flag | Default | Effect |
|---|---|---|
| `--maker` | off | When absent, v1.5 behavior (market orders). When present, activates the state machine above. Only meaningful with `--exit-rule tp-sl`. |

Validation: `--maker` requires `--exit-rule tp-sl`. With `--exit-rule hold` the flag is rejected with a clap error.

```bash
# v1.5 (current)
poly-trader --direction up --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45

# v1.7 maker mode
poly-trader --direction up --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45 --maker

# Both A/B testable side-by-side via --reset between runs.
```

## Sweep schedule

| Time | Action | Resulting `BuyState` |
|---|---|---|
| t=0 | place limit BUY @ ask − $0.01 | `Pending { id, price_step: 0 }` |
| t=30 | cancel order, re-post @ ask | `Pending { id', price_step: 1 }` |
| t=60 | cancel order, re-post @ ask + $0.01 (becomes taker) | `Pending { id'', price_step: 2 }` |
| t=90 | cancel order, return `Skipped { FillOrKillFailed }` | terminal |

If the buy fills at any time before t=90, the sweep timer is dropped and we transition to PendingTpSell immediately.

If the buy partially fills mid-sweep (e.g. 5 of 10 shares filled at 0.49, then we cancel at t=30), keep the partial fill and transition to PendingTpSell with the partial shares. The remaining unfilled portion is dropped — we do NOT post a separate buy for the remainder. (Strategy 4's per-window stake is small; chasing remainder buys adds complexity without material PnL.)

## Hold schedule

After buy fill at any t, immediately:

```rust
let tp_id = exec.place_limit(token, Side::Sell, exit_cfg.tp_price, shares).await?;
emit TpLimitPosted { id: tp_id, price: tp_price };
```

Then enter PendingTpSell with these exit conditions:

| Trigger | Action | Outcome |
|---|---|---|
| TP fills 100% of `shares` | emit `TpLimitFilled{partial:false}` + `SellFilled` | `Won { proceeds_usd: tp_proceeds }` |
| TP fills <100% of `shares`, then no further fills | emit `TpLimitFilled{partial:true, remaining}`, keep tp_id resting | continue waiting |
| Gamma bid ≤ sl_price (existing watcher) | cancel(tp_id), market_sell residual | `Won` if total > cost else `Lost` |
| t=270s reached | cancel(tp_id), market_sell residual at current bid | `Won` if total > cost else `Lost` |
| Sell-rejection on market sell | emit `Alert + SellRejected`, return `Won { proceeds_usd: tp_proceeds_so_far }` | shares stuck in wallet, redeem via `poly-redeem` |

Total proceeds = tp_partial_proceeds + market_sell_residual_proceeds. Outcome mapping: `total_proceeds > buy_dollars ? Won { total } : Lost { spent: buy_dollars - total }` (same proceeds-vs-cost rule as v1.5).

## Components

### `src/trader/executor.rs` *(modify)*

Extend the trait. Existing `buy_fok` and `sell_market` stay; new methods only used by `--maker` path.

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderId(pub String);

#[async_trait]
pub trait OrderExecutor: Send + Sync {
    async fn buy_fok(&self, token_id: &str, dollars: Decimal) -> Result<FillResult, ExecError>;
    async fn sell_market(&self, token_id: &str, shares: Decimal) -> Result<FillResult, ExecError>;
    async fn sell_at_bid(&self, token_id: &str, shares: Decimal, bid_hint: Decimal)
        -> Result<FillResult, ExecError> {
        // existing default implementation, unchanged
    }

    /// Post a limit order (BUY or SELL). Returns the CLOB order_id once accepted.
    /// Default impl returns ExecError::NotSupported — only the maker-mode path
    /// requires this; other executors (existing v1.5) can opt out.
    async fn place_limit(
        &self,
        token_id: &str,
        side: OrderSide,
        price: Decimal,
        shares: Decimal,
    ) -> Result<OrderId, ExecError> {
        let _ = (token_id, side, price, shares);
        Err(ExecError::NotSupported)
    }

    /// Cancel a previously-placed order. Default impl returns ExecError::NotSupported.
    async fn cancel(&self, order_id: &OrderId) -> Result<(), ExecError> {
        let _ = order_id;
        Err(ExecError::NotSupported)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide { Buy, Sell }
```

`ExecError::NotSupported` is a new variant.

### `src/trader/adapters/clob_executor_wrapper.rs` *(modify)*

Implement `place_limit` and `cancel` against the SDK:

```rust
async fn place_limit(...) -> Result<OrderId, ExecError> {
    let signable = self.client.limit_order()
        .token_id(tid)
        .side(sdk_side)
        .price(price)
        .amount(Amount::shares(shares.trunc_with_scale(2))?)  // CLOB precision rule
        .order_type(OrderType::GTC)  // good-till-cancel
        .build().await?;
    let signed = self.client.sign(&self.signer, signable).await?;
    let resp = self.client.post_order(signed).await?;
    if !resp.success {
        return Err(ExecError::FillOrKillFailed);
    }
    Ok(OrderId(resp.order_id))
}

async fn cancel(&self, order_id: &OrderId) -> Result<(), ExecError> {
    self.client.cancel_order(&order_id.0).await
        .map(|_| ())
        .map_err(|e| ExecError::Network(format!("cancel: {e}")))
}
```

Order side enum mapping: `OrderSide::Buy → SdkSide::Buy`, `OrderSide::Sell → SdkSide::Sell`.

### `src/trader/adapters/simulated_executor.rs` *(modify)*

For dry-run realism, the simulator must answer "did my limit fill?" deterministically. Approach:

```rust
struct PendingLimitOrder {
    id: OrderId,
    side: OrderSide,
    price: Decimal,
    shares: Decimal,
    posted_at: Instant,
}

// Internally tracked in a Mutex<HashMap<OrderId, PendingLimitOrder>>.
// Filled when:
//   - BUY: `gamma_bid >= price` (someone ate our limit)
//   - SELL: `gamma_bid >= price` (someone bought us out at our ask)
// Polled by an injected MidwindowPriceFetcher (the simulator gets one
// passed via with_price_fetcher() at construction).
```

When `--dry-run --maker`, the simulator becomes price-aware. Cleaner alternative for v1.7: skip the simulator's WS/poll integration; have `place_limit` return immediately as filled at the limit price (optimistic). Trade-off: dry-run shows perfect TP fills, less realistic but adequate for state-machine validation. **Adopt the optimistic version** to avoid bloating SimulatedExecutor with price polling. Real testing of fill timing happens against real CLOB.

### `src/trader/order_events.rs` *(new)*

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OrderEvent {
    Filled { id: OrderId, fill_price: Decimal, shares_filled: Decimal, total_shares: Decimal },
    Cancelled { id: OrderId },
    Rejected { id: OrderId, reason: String },
}

#[async_trait]
pub trait OrderEventStream: Send + Sync {
    /// Subscribe to events for a specific order_id. Returns a one-shot channel
    /// that fires when the order reaches a terminal state OR when subscribe()
    /// is itself dropped.
    async fn watch(&self, id: OrderId) -> Result<mpsc::Receiver<OrderEvent>, StreamError>;
}

pub struct PolymarketOrderEventStream {
    /* SDK WS handle + reqwest client for poll fallback */
}
```

The trader spawns a single background task at startup that:
1. Maintains the WS connection (auto-reconnect on drop with exponential backoff).
2. Polls `/data/orders` every 2 seconds as a fallback (idempotent — duplicate fills are deduped by id).
3. Multiplexes events to per-watcher channels.

For v1.7, the WS implementation can be deferred — start with **polling-only** (2s cadence). Add WS subsequently as a separate task once polling proves the rest of the state machine works. This avoids gating the whole feature on WS reliability.

### `src/trader/window.rs` *(modify tp-sl path only)*

`run_with_tp_sl` body diverges based on `--maker`:

```rust
async fn run_with_tp_sl(...) -> WindowOutcome {
    if cfg.maker {
        run_maker(deps, ladder, market, exit_cfg, window_ts).await
    } else {
        run_taker(deps, ladder, market, token_id, buy_fill, exit_cfg, window_ts).await
    }
}
```

`run_taker` is the existing v1.5 logic (unchanged). `run_maker` is the new state machine. Cleanly bisected — no merging of paths.

### `src/trader/event.rs` *(modify)*

Add four event variants for observability:

```rust
pub enum TraderEventKind {
    // ... existing variants
    BuyLimitPosted { order_id: String, price: Decimal },
    BuyLimitSwept { from_price: Decimal, to_price: Decimal },
    TpLimitPosted { order_id: String, price: Decimal },
    TpLimitFilled { order_id: String, fill_price: Decimal, shares: Decimal, partial: bool },
}
```

`BuyLimitFilled` is omitted — the existing `OrderFilled` event captures it. Same for SL/end-of-window market sells (existing `SellFilled`).

### `src/trader/config.rs` *(modify)*

```rust
#[derive(Parser, ...)]
pub struct TraderArgs {
    // ... existing fields
    /// Use limit orders for BUY entry + TP exit. Saves taker fees but may
    /// skip windows when liquidity is thin. Only valid with --exit-rule tp-sl.
    #[arg(long)]
    pub maker: bool,
}

impl TraderArgs {
    fn validate(&self) -> Result<(), ConfigError> {
        // ... existing validation
        if self.maker && !matches!(self.exit_rule, ExitRuleArg::TpSl) {
            return Err(ConfigError::MakerRequiresTpSl);
        }
        Ok(())
    }
}
```

### `src/bin/poly-trader.rs` *(modify)*

Wire `OrderEventStream` (real polling impl when CLOB authenticated, stub when dry-run). Pass into `WindowDeps` via a new `Option<Arc<dyn OrderEventStream>>` field — `None` means maker mode unavailable, validated against `args.maker` at startup.

## Race conditions and edge cases

| Scenario | Handling |
|---|---|
| TP fill notification arrives same tick as SL price ≤ 0.45 | `tokio::select!` serializes — first event wins atomically. If TP wins: emit Won. If SL wins: cancel TP (idempotent if already filled) then market sell whatever residual remains. |
| TP partial fill, then bid drops to SL, residual market sell, residual sell rejected | Same as v1.5 sell-fail-on-trigger: emit Alert + SellRejected, return `Won { proceeds: tp_partial_proceeds }`. Stuck shares clean via `poly-redeem`. |
| Buy fill arrives after we sent cancel-and-re-post (race with sweep) | `cancel_order` returns "already filled" — treat as success, use the fill we got. Buy state advances to `Filled` immediately, sweep timer is moot. |
| WS disconnects mid-window | Polling fallback catches the fill on next 2s tick. ≤2s additional latency. No state corruption. |
| Trader crash mid-window with open orders | Orders auto-cancel at market close (~5min). On restart, ladder state in Redis tells us what step to resume; old orders self-clean. New session starts fresh on next 5-min boundary. |
| `place_limit` rejected by CLOB (e.g. tick precision) | Return `ExecError::Decode(...)` → window emits `OrderRejected` + `Skipped { FillOrKillFailed }` (existing skip reason reused). Ladder stays at current step. |
| `cancel_order` returns "not found" | Treat as success — order may have already filled or been cancelled by the exchange. Verify state via `OrderEventStream::watch` if uncertain. |

## Testing

### Unit (`window.rs::run_maker` — using stubs for executor + event stream + price fetcher)

| Scenario | Assertion |
|---|---|
| Buy fills at t=0, TP fills at t=120s | Outcome `Won{proceeds≈tp×shares}`; events: `BuyLimitPosted → OrderFilled → TpLimitPosted → TpLimitFilled{partial:false} → SellFilled → LadderUpdated` |
| Buy doesn't fill at 0.49, sweep escalates, fills at 0.50 | Events include 1× `BuyLimitSwept{0.49→0.50}` |
| Buy never fills, t=90 timeout | Outcome `Skipped{FillOrKillFailed}`; ladder unchanged |
| Buy fills, SL trigger at t=200s | TP cancelled; market sell at simulated bid; outcome based on proceeds |
| Buy fills, TP partial 60%, then SL trigger on residual | Two emits: `TpLimitFilled{partial:true}` + `SellFilled`; total proceeds = partial + market sell |
| Buy fills, no triggers, t=270 timeout | TP cancelled; market sell at simulated bid; outcome depends on bid value |

### Unit (`order_events.rs` polling-only impl)

| Scenario | Assertion |
|---|---|
| Single open order fills | Polling tick after fill → emits `Filled{...}` |
| Order cancelled | Next poll → emits `Cancelled{id}` |
| HTTP error during poll | Logs warn, continues (no event emitted, doesn't crash) |
| Multiple watchers on different IDs | Events route correctly per id |

### Integration (`tests/maker_integration.rs`, `#[ignore]`)

End-to-end with testcontainers Redis + scripted CLOB stub. Verifies:
- `--maker --dry-run --max-windows 1` runs through the happy path and emits expected events to Redis stream.
- Stub fills the buy at posted price, fills the TP at tp_price → Won outcome reaches LadderUpdated.

### E2E

Manual smoke against real CLOB recommended before any real-money run with `--maker`. Document in README.

### Coverage gate

`cargo llvm-cov --lib --tests` ≥80% on the new modules. The `run_maker` state machine is the most important to cover; achievable via the stubbed unit tests above.

## Risk & operator notes

- **Polymarket fee structure verification needed.** If maker fee turns out to equal taker fee, expected savings collapse to zero. v1.7 still runs correctly — just no fee benefit. First real-money test should A/B compare 12 windows of `--maker` vs 12 without.
- **Strategy 4 backtest assumed 100% participation when in band.** If `--maker` skips ~5–10% of windows due to entry fails (sweep exhausted), backtest's $7,500/30d expectation discounts proportionally. Monitor `windows_skipped` count.
- **Latency vs SL safety.** SL detection is via existing 5s gamma price polling. If BTC drops 50¢ between polls, SL fires "late." Same as v1.5 — not worse with maker mode.
- **Order cancel + re-post is two API calls.** A network blip during sweep could leave both old and new orders open momentarily. Polymarket allows multiple open orders on the same token; not a correctness issue (eventual cancel happens), just transient duplicate exposure. Acceptable.

## Migration / rollback

- `--maker` defaults off → existing dry-run and real-money runs identical to v1.5. Zero migration risk.
- Rollback: omit `--maker`. No state migration. Ladder format unchanged.
- Coexists with `--reset` and `--max-windows`.

## Out of scope (explicit YAGNI)

- Pre-window order pre-staging (post limit before window opens).
- Multi-step TP ladder (e.g. 50% at 0.80, 50% at 0.90).
- Volume-aware sweep heuristics (escalate based on book depth).
- Mid-window crash recovery via on-chain order reconciliation.
- WebSocket-only (no polling) for production. Polling+WS hybrid is the production target; WS-only deferred.
- Cancel-on-shutdown signal handling. Orders auto-expire at market close.
- Partial-buy remainder chasing — drop the unfilled chunk, work with what filled.

## Related documents

- v1.5 trader spec: `docs/superpowers/specs/2026-05-10-trader-tp-sl-design.md`
- v1.7 placeholder in `TODO.md`
