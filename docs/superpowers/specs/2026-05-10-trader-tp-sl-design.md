# v1.5 — poly-trader TP/SL exit logic (strategy 4)

**Goal:** Add take-profit / stop-loss mid-window exits to `poly-trader` so strategy 4 (`tp_price=0.85, sl_price=0.45`, band 0.45–0.55, Martingale base $5 max-step 5) — the only profitable strategy in three independent 30-day backtests (+$5,088 / +$9,802 / +$7,747) — can run live in dry-run mode against Polymarket.

**Non-goal:** This spec does not generalize to all four `ExitRule` variants from the backtest. Only `HoldToResolution` (existing) and `TpSlOrHold` (new) are exposed. Other variants can be added later if backtest sweeps surface them.

## Context

`poly-trader` v1.1 implements one strategy: enter at window open if ask in band, hold to resolution, sell if won. The runtime structure is:

```
discover market → band check → buy_fok → await_resolution (gamma poll) → sell_market on win
```

Backtest v1.4 confirmed strategy 4 (TP+SL asymmetric) is profitable across three independent 30-day samples. To validate live, the trader needs to monitor the UP-token price during the window and exit early when the bid crosses configured thresholds.

## Architecture

A new `MidwindowPriceFetcher` polls gamma every 5s during the window. A new `ExitWatcher` checks each tick against TP/SL thresholds and either triggers a sell or runs to deadline. `run_window` selects between `ExitWatcher` and the existing `await_resolution`; whichever finishes first determines the outcome.

```
┌──────────────────────────────────────┐
│             run_window (v1.5)        │
│                                      │
│  buy_fok ─────────────┐              │
│                       ▼              │
│           ┌─────────────────────┐    │
│           │ tokio::select! {    │    │
│           │   ExitWatcher        │    │
│           │   ──────────────    │    │
│           │   poll gamma 5s     │    │
│           │   bid≥tp → SELL  ←┐ │    │
│           │   bid≤sl → SELL  ←┤ │    │
│           │                   │ │    │
│           │   await_resolution│ │    │
│           │   ──────────────  │ │    │
│           │   gamma closed?   │ │    │
│           │   winner-sweep ←──┘ │    │
│           │ }                   │    │
│           └─────────────────────┘    │
│                       │              │
│                       ▼              │
│            WindowOutcome             │
│             Won { proceeds }         │
│             Lost { spent }           │
└──────────────────────────────────────┘
```

### Why select! over modifying resolver

`WindowResolver` is a clean abstraction (`async fn await_resolution(market)`) used in 4 places. Embedding TP/SL inside it would couple price-monitoring with resolution-detection. A parallel watcher keeps both abstractions single-purpose and the code path under `--exit-rule hold` byte-for-byte identical to v1.1.

## CLI surface

```bash
# v1.1 default (unchanged)
poly-trader --direction up --base 5

# v1.5 strategy 4
poly-trader --direction up --base 5 \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45

# dry-run, both modes
poly-trader --direction up --base 5 --dry-run [--exit-rule tp-sl ...]
```

| Flag | Default | Notes |
|---|---|---|
| `--exit-rule` | `hold` | Values: `hold`, `tp-sl`. Default reproduces v1.1 behavior exactly. |
| `--tp-price` | none | Required when `--exit-rule tp-sl`. Decimal in (0, 1). |
| `--sl-price` | none | Required when `--exit-rule tp-sl`. Decimal in (0, 1). |
| `--poll-secs` | `5` | Gamma poll cadence during the window. Range 1..=30. |

Validation: `--tp-price > --sl-price`, both in (0, 1) exclusive, both required when `--exit-rule tp-sl`. Clap raises a usage error otherwise.

## Components

### `src/trader/price.rs` *(new)*

```rust
#[async_trait]
pub trait MidwindowPriceFetcher: Send + Sync {
    /// Fetch the current bid for `token_id`. Returns Err on transient failure;
    /// caller logs and retries on next tick.
    async fn current_bid(&self, token_id: &str) -> Result<Decimal, PriceError>;
}

pub struct GammaPriceFetcher { /* reuses GammaMarketDiscovery's reqwest client + cache-bust nonce */ }
```

The gamma fetcher reads the per-token entry of `outcomePrices` from the gamma `/markets?clob_token_ids=…` response. `outcomePrices` is gamma's conventional current-bid per outcome (already in `WindowMarket`'s upstream call — exposed but not stored today). For dry-run + tests, a stub impl with a scripted price series.

### `src/trader/exit_watcher.rs` *(new)*

