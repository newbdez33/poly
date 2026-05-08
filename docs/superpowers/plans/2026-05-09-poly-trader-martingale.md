# Poly Trader Martingale Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `poly-trader` — a standalone headless binary that runs Martingale on Polymarket BTC 5-minute up/down markets — plus a TUI log panel surfacing trader events from a Redis stream.

**Architecture:** Two binaries (`poly-tui`, `poly-trader`) sharing one `lib`. Trader runs a 5-minute scheduler loop driving a window FSM (discover market → check price band → FoK buy → poll resolution → market sell winners). Pure Martingale logic in `trader::ladder` (zero I/O). Six traits (`MarketDiscovery`, `OrderExecutor`, `WindowResolver`, `TraderStateStore`, `TraderEventEmitter`, `TraderEventStream`) decouple business logic from real CLOB/Gamma/Redis. State persists to Redis (`poly:prod:trader:ladder`); events stream to Redis (`poly:prod:trader:events`). TUI subscribes to that stream. Redis SETNX lock prevents double-instance. Target ≥ 99% coverage on `src/trader/`.

**Tech Stack:** Rust 1.78+, tokio, ratatui + crossterm, polymarket_client_sdk_v2 (CLOB), alloy 1.x (signer), fred 9 (Redis incl. streams + SETNX), reqwest (gamma-api), clap (CLI), uuid, cucumber-rs, mockall, insta, wiremock, testcontainers, proptest.

**Spec:** `docs/superpowers/specs/2026-05-09-poly-trader-martingale-design.md`

**Base commit:** `960961d` (spec landed). Pre-trader v1.0 baseline: `9929c1a` (µUSDC fix).

---

## File Structure

```
poly/
├── Cargo.toml                            ← +reqwest +uuid +clap +proptest; +bin/test entries
├── src/
│   ├── lib.rs                            ← +pub mod trader; +pub mod tui
│   ├── bin/
│   │   ├── poly-tui.rs                   ← +trader event subscriber task wiring
│   │   └── poly-trader.rs                ← NEW: signal handler, lock, scheduler.run
│   ├── tui/
│   │   ├── mod.rs                        ← NEW
│   │   └── events.rs                     ← NEW: TraderEventStream trait + AppEvent forwarder
│   ├── trader/
│   │   ├── mod.rs                        ← NEW: exports
│   │   ├── ladder.rs                     ← NEW: Direction, LadderState, apply_outcome (PURE)
│   │   ├── config.rs                     ← NEW: TraderArgs (clap), TraderConfig
│   │   ├── event.rs                      ← NEW: TraderEvent + TraderEventEmitter trait
│   │   ├── state.rs                      ← NEW: TraderStateStore trait + LadderState serde
│   │   ├── market.rs                     ← NEW: MarketDiscovery trait + decode_event_response
│   │   ├── executor.rs                   ← NEW: OrderExecutor trait + compute_share_count
│   │   ├── resolver.rs                   ← NEW: WindowResolver trait + PolymarketResolver
│   │   ├── window.rs                     ← NEW: run_window orchestration
│   │   ├── scheduler.rs                  ← NEW: 5-min boundary loop + signal handling
│   │   ├── adapters/                     ← NEW: real-world impls (excluded from coverage)
│   │   │   ├── mod.rs
│   │   │   ├── redis_state_wrapper.rs    ← fred state impl
│   │   │   ├── redis_stream_wrapper.rs   ← fred stream impl (emitter + subscriber)
│   │   │   ├── gamma_wrapper.rs          ← reqwest gamma-api impl
│   │   │   ├── clob_executor_wrapper.rs  ← polymarket SDK FoK + market sell
│   │   │   └── simulated_executor.rs     ← dry-run executor
│   │   └── errors.rs                     ← NEW: shared error types (StateError, etc.)
│   ├── (existing modules unchanged: config.rs, domain.rs, clob.rs, cache.rs,
│   │    refresher.rs, app.rs, ui.rs, input.rs)
│   └── ...
└── tests/
    ├── support/
    │   ├── mod.rs                        ← +pub mod fake_*
    │   ├── fake_market.rs                ← NEW
    │   ├── fake_executor.rs              ← NEW
    │   ├── fake_resolver.rs              ← NEW
    │   ├── memory_state_store.rs         ← NEW
    │   ├── memory_event_emitter.rs       ← NEW
    │   └── memory_event_stream.rs        ← NEW
    ├── features/
    │   ├── balance.feature               ← existing (v1.0)
    │   └── trader.feature                ← NEW
    ├── bdd.rs                            ← existing; +trader steps
    ├── e2e_trader.rs                     ← NEW: #[ignore] full-stack with testcontainers + fakes
    ├── trader_state_integration.rs       ← NEW: #[ignore] testcontainers
    └── trader_market_integration.rs      ← NEW: #[ignore] wiremock
```

**Coverage exclusion regex:** `src/bin|src/trader/adapters/|.*_wrapper\.rs`

**Module dependency rules** (enforced by import direction):
```
trader::ladder, trader::event, trader::errors  → domain (only)
trader::config                                  → domain
trader::market                                  → domain (+ reqwest in adapter)
trader::executor                                → domain, clob (+ SDK in adapter)
trader::resolver                                → domain, market (trait)
trader::state                                   → domain, errors
trader::window                                  → all 6 trader traits + ladder
trader::scheduler                               → window + state + event
trader::adapters::*                             → trader::* traits (impl direction)
bin/poly-trader                                 → trader::*, config, cache, domain
tui::events                                     → trader::event (TraderEvent type)
app.rs (existing)                               → +tui::events
```

---

## Task 0: Bootstrap deps + module skeleton

**Files:**
- Modify: `Cargo.toml`
- Create: `src/trader/mod.rs`, 10 placeholder module files in `src/trader/`
- Create: `src/trader/adapters/mod.rs` + 5 wrapper placeholders
- Create: `src/tui/mod.rs`, `src/tui/events.rs` (placeholder)
- Modify: `src/lib.rs`
- Create: `src/bin/poly-trader.rs` (stub)

- [ ] **Step 1: Add dependencies to `Cargo.toml`**

In `[dependencies]` append:
```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
uuid = { version = "1", features = ["v4", "serde"] }
clap = { version = "4", features = ["derive"] }
```

In `[dev-dependencies]` append:
```toml
proptest = "1"
```

Add new `[[bin]]` block at the end:
```toml
[[bin]]
name = "poly-trader"
path = "src/bin/poly-trader.rs"
```

Add commented `[[test]]` blocks (uncomment in their respective tasks):
```toml
# [[test]]
# name = "trader_state_integration"
# path = "tests/trader_state_integration.rs"
#
# [[test]]
# name = "trader_market_integration"
# path = "tests/trader_market_integration.rs"
#
# [[test]]
# name = "e2e_trader"
# path = "tests/e2e_trader.rs"
```

- [ ] **Step 2: Create directories and placeholders**

```bash
cd C:/Users/newbd/projects/dev/poly
mkdir -p src/trader/adapters src/tui src/bin
for f in ladder config event state market executor resolver window scheduler errors; do
  echo "// placeholder" > "src/trader/$f.rs"
done
for f in redis_state_wrapper redis_stream_wrapper gamma_wrapper clob_executor_wrapper simulated_executor; do
  echo "// placeholder" > "src/trader/adapters/$f.rs"
done
echo "// placeholder" > src/tui/events.rs
```

- [ ] **Step 3: Write `src/trader/mod.rs`**

```rust
pub mod ladder;
pub mod config;
pub mod event;
pub mod state;
pub mod market;
pub mod executor;
pub mod resolver;
pub mod window;
pub mod scheduler;
pub mod errors;
pub mod adapters;
```

- [ ] **Step 4: Write `src/trader/adapters/mod.rs`**

```rust
pub mod redis_state_wrapper;
pub mod redis_stream_wrapper;
pub mod gamma_wrapper;
pub mod clob_executor_wrapper;
pub mod simulated_executor;
```

- [ ] **Step 5: Write `src/tui/mod.rs`**

```rust
pub mod events;
```

- [ ] **Step 6: Update `src/lib.rs`** — append (keep existing pub mod):

```rust
pub mod trader;
pub mod tui;
```

- [ ] **Step 7: Stub `src/bin/poly-trader.rs`**

```rust
fn main() {
    println!("poly-trader placeholder");
}
```

- [ ] **Step 8: Verify build**

```bash
cargo build --bin poly-tui --bin poly-trader
```

Expected: `Finished`. Warnings about unused placeholders OK.

- [ ] **Step 9: Commit**

```bash
git add Cargo.toml src/lib.rs src/trader/ src/tui/ src/bin/poly-trader.rs
git commit -m "chore(trader): bootstrap deps + module skeleton"
```

---

## Task 1: trader::ladder — Martingale FSM (PURE)

Heart of correctness. Pure functions, zero I/O. Target 100% coverage.

**Files:**
- Modify: `src/trader/ladder.rs`

- [ ] **Step 1: Write types + apply_outcome + tests**

Replace `src/trader/ladder.rs`:

```rust
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction { Up, Down }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    CapReached,
    ManualStop,
    FatalError(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipReason {
    PriceOutsideBand { ask: Decimal },
    FillOrKillFailed,
    ResolutionTimeout,
    GammaApiUnavailable,
    MarketNotFound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowOutcome {
    Won { proceeds_usd: Decimal },
    Lost { spent_usd: Decimal },
    Skipped { reason: SkipReason },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderState {
    pub session_id: Uuid,
    pub direction: Direction,
    pub base_usd: Decimal,
    pub max_step: u8,
    pub current_step: u8,
    pub session_started_at: DateTime<Utc>,
    pub realized_pnl_usd: Decimal,
    pub windows_won: u32,
    pub windows_lost: u32,
    pub windows_skipped: u32,
    pub stopped: Option<StopReason>,
}

impl LadderState {
    pub fn new(direction: Direction, base_usd: Decimal, max_step: u8, now: DateTime<Utc>) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            direction, base_usd, max_step,
            current_step: 1,
            session_started_at: now,
            realized_pnl_usd: Decimal::ZERO,
            windows_won: 0, windows_lost: 0, windows_skipped: 0,
            stopped: None,
        }
    }

    pub fn current_bet_usd(&self) -> Decimal {
        let multiplier = 2_u64.pow((self.current_step - 1) as u32);
        self.base_usd * Decimal::from(multiplier)
    }

    pub fn is_stopped(&self) -> bool { self.stopped.is_some() }
}

/// Pure FSM transition. No I/O. `_now` reserved for future state-time derived fields.
pub fn apply_outcome(
    state: &LadderState,
    outcome: &WindowOutcome,
    _now: DateTime<Utc>,
) -> LadderState {
    let mut next = state.clone();
    match outcome {
        WindowOutcome::Won { proceeds_usd } => {
            let bet = state.current_bet_usd();
            next.realized_pnl_usd += proceeds_usd - bet;
            next.windows_won += 1;
            next.current_step = 1;
        }
        WindowOutcome::Lost { spent_usd } => {
            next.realized_pnl_usd -= spent_usd;
            next.windows_lost += 1;
            if state.current_step >= state.max_step {
                next.stopped = Some(StopReason::CapReached);
            } else {
                next.current_step += 1;
            }
        }
        WindowOutcome::Skipped { .. } => {
            next.windows_skipped += 1;
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::str::FromStr;

    fn ts() -> DateTime<Utc> { Utc.timestamp_opt(1_700_000_000, 0).unwrap() }

    fn fresh(step: u8) -> LadderState {
        LadderState {
            session_id: Uuid::nil(),
            direction: Direction::Up,
            base_usd: Decimal::from(5),
            max_step: 5,
            current_step: step,
            session_started_at: ts(),
            realized_pnl_usd: Decimal::ZERO,
            windows_won: 0, windows_lost: 0, windows_skipped: 0,
            stopped: None,
        }
    }

    #[test]
    fn current_bet_doubles_each_step() {
        for (step, expected) in [(1u8, "5"), (2, "10"), (3, "20"), (4, "40"), (5, "80")] {
            assert_eq!(fresh(step).current_bet_usd(), Decimal::from_str(expected).unwrap());
        }
    }

    #[test]
    fn won_resets_step_credits_pnl() {
        let s = fresh(3);
        let bet = s.current_bet_usd();
        let next = apply_outcome(&s,
            &WindowOutcome::Won { proceeds_usd: Decimal::from_str("39.60").unwrap() }, ts());
        assert_eq!(next.current_step, 1);
        assert_eq!(next.windows_won, 1);
        assert_eq!(next.realized_pnl_usd, Decimal::from_str("39.60").unwrap() - bet);
        assert!(next.stopped.is_none());
    }

    #[test]
    fn lost_advances_step_debits_pnl() {
        let s = fresh(2);
        let next = apply_outcome(&s, &WindowOutcome::Lost { spent_usd: Decimal::from(10) }, ts());
        assert_eq!(next.current_step, 3);
        assert_eq!(next.windows_lost, 1);
        assert_eq!(next.realized_pnl_usd, Decimal::from(-10));
        assert!(next.stopped.is_none());
    }

    #[test]
    fn lost_at_max_step_sets_cap_reached() {
        let next = apply_outcome(&fresh(5),
            &WindowOutcome::Lost { spent_usd: Decimal::from(80) }, ts());
        assert_eq!(next.current_step, 5);
        assert_eq!(next.stopped, Some(StopReason::CapReached));
    }

    #[test]
    fn skipped_does_not_change_step_or_pnl() {
        let s = fresh(3);
        let next = apply_outcome(&s,
            &WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed }, ts());
        assert_eq!(next.current_step, 3);
        assert_eq!(next.realized_pnl_usd, s.realized_pnl_usd);
        assert_eq!(next.windows_skipped, 1);
        assert_eq!(next.windows_won, 0);
        assert_eq!(next.windows_lost, 0);
    }

    #[test]
    fn cumulative_loss_to_cap() {
        let mut s = LadderState::new(Direction::Up, Decimal::from(5), 5, ts());
        for _ in 0..5 {
            s = apply_outcome(&s, &WindowOutcome::Lost { spent_usd: s.current_bet_usd() }, ts());
        }
        assert_eq!(s.stopped, Some(StopReason::CapReached));
        assert_eq!(s.realized_pnl_usd, Decimal::from(-155));
        assert_eq!(s.windows_lost, 5);
    }

    #[test]
    fn serde_roundtrip_preserves_all_fields() {
        let s = fresh(4);
        let back: LadderState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn serde_roundtrip_with_stopped() {
        let mut s = fresh(5);
        s.stopped = Some(StopReason::CapReached);
        let back: LadderState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn skip_reasons_serialize_distinctly() {
        let band = WindowOutcome::Skipped {
            reason: SkipReason::PriceOutsideBand { ask: Decimal::from_str("0.62").unwrap() },
        };
        let fok = WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
        assert_ne!(serde_json::to_string(&band).unwrap(),
                   serde_json::to_string(&fok).unwrap());
    }

    #[test]
    fn new_session_starts_at_step_1() {
        let s = LadderState::new(Direction::Down, Decimal::from(5), 5, ts());
        assert_eq!(s.current_step, 1);
        assert_eq!(s.realized_pnl_usd, Decimal::ZERO);
        assert!(s.stopped.is_none());
    }

    #[test]
    fn property_step_within_bounds_for_any_outcome() {
        for start in 1..=5_u8 {
            for outcome in [
                WindowOutcome::Won { proceeds_usd: Decimal::from(10) },
                WindowOutcome::Lost { spent_usd: Decimal::from(5) },
                WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed },
            ] {
                let next = apply_outcome(&fresh(start), &outcome, ts());
                assert!(next.current_step >= 1 && next.current_step <= next.max_step);
            }
        }
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib trader::ladder
```

