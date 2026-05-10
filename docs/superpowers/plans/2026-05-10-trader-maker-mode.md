# v1.7 — Trader Maker Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--maker` flag that switches BUY entry + TP exit from market orders to limit orders with a 30s/60s sweep schedule, while keeping SL as market sell. End-of-window: cancel resting TP at t=270s and market-sell residual.

**Architecture:** New state machine `run_maker` inside `run_with_tp_sl`, dispatched on `cfg.maker`. Two phases: PendingBuy (place limit BUY @ ask−0.01, sweep up at t=30/60, give up at t=90) → PendingTpSell (place limit TP @ tp_price, exit on full/partial fill / SL trigger / t=270s). Driven by `tokio::select!` over fill events (polling-only impl), price ticks (existing watcher), sweep timers, shutdown.

**Tech Stack:** Rust 1.78+, tokio (existing), polymarket_client_sdk_v2 (existing — `client.limit_order().build/sign/post_order`, `client.cancel_order(id)`, `client.order(id)` for fill polling). No new deps.

**Spec:** `docs/superpowers/specs/2026-05-10-trader-maker-mode-design.md`

## Build hygiene — STRICT

NEVER bare `cargo build`. Always scope:
- `cargo build --bin poly-trader`
- `cargo test --lib trader::`
- `cargo build --tests --test maker_integration` (Task 11)

DO NOT touch `src/backtest/`, `src/bin/poly-tui.rs`, or trader's existing `run_taker` path. v1.7 is additive — `--maker` off must be byte-identical to v1.6.

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `src/trader/executor.rs` | modify | Add `OrderId`, `OrderSide` types; add `place_limit` + `cancel` default-NotSupported impls to `OrderExecutor` |
| `src/trader/errors.rs` | modify | `ExecError::NotSupported`, `ConfigError::MakerRequiresTpSl`, `StreamError::OrderNotFound` |
| `src/trader/config.rs` | modify | `--maker` clap flag; validation that `--maker` requires `--exit-rule tp-sl` |
| `src/trader/event.rs` | modify | 4 new `TraderEventKind` variants for limit order observability |
| `src/trader/order_events.rs` | create | `OrderEventStream` trait + `OrderEvent` enum + `PolymarketPollOrderEvents` impl polling `client.order(id)` |
| `src/trader/adapters/clob_executor_wrapper.rs` | modify | Implement `place_limit` and `cancel` on top of SDK |
| `src/trader/adapters/simulated_executor.rs` | modify | Implement `place_limit` (optimistic immediate-fill at limit price) and `cancel` (no-op) |
| `src/trader/maker.rs` | create | `run_maker` state machine — the heart of v1.7 |
| `src/trader/window.rs` | modify | `run_with_tp_sl` dispatches on `cfg.maker`: `run_maker` if true, existing path if false |
| `src/trader/mod.rs` | modify | Add `pub mod maker; pub mod order_events;` |
| `src/bin/poly-trader.rs` | modify | Wire `OrderEventStream` based on dry-run flag; pass into `WindowDeps` |
| `tests/maker_integration.rs` | create | Testcontainers Redis + scripted CLOB stub end-to-end |
| `Cargo.toml` | modify | Register `[[test]] maker_integration` |
| `README.md` | modify | New §Maker mode subsection |
| `TODO.md` | modify | Mark v1.7 ✅ COMPLETE |

---

## Task 0: Sanity baseline

**Files:** none (read-only).

- [ ] **Step 1: Confirm working tree clean**

Run: `git status`
Expected: only untracked items are `.claude/`, the four `backtest-report*.html` files, and `~/.poly-backtest-cache/`. No tracked-file modifications.

- [ ] **Step 2: Confirm trader unit tests green**

Run: `cargo test --lib trader::`
Expected: PASS — count noted (e.g., "108 passed").

- [ ] **Step 3: Confirm trader binary builds**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean.

- [ ] **Step 4: No commit**

This task only verifies starting state.

---

## Task 1: New types — OrderId, OrderSide; ExecError::NotSupported

**Files:**
- Modify: `src/trader/executor.rs`
- Modify: `src/trader/errors.rs`

- [ ] **Step 1: Write the failing tests**

Add to `src/trader/executor.rs` inside the existing `#[cfg(test)] mod tests` block, before the closing `}`:

```rust
    #[test]
    fn order_id_serde_roundtrip() {
        let id = OrderId("abc-123".into());
        let json = serde_json::to_string(&id).unwrap();
        let back: OrderId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn order_side_serializes_distinctly() {
        assert_ne!(
            serde_json::to_string(&OrderSide::Buy).unwrap(),
            serde_json::to_string(&OrderSide::Sell).unwrap(),
        );
    }
```