```rust
pub struct ExitConfig {
    pub tp_price: Decimal,
    pub sl_price: Decimal,
    pub poll_secs: u32,
}

pub enum ExitTrigger {
    Tp { bid: Decimal },
    Sl { bid: Decimal },
}

pub struct ExitWatcher {
    fetcher: Arc<dyn MidwindowPriceFetcher>,
    cfg: ExitConfig,
    deadline: tokio::time::Instant,
}

impl ExitWatcher {
    /// Polls until trigger fires, deadline reached, or cancellation.
    /// Returns Some(trigger) on TP/SL hit, None on deadline (caller falls through to resolver).
    pub async fn watch(&self, token_id: &str) -> Option<ExitTrigger>;
}
```

Polling loop:
1. `tokio::time::sleep(poll_secs)`.
2. Call `fetcher.current_bid(token_id)`. On error, log warning, continue.
3. If `bid ≥ tp_price` → return `Some(Tp { bid })`.
4. If `bid ≤ sl_price` → return `Some(Sl { bid })`.
5. If now ≥ deadline → return `None`.

### `src/trader/window.rs` *(modified)*

`run_window` gains an `Option<ExitConfig>` in `WindowConfig`. After `buy_fok`:

```rust
match cfg.exit {
    None => {
        // v1.1 path — existing code unchanged
        let resolution = deps.resolver.await_resolution(&market).await?;
        // ... winner sweep
    }
    Some(exit_cfg) => {
        let watcher = ExitWatcher::new(deps.price.clone(), exit_cfg, deadline);
        let trigger = tokio::select! {
            t = watcher.watch(&token_id) => t,                // Some(trigger) | None on deadline
            r = deps.resolver.await_resolution(&market)       // resolver beat watcher
                => return winner_sweep(r, &deps, &buy_fill, &token_id).await,
        };
        if let Some(t) = trigger {
            // TP or SL fired
            emit ExitTriggered { kind, bid };
            let sell_fill = deps.executor.sell_market(&token_id, buy_fill.shares).await?;
            return if sell_fill.dollars > buy_fill.dollars {
                WindowOutcome::Won { proceeds_usd: sell_fill.dollars }
            } else {
                WindowOutcome::Lost { spent_usd: buy_fill.dollars - sell_fill.dollars }
            };
        }
        // Watcher hit deadline without trigger — wait for resolution and sweep
        let resolution = deps.resolver.await_resolution(&market).await?;
        return winner_sweep(resolution, &deps, &buy_fill, &token_id).await;
    }
}
```

`apply_outcome` and `LadderState` are not modified.

### `src/trader/event.rs` *(modified)*

```rust
pub enum TraderEventKind {
    // ... existing variants
    ExitTriggered {
        kind: ExitKind,        // Tp | Sl
        bid: Decimal,
    },
}
```

Logged before the `sell_market` call so the trace shows trigger reason regardless of fill success.

### `src/bin/poly-trader.rs` *(modified)*

Wires:
1. New `MidwindowPriceFetcher` — `GammaPriceFetcher` always (read-only, no real-money risk in dry-run).
2. Build `Option<ExitConfig>` from CLI args (`None` if `--exit-rule hold`).
3. Pass into `WindowConfig`.

`SimulatedExecutor` already supports `sell_market` for the existing winner-sweep path; reused unchanged.

## Data flow (TP triggers at t=130)

```
t=0   discover market: ask=0.50, token_id=tok-up
      band check: 0.50 in [0.45, 0.55] ✓
      buy_fok($5 @ 0.50) → 10 shares
      emit OrderFilled
      spawn ExitWatcher(tp=0.85, sl=0.45, poll=5s)
      tokio::select! { watcher.watch | resolver.await_resolution }

t=5   poll gamma: bid=0.52 (no trigger)
t=10  poll gamma: bid=0.61 (no trigger)
t=…
t=130 poll gamma: bid=0.86 → trigger TP
      cancel resolver (select drops the future)
      emit ExitTriggered { kind: Tp, bid: 0.86 }
      sell_market(10 shares) → fill_price=0.84, proceeds=$8.40
      emit SellFilled { proceeds: 8.40 }
      return WindowOutcome::Won { proceeds_usd: 8.40 }
      apply_outcome → ladder reset to base $5
```

## Outcome mapping

| Trigger | Sell proceeds | Outcome variant |
|---|---|---|
| TP fires, sell ok | proceeds > spent (typical) | `Won { proceeds_usd }` |
| TP fires, low liquidity | proceeds < spent (rare) | `Lost { spent_usd: spent − proceeds }` |
| SL fires, sell ok | proceeds < spent (typical) | `Lost { spent_usd: spent − proceeds }` |
| SL fires, lucky bounce | proceeds > spent (rare) | `Won { proceeds_usd }` |
| No trigger, our side wins | $0.99 winner sweep | `Won { proceeds_usd: shares * 0.99 }` |
| No trigger, our side loses | shares → 0 | `Lost { spent_usd: dollars }` |
| Sell fails after trigger | n/a | `Won { proceeds_usd: 0 }` + `Alert` (manual reconcile) |