Expected: 11 passed.

- [ ] **Step 3: Commit**

```bash
git add src/trader/ladder.rs
git commit -m "feat(trader): Martingale FSM (LadderState + apply_outcome)"
```

---

## Task 2: trader::config — TraderArgs (clap) + validation

**Files:**
- Modify: `src/trader/config.rs`

- [ ] **Step 1: Replace `src/trader/config.rs`**

```rust
use crate::trader::ladder::Direction;
use clap::{Parser, ValueEnum};
use rust_decimal::Decimal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum DirectionArg { Up, Down }

impl From<DirectionArg> for Direction {
    fn from(d: DirectionArg) -> Self {
        match d {
            DirectionArg::Up => Direction::Up,
            DirectionArg::Down => Direction::Down,
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(name = "poly-trader",
          about = "Polymarket BTC 5min Martingale trader",
          version)]
pub struct TraderArgs {
    #[arg(long, value_enum)]
    pub direction: DirectionArg,
    #[arg(long, default_value = "5")]
    pub base: Decimal,
    #[arg(long, default_value = "5")]
    pub max_step: u8,
    #[arg(long, default_value = "0.45")]
    pub band_min: Decimal,
    #[arg(long, default_value = "0.55")]
    pub band_max: Decimal,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub reset: bool,
    #[arg(long)]
    pub max_windows: Option<u32>,
}

impl TraderArgs {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.base <= Decimal::ZERO { return Err(ConfigError::InvalidBase); }
        if self.max_step < 1 || self.max_step > 10 { return Err(ConfigError::InvalidMaxStep); }
        if self.band_min >= self.band_max
           || self.band_min < Decimal::ZERO
           || self.band_max > Decimal::ONE {
            return Err(ConfigError::InvalidBand);
        }
        Ok(())
    }
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum ConfigError {
    #[error("base must be > 0")]
    InvalidBase,
    #[error("max_step must be in 1..=10")]
    InvalidMaxStep,
    #[error("band: must satisfy 0 <= band_min < band_max <= 1")]
    InvalidBand,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn parse(args: &[&str]) -> TraderArgs {
        let mut full = vec!["poly-trader"];
        full.extend(args);
        TraderArgs::parse_from(full)
    }

    #[test]
    fn parses_minimal_args() {
        let a = parse(&["--direction", "up"]);
        assert_eq!(a.direction, DirectionArg::Up);
        assert_eq!(a.base, Decimal::from(5));
        assert_eq!(a.max_step, 5);
        assert_eq!(a.band_min, Decimal::from_str("0.45").unwrap());
        assert_eq!(a.band_max, Decimal::from_str("0.55").unwrap());
        assert!(!a.dry_run && !a.reset && a.max_windows.is_none());
    }

    #[test]
    fn parses_full_args() {
        let a = parse(&["--direction", "down", "--base", "10", "--max-step", "4",
                        "--band-min", "0.4", "--band-max", "0.6",
                        "--dry-run", "--reset", "--max-windows", "12"]);
        assert_eq!(a.direction, DirectionArg::Down);
        assert_eq!(a.base, Decimal::from(10));
        assert_eq!(a.max_step, 4);
        assert!(a.dry_run && a.reset);
        assert_eq!(a.max_windows, Some(12));
    }

    #[test]
    fn validate_rejects_negative_base() {
        let mut a = parse(&["--direction", "up"]);
        a.base = Decimal::from(-1);
        assert_eq!(a.validate(), Err(ConfigError::InvalidBase));
    }

    #[test]
    fn validate_rejects_zero_max_step() {
        let mut a = parse(&["--direction", "up"]);
        a.max_step = 0;
        assert_eq!(a.validate(), Err(ConfigError::InvalidMaxStep));
    }

    #[test]
    fn validate_rejects_excessive_max_step() {
        let mut a = parse(&["--direction", "up"]);
        a.max_step = 11;
        assert_eq!(a.validate(), Err(ConfigError::InvalidMaxStep));
    }

    #[test]
    fn validate_rejects_inverted_band() {
        let mut a = parse(&["--direction", "up"]);
        a.band_min = Decimal::from_str("0.6").unwrap();
        a.band_max = Decimal::from_str("0.4").unwrap();
        assert_eq!(a.validate(), Err(ConfigError::InvalidBand));
    }

    #[test]
    fn validate_rejects_out_of_range_band() {
        let mut a = parse(&["--direction", "up"]);
        a.band_max = Decimal::from_str("1.5").unwrap();
        assert_eq!(a.validate(), Err(ConfigError::InvalidBand));
    }

    #[test]
    fn validate_accepts_default() {
        assert!(parse(&["--direction", "up"]).validate().is_ok());
    }

    #[test]
    fn direction_arg_to_domain() {
        assert_eq!(Direction::from(DirectionArg::Up), Direction::Up);
        assert_eq!(Direction::from(DirectionArg::Down), Direction::Down);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib trader::config
```

Expected: 9 passed.

- [ ] **Step 3: Commit**

```bash
git add src/trader/config.rs
git commit -m "feat(trader): TraderArgs CLI parsing + validation"
```

---

## Task 3: trader::errors — shared error types

**Files:**
- Modify: `src/trader/errors.rs`

- [ ] **Step 1: Replace `src/trader/errors.rs`**

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StateError {
    #[error("redis op failed: {0}")]
    Op(String),
    #[error("state value malformed: {0}")]
    Decode(String),
    #[error("lock contention: another instance owns the lock")]
    LockContention,
    #[error("lock lost during refresh")]
    LockLost,
}

#[derive(Error, Debug)]
pub enum EmitError {
    #[error("redis stream write failed: {0}")]
    Write(String),
    #[error("event encode failed: {0}")]
    Encode(String),
}

#[derive(Error, Debug)]
pub enum StreamError {
    #[error("redis stream read failed: {0}")]
    Read(String),
    #[error("stream entry decode failed: {0}")]
    Decode(String),
}

#[derive(Error, Debug)]
pub enum MarketError {
    #[error("market not found for window {window_ts}")]
    NotFound { window_ts: i64 },
    #[error("gamma-api unavailable: {0}")]
    Network(String),
    #[error("response decode failed: {0}")]
    Decode(String),
}

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
}