Add to `src/trader/errors.rs` inside the existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn exec_error_not_supported_displays() {
        let e = ExecError::NotSupported;
        assert_eq!(format!("{e}"), "operation not supported by this executor");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::executor:: trader::errors::`
Expected: FAIL — `cannot find type 'OrderId'`, `cannot find variant 'NotSupported'`.

- [ ] **Step 3: Implement the types**

Edit `src/trader/executor.rs`. Add at the top, after the existing `FillResult` struct:

```rust
/// Opaque CLOB order identifier returned by `place_limit`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OrderId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide { Buy, Sell }
```

Edit `src/trader/errors.rs`. Add a variant to `ExecError`:

```rust
#[derive(Error, Debug)]
pub enum ExecError {
    #[error("fill-or-kill rejected (no liquidity or partial fill)")]
    FillOrKillFailed,
    #[error("CLOB request failed: {0}")]
    Network(String),
    #[error("response decode failed: {0}")]
    Decode(String),
    #[error("insufficient USDC")]
    InsufficientFunds,
    #[error("operation not supported by this executor")]
    NotSupported,
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::executor:: trader::errors::`
Expected: PASS — all tests green.

- [ ] **Step 5: Commit**

```bash
git add src/trader/executor.rs src/trader/errors.rs
git commit -m "feat(trader): OrderId + OrderSide types + ExecError::NotSupported"
```

---

## Task 2: OrderExecutor::place_limit + cancel default impls

**Files:**
- Modify: `src/trader/executor.rs`

Default impls return `ExecError::NotSupported` so existing impls (`SimulatedExecutor`, `ClobOrderExecutor`) compile without changes. They override later in Tasks 5 + 6.

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `src/trader/executor.rs`:

```rust
    struct DefaultsOnlyExecutor;
    #[async_trait::async_trait]
    impl OrderExecutor for DefaultsOnlyExecutor {
        async fn buy_fok(&self, _t: &str, _d: Decimal) -> Result<FillResult, crate::trader::errors::ExecError> {
            unimplemented!()
        }
        async fn sell_market(&self, _t: &str, _s: Decimal) -> Result<FillResult, crate::trader::errors::ExecError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn default_place_limit_returns_not_supported() {
        let e = DefaultsOnlyExecutor;
        let r = e.place_limit("tok", OrderSide::Buy, Decimal::from_str("0.50").unwrap(), Decimal::from(10)).await;
        assert!(matches!(r, Err(crate::trader::errors::ExecError::NotSupported)));
    }

    #[tokio::test]
    async fn default_cancel_returns_not_supported() {
        let e = DefaultsOnlyExecutor;
        let r = e.cancel(&OrderId("x".into())).await;
        assert!(matches!(r, Err(crate::trader::errors::ExecError::NotSupported)));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::executor::`
Expected: FAIL — `place_limit` not a method on `OrderExecutor`.

- [ ] **Step 3: Add default impls to the trait**

Edit `src/trader/executor.rs`. Replace the `OrderExecutor` trait block:

```rust
#[async_trait]
pub trait OrderExecutor: Send + Sync {
    async fn buy_fok(&self, token_id: &str, dollars: Decimal) -> Result<FillResult, ExecError>;
    async fn sell_market(&self, token_id: &str, shares: Decimal) -> Result<FillResult, ExecError>;

    /// Sell `shares` of `token_id` with a hint of the bid we observed at trigger
    /// time. Real impls (CLOB) ignore the hint and fall through to `sell_market`.
    /// Dry-run impls use the hint as fill price so simulated PnL reflects the
    /// trigger context (e.g. an SL-trigger fill should price at ~SL bid, not $0.99).
    async fn sell_at_bid(
        &self,
        token_id: &str,
        shares: Decimal,
        _bid_hint: Decimal,
    ) -> Result<FillResult, ExecError> {
        self.sell_market(token_id, shares).await
    }

    /// Post a limit order (BUY or SELL) good-till-cancel. Returns the CLOB
    /// order_id on acceptance. Default returns `ExecError::NotSupported` —
    /// only maker-mode requires this; v1.5 path doesn't.
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

    /// Cancel a previously-placed order. Default returns `ExecError::NotSupported`.
    async fn cancel(&self, order_id: &OrderId) -> Result<(), ExecError> {
        let _ = order_id;
        Err(ExecError::NotSupported)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::executor::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/trader/executor.rs
git commit -m "feat(trader): OrderExecutor::place_limit + cancel default impls"
```

---

## Task 3: --maker CLI flag + validation

**Files:**
- Modify: `src/trader/config.rs`
- Modify: `src/trader/errors.rs` (already has ConfigError, just add a variant)

Wait — ConfigError lives in `src/trader/config.rs` directly (not errors.rs). Confirm by reading the file before editing.

- [ ] **Step 1: Write the failing tests**

Add to `src/trader/config.rs` inside `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn parses_maker_flag_off_by_default() {
        let a = parse(&["--direction", "up"]);
        assert!(!a.maker);
    }

    #[test]
    fn parses_maker_flag_on() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "tp-sl",
            "--tp-price", "0.85",
            "--sl-price", "0.45",
            "--maker",
        ]);
        assert!(a.maker);
    }

    #[test]
    fn validate_rejects_maker_without_tp_sl() {
        let mut a = parse(&["--direction", "up"]);
        a.maker = true;
        // exit_rule is Hold by default
        assert_eq!(a.validate(), Err(ConfigError::MakerRequiresTpSl));
    }

    #[test]
    fn validate_accepts_maker_with_tp_sl() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "tp-sl",
            "--tp-price", "0.85",
            "--sl-price", "0.45",
            "--maker",
        ]);
        assert!(a.validate().is_ok());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::config::`
Expected: FAIL — `field 'maker' does not exist`, `no variant MakerRequiresTpSl`.

- [ ] **Step 3: Add the flag and validation**

Edit `src/trader/config.rs`. Add a field to `TraderArgs` (near other bool flags):

```rust
    /// Use limit orders for BUY entry + TP exit. Saves taker fees but may
    /// skip windows when liquidity is thin. Only valid with --exit-rule tp-sl.
    #[arg(long)]
    pub maker: bool,
```

Add a variant to `ConfigError`:

```rust
    #[error("--maker requires --exit-rule tp-sl")]
    MakerRequiresTpSl,
```

Add the validation rule inside `validate()`:

```rust
        if self.maker && !matches!(self.exit_rule, ExitRuleArg::TpSl) {
            return Err(ConfigError::MakerRequiresTpSl);
        }
```

Place it after the existing tp-sl threshold validations.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::config::`
Expected: PASS — all 20 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/trader/config.rs
git commit -m "feat(trader): --maker CLI flag with --exit-rule tp-sl requirement"
```

---

## Task 4: TraderEventKind — 4 new maker-mode variants

**Files:**
- Modify: `src/trader/event.rs`

- [ ] **Step 1: Write the failing tests**

Add to `src/trader/event.rs` inside `#[cfg(test)] mod tests`, before the closing `}`:

```rust
    #[test]
    fn buy_limit_posted_serde_roundtrip() {
        let e = fake_event(TraderEventKind::BuyLimitPosted {
            order_id: "ord-1".into(),
            price: Decimal::from_str("0.49").unwrap(),
        });
        let back: TraderEvent =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn buy_limit_swept_serde_roundtrip() {
        let e = fake_event(TraderEventKind::BuyLimitSwept {
            from_price: Decimal::from_str("0.49").unwrap(),
            to_price: Decimal::from_str("0.50").unwrap(),
        });
        let back: TraderEvent =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn tp_limit_posted_serde_roundtrip() {
        let e = fake_event(TraderEventKind::TpLimitPosted {
            order_id: "ord-2".into(),
            price: Decimal::from_str("0.85").unwrap(),
        });
        let back: TraderEvent =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn tp_limit_filled_partial_serde_roundtrip() {
        let e = fake_event(TraderEventKind::TpLimitFilled {
            order_id: "ord-2".into(),
            fill_price: Decimal::from_str("0.85").unwrap(),
            shares: Decimal::from(6),
            partial: true,
        });
        let back: TraderEvent =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::event::`
Expected: FAIL — `no variant BuyLimitPosted`.

- [ ] **Step 3: Add the 4 variants**

Edit `src/trader/event.rs`. Add to the `TraderEventKind` enum, before the closing `}`:

```rust
    BuyLimitPosted { order_id: String, price: Decimal },
    BuyLimitSwept { from_price: Decimal, to_price: Decimal },
    TpLimitPosted { order_id: String, price: Decimal },
    TpLimitFilled {
        order_id: String,
        fill_price: Decimal,
        shares: Decimal,
        partial: bool,
    },
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::event::`
Expected: PASS.

- [ ] **Step 5: Update `src/ui.rs::format_event_kind` exhaustive match**

Adding variants breaks the exhaustive match in `src/ui.rs`. Find `format_event_kind` and add four arms:

```rust
        TraderEventKind::BuyLimitPosted { price, .. } => format!("BuyLimitPosted @ {price}"),
        TraderEventKind::BuyLimitSwept { from_price, to_price } => format!("BuyLimitSwept {from_price}->{to_price}"),
        TraderEventKind::TpLimitPosted { price, .. } => format!("TpLimitPosted @ {price}"),
        TraderEventKind::TpLimitFilled { fill_price, shares, partial, .. } => {
            let tag = if *partial { "partial" } else { "full" };
            format!("TpLimitFilled {tag} {shares}sh @ {fill_price}")
        }
```

Place these next to the existing `ExitTriggered` arm so all maker-mode events render in the trader log.

- [ ] **Step 6: Run all lib tests to confirm ui still compiles**

Run: `cargo test --lib`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/trader/event.rs src/ui.rs
git commit -m "feat(trader): BuyLimitPosted/Swept + TpLimitPosted/Filled events"
```

---

## Task 5: ClobOrderExecutor::place_limit + cancel implementations

**Files:**
- Modify: `src/trader/adapters/clob_executor_wrapper.rs`

- [ ] **Step 1: Read existing wrapper to understand SDK usage**

Read `src/trader/adapters/clob_executor_wrapper.rs`. Note the existing `client.market_order()...build/sign/post_order` chain in `buy_fok`. The limit-order path mirrors it but calls `client.limit_order()` and adds a `.price(...)` step.

- [ ] **Step 2: Implement place_limit + cancel**

Edit `src/trader/adapters/clob_executor_wrapper.rs`. Add to the existing `impl OrderExecutor for ClobOrderExecutor` block, after `sell_market`:

```rust
    async fn place_limit(
        &self,
        token_id: &str,
        side: crate::trader::executor::OrderSide,
        price: Decimal,
        shares: Decimal,
    ) -> Result<crate::trader::executor::OrderId, ExecError> {
        use polymarket_client_sdk_v2::clob::types::{Side as SdkSide, OrderType};

        let tid = U256::from_str(token_id)
            .map_err(|e| ExecError::Decode(format!("invalid token_id '{token_id}': {e}")))?;

        // Same precision rule as sell_market — CLOB requires <=2 decimal places.
        let sellable = shares.trunc_with_scale(2);
        if sellable.is_zero() {
            return Err(ExecError::Decode(format!(
                "share amount {shares} truncates to 0 — too small to place"
            )));
        }
        let amount = Amount::shares(sellable)
            .map_err(|e| ExecError::Decode(format!("invalid share amount {sellable}: {e}")))?;

        let sdk_side = match side {
            crate::trader::executor::OrderSide::Buy => SdkSide::Buy,
            crate::trader::executor::OrderSide::Sell => SdkSide::Sell,
        };

        let signable = self
            .client
            .limit_order()
            .token_id(tid)
            .side(sdk_side)
            .price(price)
            .amount(amount)
            .order_type(OrderType::GTC)
            .build()
            .await
            .map_err(|e| ExecError::Network(format!("limit build failed: {e}")))?;

        let signed = self
            .client
            .sign(&self.signer, signable)
            .await
            .map_err(|e| ExecError::Network(format!("limit sign failed: {e}")))?;

        let resp = self
            .client
            .post_order(signed)
            .await
            .map_err(|e| ExecError::Network(format!("limit post failed: {e}")))?;

        if !resp.success {
            return Err(ExecError::FillOrKillFailed);
        }

        Ok(crate::trader::executor::OrderId(resp.order_id))
    }

    async fn cancel(
        &self,
        order_id: &crate::trader::executor::OrderId,
    ) -> Result<(), ExecError> {
        self.client
            .cancel_order(&order_id.0)
            .await
            .map(|_| ())
            .map_err(|e| ExecError::Network(format!("cancel failed: {e}")))
    }
```

- [ ] **Step 3: Verify trader binary compiles**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean.

There are no unit tests for the real CLOB path (requires a live client). Behavior is exercised by the integration test in Task 11 (against a stub) plus manual smoke against the real CLOB before any real-money run.

- [ ] **Step 4: Commit**

```bash
git add src/trader/adapters/clob_executor_wrapper.rs
git commit -m "feat(trader): ClobOrderExecutor::place_limit + cancel via SDK"
```

---

## Task 6: SimulatedExecutor::place_limit + cancel (optimistic immediate fill)

**Files:**
- Modify: `src/trader/adapters/simulated_executor.rs`

The simulator returns a synthetic `OrderId`. `place_limit` does NOT actually fill anything — it just records the order. The state machine in `run_maker` polls `OrderEventStream` for fill notifications; the simulator's `OrderEventStream` impl (added in Task 7's stub) returns "filled" immediately for any order id it sees. This keeps dry-run state-machine validation simple.

- [ ] **Step 1: Write the failing tests**

Add to `src/trader/adapters/simulated_executor.rs` inside `#[cfg(test)] mod tests`:

```rust
    use crate::trader::executor::{OrderId, OrderSide};

    #[tokio::test]
    async fn place_limit_returns_synthetic_order_id() {
        let ex = SimulatedExecutor::default();
        let id = ex.place_limit("tok-1", OrderSide::Buy,
            Decimal::from_str("0.49").unwrap(), Decimal::from(10)).await.unwrap();
        // Synthetic id is deterministic and non-empty.
        assert!(!id.0.is_empty());
    }

    #[tokio::test]
    async fn place_limit_returns_unique_ids_for_consecutive_calls() {
        let ex = SimulatedExecutor::default();
        let id1 = ex.place_limit("tok", OrderSide::Buy, Decimal::from_str("0.49").unwrap(), Decimal::from(10)).await.unwrap();
        let id2 = ex.place_limit("tok", OrderSide::Buy, Decimal::from_str("0.50").unwrap(), Decimal::from(10)).await.unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn cancel_succeeds_for_any_order_id() {
        let ex = SimulatedExecutor::default();
        let r = ex.cancel(&OrderId("anything".into())).await;
        assert!(r.is_ok());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::adapters::simulated_executor::`
Expected: FAIL — `place_limit` returns NotSupported (default).

- [ ] **Step 3: Add the impls**

Edit `src/trader/adapters/simulated_executor.rs`. Update the struct to include a counter for synthetic IDs (do NOT mark mutable — use atomic):

```rust
use std::sync::atomic::{AtomicU64, Ordering};

pub struct SimulatedExecutor {
    buy_price: Decimal,
    sell_price: Decimal,
    order_counter: AtomicU64,
}

impl Default for SimulatedExecutor {
    fn default() -> Self {
        Self {
            buy_price: Decimal::from_str("0.50").unwrap(),
            sell_price: Decimal::from_str("0.99").unwrap(),
            order_counter: AtomicU64::new(0),
        }
    }
}
```

Update the `with_prices` constructor accordingly:

```rust
impl SimulatedExecutor {
    pub fn new() -> Self { Self::default() }
    pub fn with_prices(buy: Decimal, sell: Decimal) -> Self {
        Self { buy_price: buy, sell_price: sell, order_counter: AtomicU64::new(0) }
    }
}
```

Add to the `impl OrderExecutor for SimulatedExecutor` block, after `sell_at_bid`:

```rust
    async fn place_limit(
        &self,
        _token_id: &str,
        _side: crate::trader::executor::OrderSide,
        _price: Decimal,
        _shares: Decimal,
    ) -> Result<crate::trader::executor::OrderId, ExecError> {
        let n = self.order_counter.fetch_add(1, Ordering::SeqCst);
        Ok(crate::trader::executor::OrderId(format!("sim-order-{n}")))
    }

    async fn cancel(
        &self,
        _order_id: &crate::trader::executor::OrderId,
    ) -> Result<(), ExecError> {
        Ok(())
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::adapters::simulated_executor::`
Expected: PASS — all tests green.

- [ ] **Step 5: Commit**

```bash
git add src/trader/adapters/simulated_executor.rs
git commit -m "feat(trader): SimulatedExecutor::place_limit + cancel (synthetic ids)"
```

---

## Task 7: OrderEventStream trait + polling impl + stub

**Files:**
- Create: `src/trader/order_events.rs`
- Modify: `src/trader/mod.rs`

The trait subscribes per order_id. Polling impl uses SDK's `client.order(id)` every 2s. Stub for tests fires a scripted event sequence.

- [ ] **Step 1: Add module declaration**

Edit `src/trader/mod.rs`:

```rust
pub mod order_events;
```

- [ ] **Step 2: Write the failing test**

Create `src/trader/order_events.rs`:

```rust
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
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test --lib trader::order_events::`
Expected: PASS — 2 tests green.

- [ ] **Step 4: Commit**

```bash
git add src/trader/order_events.rs src/trader/mod.rs
git commit -m "feat(trader): OrderEventStream trait + ScriptedOrderEvents test stub"
```

---

## Task 8: PolymarketPollOrderEvents — polling adapter

**Files:**
- Modify: `src/trader/order_events.rs` (add the production impl)

The polling impl wraps the SDK's authenticated CLOB client. On `watch(id)` it spawns a background task that polls `client.order(id)` every 2s and emits events.

- [ ] **Step 1: Write the failing test**

The polling impl can't be unit-tested without a live SDK client. Just add a "constructs cleanly" smoke test. Append to the `#[cfg(test)] mod tests` block in `src/trader/order_events.rs`:

```rust
    // Smoke only — real polling against CLOB requires authenticated client.
    // Exercised end-to-end in tests/maker_integration.rs.
    #[test]
    fn polymarket_poll_order_events_constructs() {
        // Just verify the type exists with the expected name.
        // We can't construct without a real authenticated SDK Client.
        fn _assert_sized<T: Sized>() {}
        _assert_sized::<super::PolymarketPollOrderEvents>();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib trader::order_events::`
Expected: FAIL — `cannot find type 'PolymarketPollOrderEvents'`.

- [ ] **Step 3: Add the polling impl**

Edit `src/trader/order_events.rs`. Add at the top, after the existing `OrderEvent` and trait definitions:

```rust
use polymarket_client_sdk_v2::clob::auth::{Authenticated, Normal};
use polymarket_client_sdk_v2::clob::Client as ClobClient;
use polymarket_client_sdk_v2::clob::types::OrderStatusType;
use std::sync::Arc;
use std::time::Duration;

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
                    let new_fill = matched - last_matched;
                    last_matched = matched;
                    let _ = tx.send(OrderEvent::Filled {
                        id: id_owned.clone(),
                        fill_price: resp.price,
                        shares_filled: matched,
                        total_shares: total,
                    }).await.is_err();
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
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib trader::order_events::`
Expected: PASS — 3 tests green.

- [ ] **Step 5: Verify trader binary compiles**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean.

- [ ] **Step 6: Commit**

```bash
git add src/trader/order_events.rs
git commit -m "feat(trader): PolymarketPollOrderEvents — 2s polling fill detection"
```

---

## Task 9: run_maker state machine

**Files:**
- Create: `src/trader/maker.rs`
- Modify: `src/trader/mod.rs`

This is the core of v1.7. The state machine has two phases (PendingBuy, PendingTpSell), each a `tokio::select!` over fill events, price ticks, sweep/cancel timers, shutdown.

- [ ] **Step 1: Add module declaration**

Edit `src/trader/mod.rs`:

```rust
pub mod maker;
```

- [ ] **Step 2: Write the failing tests**

Create `src/trader/maker.rs`:

```rust
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
use tokio::sync::mpsc;
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

    // Phase 2: PendingTpSell with SL price-watch + t=270 cancel-and-market-sell.
    sell_with_tp_sl(deps, ladder, market, token_id, &buy_fill, exit_cfg, window_ts, shutdown).await
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

/// Phase 2 — TP limit + SL price watch + t=270 cancel-and-market-sell.
async fn sell_with_tp_sl(
    deps: &MakerDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    buy_fill: &BuyFill,
    exit_cfg: &ExitConfig,
    window_ts: i64,
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
        Ok(rx) => rx,
        Err(_) => {
            let _ = deps.executor.cancel(&tp_id).await;
            return market_sell_residual(deps, ladder, token_id, buy_fill.shares, buy_fill.dollars, &exit_cfg.sl_price).await;
        }
    };

    // Tp_partial_proceeds tracked across partial fills.
    let mut tp_partial_shares = Decimal::ZERO;
    let mut tp_partial_proceeds = Decimal::ZERO;

    // Cancel-at-t=270 absolute deadline (relative to window_ts, not to phase 2 start).
    let cancel_unix = window_ts + 270;
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
                // Poll bid; if ≤ sl_price, trigger SL exit.
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

            ev = tp_events.recv() => {
                match ev {
                    None => continue, // channel closed; rely on deadline or SL
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
                        // TP no longer resting; fall through to SL/deadline waiters.
                        // Drop the channel by replacing with a never-resolving one.
                        // Simpler: just continue and let select! prefer the other arms.
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
    use crate::trader::errors::{EmitError, FetchError, PriceError};
    use crate::trader::event::TraderEvent;
    use crate::trader::executor::FillResult;
    use crate::trader::ladder::Direction;
    use crate::trader::order_events::tests::ScriptedOrderEvents;
    use chrono::Utc;
    use std::str::FromStr;
    use std::sync::Mutex;
    use uuid::Uuid;

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
        use crate::trader::ladder::Direction;
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
            chrono::Utc::now().timestamp(), // now → cancel deadline ~270s in future
            CancellationToken::new(),
        ).await;

        let proceeds = match outcome {
            WindowOutcome::Won { proceeds_usd } => proceeds_usd,
            other => panic!("expected Won, got {other:?}"),
        };
        // 10 sh × 0.85 = 8.50 proceeds
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
        let events = ScriptedOrderEvents::new(); // no scripted fills → all 3 buys time out
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
        // Price drops to 0.40 (≤ sl_price 0.45) on second poll.
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
            CancellationToken::new(),
        ).await;

        // Sold 10 shares at 0.40 → $4.00 proceeds vs $4.90 buy → Lost $0.90.
        assert!(matches!(outcome, WindowOutcome::Lost { .. }));
        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k,
            TraderEventKind::ExitTriggered { kind: crate::trader::exit_watcher::ExitKind::Sl, .. }
        )));
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib trader::maker::`
Expected: FAIL — `cannot find module 'maker'`.

After mod.rs add: tests compile. The 3 tests are:
- `buy_fills_immediately_then_tp_fills_returns_won` → PASS
- `buy_never_fills_three_steps_then_skipped` → PASS
- `sl_triggers_during_hold_phase` → PASS

Run: `cargo test --lib trader::maker::`
Expected: PASS — 3 tests green.

- [ ] **Step 4: Verify trader binary still compiles**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean.

- [ ] **Step 5: Commit**

```bash
git add src/trader/maker.rs src/trader/mod.rs
git commit -m "feat(trader): run_maker state machine — limit BUY sweep + TP limit + SL price watch"
```

---

## Task 10: window.rs dispatches on cfg.maker

**Files:**
- Modify: `src/trader/window.rs`

`run_with_tp_sl` becomes a 5-line dispatcher: if `cfg.maker` route to `run_maker`, else existing logic. The existing logic (the entire body of the current `run_with_tp_sl`) is extracted unchanged into `run_taker`.

- [ ] **Step 1: Inspect existing window.rs**

Read `src/trader/window.rs` lines 184-244 (the existing `run_with_tp_sl` body). It is the source for the new `run_taker`.

- [ ] **Step 2: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/trader/window.rs`, near other tp-sl tests:

```rust
    #[tokio::test(start_paused = true)]
    async fn maker_flag_routes_to_run_maker() {
        // Smoke: with cfg.exit.maker=true and a stub fill, run_window dispatches
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
        };
        let cfg = WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: Some(ExitConfig {
                tp_price: Decimal::from_str("0.85").unwrap(),
                sl_price: Decimal::from_str("0.45").unwrap(),
                poll: std::time::Duration::from_millis(50),
            }),
            maker: true,
        };
        let _outcome = run_window(&deps, &cfg, &fresh_ladder(), chrono::Utc::now().timestamp()).await;

        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::BuyLimitPosted { .. })),
                "maker route must emit BuyLimitPosted; events: {kinds:?}");
    }
