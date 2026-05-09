# v1.5 — poly-trader TP/SL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--exit-rule tp-sl --tp-price 0.85 --sl-price 0.45` to `poly-trader` so backtest-validated strategy 4 can run live (initially in dry-run) against Polymarket.

**Architecture:** A new `MidwindowPriceFetcher` polls gamma every 5s during the window. A new `ExitWatcher` checks each tick against TP/SL thresholds. `run_window` selects between the watcher and existing `await_resolution` — whichever finishes first determines the outcome. Default `--exit-rule hold` reproduces v1.1 behavior bit-for-bit.

**Tech Stack:** Rust 1.78+, tokio (existing), reqwest (existing gamma client), rust_decimal (existing), clap (existing), async_trait (existing). No new deps.

**Spec:** `docs/superpowers/specs/2026-05-10-trader-tp-sl-design.md`

## Build hygiene — STRICT

NEVER run bare `cargo build`. Always scope:
- `cargo build --bin poly-trader`
- `cargo test --lib trader::`
- `cargo test --test e2e_trader -- --ignored` (final E2E only)

Do NOT touch `src/backtest/` or `poly-tui` code in this plan. The trader is the only target.

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `src/trader/config.rs` | modify | Extend `TraderArgs` with `--exit-rule`, `--tp-price`, `--sl-price`, `--poll-secs`. Add `ExitRuleArg` enum. Add `validate()` rules. |
| `src/trader/errors.rs` | modify | Add `PriceError` enum (Network / Decode). |
| `src/trader/price.rs` | create | `MidwindowPriceFetcher` trait. |
| `src/trader/exit_watcher.rs` | create | `ExitWatcher` polling loop + `ExitConfig` + `ExitTrigger` + `ExitKind`. |
| `src/trader/event.rs` | modify | Add `TraderEventKind::ExitTriggered { kind, bid, proceeds_usd }`. |
| `src/trader/window.rs` | modify | `WindowConfig.exit: Option<ExitConfig>` + `WindowDeps.price` + branch in `run_window` with `tokio::select!`. |
| `src/trader/adapters/gamma_price_wrapper.rs` | create | `GammaPriceFetcher` impl reusing reqwest client + cache-bust nonce pattern. |
| `src/trader/adapters/mod.rs` | modify | `pub mod gamma_price_wrapper;` |
| `src/trader/mod.rs` | modify | Add `pub mod price;` and `pub mod exit_watcher;`. |
| `src/bin/poly-trader.rs` | modify | Wire `GammaPriceFetcher` and pass `Option<ExitConfig>` into `WindowConfig`. |
| `tests/e2e_trader.rs` | modify | New `#[ignore]` E2E asserting `ExitTriggered` event reaches Redis stream. |
| `README.md` | modify | New v1.5 section: usage, expected event order, fall-back to v1.1. |
| `TODO.md` | modify | Tick v1.5 ✅. |

---

## Task 0: Sanity baseline

**Files:** none (read-only).

- [ ] **Step 1: Confirm working tree clean**

Run: `git status`
Expected: only untracked items are `.claude/`, the four `backtest-report*.html` files, and the cache directory; no tracked-file modifications.

- [ ] **Step 2: Confirm trader unit tests green**

Run: `cargo test --lib trader::`
Expected: PASS — all existing trader tests green. Take note of the count for diff later (e.g. "62 passed").

- [ ] **Step 3: Confirm trader binary builds**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean (warnings ok, but no errors).

- [ ] **Step 4: No commit (read-only baseline)**

Skip — this task only verifies starting state.

---

## Task 1: PriceError type

**Files:**
- Modify: `src/trader/errors.rs` (append new enum)

- [ ] **Step 1: Write the failing test**