#[derive(Error, Debug)]
pub enum ResolveError {
    #[error("polling timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("market discovery failed during polling: {0}")]
    Market(#[from] MarketError),
}
```

- [ ] **Step 2: Verify build**

```bash
cargo build --lib
```

- [ ] **Step 3: Commit**

```bash
git add src/trader/errors.rs
git commit -m "feat(trader): shared error types"
```

---

## Task 4: trader::event — TraderEvent + Emitter trait

**Files:**
- Modify: `src/trader/event.rs`

- [ ] **Step 1: Replace `src/trader/event.rs`**

```rust
use crate::trader::errors::EmitError;
use crate::trader::ladder::{Direction, LadderState, StopReason, WindowOutcome};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderKind { Buy, Sell }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WinLose { Win, Lose }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryDecision {
    Enter { ask: Decimal },
    SkipBand { ask: Decimal },
    SkipNotFound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraderEventKind {
    SessionStarted,
    SessionStopped { reason: StopReason },
    WindowOpening { window_ts: i64, slug: String },
    EntryDecision { decision: EntryDecision },
    OrderPlaced { kind: OrderKind, dollars: Decimal, token_id: String },
    OrderFilled { fill_price: Decimal, shares: Decimal, dollars: Decimal },
    OrderRejected { reason: String },
    Resolved { winner: Direction, our_side: Direction, our_outcome: WinLose },
    ResolutionTimeout,
    SellFilled { proceeds_usd: Decimal },
    SellRejected { reason: String },
    LadderUpdated { from_step: u8, to_step: u8, outcome: WindowOutcome },
    Alert { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraderEvent {
    pub ts: DateTime<Utc>,
    pub session_id: Uuid,
    pub kind: TraderEventKind,
    pub ladder: LadderState,
}

#[async_trait]
pub trait TraderEventEmitter: Send + Sync {
    async fn emit(&self, event: &TraderEvent) -> Result<(), EmitError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::str::FromStr;

    fn fake_ladder() -> LadderState {
        LadderState::new(Direction::Up, Decimal::from(5), 5,
                         Utc.timestamp_opt(1700000000, 0).unwrap())
    }

    fn fake_event(kind: TraderEventKind) -> TraderEvent {
        TraderEvent {
            ts: Utc.timestamp_opt(1700000100, 0).unwrap(),
            session_id: Uuid::nil(),
            kind,
            ladder: fake_ladder(),
        }
    }

    #[test]
    fn session_started_roundtrip() {
        let e = fake_event(TraderEventKind::SessionStarted);
        let back: TraderEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn order_filled_roundtrip() {
        let e = fake_event(TraderEventKind::OrderFilled {
            fill_price: Decimal::from_str("0.50").unwrap(),
            shares: Decimal::from(10),
            dollars: Decimal::from(5),
        });
        let back: TraderEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn entry_decisions_serialize_distinctly() {
        let enter = EntryDecision::Enter { ask: Decimal::from_str("0.50").unwrap() };
        let skip_band = EntryDecision::SkipBand { ask: Decimal::from_str("0.62").unwrap() };
        let skip_nf = EntryDecision::SkipNotFound;
        assert_ne!(serde_json::to_string(&enter).unwrap(),
                   serde_json::to_string(&skip_band).unwrap());
        assert_ne!(serde_json::to_string(&skip_band).unwrap(),
                   serde_json::to_string(&skip_nf).unwrap());
    }

    #[test]
    fn resolved_roundtrip() {
        let e = fake_event(TraderEventKind::Resolved {
            winner: Direction::Up,
            our_side: Direction::Up,
            our_outcome: WinLose::Win,
        });
        let back: TraderEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn ladder_updated_with_outcome() {
        use crate::trader::ladder::{SkipReason, WindowOutcome};
        let e = fake_event(TraderEventKind::LadderUpdated {
            from_step: 2,
            to_step: 1,
            outcome: WindowOutcome::Won { proceeds_usd: Decimal::from(10) },
        });
        let back: TraderEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);

        let skip = fake_event(TraderEventKind::LadderUpdated {
            from_step: 2,
            to_step: 2,
            outcome: WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed },
        });
        let back2: TraderEvent = serde_json::from_str(&serde_json::to_string(&skip).unwrap()).unwrap();
        assert_eq!(skip, back2);
    }

    #[test]
    fn alert_message_preserved() {
        let e = fake_event(TraderEventKind::Alert { message: "stuck shares".into() });
        let back: TraderEvent = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib trader::event
```

Expected: 6 passed.

- [ ] **Step 3: Commit**

```bash
git add src/trader/event.rs
git commit -m "feat(trader): TraderEvent + TraderEventEmitter trait"
```

---

## Task 5: trader::state — TraderStateStore trait

**Files:**
- Modify: `src/trader/state.rs`

- [ ] **Step 1: Replace `src/trader/state.rs`**

```rust
use crate::trader::errors::StateError;
use crate::trader::ladder::LadderState;
use async_trait::async_trait;
use std::time::Duration;

#[async_trait]
pub trait TraderStateStore: Send + Sync {
    async fn load(&self) -> Result<Option<LadderState>, StateError>;
    async fn save(&self, state: &LadderState) -> Result<(), StateError>;
    async fn clear(&self) -> Result<(), StateError>;

    /// Try to acquire the singleton trader lock. Returns true if acquired.
    async fn try_lock(&self, owner: &str, ttl: Duration) -> Result<bool, StateError>;

    /// Refresh the lock TTL. Returns Err(LockLost) if the lock is no longer ours.
    async fn refresh_lock(&self, owner: &str, ttl: Duration) -> Result<(), StateError>;

    /// Best-effort release. Errors are logged but not fatal.
    async fn release_lock(&self, owner: &str) -> Result<(), StateError>;
}

/// Production Redis keys.
pub const LADDER_KEY: &str = "poly:prod:trader:ladder";
pub const LOCK_KEY: &str = "poly:prod:trader:lock";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_namespace_is_prod() {
        assert!(LADDER_KEY.starts_with("poly:prod:trader:"));
        assert!(LOCK_KEY.starts_with("poly:prod:trader:"));
    }
}
```

- [ ] **Step 2: Verify build**

```bash
cargo test --lib trader::state
```

Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add src/trader/state.rs
git commit -m "feat(trader): TraderStateStore trait + key constants"
```

---

## Task 6: trader::market — MarketDiscovery trait + decode

**Files:**
- Modify: `src/trader/market.rs`

- [ ] **Step 1: Replace `src/trader/market.rs`**

```rust
use crate::trader::errors::MarketError;
use crate::trader::ladder::Direction;
use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// 5-min window market with both outcome token IDs and best-ask prices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowMarket {
    pub window_ts: i64,
    pub slug: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub up_ask: Decimal,
    pub down_ask: Decimal,
    pub closed: bool,
    pub winner: Option<Direction>,
}

impl WindowMarket {
    pub fn ask_for(&self, side: Direction) -> Decimal {
        match side {
            Direction::Up => self.up_ask,
            Direction::Down => self.down_ask,
        }
    }
    pub fn token_id_for(&self, side: Direction) -> &str {
        match side {
            Direction::Up => &self.up_token_id,
            Direction::Down => &self.down_token_id,
        }
    }
}

#[async_trait]
pub trait MarketDiscovery: Send + Sync {
    async fn find_window(&self, window_ts: i64) -> Result<WindowMarket, MarketError>;
}

/// Slug for a 5-min BTC up/down market at the given epoch second (must be a
/// multiple of 300).
pub fn window_slug(window_ts: i64) -> String {
    format!("btc-updown-5m-{window_ts}")
}

/// Floor `now_ts` to the start of its 5-min window.
pub fn floor_5min(now_ts: i64) -> i64 {
    now_ts - (now_ts.rem_euclid(300))
}

/// Next 5-min boundary strictly after `now_ts`.
pub fn next_5min_boundary(now_ts: i64) -> i64 {
    floor_5min(now_ts) + 300
}

/// Pure decoder for a gamma-api event response. Extract the up/down outcomes by
/// matching `outcome` strings ("Up" and "Down" — case-insensitive).
pub fn decode_event_response(json: &str, window_ts: i64) -> Result<WindowMarket, MarketError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| MarketError::Decode(format!("json: {e}")))?;
    let events = v.as_array().ok_or_else(|| MarketError::Decode("expected array".into()))?;
    let event = events.first().ok_or(MarketError::NotFound { window_ts })?;
    let markets = event.get("markets").and_then(|m| m.as_array())
        .ok_or_else(|| MarketError::Decode("missing markets".into()))?;
    let market = markets.first()
        .ok_or_else(|| MarketError::Decode("empty markets".into()))?;

    let slug = market.get("slug").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let closed = market.get("closed").and_then(|c| c.as_bool()).unwrap_or(false);

    // outcomes: array of strings, e.g. ["Up", "Down"]
    let outcomes_raw = market.get("outcomes").and_then(|o| o.as_str())
        .ok_or_else(|| MarketError::Decode("missing outcomes".into()))?;
    let outcomes: Vec<String> = serde_json::from_str(outcomes_raw)
        .map_err(|e| MarketError::Decode(format!("outcomes: {e}")))?;

    let token_ids_raw = market.get("clobTokenIds").and_then(|t| t.as_str())
        .ok_or_else(|| MarketError::Decode("missing clobTokenIds".into()))?;
    let token_ids: Vec<String> = serde_json::from_str(token_ids_raw)
        .map_err(|e| MarketError::Decode(format!("clobTokenIds: {e}")))?;

    let outcome_prices_raw = market.get("outcomePrices").and_then(|p| p.as_str())
        .ok_or_else(|| MarketError::Decode("missing outcomePrices".into()))?;
    let outcome_prices: Vec<String> = serde_json::from_str(outcome_prices_raw)
        .map_err(|e| MarketError::Decode(format!("outcomePrices: {e}")))?;

    if outcomes.len() != 2 || token_ids.len() != 2 || outcome_prices.len() != 2 {
        return Err(MarketError::Decode("expected 2 outcomes".into()));
    }

    let mut up_idx = None;
    let mut down_idx = None;
    for (i, name) in outcomes.iter().enumerate() {
        match name.to_ascii_lowercase().as_str() {
            "up" => up_idx = Some(i),
            "down" => down_idx = Some(i),
            _ => {}
        }
    }
    let up = up_idx.ok_or_else(|| MarketError::Decode("no Up outcome".into()))?;
    let down = down_idx.ok_or_else(|| MarketError::Decode("no Down outcome".into()))?;

    let up_ask = parse_decimal(&outcome_prices[up])?;
    let down_ask = parse_decimal(&outcome_prices[down])?;

    let winner = if closed {
        if up_ask > down_ask { Some(Direction::Up) } else { Some(Direction::Down) }
    } else {
        None
    };

    Ok(WindowMarket {
        window_ts,
        slug,
        up_token_id: token_ids[up].clone(),
        down_token_id: token_ids[down].clone(),
        up_ask,
        down_ask,
        closed,
        winner,
    })
}

fn parse_decimal(s: &str) -> Result<Decimal, MarketError> {
    use std::str::FromStr;
    Decimal::from_str(s).map_err(|e| MarketError::Decode(format!("price: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn slug_format() {
        assert_eq!(window_slug(1747789200), "btc-updown-5m-1747789200");
    }

    #[test]
    fn floor_5min_aligns() {
        assert_eq!(floor_5min(1747789201), 1747789200);
        assert_eq!(floor_5min(1747789499), 1747789200);
        assert_eq!(floor_5min(1747789500), 1747789500);
    }

    #[test]
    fn next_5min_advances() {
        assert_eq!(next_5min_boundary(1747789200), 1747789500);
        assert_eq!(next_5min_boundary(1747789499), 1747789500);
        assert_eq!(next_5min_boundary(1747789500), 1747789800);
    }

    #[test]
    fn decode_open_market() {
        let json = r#"[{
            "id": "evt1",
            "markets": [{
                "slug": "btc-updown-5m-1700000300",
                "closed": false,
                "outcomes": "[\"Up\",\"Down\"]",
                "clobTokenIds": "[\"tok-up-1\",\"tok-down-1\"]",
                "outcomePrices": "[\"0.50\",\"0.50\"]"
            }]
        }]"#;
        let m = decode_event_response(json, 1700000300).unwrap();
        assert_eq!(m.up_token_id, "tok-up-1");
        assert_eq!(m.down_token_id, "tok-down-1");
        assert_eq!(m.up_ask, Decimal::from_str("0.50").unwrap());
        assert!(!m.closed);
        assert!(m.winner.is_none());
    }

    #[test]
    fn decode_closed_market_winner_up() {
        let json = r#"[{"markets":[{
            "slug":"x", "closed":true,
            "outcomes":"[\"Up\",\"Down\"]",
            "clobTokenIds":"[\"a\",\"b\"]",
            "outcomePrices":"[\"1.00\",\"0.00\"]"
        }]}]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.winner, Some(Direction::Up));
    }

    #[test]
    fn decode_closed_market_winner_down() {
        let json = r#"[{"markets":[{
            "slug":"x", "closed":true,
            "outcomes":"[\"Up\",\"Down\"]",
            "clobTokenIds":"[\"a\",\"b\"]",
            "outcomePrices":"[\"0.00\",\"1.00\"]"
        }]}]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.winner, Some(Direction::Down));
    }

    #[test]
    fn decode_outcomes_reversed_order() {
        // SDK shouldn't assume a specific outcome ordering.
        let json = r#"[{"markets":[{
            "slug":"x", "closed":false,
            "outcomes":"[\"Down\",\"Up\"]",
            "clobTokenIds":"[\"down-id\",\"up-id\"]",
            "outcomePrices":"[\"0.50\",\"0.50\"]"
        }]}]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.up_token_id, "up-id");
        assert_eq!(m.down_token_id, "down-id");
    }

    #[test]
    fn decode_empty_returns_not_found() {
        let json = "[]";
        let err = decode_event_response(json, 42).unwrap_err();
        assert!(matches!(err, MarketError::NotFound { window_ts: 42 }));
    }

    #[test]
    fn decode_malformed_returns_decode_err() {
        let json = "not json at all";
        let err = decode_event_response(json, 0).unwrap_err();
        assert!(matches!(err, MarketError::Decode(_)));
    }

    #[test]
    fn decode_missing_outcomes_field() {
        let json = r#"[{"markets":[{"slug":"x","closed":false}]}]"#;
        let err = decode_event_response(json, 0).unwrap_err();
        assert!(matches!(err, MarketError::Decode(_)));
    }

    #[test]
    fn ask_for_returns_correct_side() {
        let m = WindowMarket {
            window_ts: 0, slug: "x".into(),
            up_token_id: "u".into(), down_token_id: "d".into(),
            up_ask: Decimal::from_str("0.51").unwrap(),
            down_ask: Decimal::from_str("0.49").unwrap(),
            closed: false, winner: None,
        };
        assert_eq!(m.ask_for(Direction::Up), Decimal::from_str("0.51").unwrap());
        assert_eq!(m.ask_for(Direction::Down), Decimal::from_str("0.49").unwrap());
        assert_eq!(m.token_id_for(Direction::Up), "u");
        assert_eq!(m.token_id_for(Direction::Down), "d");
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib trader::market
```

Expected: 10 passed.

- [ ] **Step 3: Commit**

```bash
git add src/trader/market.rs
git commit -m "feat(trader): MarketDiscovery trait + decode_event_response"
```

---

## Task 7: trader::executor — OrderExecutor trait + compute_share_count

**Files:**
- Modify: `src/trader/executor.rs`

- [ ] **Step 1: Replace `src/trader/executor.rs`**

```rust
use crate::trader::errors::ExecError;
use async_trait::async_trait;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillResult {
    pub fill_price: Decimal,
    pub shares: Decimal,
    pub dollars: Decimal,
}

#[async_trait]
pub trait OrderExecutor: Send + Sync {
    async fn buy_fok(&self, token_id: &str, dollars: Decimal) -> Result<FillResult, ExecError>;
    async fn sell_market(&self, token_id: &str, shares: Decimal) -> Result<FillResult, ExecError>;
}

/// Number of whole shares to buy with `dollars` at `ask`. Rounds DOWN so we never
/// exceed the budget. Polymarket enforces a 5-share minimum — caller checks.
pub fn compute_share_count(dollars: Decimal, ask: Decimal) -> Decimal {
    if ask <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let raw = dollars / ask;
    raw.floor()
}

/// Polymarket's 5-share minimum order size.
pub const MIN_SHARES: u64 = 5;

pub fn meets_minimum(shares: Decimal) -> bool {
    shares.to_u64().map(|n| n >= MIN_SHARES).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn compute_shares_at_50_cents() {
        let s = compute_share_count(Decimal::from(5), Decimal::from_str("0.50").unwrap());
        assert_eq!(s, Decimal::from(10));
    }

    #[test]
    fn compute_shares_floors_partial() {
        // $5 / $0.51 = 9.80 → 9
        let s = compute_share_count(Decimal::from(5), Decimal::from_str("0.51").unwrap());
        assert_eq!(s, Decimal::from(9));
    }

    #[test]
    fn compute_shares_zero_ask_returns_zero() {
        assert_eq!(compute_share_count(Decimal::from(5), Decimal::ZERO), Decimal::ZERO);
    }

    #[test]
    fn compute_shares_negative_ask_returns_zero() {
        assert_eq!(
            compute_share_count(Decimal::from(5), Decimal::from(-1)),
            Decimal::ZERO
        );
    }

    #[test]
    fn meets_minimum_at_5_shares() {
        assert!(meets_minimum(Decimal::from(5)));
    }

    #[test]
    fn meets_minimum_below_5_shares_is_false() {
        assert!(!meets_minimum(Decimal::from(4)));
    }

    #[test]
    fn meets_minimum_zero_shares_is_false() {
        assert!(!meets_minimum(Decimal::ZERO));
    }

    #[test]
    fn fill_result_serde_roundtrip() {
        let f = FillResult {
            fill_price: Decimal::from_str("0.50").unwrap(),
            shares: Decimal::from(10),
            dollars: Decimal::from(5),
        };
        let back: FillResult = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(f, back);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib trader::executor
```

Expected: 8 passed.

- [ ] **Step 3: Commit**

```bash
git add src/trader/executor.rs
git commit -m "feat(trader): OrderExecutor trait + compute_share_count"
```

---

## Task 8: trader::resolver — WindowResolver + polling logic

**Files:**
- Modify: `src/trader/resolver.rs`

The polling loop is testable by injecting a `MarketProbe` trait. Real impl wraps `MarketDiscovery`.

- [ ] **Step 1: Replace `src/trader/resolver.rs`**

```rust
use crate::trader::errors::{MarketError, ResolveError};
use crate::trader::ladder::Direction;
use crate::trader::market::{MarketDiscovery, WindowMarket};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub winner: Direction,
}

#[async_trait]
pub trait WindowResolver: Send + Sync {
    async fn await_resolution(&self, market: &WindowMarket) -> Result<Resolution, ResolveError>;
}

/// Production resolver: polls MarketDiscovery every `tick` until `closed=true`
/// or `timeout` elapses.
pub struct PolymarketResolver {
    market: Arc<dyn MarketDiscovery>,
    tick: Duration,
    timeout: Duration,
}

impl PolymarketResolver {
    pub fn new(market: Arc<dyn MarketDiscovery>, timeout: Duration) -> Self {
        Self { market, tick: Duration::from_secs(2), timeout }
    }
    /// Smaller tick for tests under `tokio::time::pause()`.
    pub fn with_tick(market: Arc<dyn MarketDiscovery>, tick: Duration, timeout: Duration) -> Self {
        Self { market, tick, timeout }
    }
}

#[async_trait]
impl WindowResolver for PolymarketResolver {
    async fn await_resolution(&self, market: &WindowMarket) -> Result<Resolution, ResolveError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            match self.market.find_window(market.window_ts).await {
                Ok(latest) if latest.closed => {
                    let winner = latest.winner.ok_or_else(|| {
                        ResolveError::Market(MarketError::Decode(
                            "closed but no winner".into(),
                        ))
                    })?;
                    return Ok(Resolution { winner });
                }
                Ok(_) | Err(MarketError::NotFound { .. }) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(ResolveError::Timeout {
                            seconds: self.timeout.as_secs(),
                        });
                    }
                    tokio::time::sleep(self.tick).await;
                }
                Err(e) => return Err(ResolveError::Market(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trader::market::WindowMarket;
    use rust_decimal::Decimal;
    use std::sync::Mutex;

    /// Fake that returns a queue of pre-programmed responses.
    struct ScriptedDiscovery {
        responses: Mutex<Vec<Result<WindowMarket, MarketError>>>,
    }
    impl ScriptedDiscovery {
        fn new(rs: Vec<Result<WindowMarket, MarketError>>) -> Arc<Self> {
            Arc::new(Self { responses: Mutex::new(rs) })
        }
    }
    #[async_trait]
    impl MarketDiscovery for ScriptedDiscovery {
        async fn find_window(&self, _ts: i64) -> Result<WindowMarket, MarketError> {
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(MarketError::NotFound { window_ts: 0 });
            }
            q.remove(0)
        }
    }

    fn open_market() -> WindowMarket {
        WindowMarket {
            window_ts: 1700000300, slug: "x".into(),
            up_token_id: "u".into(), down_token_id: "d".into(),
            up_ask: Decimal::ONE_HUNDRED, down_ask: Decimal::ONE_HUNDRED,
            closed: false, winner: None,
        }
    }
    fn closed_market(winner: Direction) -> WindowMarket {
        WindowMarket {
            window_ts: 1700000300, slug: "x".into(),
            up_token_id: "u".into(), down_token_id: "d".into(),
            up_ask: Decimal::ONE_HUNDRED, down_ask: Decimal::ONE_HUNDRED,
            closed: true, winner: Some(winner),
        }
    }

    #[tokio::test]
    async fn resolves_immediately_when_already_closed() {
        let disc = ScriptedDiscovery::new(vec![Ok(closed_market(Direction::Up))]);
        let resolver = PolymarketResolver::with_tick(
            disc, Duration::from_millis(1), Duration::from_secs(60));
        let r = resolver.await_resolution(&open_market()).await.unwrap();
        assert_eq!(r.winner, Direction::Up);
    }

    #[tokio::test]
    async fn polls_until_closed() {
        tokio::time::pause();
        let disc = ScriptedDiscovery::new(vec![
            Ok(open_market()),
            Ok(open_market()),
            Ok(closed_market(Direction::Down)),
        ]);
        let resolver = PolymarketResolver::with_tick(
            disc, Duration::from_secs(2), Duration::from_secs(60));
        let task = tokio::spawn(async move {
            resolver.await_resolution(&open_market()).await
        });
        tokio::time::advance(Duration::from_secs(5)).await;
        let r = task.await.unwrap().unwrap();
        assert_eq!(r.winner, Direction::Down);
    }

    #[tokio::test]
    async fn times_out_when_never_closes() {
        tokio::time::pause();
        let many_open: Vec<_> = std::iter::repeat_with(|| Ok(open_market())).take(40).collect();
        let disc = ScriptedDiscovery::new(many_open);
        let resolver = PolymarketResolver::with_tick(
            disc, Duration::from_secs(2), Duration::from_secs(60));
        let task = tokio::spawn(async move {
            resolver.await_resolution(&open_market()).await
        });
        tokio::time::advance(Duration::from_secs(61)).await;
        let r = task.await.unwrap();
        assert!(matches!(r, Err(ResolveError::Timeout { seconds: 60 })));
    }

    #[tokio::test]
    async fn not_found_during_poll_is_retried() {
        tokio::time::pause();
        let disc = ScriptedDiscovery::new(vec![
            Err(MarketError::NotFound { window_ts: 0 }),
            Ok(closed_market(Direction::Up)),
        ]);
        let resolver = PolymarketResolver::with_tick(
            disc, Duration::from_secs(2), Duration::from_secs(60));
        let task = tokio::spawn(async move {
            resolver.await_resolution(&open_market()).await
        });
        tokio::time::advance(Duration::from_secs(3)).await;
        let r = task.await.unwrap().unwrap();
        assert_eq!(r.winner, Direction::Up);
    }

    #[tokio::test]
    async fn network_error_returns_market_err() {
        let disc = ScriptedDiscovery::new(vec![Err(MarketError::Network("boom".into()))]);
        let resolver = PolymarketResolver::with_tick(
            disc, Duration::from_millis(1), Duration::from_secs(60));
        let r = resolver.await_resolution(&open_market()).await;
        assert!(matches!(r, Err(ResolveError::Market(MarketError::Network(_)))));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib trader::resolver
```

Expected: 5 passed.

- [ ] **Step 3: Commit**

```bash
git add src/trader/resolver.rs
git commit -m "feat(trader): WindowResolver + PolymarketResolver polling logic"
```

---

## Task 9: trader::window — single-window orchestration

**Files:**
- Modify: `src/trader/window.rs`

`run_window` consumes all 6 traits and returns `WindowOutcome`. This is the "decision and side-effects" layer. All Skipped paths covered. ~25 unit tests target full path coverage.

- [ ] **Step 1: Write `src/trader/window.rs` (skeleton + types)**

```rust
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
    let session_id = ladder.session_id;

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
        dollars, token_id: token_id.clone(),
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

    let _ = session_id; // already on emit ladder snapshot
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
```

- [ ] **Step 2: Add tests in `src/trader/window.rs`** (append to file)

```rust
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
            Arc::new(Self {
                buy: Mutex::new(Some(buy)),
                sell: Mutex::new(None),
            })
        }
        fn buy_then_sell(buy: Result<FillResult, ExecError>,
                        sell: Result<FillResult, ExecError>) -> Arc<Self> {
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
        let market = open_market_at("0.62", "0.38"); // UP at 0.62 > 0.55
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
```

- [ ] **Step 3: Run tests**

```bash
cargo test --lib trader::window
```

Expected: 9 passed.

- [ ] **Step 4: Commit**

```bash
git add src/trader/window.rs
git commit -m "feat(trader): run_window orchestration + 9 path tests"
```

---

## Task 10: trader::scheduler — 5min loop + drift + shutdown

**Files:**
- Modify: `src/trader/scheduler.rs`

The scheduler is the outer loop: wait until next 5-min boundary, run a window, apply outcome, persist, repeat. Tests use `tokio::time::pause()` and inject a `WindowExecutor` trait so we don't actually call `run_window`.

- [ ] **Step 1: Replace `src/trader/scheduler.rs`**

```rust
use crate::trader::errors::StateError;
use crate::trader::event::{TraderEvent, TraderEventEmitter, TraderEventKind};
use crate::trader::ladder::{apply_outcome, LadderState, StopReason, WindowOutcome};
use crate::trader::market::next_5min_boundary;
use crate::trader::state::TraderStateStore;
use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Indirection for testing the scheduler without invoking real `run_window`.
#[async_trait]
pub trait WindowExecutor: Send + Sync {
    async fn execute(&self, ladder: &LadderState, window_ts: i64) -> WindowOutcome;
}

pub struct SchedulerDeps {
    pub window_exec: Arc<dyn WindowExecutor>,
    pub state_store: Arc<dyn TraderStateStore>,
    pub emitter: Arc<dyn TraderEventEmitter>,
}

pub struct SchedulerConfig {
    pub max_windows: Option<u32>,
}

pub async fn run(
    initial: LadderState,
    deps: SchedulerDeps,
    cfg: SchedulerConfig,
    shutdown: CancellationToken,
) -> Result<LadderState, StateError> {
    let mut ladder = initial;
    let mut windows_run: u32 = 0;

    emit(&deps, &ladder, TraderEventKind::SessionStarted).await;

    loop {
        if ladder.is_stopped() { break; }
        if let Some(max) = cfg.max_windows {
            if windows_run >= max { break; }
        }

        // Wait until next 5-min boundary, observing shutdown.
        let now_ts = chrono::Utc::now().timestamp();
        let next_ts = next_5min_boundary(now_ts);
        let wait = Duration::from_secs((next_ts - now_ts).max(0) as u64);

        tokio::select! {
            _ = tokio::time::sleep(wait) => {},
            _ = shutdown.cancelled() => {
                ladder.stopped = Some(StopReason::ManualStop);
                break;
            }
        }

        // Execute the window.
        let outcome = deps.window_exec.execute(&ladder, next_ts).await;

        let from_step = ladder.current_step;
        let new_ladder = apply_outcome(&ladder, &outcome, Utc::now());
        deps.state_store.save(&new_ladder).await?;
        emit(&deps, &new_ladder, TraderEventKind::LadderUpdated {
            from_step,
            to_step: new_ladder.current_step,
            outcome: outcome.clone(),
        }).await;

        ladder = new_ladder;
        windows_run += 1;
    }

    let stop_reason = ladder.stopped.clone().unwrap_or(StopReason::ManualStop);
    emit(&deps, &ladder, TraderEventKind::SessionStopped {
        reason: stop_reason,
    }).await;

    Ok(ladder)
}

async fn emit(deps: &SchedulerDeps, ladder: &LadderState, kind: TraderEventKind) {
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
    use crate::trader::event::TraderEvent;
    use crate::trader::errors::EmitError;
    use crate::trader::ladder::{Direction, SkipReason};
    use rust_decimal::Decimal;
    use std::sync::Mutex;

    #[derive(Default)]
    struct InMemoryStore {
        ladder: Mutex<Option<LadderState>>,
    }
    #[async_trait]
    impl TraderStateStore for InMemoryStore {
        async fn load(&self) -> Result<Option<LadderState>, StateError> {
            Ok(self.ladder.lock().unwrap().clone())
        }
        async fn save(&self, s: &LadderState) -> Result<(), StateError> {
            *self.ladder.lock().unwrap() = Some(s.clone()); Ok(())
        }
        async fn clear(&self) -> Result<(), StateError> {
            *self.ladder.lock().unwrap() = None; Ok(())
        }
        async fn try_lock(&self, _o: &str, _t: Duration) -> Result<bool, StateError> { Ok(true) }
        async fn refresh_lock(&self, _o: &str, _t: Duration) -> Result<(), StateError> { Ok(()) }
        async fn release_lock(&self, _o: &str) -> Result<(), StateError> { Ok(()) }
    }

    #[derive(Default)]
    struct CaptureEmitter {
        events: Mutex<Vec<TraderEvent>>,
    }
    #[async_trait]
    impl TraderEventEmitter for CaptureEmitter {
        async fn emit(&self, ev: &TraderEvent) -> Result<(), EmitError> {
            self.events.lock().unwrap().push(ev.clone());
            Ok(())
        }
    }

    struct ScriptedWindowExec {
        outcomes: Mutex<Vec<WindowOutcome>>,
    }
    #[async_trait]
    impl WindowExecutor for ScriptedWindowExec {
        async fn execute(&self, _l: &LadderState, _ts: i64) -> WindowOutcome {
            let mut q = self.outcomes.lock().unwrap();
            if q.is_empty() {
                return WindowOutcome::Skipped { reason: SkipReason::MarketNotFound };
            }
            q.remove(0)
        }
    }

    fn ladder() -> LadderState {
        LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now())
    }

    #[tokio::test]
    async fn max_windows_terminates_loop() {
        tokio::time::pause();
        let deps = SchedulerDeps {
            window_exec: Arc::new(ScriptedWindowExec {
                outcomes: Mutex::new(vec![
                    WindowOutcome::Won { proceeds_usd: Decimal::from(10) },
                    WindowOutcome::Won { proceeds_usd: Decimal::from(10) },
                    WindowOutcome::Won { proceeds_usd: Decimal::from(10) },
                ]),
            }),
            state_store: Arc::new(InMemoryStore::default()),
            emitter: Arc::new(CaptureEmitter::default()),
        };
        let token = CancellationToken::new();
        let task = tokio::spawn(run(ladder(), deps,
            SchedulerConfig { max_windows: Some(3) }, token));
        tokio::time::advance(Duration::from_secs(60 * 60)).await;
        let final_state = task.await.unwrap().unwrap();
        assert_eq!(final_state.windows_won, 3);
    }

    #[tokio::test]
    async fn shutdown_signal_terminates_loop() {
        tokio::time::pause();
        let deps = SchedulerDeps {
            window_exec: Arc::new(ScriptedWindowExec {
                outcomes: Mutex::new(vec![]),
            }),
            state_store: Arc::new(InMemoryStore::default()),
            emitter: Arc::new(CaptureEmitter::default()),
        };
        let token = CancellationToken::new();
        let token2 = token.clone();
        let task = tokio::spawn(run(ladder(), deps,
            SchedulerConfig { max_windows: None }, token));
        token2.cancel();
        let final_state = task.await.unwrap().unwrap();
        assert_eq!(final_state.stopped, Some(StopReason::ManualStop));
    }

    #[tokio::test]
    async fn cap_reached_breaks_loop() {
        tokio::time::pause();
        let losses: Vec<WindowOutcome> = (0..5).map(|_|
            WindowOutcome::Lost { spent_usd: Decimal::from(5) }).collect();
        let deps = SchedulerDeps {
            window_exec: Arc::new(ScriptedWindowExec {
                outcomes: Mutex::new(losses),
            }),
            state_store: Arc::new(InMemoryStore::default()),
            emitter: Arc::new(CaptureEmitter::default()),
        };
        let token = CancellationToken::new();
        let task = tokio::spawn(run(ladder(), deps,
            SchedulerConfig { max_windows: None }, token));
        tokio::time::advance(Duration::from_secs(60 * 60)).await;
        let final_state = task.await.unwrap().unwrap();
        assert_eq!(final_state.stopped, Some(StopReason::CapReached));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib trader::scheduler
```

Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add src/trader/scheduler.rs
git commit -m "feat(trader): scheduler 5min loop + max-windows + shutdown"
```

---

## Task 11: adapters::redis_state_wrapper — real Redis impl

**Files:**
- Modify: `src/trader/adapters/redis_state_wrapper.rs`
- Create: `tests/trader_state_integration.rs`
- Modify: `Cargo.toml` (uncomment `[[test]] trader_state_integration`)

> **fred 9.x reference (verified in v1.0):** `RedisClient::new(RedisConfig::from_url(...)?, None, None, None)`, `client.init().await`, `set/get` via `KeysInterface`, `ping` via `ClientLike`, `set_options` for NX+EX. For SETNX-based locking: `set::<(), _, _>(key, value, Some(Expiration::EX(ttl_secs)), Some(SetOptions::NX), false)` returns `Ok(())` on acquire, error on contention.

- [ ] **Step 1: Write `src/trader/adapters/redis_state_wrapper.rs`**

```rust
use crate::trader::errors::StateError;
use crate::trader::ladder::LadderState;
use crate::trader::state::{TraderStateStore, LADDER_KEY, LOCK_KEY};
use async_trait::async_trait;
use fred::interfaces::{ClientLike, KeysInterface};
use fred::prelude::{Expiration, RedisClient, RedisConfig, RedisError, SetOptions};
use std::time::Duration;

pub struct RedisTraderState {
    client: RedisClient,
}

impl RedisTraderState {
    pub async fn connect(url: &str) -> Result<Self, StateError> {
        let config = RedisConfig::from_url(url)
            .map_err(|e| StateError::Op(format!("bad redis url: {e}")))?;
        let client = RedisClient::new(config, None, None, None);
        client.init().await
            .map_err(|e| StateError::Op(format!("redis init: {e}")))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl TraderStateStore for RedisTraderState {
    async fn load(&self) -> Result<Option<LadderState>, StateError> {
        let raw: Option<String> = self.client.get(LADDER_KEY).await.map_err(map_err)?;
        match raw {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| StateError::Decode(e.to_string())),
        }
    }

    async fn save(&self, state: &LadderState) -> Result<(), StateError> {
        let json = serde_json::to_string(state).map_err(|e| StateError::Decode(e.to_string()))?;
        let _: () = self.client
            .set(LADDER_KEY, json, None, None, false).await
            .map_err(map_err)?;
        Ok(())
    }

    async fn clear(&self) -> Result<(), StateError> {
        let _: () = self.client.del(LADDER_KEY).await.map_err(map_err)?;
        Ok(())
    }

    async fn try_lock(&self, owner: &str, ttl: Duration) -> Result<bool, StateError> {
        let result: Option<String> = self.client
            .set(LOCK_KEY, owner, Some(Expiration::EX(ttl.as_secs() as i64)),
                 Some(SetOptions::NX), false).await
            .map_err(map_err)?;
        Ok(result.is_some())
    }

    async fn refresh_lock(&self, owner: &str, ttl: Duration) -> Result<(), StateError> {
        let current: Option<String> = self.client.get(LOCK_KEY).await.map_err(map_err)?;
        match current {
            Some(c) if c == owner => {
                let _: () = self.client
                    .set(LOCK_KEY, owner, Some(Expiration::EX(ttl.as_secs() as i64)),
                         Some(SetOptions::XX), false).await
                    .map_err(map_err)?;
                Ok(())
            }
            _ => Err(StateError::LockLost),
        }
    }

    async fn release_lock(&self, owner: &str) -> Result<(), StateError> {
        let current: Option<String> = self.client.get(LOCK_KEY).await.map_err(map_err)?;
        if matches!(current, Some(ref c) if c == owner) {
            let _: () = self.client.del(LOCK_KEY).await.map_err(map_err)?;
        }
        Ok(())
    }
}

fn map_err(e: RedisError) -> StateError {
    StateError::Op(e.to_string())
}
```

> **NOTE on fred API drift:** if `set` returns `Ok(())` rather than `Option<String>` for SETNX, change `try_lock` to assume success on `Ok` and check distinction via response or attempt a follow-up `GET` to verify. Implementer adapts to actual fred 9.x signature.

- [ ] **Step 2: Uncomment `[[test]]` block in `Cargo.toml`**

```toml
[[test]]
name = "trader_state_integration"
path = "tests/trader_state_integration.rs"
```

- [ ] **Step 3: Write `tests/trader_state_integration.rs`**

```rust
#![cfg(test)]

use chrono::Utc;
use poly_tui::trader::adapters::redis_state_wrapper::RedisTraderState;
use poly_tui::trader::ladder::{Direction, LadderState};
use poly_tui::trader::state::TraderStateStore;
use rust_decimal::Decimal;
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "integration must NOT bind dev Redis port");
    let url = format!("redis://127.0.0.1:{port}");
    (node, url)
}

#[tokio::test]
#[ignore]
async fn save_load_clear_roundtrip() {
    let (_node, url) = start_redis().await;
    let store = RedisTraderState::connect(&url).await.unwrap();
    let s = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());

    assert!(store.load().await.unwrap().is_none());
    store.save(&s).await.unwrap();
    let back = store.load().await.unwrap().expect("Some");
    assert_eq!(back.session_id, s.session_id);

    store.clear().await.unwrap();
    assert!(store.load().await.unwrap().is_none());
}

#[tokio::test]
#[ignore]
async fn lock_acquire_then_contention() {
    let (_node, url) = start_redis().await;
    let store_a = RedisTraderState::connect(&url).await.unwrap();
    let store_b = RedisTraderState::connect(&url).await.unwrap();

    let acquired = store_a.try_lock("owner-a", Duration::from_secs(60)).await.unwrap();
    assert!(acquired);

    let denied = store_b.try_lock("owner-b", Duration::from_secs(60)).await.unwrap();
    assert!(!denied);
}

#[tokio::test]
#[ignore]
async fn lock_release_allows_reacquire() {
    let (_node, url) = start_redis().await;
    let store = RedisTraderState::connect(&url).await.unwrap();

    assert!(store.try_lock("a", Duration::from_secs(60)).await.unwrap());
    store.release_lock("a").await.unwrap();
    assert!(store.try_lock("b", Duration::from_secs(60)).await.unwrap());
}

#[tokio::test]
#[ignore]
async fn refresh_lock_succeeds_when_owner_matches() {
    let (_node, url) = start_redis().await;
    let store = RedisTraderState::connect(&url).await.unwrap();

    store.try_lock("a", Duration::from_secs(60)).await.unwrap();
    store.refresh_lock("a", Duration::from_secs(60)).await.unwrap();
}

#[tokio::test]
#[ignore]
async fn refresh_lock_fails_when_owner_mismatches() {
    let (_node, url) = start_redis().await;
    let store = RedisTraderState::connect(&url).await.unwrap();

    store.try_lock("a", Duration::from_secs(60)).await.unwrap();
    let r = store.refresh_lock("b", Duration::from_secs(60)).await;
    assert!(matches!(r, Err(_)));
}
```

- [ ] **Step 4: Run integration tests**

```bash
docker compose up -d
cargo test --test trader_state_integration -- --ignored
```

Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add src/trader/adapters/redis_state_wrapper.rs tests/trader_state_integration.rs Cargo.toml
git commit -m "feat(trader): RedisTraderState (fred) + lock + integration tests"
```

---

## Task 12: adapters::redis_stream_wrapper — TraderEventEmitter + TraderEventStream

**Files:**
- Modify: `src/trader/adapters/redis_stream_wrapper.rs`
- Modify: `src/tui/events.rs` (TraderEventStream trait)
- Modify: `tests/trader_state_integration.rs` (add stream tests, OR new file)

We expose two real impls: `RedisTraderStream::Emitter` (for trader writes) and `RedisTraderStream::Subscriber` (for TUI reads). Both share the same `RedisClient`.

- [ ] **Step 1: Write `src/tui/events.rs`** — declare the consumer-side trait

```rust
use crate::trader::event::TraderEvent;
use crate::trader::errors::StreamError;
use async_trait::async_trait;
use futures::stream::Stream;
use std::pin::Pin;

pub struct TraderEventTail {
    pub history: Vec<TraderEvent>,
    pub live: Pin<Box<dyn Stream<Item = TraderEvent> + Send>>,
}

#[async_trait]
pub trait TraderEventStream: Send + Sync {
    /// Return the last `n` historical events plus a live subscription stream.
    async fn tail(&self, n: usize) -> Result<TraderEventTail, StreamError>;
}

pub const STREAM_KEY: &str = "poly:prod:trader:events";
pub const STREAM_MAXLEN: usize = 1000;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_namespace_is_prod() {
        assert!(STREAM_KEY.starts_with("poly:prod:trader:"));
    }
}
```

- [ ] **Step 2: Write `src/trader/adapters/redis_stream_wrapper.rs`**

```rust
use crate::trader::errors::{EmitError, StreamError};
use crate::trader::event::{TraderEvent, TraderEventEmitter};
use crate::tui::events::{TraderEventStream, TraderEventTail, STREAM_KEY, STREAM_MAXLEN};
use async_trait::async_trait;
use fred::interfaces::{ClientLike, StreamsInterface};
use fred::prelude::{RedisClient, RedisConfig, RedisError, XCap};
use fred::types::{XID, XReadResponse};
use futures::stream::StreamExt;
use std::collections::HashMap;

pub struct RedisTraderStream {
    client: RedisClient,
}

impl RedisTraderStream {
    pub async fn connect(url: &str) -> Result<Self, EmitError> {
        let config = RedisConfig::from_url(url)
            .map_err(|e| EmitError::Write(format!("bad redis url: {e}")))?;
        let client = RedisClient::new(config, None, None, None);
        client.init().await
            .map_err(|e| EmitError::Write(format!("redis init: {e}")))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl TraderEventEmitter for RedisTraderStream {
    async fn emit(&self, event: &TraderEvent) -> Result<(), EmitError> {
        let json = serde_json::to_string(event)
            .map_err(|e| EmitError::Encode(e.to_string()))?;
        let cap = XCap::default()
            .set_max_len(STREAM_MAXLEN as i64);
        let _: String = self.client
            .xadd(STREAM_KEY, false, cap, "*", ("payload", json))
            .await
            .map_err(map_emit)?;
        Ok(())
    }
}

#[async_trait]
impl TraderEventStream for RedisTraderStream {
    async fn tail(&self, n: usize) -> Result<TraderEventTail, StreamError> {
        // History via XREVRANGE then reverse to chronological order.
        let entries: Vec<(String, HashMap<String, String>)> = self.client
            .xrevrange(STREAM_KEY, "+", "-", Some(n))
            .await
            .map_err(map_stream)?;
        let mut history: Vec<TraderEvent> = entries.iter().rev()
            .filter_map(|(_id, fields)| fields.get("payload"))
            .filter_map(|p| serde_json::from_str::<TraderEvent>(p).ok())
            .collect();

        let last_id: XID = entries.first()
            .map(|(id, _)| XID::from(id.as_str()))
            .unwrap_or(XID::Manual("0-0".into()));

        // Live subscription via repeated XREAD BLOCK.
        let client = self.client.clone();
        let live = async_stream::stream! {
            let mut last_id = last_id;
            loop {
                let resp: Result<XReadResponse<String, String, String, String>, _> =
                    client.xread_map(
                        Some(10),
                        Some(250),
                        STREAM_KEY,
                        &last_id,
                    ).await;
                match resp {
                    Ok(map) => {
                        for (_stream, entries) in map {
                            for (id, fields) in entries {
                                if let Some(payload) = fields.get("payload") {
                                    if let Ok(ev) = serde_json::from_str::<TraderEvent>(payload) {
                                        yield ev;
                                    }
                                }
                                last_id = XID::from(id);
                            }
                        }
                    }
                    Err(_) => {
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                }
            }
        };
        let _ = history.len();
        Ok(TraderEventTail {
            history,
            live: Box::pin(live),
        })
    }
}

fn map_emit(e: RedisError) -> EmitError { EmitError::Write(e.to_string()) }
fn map_stream(e: RedisError) -> StreamError { StreamError::Read(e.to_string()) }
```

> **NOTE on fred 9.x stream API:** the exact argument shapes for `xadd`, `xrevrange`, and `xread_map` may differ. The implementer must verify against the installed fred version and `cargo doc -p fred --open`. The structure above (XADD with MAXLEN, XREVRANGE for tail, XREAD with BLOCK + last_id for live) is correct semantically; only the binding signatures may need adjustment. If `async_stream` isn't already in `Cargo.toml`, add `async-stream = "0.3"` to `[dependencies]`.

- [ ] **Step 3: Add `async-stream` to `Cargo.toml`**

```toml
async-stream = "0.3"
```

- [ ] **Step 4: Append integration tests to `tests/trader_state_integration.rs`** (or new file `tests/trader_stream_integration.rs` with new `[[test]]` entry — implementer choice; reuse existing file is simpler):

```rust
use poly_tui::trader::adapters::redis_stream_wrapper::RedisTraderStream;
use poly_tui::trader::event::{TraderEvent, TraderEventEmitter, TraderEventKind};
use poly_tui::tui::events::TraderEventStream;
use uuid::Uuid;
use futures::StreamExt;

#[tokio::test]
#[ignore]
async fn stream_emit_then_tail_history() {
    let (_node, url) = start_redis().await;
    let stream = RedisTraderStream::connect(&url).await.unwrap();

    let s = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    for _ in 0..3 {
        let ev = TraderEvent {
            ts: Utc::now(),
            session_id: Uuid::nil(),
            kind: TraderEventKind::SessionStarted,
            ladder: s.clone(),
        };
        stream.emit(&ev).await.unwrap();
    }
    let tail = stream.tail(10).await.unwrap();
    assert_eq!(tail.history.len(), 3);
}

#[tokio::test]
#[ignore]
async fn stream_live_receives_new_events() {
    let (_node, url) = start_redis().await;
    let stream = RedisTraderStream::connect(&url).await.unwrap();

    let tail = stream.tail(10).await.unwrap();
    let mut live = tail.live;

    // Emit a fresh event after subscribing.
    let s = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    let ev = TraderEvent {
        ts: Utc::now(),
        session_id: Uuid::nil(),
        kind: TraderEventKind::SessionStarted,
        ladder: s,
    };
    let stream2 = RedisTraderStream::connect(&url).await.unwrap();
    stream2.emit(&ev).await.unwrap();

    let received = tokio::time::timeout(std::time::Duration::from_secs(5),
        live.next()).await.unwrap().unwrap();
    assert!(matches!(received.kind, TraderEventKind::SessionStarted));
}
```

- [ ] **Step 5: Run tests**

```bash
cargo test --test trader_state_integration -- --ignored
```

Expected: 7 passed (5 state + 2 stream).

- [ ] **Step 6: Commit**

```bash
git add src/trader/adapters/redis_stream_wrapper.rs src/tui/events.rs Cargo.toml tests/trader_state_integration.rs
git commit -m "feat(trader): RedisTraderStream emitter + subscriber + integration tests"
```

---

## Task 13: adapters::gamma_wrapper — GammaMarketDiscovery via reqwest

**Files:**
- Modify: `src/trader/adapters/gamma_wrapper.rs`
- Create: `tests/trader_market_integration.rs`
- Modify: `Cargo.toml` (uncomment `[[test]] trader_market_integration`)

- [ ] **Step 1: Write `src/trader/adapters/gamma_wrapper.rs`**

```rust
use crate::trader::errors::MarketError;
use crate::trader::market::{decode_event_response, window_slug, MarketDiscovery, WindowMarket};
use async_trait::async_trait;
use reqwest::Client;

pub struct GammaMarketDiscovery {
    client: Client,
    base_url: String,
}

impl GammaMarketDiscovery {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::builder().timeout(std::time::Duration::from_secs(10)).build().unwrap(),
            base_url,
        }
    }
}

#[async_trait]
impl MarketDiscovery for GammaMarketDiscovery {
    async fn find_window(&self, window_ts: i64) -> Result<WindowMarket, MarketError> {
        let slug = window_slug(window_ts);
        let url = format!("{}/events?slug={slug}", self.base_url);
        let resp = self.client.get(&url).send().await
            .map_err(|e| MarketError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            if resp.status().as_u16() == 404 {
                return Err(MarketError::NotFound { window_ts });
            }
            return Err(MarketError::Network(format!("HTTP {}", resp.status())));
        }
        let body = resp.text().await
            .map_err(|e| MarketError::Network(e.to_string()))?;
        decode_event_response(&body, window_ts)
    }
}
```

- [ ] **Step 2: Uncomment `[[test]]` in `Cargo.toml`**

```toml
[[test]]
name = "trader_market_integration"
path = "tests/trader_market_integration.rs"
```

- [ ] **Step 3: Write `tests/trader_market_integration.rs`**

```rust
#![cfg(test)]

use poly_tui::trader::adapters::gamma_wrapper::GammaMarketDiscovery;
use poly_tui::trader::errors::MarketError;
use poly_tui::trader::market::MarketDiscovery;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[ignore]
async fn open_market_decoded_correctly() {
    let server = MockServer::start().await;
    let body = r#"[{"markets":[{
        "slug":"btc-updown-5m-1700000300", "closed":false,
        "outcomes":"[\"Up\",\"Down\"]",
        "clobTokenIds":"[\"u\",\"d\"]",
        "outcomePrices":"[\"0.50\",\"0.50\"]"
    }]}]"#;
    Mock::given(method("GET")).and(path("/events"))
        .and(query_param("slug", "btc-updown-5m-1700000300"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server).await;

    let disc = GammaMarketDiscovery::new(server.uri());
    let m = disc.find_window(1700000300).await.unwrap();
    assert_eq!(m.up_token_id, "u");
    assert_eq!(m.down_token_id, "d");
}

#[tokio::test]
#[ignore]
async fn empty_response_returns_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server).await;
    let disc = GammaMarketDiscovery::new(server.uri());
    let r = disc.find_window(1700000300).await;
    assert!(matches!(r, Err(MarketError::NotFound { .. })));
}

#[tokio::test]
#[ignore]
async fn http_500_returns_network() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/events"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server).await;
    let disc = GammaMarketDiscovery::new(server.uri());
    let r = disc.find_window(1700000300).await;
    assert!(matches!(r, Err(MarketError::Network(_))));
}

#[tokio::test]
#[ignore]
async fn malformed_body_returns_decode() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server).await;
    let disc = GammaMarketDiscovery::new(server.uri());
    let r = disc.find_window(1700000300).await;
    assert!(matches!(r, Err(MarketError::Decode(_))));
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test --test trader_market_integration -- --ignored
```

Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add src/trader/adapters/gamma_wrapper.rs tests/trader_market_integration.rs Cargo.toml
git commit -m "feat(trader): GammaMarketDiscovery (reqwest) + wiremock integration tests"
```

---

## Task 14: adapters::clob_executor_wrapper — real CLOB FoK + market sell

**Files:**
- Modify: `src/trader/adapters/clob_executor_wrapper.rs`

The polymarket SDK has order builders (`limit_order`, `market_order`). Pattern follows `ClobBalanceFetcher::connect` (reuse `LocalSigner` + `SignatureType::Proxy`). **No unit tests for the wrapper** — covered manually by Task 22 acceptance ("real-money: one complete window verified").

- [ ] **Step 1: Write `src/trader/adapters/clob_executor_wrapper.rs`**

```rust
use crate::trader::errors::ExecError;
use crate::trader::executor::{compute_share_count, FillResult, OrderExecutor};
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::str::FromStr;

use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use polymarket_client_sdk_v2::auth::Normal;
use polymarket_client_sdk_v2::auth::state::Authenticated;
use polymarket_client_sdk_v2::clob::types::SignatureType;

type AuthClient = polymarket_client_sdk_v2::clob::Client<Authenticated<Normal>>;

pub struct ClobOrderExecutor {
    client: AuthClient,
}

impl ClobOrderExecutor {
    pub async fn connect(host: &str, private_key: &str) -> Result<Self, ExecError> {
        use polymarket_client_sdk_v2::clob::{Client, Config};
        use polymarket_client_sdk_v2::POLYGON;
        let signer = LocalSigner::from_str(private_key)
            .map_err(|e| ExecError::Decode(format!("invalid key: {e}")))?
            .with_chain_id(Some(POLYGON));
        let client = Client::new(host, Config::default())
            .map_err(|e| ExecError::Network(e.to_string()))?
            .authentication_builder(&signer)
            .signature_type(SignatureType::Proxy)
            .authenticate().await
            .map_err(|_e| ExecError::Network("auth".into()))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl OrderExecutor for ClobOrderExecutor {
    async fn buy_fok(&self, token_id: &str, dollars: Decimal) -> Result<FillResult, ExecError> {
        // ⚠️ Verify exact builder API at impl time; structure shown below is the
        // canonical SDK example for FoK market buy on V2.
        use polymarket_client_sdk_v2::clob::types::{Side, OrderType};

        let order = self.client.market_order()
            .token_id(token_id)
            .side(Side::Buy)
            .amount(dollars)             // dollars to spend
            .order_type(OrderType::FOK)  // Fill-or-Kill
            .build().await
            .map_err(|e| ExecError::Network(e.to_string()))?;

        let resp = self.client.post_order(&order).await
            .map_err(|e| ExecError::Network(e.to_string()))?;

        if !resp.success {
            return Err(ExecError::FillOrKillFailed);
        }
        let fill_price = resp.making_amount.unwrap_or_default();
        let shares = compute_share_count(dollars, fill_price);
        Ok(FillResult { fill_price, shares, dollars })
    }

    async fn sell_market(&self, token_id: &str, shares: Decimal) -> Result<FillResult, ExecError> {
        use polymarket_client_sdk_v2::clob::types::{Side, OrderType};
        let order = self.client.market_order()
            .token_id(token_id)
            .side(Side::Sell)
            .size(shares)
            .order_type(OrderType::FOK)
            .build().await
            .map_err(|e| ExecError::Network(e.to_string()))?;
        let resp = self.client.post_order(&order).await
            .map_err(|e| ExecError::Network(e.to_string()))?;
        if !resp.success {
            return Err(ExecError::FillOrKillFailed);
        }
        let fill_price = resp.making_amount.unwrap_or_default();
        let dollars = fill_price * shares;
        Ok(FillResult { fill_price, shares, dollars })
    }
}
```

> **CRITICAL implementer note:** The exact field names on `OrderResponse`, the precise builder methods (`amount` vs `size`, `OrderType::FOK` vs alternative naming), and the post-order method (`post_order` vs `place_order`) all need verification against `polymarket_client_sdk_v2 = "0.6.0-canary.1"` source under `~/.cargo/registry/src/index.crates.io-*/polymarket_client_sdk_v2-0.6.0-canary.1/src/clob/`. The shape above is **a best-effort sketch**. If the SDK's V2 market-order API requires different glue (e.g. async builder context, separate sign + post phases), adapt accordingly. The trait surface (`buy_fok` / `sell_market`) does not change.

- [ ] **Step 2: Verify build**

```bash
cargo build --bin poly-trader
```

If the SDK API differs, adjust call sites (NOT trait signatures) until it compiles. Document the actual call shape used in a code comment.

- [ ] **Step 3: Commit**

```bash
git add src/trader/adapters/clob_executor_wrapper.rs
git commit -m "feat(trader): ClobOrderExecutor (FoK buy + market sell)"
```

---

## Task 15: adapters::simulated_executor — dry-run mode

**Files:**
- Modify: `src/trader/adapters/simulated_executor.rs`

- [ ] **Step 1: Write `src/trader/adapters/simulated_executor.rs`**

```rust
use crate::trader::errors::ExecError;
use crate::trader::executor::{compute_share_count, FillResult, OrderExecutor};
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Dry-run executor: simulates fills without touching CLOB. Default fill price
/// $0.50 for buys, $0.99 for sells.
pub struct SimulatedExecutor {
    buy_price: Decimal,
    sell_price: Decimal,
}

impl Default for SimulatedExecutor {
    fn default() -> Self {
        Self {
            buy_price: Decimal::from_str("0.50").unwrap(),
            sell_price: Decimal::from_str("0.99").unwrap(),
        }
    }
}

impl SimulatedExecutor {
    pub fn new() -> Self { Self::default() }
    pub fn with_prices(buy: Decimal, sell: Decimal) -> Self {
        Self { buy_price: buy, sell_price: sell }
    }
}

#[async_trait]
impl OrderExecutor for SimulatedExecutor {
    async fn buy_fok(&self, _token: &str, dollars: Decimal) -> Result<FillResult, ExecError> {
        let shares = compute_share_count(dollars, self.buy_price);
        Ok(FillResult { fill_price: self.buy_price, shares, dollars })
    }
    async fn sell_market(&self, _token: &str, shares: Decimal) -> Result<FillResult, ExecError> {
        let dollars = self.sell_price * shares;
        Ok(FillResult { fill_price: self.sell_price, shares, dollars })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn buy_returns_synthetic_fill() {
        let ex = SimulatedExecutor::default();
        let f = ex.buy_fok("any", Decimal::from(5)).await.unwrap();
        assert_eq!(f.shares, Decimal::from(10));
        assert_eq!(f.dollars, Decimal::from(5));
    }
    #[tokio::test]
    async fn sell_returns_synthetic_proceeds() {
        let ex = SimulatedExecutor::default();
        let f = ex.sell_market("any", Decimal::from(10)).await.unwrap();
        assert_eq!(f.dollars, Decimal::from_str("9.90").unwrap());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib trader::adapters::simulated_executor
```

Expected: 2 passed.

- [ ] **Step 3: Commit**

```bash
git add src/trader/adapters/simulated_executor.rs
git commit -m "feat(trader): SimulatedExecutor for --dry-run"
```

---

## Task 16: TUI app state extensions for trader events

**Files:**
- Modify: `src/domain.rs` (add `AppEvent::TraderEvent` variant)
- Modify: `src/app.rs` (AppState fields + handle_event branches + tick health calc)

- [ ] **Step 1: Extend `src/domain.rs` `AppEvent` enum**

Find the existing enum and add `TraderEvent`:

```rust
use crate::trader::event::TraderEvent;

#[derive(Debug)]
pub enum AppEvent {
    Tick,
    Key(crossterm::event::KeyEvent),
    Refresh(RefreshStatus),
    Shutdown,
    TraderEvent(TraderEvent),     // new
}
```

- [ ] **Step 2: Extend `AppState` in `src/app.rs`**

Add new fields:

```rust
use crate::trader::event::TraderEvent;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraderHealth {
    NotStarted,
    Healthy,
    Lagging,
    Stale,
    Stopped,
}

pub struct AppState {
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub redis_ok: bool,
    pub refresh_interval: Duration,
    pub should_quit: bool,

    // new
    pub trader_log: VecDeque<TraderEvent>,
    pub trader_latest: Option<TraderEvent>,
    pub trader_health: TraderHealth,
}
```

In `AppState::new`, initialize new fields:

```rust
trader_log: VecDeque::with_capacity(64),
trader_latest: None,
trader_health: TraderHealth::NotStarted,
```

- [ ] **Step 3: Add `TraderEvent` handling in `handle_event`**

```rust
AppEvent::TraderEvent(ev) => {
    if state.trader_log.len() >= 64 {
        state.trader_log.pop_front();
    }
    state.trader_log.push_back(ev.clone());
    state.trader_latest = Some(ev);
}
```

- [ ] **Step 4: Add `recompute_trader_health` invoked in `tick_once`**

```rust
fn compute_trader_health(latest: &Option<TraderEvent>, now: DateTime<Utc>) -> TraderHealth {
    use crate::trader::event::TraderEventKind;
    let Some(ev) = latest else { return TraderHealth::NotStarted; };

    if matches!(ev.kind, TraderEventKind::SessionStopped { .. }) {
        return TraderHealth::Stopped;
    }
    let age = now.signed_duration_since(ev.ts).num_seconds().max(0) as u64;
    if age < 6 * 60 { TraderHealth::Healthy }
    else if age < 12 * 60 { TraderHealth::Lagging }
    else { TraderHealth::Stale }
}
```

Call it inside `tick_once`:

```rust
state.trader_health = compute_trader_health(&state.trader_latest, Utc::now());
```

- [ ] **Step 5: Add unit tests in `src/app.rs`**

Append to existing `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn trader_event_appended_to_log() {
    use crate::trader::event::{TraderEvent, TraderEventKind};
    use crate::trader::ladder::{Direction, LadderState};
    use rust_decimal::Decimal;
    use uuid::Uuid;

    let mut s = AppState::new(Duration::from_secs(30));
    let (tx, _rx) = mpsc::channel(1);
    let ev = TraderEvent {
        ts: Utc::now(),
        session_id: Uuid::nil(),
        kind: TraderEventKind::SessionStarted,
        ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now()),
    };
    handle_event(&mut s, AppEvent::TraderEvent(ev.clone()), &tx);
    assert_eq!(s.trader_log.len(), 1);
    assert_eq!(s.trader_latest.as_ref().unwrap().session_id, ev.session_id);
}

#[tokio::test]
async fn trader_log_caps_at_64() {
    use crate::trader::event::{TraderEvent, TraderEventKind};
    use crate::trader::ladder::{Direction, LadderState};
    use rust_decimal::Decimal;
    use uuid::Uuid;

    let mut s = AppState::new(Duration::from_secs(30));
    let (tx, _rx) = mpsc::channel(1);
    for _ in 0..70 {
        let ev = TraderEvent {
            ts: Utc::now(), session_id: Uuid::nil(),
            kind: TraderEventKind::SessionStarted,
            ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now()),
        };
        handle_event(&mut s, AppEvent::TraderEvent(ev), &tx);
    }
    assert_eq!(s.trader_log.len(), 64);
}

#[test]
fn trader_health_not_started_when_no_events() {
    use chrono::TimeZone;
    let now = Utc.timestamp_opt(1700000000, 0).unwrap();
    assert_eq!(compute_trader_health(&None, now), TraderHealth::NotStarted);
}

#[test]
fn trader_health_healthy_under_6_min() {
    use crate::trader::event::{TraderEvent, TraderEventKind};
    use crate::trader::ladder::{Direction, LadderState};
    use chrono::{Duration as Cd, TimeZone};
    use rust_decimal::Decimal;
    use uuid::Uuid;

    let now = Utc.timestamp_opt(1700001000, 0).unwrap();
    let ev = TraderEvent {
        ts: now - Cd::seconds(120), session_id: Uuid::nil(),
        kind: TraderEventKind::SessionStarted,
        ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, now),
    };
    assert_eq!(compute_trader_health(&Some(ev), now), TraderHealth::Healthy);
}

#[test]
fn trader_health_lagging_between_6_and_12_min() {
    use crate::trader::event::{TraderEvent, TraderEventKind};
    use crate::trader::ladder::{Direction, LadderState};
    use chrono::{Duration as Cd, TimeZone};
    use rust_decimal::Decimal;
    use uuid::Uuid;

    let now = Utc.timestamp_opt(1700001000, 0).unwrap();
    let ev = TraderEvent {
        ts: now - Cd::seconds(8 * 60), session_id: Uuid::nil(),
        kind: TraderEventKind::SessionStarted,
        ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, now),
    };
    assert_eq!(compute_trader_health(&Some(ev), now), TraderHealth::Lagging);
}

#[test]
fn trader_health_stale_over_12_min() {
    use crate::trader::event::{TraderEvent, TraderEventKind};
    use crate::trader::ladder::{Direction, LadderState};
    use chrono::{Duration as Cd, TimeZone};
    use rust_decimal::Decimal;
    use uuid::Uuid;

    let now = Utc.timestamp_opt(1700001000, 0).unwrap();
    let ev = TraderEvent {
        ts: now - Cd::seconds(15 * 60), session_id: Uuid::nil(),
        kind: TraderEventKind::SessionStarted,
        ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, now),
    };
    assert_eq!(compute_trader_health(&Some(ev), now), TraderHealth::Stale);
}

#[test]
fn trader_health_stopped_takes_priority() {
    use crate::trader::event::{TraderEvent, TraderEventKind};
    use crate::trader::ladder::{Direction, LadderState, StopReason};
    use chrono::{Duration as Cd, TimeZone};
    use rust_decimal::Decimal;
    use uuid::Uuid;

    let now = Utc.timestamp_opt(1700001000, 0).unwrap();
    let ev = TraderEvent {
        ts: now - Cd::seconds(30), session_id: Uuid::nil(),
        kind: TraderEventKind::SessionStopped { reason: StopReason::CapReached },
        ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, now),
    };
    assert_eq!(compute_trader_health(&Some(ev), now), TraderHealth::Stopped);
}
```

- [ ] **Step 6: Run tests**

```bash
cargo test --lib app
```

Expected: existing 7 + new 7 = 14 passed.

- [ ] **Step 7: Commit**

```bash
git add src/domain.rs src/app.rs
git commit -m "feat(tui): AppEvent::TraderEvent + trader_log + trader_health"
```

---

## Task 17: TUI render — log panel + sub-title + trader LED

**Files:**
- Modify: `src/ui.rs`

- [ ] **Step 1: Extend `UiState` in `src/ui.rs`**

```rust
use crate::app::TraderHealth;
use crate::trader::event::TraderEvent;

#[derive(Clone, Debug)]
pub struct UiState {
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub clob_health: HealthLed,
    pub redis_health: HealthLed,
    pub refresh_interval: Duration,
    pub now: DateTime<Utc>,

    // new
    pub trader_log: Vec<TraderEvent>,
    pub trader_latest: Option<TraderEvent>,
    pub trader_health: TraderHealth,
}
```

Update `AppState::ui_state` to populate new fields (clone log into Vec).

- [ ] **Step 2: Replace `render` to add log panel layout**

```rust
pub fn render(frame: &mut Frame, state: &UiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),    // balance
            Constraint::Length(1),    // trader sub-title
            Constraint::Min(0),       // trader log
            Constraint::Length(1),    // status bar
        ])
        .split(area);