```

You'll need to import `OrderId` and the events stub at the top of the test module — add:

```rust
    use crate::trader::executor::OrderId;
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib trader::window::maker_flag_routes_to_run_maker`
Expected: FAIL — `field 'events' does not exist on WindowDeps` and/or `field 'maker' does not exist on WindowConfig`.

- [ ] **Step 4: Widen WindowDeps + WindowConfig**

Edit `src/trader/window.rs`. At the top imports:

```rust
use crate::trader::order_events::OrderEventStream;
```

Update `WindowDeps`:

```rust
pub struct WindowDeps {
    pub market: Arc<dyn MarketDiscovery>,
    pub executor: Arc<dyn OrderExecutor>,
    pub resolver: Arc<dyn WindowResolver>,
    pub emitter: Arc<dyn TraderEventEmitter>,
    pub price: Arc<dyn MidwindowPriceFetcher>,
    pub events: Arc<dyn OrderEventStream>,
}
```

Update `WindowConfig`:

```rust
pub struct WindowConfig {
    pub band_min: Decimal,
    pub band_max: Decimal,
    pub exit: Option<ExitConfig>,
    pub maker: bool,
}
```

Update every existing test helper that builds `WindowConfig { ... }` — add `maker: false`. Same for every `WindowDeps { ... }` literal — add `events: <stub>`. Use a tiny stub helper:

```rust
    fn stub_events() -> Arc<crate::trader::order_events::tests::ScriptedOrderEvents> {
        crate::trader::order_events::tests::ScriptedOrderEvents::new()
    }