## Error & edge handling

| Scenario | Handling |
|---|---|
| Gamma 502 mid-poll | Log warning, skip tick, continue. Multiple consecutive failures still don't abort — eventually resolver completes. |
| Token ID changes mid-window | Cannot happen — gamma window is immutable once open. Watcher uses the token_id captured at buy time. |
| Sell-after-trigger fails | Emit `SellRejected` + `Alert`, return `Won { proceeds_usd: 0 }`. Same as v1.1 sell-on-win failure path. |
| Resolver wins the select (closure detected before TP/SL) | Watcher future dropped. Existing winner-sweep runs. No event loss — both paths emit through same `emit_kind`. |
| TP and SL both reachable in one tick (price oscillation) | Whichever check fires first in the `if/else if` chain wins. Documented in code; not expected in practice with 5s polls and 50-cent gap. |
| `--exit-rule hold` (default) | `cfg.exit = None`. The `match cfg.exit` branches into the original v1.1 path. Zero behavior change. |
| `--reset` mid-mode-switch | Ladder is mode-agnostic. Ladder state from a hold session resumes fine in tp-sl mode and vice versa. |

## Testing strategy

### Unit tests

| File | New tests |
|---|---|
| `price.rs` | 3 tests: gamma decoder happy path, decoder on missing field, fetcher error propagation. |
| `exit_watcher.rs` | 6 tests: TP triggers at first tick crossing, SL triggers at first tick crossing, deadline returns None, fetcher error skipped (continues), TP wins over SL when crossed simultaneously, cancellation respected. Uses `StubPriceFetcher { prices: Vec<Decimal> }`. |
| `window.rs` | 3 new tests on top of existing 8: tp-sl mode TP path, tp-sl mode SL path, tp-sl mode deadline → resolver fall-through. Stubs reuse existing `StubMarket`/`StubExec`/`StubResolver` patterns. |
| `config.rs` | 3 new tests: `--exit-rule tp-sl --tp-price 0.85 --sl-price 0.45` parses, missing tp-price errors when tp-sl, tp ≤ sl errors. |

### E2E (`tests/trader_e2e.rs`)

New scenario `tp_sl_dry_run_triggers_tp_and_resets_ladder`:
- Spin testcontainers Redis (existing pattern).
- Spawn fake gamma serving a price path crossing TP at t=60s.
- Run `poly-trader --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45 --dry-run --max-windows 1`.
- Assert event log contains `ExitTriggered{kind:Tp}` followed by `SellFilled` followed by ladder reset to step 0.

Existing E2E tests for hold mode still pass (no flag = no behavior change).

### Coverage gate

`cargo llvm-cov --lib --tests` must keep ≥80% on the trader module. Existing v1.1 paths are not modified, only branched on a new flag, so coverage of the v1.1 path is preserved by existing tests.

## Risk caps & operator notes

- `--max-step 5` Martingale cap unchanged. Strategy 4's higher cap-reset frequency (179–303 over 8.5K windows in backtest, vs 42–69 for hold) is absorbed by existing reset semantics.
- Operator should run dry-run for ≥24 hours and verify event log shows roughly 25–35% TP triggers, 50–65% SL triggers, 5–15% deadline fall-throughs (matches backtest distributions). If trigger rates are far off, suspect gamma `outcomePrices` lag or strike-mismatched window boundaries.
- README v1.5 section will document: example commands, expected event sequence, how to inspect TP/SL trigger rates from the Redis stream, and how to fall back to v1.1 hold mode.

## Migration / rollback

- Live ladder state in Redis is shared across modes. Switching from hold to tp-sl mid-session is safe — ladder math is mode-agnostic. Rollback: re-run with `--exit-rule hold` (default), no state migration needed.
- Existing dry-run sessions are unaffected: omitting `--exit-rule` produces v1.1 behavior bit-for-bit.

## Out of scope (explicit YAGNI)

- All four `ExitRule` variants from the backtest. Only `tp-sl` exposed in trader.
- CLOB orderbook polling. Gamma `outcomePrices` is sufficient per the polling-cadence decision.
- Hybrid trigger/sell pricing. One source for both.
- Multi-strategy run (running hold and tp-sl in parallel). One process, one strategy at a time.
- Position monitoring beyond a single window. Existing single-window FSM stands.
- Auto-tuning TP/SL based on observed σ. Operator sets thresholds at start of session.

## Related documents

- v1.1 trader spec: `docs/superpowers/specs/2026-05-09-poly-trader-martingale-design.md`
- v1.4 backtest spec: `docs/superpowers/specs/2026-05-09-backtest-framework-design.md`
- v1.4 backtest plan: `docs/superpowers/plans/2026-05-09-backtest-framework.md`
- Backtest results (3 samples): `backtest-report.html`, `backtest-report-A-mar.html`, `backtest-report-B-feb.html`