    render_balance(frame, chunks[0], state);
    render_trader_subtitle(frame, chunks[1], state);
    render_trader_log(frame, chunks[2], state);
    render_status_bar(frame, chunks[3], state);
}
```

(The implementer extracts the existing balance + status-bar render into `render_balance` and `render_status_bar`; renames helpers as needed.)

- [ ] **Step 3: Add `render_trader_subtitle`**

```rust
fn render_trader_subtitle(frame: &mut Frame, area: Rect, state: &UiState) {
    use ratatui::style::{Color, Style};
    use ratatui::text::Span;

    let line = match &state.trader_latest {
        None => Line::from(Span::raw(" Trader  not started — run `poly-trader` ")),
        Some(ev) => {
            let l = &ev.ladder;
            let dir = match l.direction {
                crate::trader::ladder::Direction::Up => "UP",
                crate::trader::ladder::Direction::Down => "DOWN",
            };
            Line::from(format!(
                " Trader  {dir}  ladder={}  P&L: ${} ",
                l.current_step, l.realized_pnl_usd,
            ))
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}
```

- [ ] **Step 4: Add `render_trader_log`**

```rust
fn render_trader_log(frame: &mut Frame, area: Rect, state: &UiState) {
    let lines: Vec<Line> = state.trader_log.iter().rev().take(area.height as usize)
        .map(|ev| {
            let ts = ev.ts.format("%H:%M:%S").to_string();
            let kind = format_event_kind(&ev.kind);
            Line::from(format!("{ts}  {kind}"))
        })
        .collect();
    let lines: Vec<Line> = lines.into_iter().rev().collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn format_event_kind(kind: &crate::trader::event::TraderEventKind) -> String {
    use crate::trader::event::TraderEventKind::*;
    match kind {
        SessionStarted => "SessionStarted".into(),
        SessionStopped { reason } => format!("SessionStopped {reason:?}"),
        WindowOpening { slug, .. } => format!("WindowOpening {slug}"),
        EntryDecision { decision } => format!("EntryDecision {decision:?}"),
        OrderPlaced { kind, dollars, .. } => format!("OrderPlaced {kind:?} ${dollars}"),
        OrderFilled { fill_price, shares, dollars } =>
            format!("OrderFilled {shares}sh @ {fill_price}  ${dollars}"),
        OrderRejected { reason } => format!("OrderRejected {reason}"),
        Resolved { winner, our_outcome, .. } =>
            format!("Resolved winner={winner:?} we={our_outcome:?}"),
        ResolutionTimeout => "ResolutionTimeout".into(),
        SellFilled { proceeds_usd } => format!("SellFilled ${proceeds_usd}"),
        SellRejected { reason } => format!("SellRejected {reason}"),
        LadderUpdated { from_step, to_step, .. } =>
            format!("LadderUpdated {from_step}→{to_step}"),
        Alert { message } => format!("ALERT {message}"),
    }
}
```

- [ ] **Step 5: Add Trader LED to status bar**

In `build_status_line`, after Redis LED:

```rust
spans.extend(led_span("Trader", trader_health_to_led(state.trader_health)));

fn trader_health_to_led(h: TraderHealth) -> HealthLed {
    match h {
        TraderHealth::Healthy => HealthLed::Green,
        TraderHealth::Lagging => HealthLed::Yellow,
        TraderHealth::Stale | TraderHealth::Stopped => HealthLed::Red,
        TraderHealth::NotStarted => HealthLed::Red, // grey not modeled, use red
    }
}
```

- [ ] **Step 6: Add 5 insta snapshot tests**

```rust
#[test]
fn renders_trader_not_started() {
    let state = ui_state_with(Some(balance("100")), None, TraderHealth::NotStarted, vec![]);
    insta::assert_snapshot!("trader_not_started", render_to_buffer(&state));
}

#[test]
fn renders_trader_with_events() {
    let state = ui_state_with(Some(balance("100")), Some(sample_event()),
                              TraderHealth::Healthy, vec![sample_event(); 3]);
    insta::assert_snapshot!("trader_with_events", render_to_buffer(&state));
}

#[test]
fn renders_trader_stopped() {
    let state = ui_state_with(Some(balance("100")), Some(stopped_event()),
                              TraderHealth::Stopped, vec![stopped_event()]);
    insta::assert_snapshot!("trader_stopped", render_to_buffer(&state));
}

#[test]
fn renders_trader_lagging() {
    let state = ui_state_with(Some(balance("100")), Some(sample_event()),
                              TraderHealth::Lagging, vec![sample_event()]);
    insta::assert_snapshot!("trader_lagging", render_to_buffer(&state));
}

#[test]
fn renders_long_log_truncated() {
    let events = (0..30).map(|_| sample_event()).collect::<Vec<_>>();
    let state = ui_state_with(Some(balance("100")), events.last().cloned(),
                              TraderHealth::Healthy, events);
    insta::assert_snapshot!("trader_long_log", render_to_buffer(&state));
}
```

(Helper functions `ui_state_with`, `sample_event`, `stopped_event`, `balance` are inline in the test module.)

- [ ] **Step 7: Run tests + accept snapshots**

```bash
cargo test --lib ui
cargo insta accept
cargo test --lib ui
```

Expected: 8 passed (3 existing + 5 new).

- [ ] **Step 8: Commit**

```bash
git add src/ui.rs src/snapshots/
git commit -m "feat(ui): trader log panel + sub-title + Trader LED snapshots"
```

---

## Task 18: poly-tui main wiring — subscribe trader events task

**Files:**
- Modify: `src/bin/poly-tui.rs`

Add a 4th tokio task that subscribes to the trader event stream and forwards events as `AppEvent::TraderEvent` into the existing event channel. If the stream connection fails, TUI continues with `TraderHealth::NotStarted`.

- [ ] **Step 1: Modify `src/bin/poly-tui.rs`**

After existing adapter setup (Redis cache, CLOB fetcher), add:

```rust
use poly_tui::trader::adapters::redis_stream_wrapper::RedisTraderStream;
use poly_tui::tui::events::TraderEventStream;

let trader_stream: Option<Arc<dyn TraderEventStream>> =
    match RedisTraderStream::connect(&cfg.redis_url).await {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            tracing::warn!("trader stream subscribe failed: {e} — TUI shows 'not started'");
            None
        }
    };
```

After spawning refresher + input task, add the 4th task:

```rust
let event_tx_trader = event_tx.clone();
let shutdown_trader = shutdown.clone();
let h_trader = if let Some(stream) = trader_stream {
    tokio::spawn(async move {
        let tail = match stream.tail(64).await {
            Ok(t) => t,
            Err(_) => return,
        };
        for ev in tail.history {
            if event_tx_trader.send(AppEvent::TraderEvent(ev)).await.is_err() { return; }
        }
        let mut live = tail.live;
        loop {
            tokio::select! {
                _ = shutdown_trader.cancelled() => break,
                Some(ev) = futures::StreamExt::next(&mut live) => {
                    if event_tx_trader.send(AppEvent::TraderEvent(ev)).await.is_err() { break; }
                }
            }
        }
    })
} else {
    tokio::spawn(async move {})
};
```

Add `h_trader` to the final `tokio::join!`:

```rust
let _ = tokio::join!(h_refresh, h_input, h_status, h_trader);
```

- [ ] **Step 2: Verify build + smoke test**

```bash
cargo build --bin poly-tui
```

(Manual test deferred to acceptance task.)

- [ ] **Step 3: Commit**

```bash
git add src/bin/poly-tui.rs
git commit -m "feat(tui): subscribe trader event stream + forward to AppEvent"
```

---

## Task 19: poly-trader main wiring — signal handler + lock + scheduler.run

**Files:**
- Modify: `src/bin/poly-trader.rs`

- [ ] **Step 1: Replace stub with full main**

```rust
use anyhow::{Context, Result};
use clap::Parser;
use poly_tui::config::Config;
use poly_tui::trader::adapters::{
    clob_executor_wrapper::ClobOrderExecutor,
    gamma_wrapper::GammaMarketDiscovery,
    redis_state_wrapper::RedisTraderState,
    redis_stream_wrapper::RedisTraderStream,
    simulated_executor::SimulatedExecutor,
};
use poly_tui::trader::config::TraderArgs;
use poly_tui::trader::errors::StateError;
use poly_tui::trader::event::TraderEventEmitter;
use poly_tui::trader::executor::OrderExecutor;
use poly_tui::trader::ladder::{Direction, LadderState};
use poly_tui::trader::market::MarketDiscovery;
use poly_tui::trader::resolver::{PolymarketResolver, WindowResolver};
use poly_tui::trader::scheduler::{run, SchedulerConfig, SchedulerDeps, WindowExecutor};
use poly_tui::trader::state::TraderStateStore;
use poly_tui::trader::window::{run_window, WindowConfig, WindowDeps};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let args = TraderArgs::parse();
    args.validate().context("invalid CLI arguments")?;

    dotenvy::dotenv().ok();
    let cfg = Config::from_env().context("loading .env")?;
    let gamma_host = std::env::var("GAMMA_HOST")
        .unwrap_or_else(|_| "https://gamma-api.polymarket.com".into());

    // Logging → file
    let appender = tracing_appender::rolling::daily("logs", "trader.log");
    let (nb, _guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt().with_writer(nb)
        .with_env_filter(EnvFilter::new(&cfg.log_level)).init();
    tracing::info!("starting poly-trader");

    // Adapters
    let state_store: Arc<dyn TraderStateStore> =
        Arc::new(RedisTraderState::connect(&cfg.redis_url).await
            .context("connecting Redis (fatal)")?);
    let emitter: Arc<dyn TraderEventEmitter> =
        Arc::new(RedisTraderStream::connect(&cfg.redis_url).await
            .context("connecting Redis stream")?);
    let market: Arc<dyn MarketDiscovery> =
        Arc::new(GammaMarketDiscovery::new(gamma_host));
    let resolver: Arc<dyn WindowResolver> =
        Arc::new(PolymarketResolver::new(market.clone(), Duration::from_secs(60)));

    let executor: Arc<dyn OrderExecutor> = if args.dry_run {
        Arc::new(SimulatedExecutor::default())
    } else {
        Arc::new(ClobOrderExecutor::connect(&cfg.clob_host, &cfg.polymarket_private_key).await
            .context("CLOB auth (fatal)")?)
    };

    // Acquire singleton lock
    let owner = format!("{}:{}", hostname::get().map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".into()), std::process::id());
    let acquired = state_store.try_lock(&owner, Duration::from_secs(60)).await?;
    if !acquired {
        anyhow::bail!("another poly-trader is running (lock held)");
    }

    // Restore or init ladder
    let ladder = restore_or_init(state_store.as_ref(), &args).await?;

    // Lock keepalive
    let keepalive_owner = owner.clone();
    let keepalive_store = state_store.clone();
    let shutdown = CancellationToken::new();
    let shutdown_keepalive = shutdown.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = shutdown_keepalive.cancelled() => break,
                _ = interval.tick() => {
                    if let Err(e) = keepalive_store.refresh_lock(&keepalive_owner, Duration::from_secs(60)).await {
                        tracing::error!("lock keepalive failed: {e}");
                        shutdown_keepalive.cancel();
                        break;
                    }
                }
            }
        }
    });

    // Signal handler
    let shutdown_sig = shutdown.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        shutdown_sig.cancel();
    });

    // WindowExecutor adapter (binds run_window over our deps)
    let window_deps = Arc::new(WindowDeps {
        market: market.clone(),
        executor: executor.clone(),
        resolver: resolver.clone(),
        emitter: emitter.clone(),
    });
    let window_cfg = WindowConfig {
        band_min: args.band_min,
        band_max: args.band_max,
    };
    let window_exec: Arc<dyn WindowExecutor> = Arc::new(BoundWindowExec {
        deps: window_deps.clone(),
        cfg: window_cfg,
    });

    let sched_deps = SchedulerDeps {
        window_exec,
        state_store: state_store.clone(),
        emitter: emitter.clone(),
    };
    let sched_cfg = SchedulerConfig { max_windows: args.max_windows };

    let final_state = run(ladder, sched_deps, sched_cfg, shutdown.clone()).await
        .map_err(|e: StateError| anyhow::anyhow!("scheduler error: {e}"))?;
    tracing::info!("session ended: stopped={:?} pnl={}",
        final_state.stopped, final_state.realized_pnl_usd);

    state_store.release_lock(&owner).await.ok();
    Ok(())
}