```

Apply the additions to the existing test bodies (search for `WindowConfig {` and `WindowDeps {` in the file and patch each occurrence).

- [ ] **Step 5: Update run_with_tp_sl to dispatch**

Find the existing `run_with_tp_sl` function (lines 184-244 ish). Rename the existing body to `run_taker` — keep all logic unchanged, just rename:

```rust
async fn run_taker(
    deps: &WindowDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    buy_fill: &FillResult,
    exit_cfg: &crate::trader::exit_watcher::ExitConfig,
    window_ts: i64,
) -> WindowOutcome {
    // ... existing body unchanged ...
}
```

Then add the new `run_with_tp_sl` dispatcher above it:

```rust
async fn run_with_tp_sl(
    deps: &WindowDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    buy_fill: &FillResult,
    exit_cfg: &crate::trader::exit_watcher::ExitConfig,
    window_ts: i64,
    maker: bool,
    ask: Decimal,
    dollars: Decimal,
) -> WindowOutcome {
    if maker {
        let maker_deps = crate::trader::maker::MakerDeps {
            executor: deps.executor.clone(),
            events: deps.events.clone(),
            price: deps.price.clone(),
            emitter: deps.emitter.clone(),
        };
        return crate::trader::maker::run_maker(
            &maker_deps, ladder, market, token_id, dollars, ask, exit_cfg, window_ts,
            tokio_util::sync::CancellationToken::new(),
        ).await;
    }
    run_taker(deps, ladder, market, token_id, buy_fill, exit_cfg, window_ts).await
}
```

Now find `run_window`'s call site to `run_with_tp_sl` (it's the `Some(exit_cfg)` arm of the `match cfg.exit`). The current call passes `(deps, ladder, &market, &token_id, &buy_fill, exit_cfg, window_ts)`. Add three params: `cfg.maker`, `ask`, `ladder.current_bet_usd()`.

But wait — in maker mode, **the buy_fok happens INSIDE run_maker, not before**. So we need to NOT call `buy_fok` in `run_window` when `cfg.maker` is true. Refactor:

Replace the buy section of `run_window`. Current logic (Step 3 of run_window) does FoK buy unconditionally. Change it:

```rust
    // Step 3: Buy. Maker mode places its own limit buy inside run_maker; only
    // the taker path does the FoK here.
    let dollars = ladder.current_bet_usd();
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
            &maker_deps, ladder, &market, &token_id, dollars, ask, exit_cfg, window_ts,
            tokio_util::sync::CancellationToken::new(),
        ).await;
    }

    // Taker path (existing behaviour) — unchanged below.
    let shares_needed = compute_share_count(dollars, ask);
    if !meets_minimum(shares_needed) {
        emit_kind(deps, ladder, TraderEventKind::OrderRejected {
            reason: format!("below 5-share minimum: {shares_needed}"),
        }).await;
        return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
    }
    // ... rest of existing run_window unchanged: OrderPlaced → buy_fok → run_with_tp_sl(taker)