Add to `src/trader/errors.rs` (append at end of file, before any existing `#[cfg(test)]` if present, else add a `#[cfg(test)]` block):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_error_displays() {
        let e = PriceError::Network("502".into());
        assert_eq!(format!("{e}"), "gamma price fetch failed: 502");
        let d = PriceError::Decode("missing field".into());
        assert_eq!(format!("{d}"), "price response decode failed: missing field");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib trader::errors::`
Expected: FAIL — `cannot find value 'PriceError' in module 'super'`.

- [ ] **Step 3: Implement PriceError**

Append to `src/trader/errors.rs`:

```rust
#[derive(Error, Debug, Clone)]
pub enum PriceError {
    #[error("gamma price fetch failed: {0}")]
    Network(String),
    #[error("price response decode failed: {0}")]
    Decode(String),
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib trader::errors::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/trader/errors.rs
git commit -m "feat(trader): PriceError enum for mid-window fetcher"
```

---

## Task 2: ExitRuleArg + new CLI flags + validation

**Files:**
- Modify: `src/trader/config.rs`

- [ ] **Step 1: Write the failing tests**

Replace the `#[cfg(test)] mod tests` block tail in `src/trader/config.rs` — add these tests *before* the closing `}` of the `mod tests` block:

```rust
    #[test]
    fn parses_exit_rule_hold_default() {
        let a = parse(&["--direction", "up"]);
        assert_eq!(a.exit_rule, ExitRuleArg::Hold);
        assert_eq!(a.tp_price, None);
        assert_eq!(a.sl_price, None);
        assert_eq!(a.poll_secs, 5);
    }

    #[test]
    fn parses_exit_rule_tp_sl_with_thresholds() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "tp-sl",
            "--tp-price", "0.85",
            "--sl-price", "0.45",
        ]);
        assert_eq!(a.exit_rule, ExitRuleArg::TpSl);
        assert_eq!(a.tp_price, Some(Decimal::from_str("0.85").unwrap()));
        assert_eq!(a.sl_price, Some(Decimal::from_str("0.45").unwrap()));
    }

    #[test]
    fn validate_rejects_tp_sl_without_thresholds() {
        let mut a = parse(&["--direction", "up", "--exit-rule", "tp-sl"]);
        a.tp_price = None;
        a.sl_price = Some(Decimal::from_str("0.45").unwrap());
        assert_eq!(a.validate(), Err(ConfigError::ExitRuleMissingThresholds));
    }

    #[test]
    fn validate_rejects_tp_le_sl() {
        let mut a = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                            "--tp-price", "0.50", "--sl-price", "0.50"]);
        assert_eq!(a.validate(), Err(ConfigError::ExitRuleInvertedThresholds));
    }

    #[test]
    fn validate_rejects_thresholds_out_of_range() {
        let mut a = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                            "--tp-price", "1.0", "--sl-price", "0.45"]);
        assert_eq!(a.validate(), Err(ConfigError::ExitRuleInvalidThreshold));
        let mut b = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                            "--tp-price", "0.85", "--sl-price", "0.0"]);
        assert_eq!(b.validate(), Err(ConfigError::ExitRuleInvalidThreshold));
    }

    #[test]
    fn validate_rejects_poll_secs_zero_or_huge() {
        let mut a = parse(&["--direction", "up"]);
        a.poll_secs = 0;
        assert_eq!(a.validate(), Err(ConfigError::InvalidPollSecs));
        a.poll_secs = 31;
        assert_eq!(a.validate(), Err(ConfigError::InvalidPollSecs));
    }

    #[test]
    fn validate_accepts_tp_sl_full() {
        let a = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                        "--tp-price", "0.85", "--sl-price", "0.45"]);
        assert!(a.validate().is_ok());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::config::`
Expected: FAIL — `cannot find type 'ExitRuleArg'`, `field 'exit_rule' does not exist`, etc.

- [ ] **Step 3: Implement ExitRuleArg + new fields + validation**

Replace `src/trader/config.rs` entirely with:

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ExitRuleArg { Hold, TpSl }

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
    #[arg(long, value_enum, default_value = "hold")]
    pub exit_rule: ExitRuleArg,
    #[arg(long)]
    pub tp_price: Option<Decimal>,
    #[arg(long)]
    pub sl_price: Option<Decimal>,
    #[arg(long, default_value = "5")]
    pub poll_secs: u32,
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
        if self.poll_secs == 0 || self.poll_secs > 30 {
            return Err(ConfigError::InvalidPollSecs);
        }
        if matches!(self.exit_rule, ExitRuleArg::TpSl) {
            let (tp, sl) = match (self.tp_price, self.sl_price) {
                (Some(tp), Some(sl)) => (tp, sl),
                _ => return Err(ConfigError::ExitRuleMissingThresholds),
            };
            if tp <= Decimal::ZERO || tp >= Decimal::ONE
               || sl <= Decimal::ZERO || sl >= Decimal::ONE {
                return Err(ConfigError::ExitRuleInvalidThreshold);
            }
            if tp <= sl {
                return Err(ConfigError::ExitRuleInvertedThresholds);
            }
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
    #[error("poll-secs must be in 1..=30")]
    InvalidPollSecs,
    #[error("--exit-rule tp-sl requires --tp-price and --sl-price")]
    ExitRuleMissingThresholds,
    #[error("tp-price and sl-price must each be in (0, 1)")]
    ExitRuleInvalidThreshold,
    #[error("tp-price must be greater than sl-price")]
    ExitRuleInvertedThresholds,
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

    #[test]
    fn parses_exit_rule_hold_default() {
        let a = parse(&["--direction", "up"]);
        assert_eq!(a.exit_rule, ExitRuleArg::Hold);
        assert_eq!(a.tp_price, None);
        assert_eq!(a.sl_price, None);
        assert_eq!(a.poll_secs, 5);
    }

    #[test]
    fn parses_exit_rule_tp_sl_with_thresholds() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "tp-sl",
            "--tp-price", "0.85",
            "--sl-price", "0.45",
        ]);
        assert_eq!(a.exit_rule, ExitRuleArg::TpSl);
        assert_eq!(a.tp_price, Some(Decimal::from_str("0.85").unwrap()));
        assert_eq!(a.sl_price, Some(Decimal::from_str("0.45").unwrap()));
    }

    #[test]
    fn validate_rejects_tp_sl_without_thresholds() {
        let mut a = parse(&["--direction", "up", "--exit-rule", "tp-sl"]);
        a.tp_price = None;
        a.sl_price = Some(Decimal::from_str("0.45").unwrap());
        assert_eq!(a.validate(), Err(ConfigError::ExitRuleMissingThresholds));
    }

    #[test]
    fn validate_rejects_tp_le_sl() {
        let a = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                        "--tp-price", "0.50", "--sl-price", "0.50"]);
        assert_eq!(a.validate(), Err(ConfigError::ExitRuleInvertedThresholds));
    }

    #[test]
    fn validate_rejects_thresholds_out_of_range() {
        let a = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                        "--tp-price", "1.0", "--sl-price", "0.45"]);
        assert_eq!(a.validate(), Err(ConfigError::ExitRuleInvalidThreshold));
        let b = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                        "--tp-price", "0.85", "--sl-price", "0.0"]);
        assert_eq!(b.validate(), Err(ConfigError::ExitRuleInvalidThreshold));
    }

    #[test]
    fn validate_rejects_poll_secs_zero_or_huge() {
        let mut a = parse(&["--direction", "up"]);
        a.poll_secs = 0;
        assert_eq!(a.validate(), Err(ConfigError::InvalidPollSecs));
        a.poll_secs = 31;
        assert_eq!(a.validate(), Err(ConfigError::InvalidPollSecs));
    }

    #[test]
    fn validate_accepts_tp_sl_full() {
        let a = parse(&["--direction", "up", "--exit-rule", "tp-sl",
                        "--tp-price", "0.85", "--sl-price", "0.45"]);
        assert!(a.validate().is_ok());
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::config::`
Expected: PASS — all 16 tests green (8 original + 8 new).

- [ ] **Step 5: Commit**

```bash
git add src/trader/config.rs
git commit -m "feat(trader): --exit-rule + --tp-price + --sl-price + --poll-secs CLI flags"
```

---

## Task 3: MidwindowPriceFetcher trait

**Files:**
- Create: `src/trader/price.rs`
- Modify: `src/trader/mod.rs`

- [ ] **Step 1: Add module declaration**

Edit `src/trader/mod.rs` — add at end:

```rust
pub mod price;
```

- [ ] **Step 2: Write the failing test**

Create `src/trader/price.rs`:

```rust
use crate::trader::errors::PriceError;
use async_trait::async_trait;
use rust_decimal::Decimal;

#[async_trait]
pub trait MidwindowPriceFetcher: Send + Sync {
    /// Fetch the current bid for `token_id`. Returns `Err` on transient failure
    /// (network/decode); caller should log and retry on the next poll tick.
    async fn current_bid(&self, token_id: &str) -> Result<Decimal, PriceError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use std::sync::Mutex;

    pub struct StubPriceFetcher {
        pub responses: Mutex<Vec<Result<Decimal, PriceError>>>,
    }
    impl StubPriceFetcher {
        pub fn new(responses: Vec<Result<Decimal, PriceError>>) -> Self {
            Self { responses: Mutex::new(responses) }
        }
    }
    #[async_trait]
    impl MidwindowPriceFetcher for StubPriceFetcher {
        async fn current_bid(&self, _token_id: &str) -> Result<Decimal, PriceError> {
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                Err(PriceError::Network("queue empty".into()))
            } else {
                q.remove(0)
            }
        }
    }

    #[tokio::test]
    async fn stub_dispenses_responses_in_order() {
        let f = StubPriceFetcher::new(vec![
            Ok(Decimal::from_str("0.50").unwrap()),
            Err(PriceError::Network("502".into())),
            Ok(Decimal::from_str("0.85").unwrap()),
        ]);
        assert_eq!(f.current_bid("tok").await.unwrap(), Decimal::from_str("0.50").unwrap());
        assert!(matches!(f.current_bid("tok").await, Err(PriceError::Network(_))));
        assert_eq!(f.current_bid("tok").await.unwrap(), Decimal::from_str("0.85").unwrap());
    }

    #[tokio::test]
    async fn stub_returns_err_when_drained() {
        let f = StubPriceFetcher::new(vec![]);
        assert!(matches!(f.current_bid("tok").await, Err(PriceError::Network(_))));
    }
}
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test --lib trader::price::`
Expected: PASS — 2 tests green. (Trait + stub compile, stub behaves as expected.)

- [ ] **Step 4: Commit**

```bash
git add src/trader/price.rs src/trader/mod.rs
git commit -m "feat(trader): MidwindowPriceFetcher trait + StubPriceFetcher for tests"
```

---

## Task 4: ExitWatcher (polling loop)

**Files:**
- Create: `src/trader/exit_watcher.rs`
- Modify: `src/trader/mod.rs`

- [ ] **Step 1: Add module declaration**

Edit `src/trader/mod.rs` — add at end:

```rust
pub mod exit_watcher;
```

- [ ] **Step 2: Write the failing tests**

Create `src/trader/exit_watcher.rs`:

```rust
use crate::trader::price::MidwindowPriceFetcher;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitConfig {
    pub tp_price: Decimal,
    pub sl_price: Decimal,
    pub poll: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExitKind { Tp, Sl }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExitTrigger {
    pub kind: ExitKind,
    pub bid: Decimal,
}

pub struct ExitWatcher {
    fetcher: Arc<dyn MidwindowPriceFetcher>,
    cfg: ExitConfig,
}

impl ExitWatcher {
    pub fn new(fetcher: Arc<dyn MidwindowPriceFetcher>, cfg: ExitConfig) -> Self {
        Self { fetcher, cfg }
    }

    /// Polls until trigger fires OR `deadline` reached.
    /// Returns `Some(trigger)` on TP/SL hit, `None` on deadline.
    pub async fn watch(
        &self,
        token_id: &str,
        deadline: tokio::time::Instant,
    ) -> Option<ExitTrigger> {
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return None;
            }
            let sleep_until = (now + self.cfg.poll).min(deadline);
            tokio::time::sleep_until(sleep_until).await;
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            match self.fetcher.current_bid(token_id).await {
                Err(e) => {
                    tracing::warn!("exit-watcher price fetch failed: {e}; skipping tick");
                    continue;
                }
                Ok(bid) => {
                    if bid >= self.cfg.tp_price {
                        return Some(ExitTrigger { kind: ExitKind::Tp, bid });
                    }
                    if bid <= self.cfg.sl_price {
                        return Some(ExitTrigger { kind: ExitKind::Sl, bid });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trader::errors::PriceError;
    use crate::trader::price::tests::StubPriceFetcher;
    use rust_decimal_macros::dec;
    use std::str::FromStr;
    use std::sync::Arc;

    fn cfg() -> ExitConfig {
        ExitConfig {
            tp_price: dec!(0.85),
            sl_price: dec!(0.45),
            poll: Duration::from_millis(100),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn tp_triggers_on_first_crossing() {
        let f = Arc::new(StubPriceFetcher::new(vec![
            Ok(dec!(0.50)),
            Ok(dec!(0.70)),
            Ok(dec!(0.85)),
        ]));
        let w = ExitWatcher::new(f, cfg());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let t = w.watch("tok", deadline).await.expect("trigger");
        assert_eq!(t.kind, ExitKind::Tp);
        assert_eq!(t.bid, dec!(0.85));
    }

    #[tokio::test(start_paused = true)]
    async fn sl_triggers_on_first_crossing() {
        let f = Arc::new(StubPriceFetcher::new(vec![
            Ok(dec!(0.50)),
            Ok(dec!(0.45)),  // exactly at threshold counts
        ]));
        let w = ExitWatcher::new(f, cfg());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let t = w.watch("tok", deadline).await.expect("trigger");
        assert_eq!(t.kind, ExitKind::Sl);
        assert_eq!(t.bid, dec!(0.45));
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_returns_none_when_no_trigger() {
        // Always 0.50 → never crosses tp=0.85 or sl=0.45
        let mut responses = vec![];
        for _ in 0..1000 { responses.push(Ok(dec!(0.50))); }
        let f = Arc::new(StubPriceFetcher::new(responses));
        let w = ExitWatcher::new(f, cfg());
        let deadline = tokio::time::Instant::now() + Duration::from_millis(350);
        let outcome = w.watch("tok", deadline).await;
        assert!(outcome.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn fetcher_error_is_skipped_and_polling_continues() {
        let f = Arc::new(StubPriceFetcher::new(vec![
            Err(PriceError::Network("502".into())),
            Err(PriceError::Decode("bad json".into())),
            Ok(dec!(0.85)),
        ]));
        let w = ExitWatcher::new(f, cfg());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let t = w.watch("tok", deadline).await.expect("trigger");
        assert_eq!(t.kind, ExitKind::Tp);
    }

    #[tokio::test(start_paused = true)]
    async fn tp_wins_when_both_crossed_simultaneously() {
        // bid=0.90 — above tp (0.85) AND would also be above sl (0.45). Tp checked first.
        // Construct a config where both would trigger to make the precedence explicit.
        let cfg_overlap = ExitConfig {
            tp_price: dec!(0.50),
            sl_price: dec!(0.95),  // intentionally inverted to force overlap
            poll: Duration::from_millis(100),
        };
        let f = Arc::new(StubPriceFetcher::new(vec![Ok(dec!(0.80))]));
        let w = ExitWatcher::new(f, cfg_overlap);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        let t = w.watch("tok", deadline).await.expect("trigger");
        assert_eq!(t.kind, ExitKind::Tp,
                   "tp branch must be checked before sl branch");
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_in_the_past_returns_none_immediately() {
        let f = Arc::new(StubPriceFetcher::new(vec![Ok(dec!(0.50))]));
        let w = ExitWatcher::new(f, cfg());
        let deadline = tokio::time::Instant::now() - Duration::from_secs(1);
        let outcome = w.watch("tok", deadline).await;
        assert!(outcome.is_none());
    }

    #[test]
    fn exit_kind_serializes_distinctly() {
        let tp = serde_json::to_string(&ExitKind::Tp).unwrap();
        let sl = serde_json::to_string(&ExitKind::Sl).unwrap();
        assert_ne!(tp, sl);
    }
}
```

Note: this references `crate::trader::price::tests::StubPriceFetcher` as a `pub` test helper. The Task 3 stub already has `pub` on the struct.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib trader::exit_watcher::`
Expected: FAIL — `cannot find type 'ExitWatcher'` initially. (Code in step 2 is both test and impl in the same file — running tests will check the impl block compiles AND the tests pass.)

If the impl from Step 2 was written, this should already PASS — that's fine. Skip to Step 5.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::exit_watcher::`
Expected: PASS — 6 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/trader/exit_watcher.rs src/trader/mod.rs
git commit -m "feat(trader): ExitWatcher polling loop + ExitConfig + ExitTrigger"
```

---

## Task 5: ExitTriggered event variant

**Files:**
- Modify: `src/trader/event.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/trader/event.rs` inside the `#[cfg(test)] mod tests` block, before the closing `}`:

```rust
    #[test]
    fn exit_triggered_roundtrip() {
        use crate::trader::exit_watcher::ExitKind;
        let e = fake_event(TraderEventKind::ExitTriggered {
            kind: ExitKind::Tp,
            bid: Decimal::from_str("0.86").unwrap(),
            proceeds_usd: Decimal::from_str("8.40").unwrap(),
        });
        let back: TraderEvent =
            serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn exit_triggered_tp_and_sl_serialize_distinctly() {
        use crate::trader::exit_watcher::ExitKind;
        let tp = TraderEventKind::ExitTriggered {
            kind: ExitKind::Tp,
            bid: Decimal::from_str("0.85").unwrap(),
            proceeds_usd: Decimal::from_str("8.40").unwrap(),
        };
        let sl = TraderEventKind::ExitTriggered {
            kind: ExitKind::Sl,
            bid: Decimal::from_str("0.45").unwrap(),
            proceeds_usd: Decimal::from_str("4.50").unwrap(),
        };
        assert_ne!(serde_json::to_string(&tp).unwrap(),
                   serde_json::to_string(&sl).unwrap());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::event::`
Expected: FAIL — `no variant ExitTriggered on TraderEventKind`.

- [ ] **Step 3: Add the new variant**

Edit `src/trader/event.rs` — find the `pub enum TraderEventKind` block and add `ExitTriggered { ... }` before the closing `}`. Add an import for `ExitKind` at the top.

After change the top of the file looks like:

```rust
use crate::trader::errors::EmitError;
use crate::trader::exit_watcher::ExitKind;
use crate::trader::ladder::{Direction, LadderState, StopReason, WindowOutcome};
```

And the enum looks like (showing the new variant in context):

```rust
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
    ExitTriggered {
        kind: ExitKind,
        bid: Decimal,
        proceeds_usd: Decimal,
    },
    SellFilled { proceeds_usd: Decimal },
    SellRejected { reason: String },
    LadderUpdated { from_step: u8, to_step: u8, outcome: WindowOutcome },
    Alert { message: String },
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::event::`
Expected: PASS — all event tests green (existing 6 + new 2).

- [ ] **Step 5: Commit**

```bash
git add src/trader/event.rs
git commit -m "feat(trader): TraderEventKind::ExitTriggered { kind, bid, proceeds_usd }"
```

---

## Task 6: WindowConfig + WindowDeps gain price + exit fields

**Files:**
- Modify: `src/trader/window.rs`

This task only widens types — no new behavior. The `Option<ExitConfig>` is `None` everywhere, and the `price` dep is wired but unused. Existing tests must stay green.

- [ ] **Step 1: Inspect existing window.rs head**

Read `src/trader/window.rs` lines 1-25. Confirm `WindowDeps` and `WindowConfig` shapes match what the spec assumes.

- [ ] **Step 2: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `src/trader/window.rs`, inside the existing `mod tests`, near other helper fns (after `fn cfg() -> WindowConfig`):

```rust
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
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib trader::window::`
Expected: FAIL — `field 'exit' does not exist on WindowConfig`.

- [ ] **Step 4: Widen `WindowConfig` and `WindowDeps`**

Edit `src/trader/window.rs`. Replace the `WindowDeps` and `WindowConfig` structs near the top (lines 12-22):

```rust
use crate::trader::exit_watcher::ExitConfig;
use crate::trader::price::MidwindowPriceFetcher;

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
```

Update the existing test helper `fn cfg()` to return `exit: None`:

```rust
    fn cfg() -> WindowConfig {
        WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: None,
        }
    }
```

Update every existing `WindowDeps { … }` struct literal in `mod tests` to include `price: stub_price_fetcher_returning_const("0.50")`. Add this helper above the `fn cfg()` helper:

```rust
    fn stub_price(constant: &str) -> Arc<crate::trader::price::tests::StubPriceFetcher> {
        let value = Decimal::from_str(constant).unwrap();
        let mut q = vec![];
        for _ in 0..1000 { q.push(Ok(value)); }
        Arc::new(crate::trader::price::tests::StubPriceFetcher::new(q))
    }
```

For each existing test that builds `WindowDeps { … }`, add `price: stub_price("0.50"),` as a field. Tests affected (each present in `src/trader/window.rs` `#[cfg(test)] mod tests`):
- `happy_path_won`
- `happy_path_lost`
- `skip_market_not_found`
- `skip_gamma_api_error`
- `skip_price_outside_band`
- `skip_fok_failed`
- `skip_resolution_timeout`
- `won_but_sell_failed_emits_alert_and_returns_zero_proceeds`
- `happy_path_emits_expected_event_sequence`

The `cfg_with_exit` helper from Step 2 also belongs here (already added).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib trader::window::`
Expected: PASS — all 9 existing window tests + 1 new smoke test green (10 total). `run_window` body unchanged so behavior is identical.

- [ ] **Step 6: Verify trader binary still builds**

Run: `cargo build --bin poly-trader`
Expected: FAIL — `src/bin/poly-trader.rs` constructs `WindowConfig` and `WindowDeps` without the new fields. We will fix in Task 8.

This is acceptable mid-plan; we widen here and wire the binary later. Continue.

- [ ] **Step 7: Commit**

```bash
git add src/trader/window.rs
git commit -m "refactor(trader): widen WindowConfig/WindowDeps with optional exit + price fetcher"
```

---

## Task 7: run_window branches on cfg.exit (the heart of v1.5)

**Files:**
- Modify: `src/trader/window.rs`

- [ ] **Step 1: Write the failing tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
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
        let outcome = run_window(&deps, &cfg, &fresh_ladder(), 1700000300).await;
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
        let outcome = run_window(&deps, &cfg, &fresh_ladder(), 1700000300).await;
        assert!(matches!(outcome,
            WindowOutcome::Lost { ref spent_usd } if *spent_usd == Decimal::from_str("0.60").unwrap()));
        let kinds = emitter.kinds();
        assert!(kinds.iter().any(|k| matches!(k, TraderEventKind::ExitTriggered { kind: ExitKind::Sl, .. })));
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
```

You will also need this import inside the test module, near `use async_trait::async_trait;`:

```rust
    use crate::trader::exit_watcher::{ExitConfig, ExitKind};
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::window::`
Expected: FAIL — the new tp-sl path tests fail because `run_window` ignores `cfg.exit`.

- [ ] **Step 3: Implement the branch in run_window**

Edit `src/trader/window.rs`. Replace the body of `run_window` from "Step 4: await resolution" through the end of the function with:

```rust
    // Step 4: branch on exit rule
    let buy_dollars = buy_fill.dollars;
    match &cfg.exit {
        None => {
            // v1.1 path: hold to resolution, sell winner
            await_resolution_and_sweep(deps, ladder, &market, &token_id, &buy_fill).await
        }
        Some(exit_cfg) => {
            // v1.5 path: race ExitWatcher vs await_resolution
            run_with_tp_sl(deps, ladder, &market, &token_id, &buy_fill, exit_cfg, buy_dollars).await
        }
    }
}

/// v1.1 path: existing await_resolution + winner sweep, extracted unchanged.
async fn await_resolution_and_sweep(
    deps: &WindowDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    buy_fill: &FillResult,
) -> WindowOutcome {
    let resolution = match deps.resolver.await_resolution(market).await {
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
) -> WindowOutcome {
    use crate::trader::exit_watcher::{ExitKind, ExitWatcher};
    let watcher = ExitWatcher::new(deps.price.clone(), exit_cfg.clone());
    // Window closes 5 min after window_ts; allow a small grace for resolver post-close.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(290);

    let trigger: Option<crate::trader::exit_watcher::ExitTrigger> = tokio::select! {
        t = watcher.watch(token_id, deadline) => t,
        r = deps.resolver.await_resolution(market) => {
            return resolve_after_select(deps, ladder, token_id, buy_fill, r).await;
        }
    };

    let trig = match trigger {
        Some(t) => t,
        None => {
            // Watcher hit deadline without crossing tp/sl. Fall through to resolver.
            return await_resolution_and_sweep(deps, ladder, market, token_id, buy_fill).await;
        }
    };

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
    let kind = match trig.kind {
        ExitKind::Tp => crate::trader::exit_watcher::ExitKind::Tp,
        ExitKind::Sl => crate::trader::exit_watcher::ExitKind::Sl,
    };
    emit_kind(deps, ladder, TraderEventKind::ExitTriggered {
        kind,
        bid: trig.bid,
        proceeds_usd: sell_fill.dollars,
    }).await;
    emit_kind(deps, ladder, TraderEventKind::SellFilled { proceeds_usd: sell_fill.dollars }).await;
    if sell_fill.dollars > buy_dollars {
        WindowOutcome::Won { proceeds_usd: sell_fill.dollars }
    } else {
        WindowOutcome::Lost { spent_usd: buy_dollars - sell_fill.dollars }
    }
}

/// When resolver wins the select! before any tp/sl trigger.
async fn resolve_after_select(
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
```

Note the original `run_window` body up to and including "Step 3: FoK buy" stays unchanged. Only the section from "Step 4: await resolution" onward is replaced by the `match &cfg.exit { … }` block above. Add the three helper fns (`await_resolution_and_sweep`, `run_with_tp_sl`, `resolve_after_select`) below the closing `}` of `run_window`.

Add at top of file alongside existing uses:

```rust
use crate::trader::executor::FillResult;
use crate::trader::resolver::Resolution;
```

(Some are already imported in tests; ensure the non-test top of file has them.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::window::`
Expected: PASS — all 13 tests green (9 hold-path + 1 smoke + 3 tp-sl path).

- [ ] **Step 5: Run all trader unit tests**

Run: `cargo test --lib trader::`
Expected: PASS. The existing tests for ladder/state/event/etc. should not regress.

- [ ] **Step 6: Commit**

```bash
git add src/trader/window.rs
git commit -m "feat(trader): run_window branches on cfg.exit; tp/sl race via tokio::select!"
```

---

## Task 8: GammaPriceFetcher adapter

**Files:**
- Create: `src/trader/adapters/gamma_price_wrapper.rs`
- Modify: `src/trader/adapters/mod.rs`

- [ ] **Step 1: Inspect adapters/mod.rs**

Run: `cat src/trader/adapters/mod.rs` — note current `pub mod` declarations.

- [ ] **Step 2: Add module declaration**

Edit `src/trader/adapters/mod.rs` — add at end:

```rust
pub mod gamma_price_wrapper;
```

- [ ] **Step 3: Write the implementation**

Create `src/trader/adapters/gamma_price_wrapper.rs`:

```rust
use crate::trader::errors::PriceError;
use crate::trader::price::MidwindowPriceFetcher;
use async_trait::async_trait;
use reqwest::Client;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Fetches the current bid for a token from gamma-api's /markets endpoint.
///
/// Polymarket's gamma-api accepts `?clob_token_ids=<id>` and returns a market
/// payload whose `outcomePrices` reflects the latest mid (used here as bid
/// proxy). A cache-busting nonce is appended to defeat upstream caching, same
/// pattern as `GammaMarketDiscovery`.
pub struct GammaPriceFetcher {
    client: Client,
    base_url: String,
}

impl GammaPriceFetcher {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap(),
            base_url,
        }
    }
}

#[async_trait]
impl MidwindowPriceFetcher for GammaPriceFetcher {
    async fn current_bid(&self, token_id: &str) -> Result<Decimal, PriceError> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let url = format!(
            "{}/markets?clob_token_ids={token_id}&_t={nonce}",
            self.base_url
        );
        let resp = self
            .client
            .get(&url)
            .header("Cache-Control", "no-cache")
            .send()
            .await
            .map_err(|e| PriceError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(PriceError::Network(format!("HTTP {}", resp.status())));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| PriceError::Network(e.to_string()))?;
        decode_price_for_token(&body, token_id)
    }
}

/// Pure decoder. Pulls `outcomePrices` (JSON-encoded string array of two
/// stringified decimals) and `clobTokenIds` (likewise) from the first
/// market in the response array. Returns the price corresponding to
/// `token_id`'s position in `clobTokenIds`.
pub fn decode_price_for_token(body: &str, token_id: &str) -> Result<Decimal, PriceError> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| PriceError::Decode(format!("json: {e}")))?;
    let arr = v
        .as_array()
        .ok_or_else(|| PriceError::Decode("expected array".into()))?;
    let market = arr
        .first()
        .ok_or_else(|| PriceError::Decode("empty markets array".into()))?;

    let token_ids_raw = market
        .get("clobTokenIds")
        .and_then(|t| t.as_str())
        .ok_or_else(|| PriceError::Decode("missing clobTokenIds".into()))?;
    let token_ids: Vec<String> = serde_json::from_str(token_ids_raw)
        .map_err(|e| PriceError::Decode(format!("clobTokenIds: {e}")))?;

    let prices_raw = market
        .get("outcomePrices")
        .and_then(|p| p.as_str())
        .ok_or_else(|| PriceError::Decode("missing outcomePrices".into()))?;
    let prices: Vec<String> = serde_json::from_str(prices_raw)
        .map_err(|e| PriceError::Decode(format!("outcomePrices: {e}")))?;

    if token_ids.len() != prices.len() || token_ids.is_empty() {
        return Err(PriceError::Decode("clobTokenIds/outcomePrices size mismatch".into()));
    }
    let idx = token_ids
        .iter()
        .position(|t| t == token_id)
        .ok_or_else(|| PriceError::Decode(format!("token {token_id} not in market")))?;
    Decimal::from_str(&prices[idx])
        .map_err(|e| PriceError::Decode(format!("decimal parse: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn decodes_first_token_price() {
        let body = r#"[{
            "clobTokenIds": "[\"tok-up\",\"tok-down\"]",
            "outcomePrices": "[\"0.86\",\"0.14\"]"
        }]"#;
        let p = decode_price_for_token(body, "tok-up").unwrap();
        assert_eq!(p, Decimal::from_str("0.86").unwrap());
    }

    #[test]
    fn decodes_second_token_price() {
        let body = r#"[{
            "clobTokenIds": "[\"tok-up\",\"tok-down\"]",
            "outcomePrices": "[\"0.86\",\"0.14\"]"
        }]"#;
        let p = decode_price_for_token(body, "tok-down").unwrap();
        assert_eq!(p, Decimal::from_str("0.14").unwrap());
    }

    #[test]
    fn err_when_token_id_missing() {
        let body = r#"[{
            "clobTokenIds": "[\"tok-up\",\"tok-down\"]",
            "outcomePrices": "[\"0.86\",\"0.14\"]"
        }]"#;
        assert!(matches!(
            decode_price_for_token(body, "tok-other"),
            Err(PriceError::Decode(_))
        ));
    }

    #[test]
    fn err_when_outcome_prices_missing() {
        let body = r#"[{"clobTokenIds":"[\"tok-up\"]"}]"#;
        assert!(matches!(
            decode_price_for_token(body, "tok-up"),
            Err(PriceError::Decode(_))
        ));
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::adapters::gamma_price_wrapper::`
Expected: PASS — 4 tests green.

- [ ] **Step 5: Commit**

```bash
git add src/trader/adapters/mod.rs src/trader/adapters/gamma_price_wrapper.rs
git commit -m "feat(trader): GammaPriceFetcher adapter for mid-window bid polling"
```

---

## Task 9: Wire price fetcher + ExitConfig in poly-trader binary

**Files:**
- Modify: `src/bin/poly-trader.rs`

- [ ] **Step 1: Read existing wiring**

Read `src/bin/poly-trader.rs` lines 100-130 — note where `WindowDeps` and `WindowConfig` are constructed.

- [ ] **Step 2: Modify the binary**

Edit `src/bin/poly-trader.rs`. Find the existing `WindowDeps` and `WindowConfig` construction (around lines 109-122) and replace with:

```rust
    let price: Arc<dyn poly_tui::trader::price::MidwindowPriceFetcher> = Arc::new(
        poly_tui::trader::adapters::gamma_price_wrapper::GammaPriceFetcher::new(gamma_host.clone()),
    );

    // WindowExecutor adapter (binds run_window over our deps)
    let window_deps = Arc::new(WindowDeps {
        market: market.clone(),
        executor: executor.clone(),
        resolver: resolver.clone(),
        emitter: emitter.clone(),
        price: price.clone(),
    });
    let exit_cfg = match args.exit_rule {
        poly_tui::trader::config::ExitRuleArg::Hold => None,
        poly_tui::trader::config::ExitRuleArg::TpSl => Some(
            poly_tui::trader::exit_watcher::ExitConfig {
                tp_price: args.tp_price.expect("validated: --tp-price required"),
                sl_price: args.sl_price.expect("validated: --sl-price required"),
                poll: std::time::Duration::from_secs(args.poll_secs as u64),
            }
        ),
    };
    let window_cfg = WindowConfig {
        band_min: args.band_min,
        band_max: args.band_max,
        exit: exit_cfg,
    };
```

You may need to update existing imports near the top of `src/bin/poly-trader.rs`. Add:

```rust
use poly_tui::trader::config::ExitRuleArg;
```

(or use the fully qualified path inline as shown above — either works.)

Also confirm `args.validate()` is called early in `main` before any work — if not present, add `args.validate().context("config validation")?;` immediately after `args = TraderArgs::parse()`.

- [ ] **Step 3: Verify binary builds**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean (warnings ok).

- [ ] **Step 4: Smoke-test the new flag via --help**

Run: `./target/debug/poly-trader.exe --help`
Expected: STDOUT shows `--exit-rule <EXIT_RULE>`, `--tp-price <TP_PRICE>`, `--sl-price <SL_PRICE>`, `--poll-secs <POLL_SECS>` flags listed.

- [ ] **Step 5: Smoke-test that hold default still parses**

Run: `./target/debug/poly-trader.exe --direction up --base 5 --dry-run --max-windows 0 --help`
Expected: Help prints (and exits before doing work). No clap parse errors.

- [ ] **Step 6: Smoke-test that tp-sl with thresholds parses**

Run: `./target/debug/poly-trader.exe --direction up --base 5 --dry-run --max-windows 0 --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45 --help`
Expected: Help prints. No parse errors.

- [ ] **Step 7: Commit**

```bash
git add src/bin/poly-trader.rs
git commit -m "feat(trader): wire GammaPriceFetcher + ExitConfig into poly-trader binary"
```

---

## Task 10: E2E test — ExitTriggered reaches Redis stream

**Files:**
- Modify: `tests/e2e_trader.rs`

- [ ] **Step 1: Append the new test**

Append to `tests/e2e_trader.rs` (above the closing module if any, or end of file):

```rust
#[tokio::test]
#[ignore]
async fn e2e_exit_triggered_event_reaches_stream() {
    // Build a TraderEvent with TraderEventKind::ExitTriggered, push it through
    // RedisTraderStream, then read it back via TraderEventStream and verify
    // it round-trips with kind/bid/proceeds intact. Validates the new variant
    // is wire-compatible with the existing event log.
    use poly_tui::trader::event::{TraderEvent, TraderEventKind};
    use poly_tui::trader::exit_watcher::ExitKind;
    use poly_tui::trader::ladder::{Direction, LadderState};
    use std::str::FromStr;

    let (_node, url) = start_redis().await;
    let emitter = Arc::new(RedisTraderStream::connect(&url).await.unwrap());

    let ladder = LadderState::new(Direction::Up, Decimal::from(5), 5, Utc::now());
    let session_id = ladder.session_id;
    let event = TraderEvent {
        ts: Utc::now(),
        session_id,
        kind: TraderEventKind::ExitTriggered {
            kind: ExitKind::Tp,
            bid: Decimal::from_str("0.86").unwrap(),
            proceeds_usd: Decimal::from_str("8.40").unwrap(),
        },
        ladder,
    };
    emitter.emit(&event).await.unwrap();

    let stream = TraderEventStream::connect(&url).await.unwrap();
    let mut received = stream.subscribe(session_id, None).await.unwrap();
    let recv_event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        received.recv(),
    ).await.expect("recv timeout").expect("stream closed");

    match recv_event.kind {
        TraderEventKind::ExitTriggered { kind, bid, proceeds_usd } => {
            assert_eq!(kind, ExitKind::Tp);
            assert_eq!(bid, Decimal::from_str("0.86").unwrap());
            assert_eq!(proceeds_usd, Decimal::from_str("8.40").unwrap());
        }
        other => panic!("unexpected event kind: {other:?}"),
    }
}
```

If your `TraderEventStream` API differs from `subscribe(session_id, None)` / `recv()`, inspect `src/tui/events.rs` first and adapt the read side. The point of the test is: emit → readback round-trip of the new variant.

- [ ] **Step 2: Verify tests compile (do not run)**

Run: `cargo build --tests --test e2e_trader`
Expected: Compiles clean.

- [ ] **Step 3: (Optional) run if Docker available**

Run: `cargo test --test e2e_trader e2e_exit_triggered_event_reaches_stream -- --ignored`
Expected: PASS in <10s. If Docker unavailable, skip.

- [ ] **Step 4: Commit**

```bash
git add tests/e2e_trader.rs
git commit -m "test(trader): e2e ExitTriggered roundtrips through Redis stream"
```

---

## Task 11: README + TODO

**Files:**
- Modify: `README.md`
- Modify: `TODO.md`

- [ ] **Step 1: Add v1.5 section to README**

Edit `README.md`. Find the existing "Trader" section. Below the existing dry-run examples, add:

````markdown
### Take-profit / stop-loss exits (v1.5, strategy 4)

Backtest validated strategy 4 (TP+SL asymmetric) profitable across three independent 30-day samples (+$5,088 / +$9,802 / +$7,747). To run it live (start in dry-run):

```bash
poly-trader --direction up --base 5 --dry-run \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45
```

| Flag | Default | Notes |
|---|---|---|
| `--exit-rule` | `hold` | `hold` = v1.1 behavior. `tp-sl` enables strategy 4. |
| `--tp-price` | — | Required for `tp-sl`. UP-token bid level that triggers a take-profit sell. |
| `--sl-price` | — | Required for `tp-sl`. UP-token bid level that triggers a stop-loss sell. |
| `--poll-secs` | `5` | Gamma poll cadence during the window (1..=30). |

**Expected event order** (one TP-trigger window):
`WindowOpening → EntryDecision{Enter} → OrderPlaced → OrderFilled → ExitTriggered{Tp,bid,proceeds} → SellFilled → LadderUpdated`

**Inspect trigger rate from Redis:**

```bash
docker exec poly-redis redis-cli XREVRANGE poly:prod:trader:events + - COUNT 100 \
  | grep -c ExitTriggered
```

Backtest distribution: ~29% TP, ~58% SL, ~13% deadline fall-through. If your live trace is far off, suspect gamma `last_price` lag.

**Fall back to v1.1:** omit `--exit-rule` (or pass `--exit-rule hold`). No state migration needed; the ladder is mode-agnostic.
````

Find the Roadmap table and add v1.5:

```markdown
- **v1.5** ✅ — TP/SL exits in trader (`--exit-rule tp-sl`)
```

Find the Documentation list and add:

```markdown
- `docs/superpowers/specs/2026-05-10-trader-tp-sl-design.md` — v1.5 design
- `docs/superpowers/plans/2026-05-10-trader-tp-sl.md` — v1.5 plan
```

- [ ] **Step 2: Tick v1.5 in TODO.md**

Edit `TODO.md`. Insert before any existing "v1.4" or "v1.3" sections (or wherever recent versions are tracked):

```markdown
## v1.5 — TP/SL exits in trader ✅ COMPLETE

Strategy 4 (validated by backtest +$5K-$10K/30d) lives behind `--exit-rule tp-sl --tp-price 0.85 --sl-price 0.45`. Default behavior unchanged. See `docs/superpowers/specs/2026-05-10-trader-tp-sl-design.md`.

- [x] CLI: `--exit-rule {hold|tp-sl}`, `--tp-price`, `--sl-price`, `--poll-secs`
- [x] `MidwindowPriceFetcher` trait + `GammaPriceFetcher` adapter
- [x] `ExitWatcher` polling loop, `ExitConfig`, `ExitTrigger`, `ExitKind`
- [x] `run_window` branches on `cfg.exit`, races watcher vs resolver via `tokio::select!`
- [x] `TraderEventKind::ExitTriggered { kind, bid, proceeds_usd }`
- [x] Outcome mapped from `proceeds vs cost`; ladder math unchanged
- [x] E2E: `ExitTriggered` round-trips through Redis stream
```

- [ ] **Step 3: Verify README/TODO render**

Run: `cargo build --bin poly-trader` (sanity — no breakage from doc edits).
Expected: Compiles clean.

- [ ] **Step 4: Commit**

```bash
git add README.md TODO.md
git commit -m "docs: README + TODO updated for v1.5 TP/SL trader"
```

---

## Task 12: Coverage gate

**Files:** none (verification only).

- [ ] **Step 1: Run coverage**

Run: `cargo llvm-cov --lib --tests --no-fail-fast`
Expected: Reports total ≥80% lines on the trader module. Files of interest:
- `src/trader/exit_watcher.rs` — should be ≥90% (pure logic)
- `src/trader/window.rs` — should be ≥85% (race paths exercised by tests)
- `src/trader/price.rs` — trait definition; only stub exercised
- `src/trader/adapters/gamma_price_wrapper.rs` — decoder ≥90%; HTTP path is network-only and acceptably uncovered

If `cargo-llvm-cov` is not installed: skip and note in the final report.

- [ ] **Step 2: No commit (read-only)**

If coverage is below 80%, identify which file falls short and add targeted unit tests before proceeding. Do not lower the gate.

---

## Self-review

After all tasks:

1. **Spec coverage**
   - Spec §"Architecture" → Tasks 4 + 7 (watcher + select)
   - Spec §"CLI surface" → Task 2
   - Spec §"Components/price.rs" → Task 3
   - Spec §"Components/exit_watcher.rs" → Task 4
   - Spec §"Components/window.rs" → Tasks 6 + 7
   - Spec §"Components/event.rs" → Task 5
   - Spec §"Components/poly-trader.rs" → Task 9
   - Spec §"Outcome mapping" → Task 7 (proceeds-vs-cost branch)
   - Spec §"Error & edge handling" → Tasks 4 (fetch error skip) + 7 (sell fail / deadline / select race)
   - Spec §"Testing strategy/Unit" → Tasks 1, 2, 3, 4, 5, 7, 8 (all covered)
   - Spec §"Testing strategy/E2E" → Task 10
   - Spec §"Coverage gate" → Task 12
   - Spec §"Migration / rollback" → Task 9 default `hold` + Task 11 README "fall back"

2. **Placeholder scan** — none found. Every step has full code or a concrete shell command.

3. **Type consistency** — `ExitConfig`, `ExitKind`, `ExitTrigger`, `ExitWatcher`, `MidwindowPriceFetcher`, `GammaPriceFetcher`, `PriceError`, `ExitRuleArg`, `TraderEventKind::ExitTriggered { kind, bid, proceeds_usd }` are spelled identically across Tasks 3, 4, 5, 7, 8, 9.