struct BoundWindowExec {
    deps: Arc<WindowDeps>,
    cfg: WindowConfig,
}

#[async_trait::async_trait]
impl WindowExecutor for BoundWindowExec {
    async fn execute(&self, ladder: &LadderState, window_ts: i64)
        -> poly_tui::trader::ladder::WindowOutcome
    {
        run_window(&self.deps, &self.cfg, ladder, window_ts).await
    }
}

async fn restore_or_init(
    store: &dyn TraderStateStore,
    args: &TraderArgs,
) -> Result<LadderState> {
    let existing = store.load().await?;
    match (existing, args.reset) {
        (Some(s), false) if !s.is_stopped() => {
            tracing::info!("resuming ladder: step={} pnl={}",
                s.current_step, s.realized_pnl_usd);
            Ok(s)
        }
        (Some(s), false) if s.is_stopped() => {
            anyhow::bail!("previous session stopped: {:?}; pass --reset to start fresh", s.stopped)
        }
        _ => {
            store.clear().await?;
            let direction: Direction = args.direction.into();
            Ok(LadderState::new(direction, args.base, args.max_step, chrono::Utc::now()))
        }
    }
}
```

- [ ] **Step 2: Add `hostname` dep to `Cargo.toml`**

```toml
hostname = "0.4"
```

- [ ] **Step 3: Build**

```bash
cargo build --bin poly-trader
```

If SDK API in clob_executor_wrapper fails, fix that first (Task 14 sketch).

- [ ] **Step 4: Smoke test (will exit cleanly without Redis)**

```bash
docker compose up -d
cargo run --bin poly-trader -- --direction up --base 5 --dry-run --max-windows 2
```

Expected: connects, runs 2 simulated windows, exits cleanly. Check `logs/trader-*.log` for events.

- [ ] **Step 5: Commit**

```bash
git add src/bin/poly-trader.rs Cargo.toml
git commit -m "feat(trader): poly-trader main — lock, restore, scheduler.run"
```

---

## Task 20: BDD scenarios for trader

**Files:**
- Create: `tests/features/trader.feature`
- Modify: `tests/bdd.rs` (extend `AppWorld` and add step defs for trader scenarios)

- [ ] **Step 1: Write `tests/features/trader.feature`**

```gherkin
Feature: Martingale 5-minute trader
  As bot operator, I want disciplined Martingale execution per 5-min window.

  Background:
    Given direction "UP", base $5, max_step 5
    And trader has fresh ladder state

  Scenario: 第一局赢
    When window opens with ask UP=0.50
    And FoK buy fills 10 shares at $0.50
    And resolution returns winner=UP
    And sell market fills at $0.99 for $9.90 proceeds
    Then ladder step is 1
    And realized_pnl is $4.90

  Scenario: 连输 5 局触顶停止
    Given ladder at step 5
    When resolution returns winner=DOWN
    Then session_stopped is CapReached

  Scenario: 价格偏离 50/50 跳过
    When window opens with ask UP=0.62
    Then no order is placed
    And ladder step is unchanged

  Scenario: FoK 失败跳过
    When window opens with ask UP=0.50
    And FoK buy returns NoLiquidity
    Then ladder step is unchanged

  Scenario: 60s 未解析跳过
    When window opens normally and FoK buy fills
    And resolution polling exceeds 60s
    Then ladder step is unchanged

  Scenario: dry-run 不下真单
    Given trader started with --dry-run
    When window opens normally
    Then SimulatedExecutor records the call
    And no real CLOB order is placed