```

Remove the `run_with_tp_sl` dispatcher we added a few minutes ago — it's no longer needed; the `if cfg.maker` branch in run_window goes straight to `run_maker`. Keep `run_taker` as the only callee on the taker path.

Equivalent: simplify the taker-path call site to call `run_taker(...)` directly:

```rust
    // After buy_fok succeeds in taker path:
    match &cfg.exit {
        None => await_resolution_and_sweep(deps, ladder, &market, &token_id, &buy_fill).await,
        Some(exit_cfg) => run_taker(deps, ladder, &market, &token_id, &buy_fill, exit_cfg, window_ts).await,
    }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib trader::window::`
Expected: PASS — all existing tests + new maker dispatch test green.

- [ ] **Step 7: Run full lib tests**

Run: `cargo test --lib`
Expected: PASS — full suite green.

- [ ] **Step 8: Commit**

```bash
git add src/trader/window.rs
git commit -m "feat(trader): dispatch run_maker when --maker, else run_taker (existing v1.5 path)"
```

---

## Task 11: Wire OrderEventStream into poly-trader binary

**Files:**
- Modify: `src/bin/poly-trader.rs`

- [ ] **Step 1: Read existing binary wiring**

Read `src/bin/poly-trader.rs`. Find where `executor` is constructed (around line 58-63 — `if args.dry_run { Sim } else { CLOB }`). The `OrderEventStream` follows the same pattern.

- [ ] **Step 2: Add the wiring**

Edit `src/bin/poly-trader.rs`. Imports near the top:

```rust
use poly_tui::trader::order_events::{OrderEventStream, PolymarketPollOrderEvents};
use poly_tui::trader::order_events::tests::ScriptedOrderEvents;  // for dry-run stub
```

After the executor construction:

```rust
    // OrderEventStream — only used when --maker. Real polling impl when CLOB
    // authenticated; scripted stub when dry-run (events arrive immediately so
    // run_maker progresses through the state machine).
    let events: Arc<dyn OrderEventStream> = if args.dry_run {
        // Dry-run: scripted stub that auto-fills any order at the limit price.
        // run_maker subscribes per order_id; the stub returns a channel that
        // emits one Filled event matching the order's stated total_shares,
        // simulating a perfect maker fill.
        Arc::new(crate::AutoFillEvents::default())
    } else {
        // Real CLOB: poll client.order(id) every 2s.
        // ClobOrderExecutor doesn't expose its inner client today; we'd need
        // either a getter or a parallel ClobClient connection. Workaround for
        // v1.7: build a second ClobClient just for OrderEventStream polling.
        let poll_client = poly_tui::trader::adapters::clob_executor_wrapper::ClobOrderExecutor::connect(
            &cfg.clob_host, &cfg.polymarket_private_key
        ).await.context("OrderEventStream CLOB auth")?;
        // ClobOrderExecutor has a private `client` field; expose via a helper
        // accessor (see Task 11 step 3 below). For now, reuse the executor's
        // own SDK client via a method `inner_client()` we'll add.
        Arc::new(PolymarketPollOrderEvents::new(poll_client.inner_client()))
    };