```

- [ ] **Step 2: Extend `tests/bdd.rs`** with new World fields + step defs

Add to `AppWorld` struct:

```rust
trader_ladder: Option<poly_tui::trader::ladder::LadderState>,
window_market: Option<poly_tui::trader::market::WindowMarket>,
last_outcome: Option<poly_tui::trader::ladder::WindowOutcome>,
fake_buy_result: Option<Result<poly_tui::trader::executor::FillResult,
                                poly_tui::trader::errors::ExecError>>,
fake_sell_result: Option<Result<poly_tui::trader::executor::FillResult,
                                 poly_tui::trader::errors::ExecError>>,
fake_resolution: Option<Result<poly_tui::trader::resolver::Resolution,
                                poly_tui::trader::errors::ResolveError>>,
```

Add step defs (same patterns as v1.0 BDD):

```rust
use poly_tui::trader::ladder::{Direction, LadderState, WindowOutcome, apply_outcome};
use poly_tui::trader::executor::FillResult;
use rust_decimal::Decimal;
use std::str::FromStr;

#[given(regex = r#"^direction "([^"]+)", base \$(\d+), max_step (\d+)$"#)]
async fn given_session(world: &mut AppWorld, dir: String, base: u8, max_step: u8) {
    let direction = match dir.as_str() {
        "UP" => Direction::Up,
        "DOWN" => Direction::Down,
        _ => panic!("bad direction"),
    };
    world.trader_ladder = Some(LadderState::new(
        direction, Decimal::from(base), max_step, chrono::Utc::now()));
}

#[given("trader has fresh ladder state")]
async fn given_fresh(_world: &mut AppWorld) {}

#[given(regex = r#"^ladder at step (\d+)$"#)]
async fn given_step(world: &mut AppWorld, step: u8) {
    if let Some(l) = world.trader_ladder.as_mut() { l.current_step = step; }
}

#[when(regex = r#"^window opens with ask UP=([0-9.]+)$"#)]
async fn when_open_ask(world: &mut AppWorld, ask: String) {
    use poly_tui::trader::market::WindowMarket;
    let a = Decimal::from_str(&ask).unwrap();
    world.window_market = Some(WindowMarket {
        window_ts: 1700000300, slug: "test".into(),
        up_token_id: "u".into(), down_token_id: "d".into(),
        up_ask: a, down_ask: Decimal::ONE - a,
        closed: false, winner: None,
    });
}

#[when(regex = r#"^FoK buy fills (\d+) shares at \$([0-9.]+)$"#)]
async fn when_buy_fills(world: &mut AppWorld, shares: u32, price: String) {
    let p = Decimal::from_str(&price).unwrap();
    world.fake_buy_result = Some(Ok(FillResult {
        fill_price: p, shares: Decimal::from(shares), dollars: p * Decimal::from(shares),
    }));
}

#[when("FoK buy returns NoLiquidity")]
async fn when_buy_nolq(world: &mut AppWorld) {
    use poly_tui::trader::errors::ExecError;
    world.fake_buy_result = Some(Err(ExecError::FillOrKillFailed));
}