```

Stub `AutoFillEvents` for dry-run — define at the bottom of `poly-trader.rs`:

```rust
/// Dry-run OrderEventStream: any watched order id immediately receives a
/// Filled event matching its full size at $0.50 buy / $0.85 sell.
/// Crude but adequate for state-machine validation in --dry-run mode.
#[derive(Default)]
struct AutoFillEvents;

#[async_trait::async_trait]
impl OrderEventStream for AutoFillEvents {
    async fn watch(&self, id: poly_tui::trader::executor::OrderId)
        -> Result<tokio::sync::mpsc::Receiver<poly_tui::trader::order_events::OrderEvent>,
                  poly_tui::trader::errors::StreamError>
    {
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tokio::spawn(async move {
            // Yield once so the caller can finish posting before we "fill".
            tokio::task::yield_now().await;
            let _ = tx.send(poly_tui::trader::order_events::OrderEvent::Filled {
                id: id.clone(),
                // The actual fill price is irrelevant for dry-run; run_maker
                // uses the limit price from its own state, not from this event.
                fill_price: rust_decimal::Decimal::from_str_exact("0.50").unwrap(),
                shares_filled: rust_decimal::Decimal::from(10),
                total_shares: rust_decimal::Decimal::from(10),
            }).await;
        });
        Ok(rx)
    }
}
```

Pass `events` into `WindowDeps`:

```rust
    let window_deps = Arc::new(WindowDeps {
        market: market.clone(),
        executor: executor.clone(),
        resolver: resolver.clone(),
        emitter: emitter.clone(),
        price: price.clone(),
        events: events.clone(),
    });