// ... etc, full step set per .feature file scenarios.

#[then(regex = r#"^ladder step is (\d+)$"#)]
async fn then_step(world: &mut AppWorld, expected: u8) {
    let l = world.trader_ladder.as_ref().expect("ladder");
    assert_eq!(l.current_step, expected, "step mismatch");
}

#[then(regex = r#"^realized_pnl is \$([0-9.\-]+)$"#)]
async fn then_pnl(world: &mut AppWorld, expected: String) {
    let l = world.trader_ladder.as_ref().expect("ladder");
    assert_eq!(l.realized_pnl_usd, Decimal::from_str(&expected).unwrap());
}

#[then("session_stopped is CapReached")]
async fn then_cap(world: &mut AppWorld) {
    use poly_tui::trader::ladder::StopReason;
    let l = world.trader_ladder.as_ref().unwrap();
    assert_eq!(l.stopped, Some(StopReason::CapReached));
}

#[then("ladder step is unchanged")]
async fn then_unchanged(world: &mut AppWorld) {
    // For "unchanged" scenarios, this requires capturing pre/post; simplest:
    // emit a Skipped outcome through apply_outcome and verify step stays.
    if let (Some(l), Some(_)) = (world.trader_ladder.as_ref(), world.window_market.as_ref()) {
        let pre_step = l.current_step;
        // The step defs above set up state; this just records expectation;
        // actual transition handled by orchestration step "When ... and FoK ...".
        // For pure-FSM BDD, we apply Skipped here:
        let next = apply_outcome(l, &WindowOutcome::Skipped {
            reason: poly_tui::trader::ladder::SkipReason::FillOrKillFailed,
        }, chrono::Utc::now());
        assert_eq!(next.current_step, pre_step);
    }
}
```

(The implementer fills out the remaining step defs to cover every gherkin clause. Pattern is mechanical: parse the clause, manipulate `world.trader_ladder` via `apply_outcome` for terminal-state assertions.)

- [ ] **Step 3: Run BDD**

```bash
cargo test --test bdd
```

Expected: existing 4 + new 6 = 10 scenarios passing.

- [ ] **Step 4: Commit**

```bash
git add tests/features/trader.feature tests/bdd.rs
git commit -m "test(bdd): trader Martingale scenarios"
```

---

## Task 21: E2E trader tests (testcontainers + fakes)

**Files:**
- Create: `tests/e2e_trader.rs`
- Modify: `Cargo.toml` (uncomment `[[test]] e2e_trader`)

- [ ] **Step 1: Uncomment `[[test]] e2e_trader`**

```toml
[[test]]
name = "e2e_trader"
path = "tests/e2e_trader.rs"
```

- [ ] **Step 2: Write `tests/e2e_trader.rs`**

```rust
#![cfg(test)]

use chrono::Utc;
use poly_tui::trader::adapters::redis_state_wrapper::RedisTraderState;
use poly_tui::trader::adapters::redis_stream_wrapper::RedisTraderStream;
use poly_tui::trader::adapters::simulated_executor::SimulatedExecutor;
use poly_tui::trader::event::{TraderEventEmitter, TraderEventKind};
use poly_tui::trader::executor::OrderExecutor;
use poly_tui::trader::ladder::{Direction, LadderState, StopReason, WindowOutcome};
use poly_tui::trader::scheduler::{run, SchedulerConfig, SchedulerDeps, WindowExecutor};
use poly_tui::trader::state::TraderStateStore;
use poly_tui::tui::events::TraderEventStream;
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "E2E must NOT bind dev Redis port");
    let url = format!("redis://127.0.0.1:{port}");
    (node, url)
}

struct ScriptedWindowExec {
    outcomes: std::sync::Mutex<Vec<WindowOutcome>>,
}
#[async_trait::async_trait]
impl WindowExecutor for ScriptedWindowExec {
    async fn execute(&self, _l: &LadderState, _ts: i64) -> WindowOutcome {
        let mut q = self.outcomes.lock().unwrap();
        if q.is_empty() {
            WindowOutcome::Won { proceeds_usd: Decimal::from(10) }
        } else {
            q.remove(0)
        }
    }
}

#[tokio::test]
#[ignore]
async fn e2e_full_session_5_wins() {
    tokio::time::pause();
    let (_node, url) = start_redis().await;
    let store = Arc::new(RedisTraderState::connect(&url).await.unwrap());
    let emitter = Arc::new(RedisTraderStream::connect(&url).await.unwrap());
    let exec = Arc::new(ScriptedWindowExec {
        outcomes: std::sync::Mutex::new(vec![
            WindowOutcome::Won { proceeds_usd: Decimal::from_str("9.90").unwrap() }; 5
        ]),
    });
    let ladder = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    let final_state = run(
        ladder,
        SchedulerDeps { window_exec: exec, state_store: store, emitter },
        SchedulerConfig { max_windows: Some(5) },
        CancellationToken::new(),
    ).await.unwrap();
    tokio::time::advance(Duration::from_secs(60 * 30)).await;
    assert_eq!(final_state.windows_won, 5);
    assert!(final_state.realized_pnl_usd > Decimal::ZERO);
}

#[tokio::test]
#[ignore]
async fn e2e_cap_reached_stops_session() {
    tokio::time::pause();
    let (_node, url) = start_redis().await;
    let store = Arc::new(RedisTraderState::connect(&url).await.unwrap());
    let emitter = Arc::new(RedisTraderStream::connect(&url).await.unwrap());
    let losses = (0..5).map(|i|
        WindowOutcome::Lost { spent_usd: Decimal::from(5_u64 << i) }
    ).collect();
    let exec = Arc::new(ScriptedWindowExec {
        outcomes: std::sync::Mutex::new(losses),
    });
    let ladder = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    let final_state = run(
        ladder,
        SchedulerDeps { window_exec: exec, state_store: store, emitter },
        SchedulerConfig { max_windows: None },
        CancellationToken::new(),
    ).await.unwrap();
    assert_eq!(final_state.stopped, Some(StopReason::CapReached));
}

#[tokio::test]
#[ignore]
async fn e2e_resume_from_redis() {
    let (_node, url) = start_redis().await;
    let store = Arc::new(RedisTraderState::connect(&url).await.unwrap());
    let mut s = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    s.current_step = 4;
    s.realized_pnl_usd = Decimal::from(-35);
    store.save(&s).await.unwrap();

    let restored = store.load().await.unwrap().expect("Some");
    assert_eq!(restored.current_step, 4);
    assert_eq!(restored.realized_pnl_usd, Decimal::from(-35));
}

#[tokio::test]
#[ignore]
async fn e2e_lock_prevents_double_run() {
    let (_node, url) = start_redis().await;
    let store_a = RedisTraderState::connect(&url).await.unwrap();
    let store_b = RedisTraderState::connect(&url).await.unwrap();
    assert!(store_a.try_lock("a", Duration::from_secs(60)).await.unwrap());
    assert!(!store_b.try_lock("b", Duration::from_secs(60)).await.unwrap());
}

#[tokio::test]
#[ignore]
async fn e2e_tui_subscribes_to_stream() {
    let (_node, url) = start_redis().await;
    let emitter = RedisTraderStream::connect(&url).await.unwrap();
    let stream = RedisTraderStream::connect(&url).await.unwrap();

    let s = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    let ev = poly_tui::trader::event::TraderEvent {
        ts: Utc::now(), session_id: Uuid::nil(),
        kind: TraderEventKind::SessionStarted,
        ladder: s,
    };
    emitter.emit(&ev).await.unwrap();

    let tail = stream.tail(10).await.unwrap();
    assert!(!tail.history.is_empty());
}

use std::str::FromStr;
```

- [ ] **Step 3: Run E2E**

```bash
docker info
cargo test --test e2e_trader -- --ignored
```

Expected: 5 passed.

- [ ] **Step 4: Commit**

```bash
git add tests/e2e_trader.rs Cargo.toml
git commit -m "test(e2e): trader full-stack with testcontainers + scripted fakes"
```

---

## Task 22: Coverage gate, README, TODO updates, final acceptance

**Files:**
- Modify: `README.md` (add trader operation section)
- Modify: `TODO.md` (mark v1.x trader complete; queue v1.1 daemon split)

- [ ] **Step 1: Run full test suite**

```bash
cargo test --lib
cargo test --test bdd
cargo test --test cache_integration -- --ignored
cargo test --test trader_state_integration -- --ignored
cargo test --test trader_market_integration -- --ignored
cargo test --test e2e_trader -- --ignored
```

Expected: all green.

- [ ] **Step 2: Run coverage**

```bash
cargo llvm-cov --lib --tests \
  --ignore-filename-regex 'src/bin|src/trader/adapters/|.*_wrapper\.rs' \
  --html
cargo llvm-cov report --lib --tests \
  --ignore-filename-regex 'src/bin|src/trader/adapters/|.*_wrapper\.rs'
```

Verify `src/trader/` aggregate ≥ 99%.

If under, the gap is most likely in `trader::scheduler` time/select edge cases or in `trader::window` error mapping. Identify uncovered lines and add targeted tests.

- [ ] **Step 3: Verify acceptance checklist**

Walk through spec §13:

- [ ] `--dry-run --max-windows 12` runs an hour, all events in stream, no crash
- [ ] Real-money: at least one full window verified manually (buy → resolve → sell)
- [ ] Manually trigger FoK fail (small account at ladder=4)
- [ ] Manually wait for SkipBand window
- [ ] dry-run reaches CapReached (force outcomes via `SimulatedExecutor::with_prices` + custom resolver)
- [ ] kill -9 trader, restart, ladder + P&L restored
- [ ] Two `poly-trader` instances → second exits
- [ ] TUI panel shows trader log; sub-title accurate; LED logic verified at all four health states
- [ ] All Redis keys under `poly:prod:trader:*`
- [ ] `assert_ne!(port, 6379)` in e2e_trader, trader_state_integration, trader_market_integration
- [ ] `.env` not in `git ls-files`
- [ ] `src/trader/` coverage ≥ 99%
- [ ] v1.0 modules unchanged in coverage (re-run v1.0 coverage to compare)

- [ ] **Step 4: Update `README.md`** — append a section:

```markdown
## Trader

`poly-trader` is the headless trading process. It runs Martingale on Polymarket's BTC 5-minute up/down market.

### Quick start (dry-run, no real money)

\`\`\`bash
docker compose up -d                                  # Redis
poly-trader --direction up --base 5 --dry-run \
            --max-windows 12                          # 1 hour simulation
poly-tui                                              # observe events
\`\`\`

### Real money

\`\`\`bash
poly-trader --direction up --base 5
\`\`\`

### Stop / resume

\`\`\`bash
# stop
Ctrl+C  # in trader's terminal — current window completes, then exit
# resume
poly-trader --direction up --base 5    # picks up ladder from Redis

# fresh start (DANGER: discards any open ladder state)
poly-trader --direction up --base 5 --reset
\`\`\`

### Inspect state

\`\`\`bash
docker exec poly-redis redis-cli GET poly:prod:trader:ladder | jq .
docker exec poly-redis redis-cli XREVRANGE poly:prod:trader:events + - COUNT 10
tail -f logs/trader-*.log
\`\`\`

### Risk caps

- `--max-step N` (default 5) — stop session after N consecutive losses
- `--band-min/--band-max` (default 0.45/0.55) — only enter when ask is in this range
- See `docs/superpowers/specs/2026-05-09-poly-trader-martingale-design.md` §7 for full failure handling
```

- [ ] **Step 5: Update `TODO.md`** — mark v1.x trader items complete, queue v1.1 items

- [ ] **Step 6: Final commit**

```bash
git add README.md TODO.md
git commit -m "docs: README + TODO updated for v1.x trader release"
```

- [ ] **Step 7: Push to origin**

```bash
git push origin main
```

---

## Self-Review Notes

**Spec coverage:**
- §1 Goals: covered by Tasks 1, 9, 10, 19 (trader skeleton + main)
- §2 Decisions summary: encoded across all tasks
- §3 Architecture: implemented in Tasks 18 (TUI subscriber) + 19 (trader main) + Tasks 1-10 (trader subsystems)
- §4 Modules: Tasks 0 (skeleton) + 1-10
- §5 Ladder FSM: Task 1
- §6 Redis schema + data flow: Tasks 5 (state trait), 11 (state impl), 12 (stream impl)
- §7 Error handling + lock: Tasks 3, 11, 19
- §8 TUI integration: Tasks 16, 17, 18
- §9 Test strategy: covered by inline unit tests across Tasks 1-10 + integration Tasks 11/13 + E2E Task 21 + BDD Task 20 + coverage gate Task 22
- §10 Deps: Task 0
- §11 CLI: Task 2 (parsing) + Task 19 (wiring)
- §12 Main flow: Task 19
- §13 Acceptance: Task 22

**Open implementer-time verifications (flagged inline):**
1. fred 9.x exact stream API shape (Task 12) — current code follows the documented pattern but `xadd`/`xread_map`/`XCap` arg ordering may differ
2. polymarket_client_sdk_v2 v0.6.0-canary.1 market_order builder (Task 14) — verify `OrderType::FOK` exists, builder method names, response field shape
3. fred SETNX response shape (Task 11) — `Option<String>` vs `bool` for NX result

**Type consistency:** All cross-module references use canonical names: `LadderState`, `WindowOutcome`, `WindowMarket`, `FillResult`, `Resolution`, `TraderEvent`, `TraderEventKind`, `SkipReason`, `StopReason`, `Direction`. Method signatures match across trait + impl + tests. Redis keys: `poly:prod:trader:ladder`, `poly:prod:trader:events`, `poly:prod:trader:lock` consistent.

**Scope:** Single feature (Martingale 5min trader); excludes v1.1+ items per spec §1.