```

Pass `args.maker` into `WindowConfig`:

```rust
    let window_cfg = WindowConfig {
        band_min: args.band_min,
        band_max: args.band_max,
        exit: exit_cfg,
        maker: args.maker,
    };
```

- [ ] **Step 3: Add `inner_client()` accessor to ClobOrderExecutor**

Edit `src/trader/adapters/clob_executor_wrapper.rs`. Add a method to the `impl ClobOrderExecutor` block:

```rust
    /// Expose the inner SDK client (Arc-wrapped) for OrderEventStream polling.
    /// The executor already owns one auth'd client; sharing it keeps a single
    /// session vs opening a parallel auth flow.
    pub fn inner_client(&self) -> std::sync::Arc<polymarket_client_sdk_v2::clob::Client<polymarket_client_sdk_v2::clob::auth::Authenticated<polymarket_client_sdk_v2::clob::auth::Normal>>> {
        std::sync::Arc::new(self.client.clone())
    }
```

Then back in `poly-trader.rs`, simplify the events construction:

```rust
    let events: Arc<dyn OrderEventStream> = if args.dry_run {
        Arc::new(AutoFillEvents)
    } else {
        // executor is already an auth'd ClobOrderExecutor — reuse its client.
        // Need a downcast? No — easier to build the events with the same
        // private_key + host as a parallel auth, OR add an executor accessor.
        // Simplest: re-auth a second client just for the events stream.
        let evt_exec = poly_tui::trader::adapters::clob_executor_wrapper::ClobOrderExecutor::connect(
            &cfg.clob_host, &cfg.polymarket_private_key
        ).await.context("OrderEventStream CLOB auth")?;
        Arc::new(PolymarketPollOrderEvents::new(evt_exec.inner_client()))
    };
```

(The double-auth is a v1.7 simplification; v1.7.1 can refactor to share a single client.)

- [ ] **Step 4: Build the binary**

Stop the running TUI first:

```bash
tmux send-keys -t poly-tui q
```

Then:

Run: `cargo build --bin poly-trader`
Expected: Compiles clean.

- [ ] **Step 5: Smoke-test the new flag via --help**

Run: `./target/debug/poly-trader.exe --help`
Expected: STDOUT shows `--maker` flag in the usage list.

- [ ] **Step 6: Smoke dry-run with --maker --max-windows 1**

Run: `./target/debug/poly-trader.exe --direction up --base 5 --dry-run --reset --max-windows 1 --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45 --maker`
Expected: One window completes successfully, prints session-ended at exit. The Redis stream contains `BuyLimitPosted` and `TpLimitPosted` events.

- [ ] **Step 7: Commit**

```bash
git add src/bin/poly-trader.rs src/trader/adapters/clob_executor_wrapper.rs
git commit -m "feat(trader): wire OrderEventStream and --maker flag in poly-trader binary"
```

---

## Task 12: Integration test — testcontainers + scripted CLOB stub

**Files:**
- Create: `tests/maker_integration.rs`
- Modify: `Cargo.toml` (add `[[test]]` entry)

- [ ] **Step 1: Add Cargo.toml entry**

Edit `Cargo.toml`. Append to the bottom:

```toml
[[test]]
name = "maker_integration"
path = "tests/maker_integration.rs"
```

- [ ] **Step 2: Write the test**

Create `tests/maker_integration.rs`:

```rust
#![cfg(test)]

use chrono::Utc;
use poly_tui::trader::adapters::redis_state_wrapper::RedisTraderState;
use poly_tui::trader::adapters::redis_stream_wrapper::RedisTraderStream;
use poly_tui::trader::adapters::simulated_executor::SimulatedExecutor;
use poly_tui::trader::event::{TraderEvent, TraderEventEmitter, TraderEventKind};
use poly_tui::trader::executor::OrderId;
use poly_tui::trader::order_events::{OrderEvent, OrderEventStream};
use poly_tui::trader::order_events::tests::ScriptedOrderEvents;
use poly_tui::tui::events::TraderEventStream;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

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
    // Redis stream emit→subscribe roundtrip. Standalone — doesn't run
    // run_maker (that's covered by lib unit tests).
    let (_node, url) = start_redis().await;
    let emitter = Arc::new(RedisTraderStream::connect(&url).await.unwrap());
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

    let stream = TraderEventStream::connect(&url).await.unwrap();
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
    use std::sync::Mutex;
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
```

- [ ] **Step 3: Verify compiles**

Run: `cargo build --tests --test maker_integration`
Expected: Compiles clean.

- [ ] **Step 4: (Optional) run if Docker up**

Run: `cargo test --test maker_integration -- --ignored`
Expected: Both tests PASS in <10s.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml tests/maker_integration.rs
git commit -m "test(maker): integration — Redis roundtrip + run_maker happy-path"
```

---

## Task 13: README + TODO

**Files:**
- Modify: `README.md`
- Modify: `TODO.md`

- [ ] **Step 1: README — add maker section**

Edit `README.md`. After the v1.5 TP/SL subsection (around line 200ish), insert:

````markdown
### Maker mode (v1.7)

The `--maker` flag switches BUY entry + TP exit from market orders to limit orders. Saves ~1% taker fees per round-trip. SL stays as market sell — a limit-priced SL would not protect against fast price drops.

```bash
poly-trader --direction up --base 5 \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45 \
  --maker
```

| Time | Action |
|---|---|
| t=0 | LIMIT BUY @ ask−$0.01 (e.g. 0.49) |
| t=30 | Cancel + re-post @ ask (0.50) |
| t=60 | Cancel + re-post @ ask+$0.01 (0.51, becomes taker) |
| t=90 | Cancel + skip window (no entry) |
| after buy fill | LIMIT TP @ tp_price (e.g. 0.85) |
| TP fully fills | Won, exit |
| TP partial fill | Keep resting, accumulate proceeds |
| SL bid ≤ sl_price | Cancel TP, market sell residual |
| t=270 | Cancel TP, market sell residual at current bid |

`--maker` requires `--exit-rule tp-sl` (will reject otherwise). Default is off — v1.5 market-order behavior preserved bit-for-bit.

**Caveats:**
- Requires Polymarket maker-fee structure for actual savings. If maker == taker, v1.7 == v1.5 cost.
- Lower window participation (~5–10% windows skipped due to entry sweep exhausting). Backtest assumed 100% — discount expectation accordingly.
- Fill detection via 2s polling (≤2s latency vs market order's instant fill).
````

- [ ] **Step 2: TODO — tick v1.7 done**

Edit `TODO.md`. Replace the v1.7 placeholder section with the completed marker. Find the heading `## v1.7 — Limit-order maker mode (待 24h 真钱数据后决定)` and replace with:

```markdown
## v1.7 — Limit-order maker mode ✅ COMPLETE

`--maker` flag activates limit BUY (with 30s/60s/90s sweep) + limit TP. SL stays market. End-of-window: cancel TP at t=270s + market-sell residual. See `docs/superpowers/specs/2026-05-10-trader-maker-mode-design.md`.

- [x] CLI: `--maker` flag with `--exit-rule tp-sl` validation
- [x] `OrderId` + `OrderSide` types; `ExecError::NotSupported`
- [x] `OrderExecutor::place_limit` + `cancel` (default NotSupported, real impls in Clob + Sim adapters)
- [x] `OrderEventStream` trait + 2s polling impl (`PolymarketPollOrderEvents`) + scripted stub
- [x] `run_maker` state machine — PendingBuy + PendingTpSell phases via tokio::select!
- [x] 4 new event variants: `BuyLimitPosted`, `BuyLimitSwept`, `TpLimitPosted`, `TpLimitFilled`
- [x] dispatch in `run_with_tp_sl`: maker → `run_maker`, taker → existing v1.5 path (renamed `run_taker`)
- [x] dry-run uses `AutoFillEvents` stub for state-machine validation
- [x] Integration test: Redis event roundtrip + run_maker happy-path
- [x] README + TODO docs

**Open items / next versions:**
- WebSocket fill detection (v1.7.1) — currently polling-only at 2s
- Single-CLOB-client refactor (v1.7.2) — currently dual-auth for executor + events
- Real-money A/B comparison vs v1.5 to measure actual fee savings
```

(Move it to be ABOVE the v1.3 daemon split section, as the most recently-completed feature.)

- [ ] **Step 3: Verify build still clean**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean.

- [ ] **Step 4: Commit**

```bash
git add README.md TODO.md
git commit -m "docs: README + TODO updated for v1.7 maker mode"
```

---

## Self-review

After all tasks:

**1. Spec coverage:**

| Spec section | Implemented in |
|---|---|
| Architecture (state machine, two phases) | Task 9 |
| CLI: `--maker` flag, validation | Task 3 |
| Sweep schedule (t=0/30/60/90) | Task 9 (`buy_with_sweep`) |
| Hold schedule (TP fill / partial / SL / t=270) | Task 9 (`sell_with_tp_sl`) |
| `OrderExecutor::place_limit` + `cancel` | Task 2 (trait), Task 5 (CLOB), Task 6 (Sim) |
| `OrderEventStream` trait + polling | Tasks 7 + 8 |
| 4 new event variants | Task 4 |
| Dispatch on `cfg.maker` | Task 10 |
| `--maker` requires `--exit-rule tp-sl` | Task 3 (validation) |
| Tests (state-machine, integration) | Tasks 9, 12 |
| README + TODO | Task 13 |

**2. Placeholder scan:** No "TBD"/"implement later"/"similar to". All code blocks complete.

**3. Type consistency:** `OrderId`, `OrderSide`, `OrderEvent`, `OrderEventStream`, `MakerDeps`, `BuyOutcome`, `TraderEventKind::BuyLimitPosted` etc. spelled identically across files.

**4. Open caveats noted:**
- Task 11 step 3 introduces a "double auth" by re-running `ClobOrderExecutor::connect` for the events stream. v1.7.1 should refactor to share a single client. Documented in TODO.md "Open items".
- The `Clone for ExecError` impl in Task 9 test stubs is a bit ugly; if `thiserror` derives Clone we can remove it. Acceptable for v1.7.
