# v1.8 — Trader `--exit-rule hold-early-exit` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a new trader exit mode `--exit-rule hold-early-exit` that buys at entry, holds, then market-sells at `t = --exit-at-secs` (default 270 for 5-min windows). Avoids the post-resolution redemption path entirely (no MATIC needed). Driven by v1.7.5 real-trade backtest finding: strategy `13_hold_early_exit_270` returns **+$1,505 over 8503 windows**, on par with the existing `1_hold_martingale` baseline but without the redemption blocker.

**Architecture:** New `ExitRuleArg::HoldEarlyExit` variant. New `--exit-at-secs <u32>` CLI flag (validated 1..=window_seconds-30). Window dispatch gains a new path `run_hold_early_exit(deps, ladder, market, token_id, buy_fill, exit_at_secs, window_ts, window_seconds)` that does: taker BUY → `tokio::time::sleep_until(window_start + exit_at_secs)` → `executor.sell_market(token_id, shares)` → return `Won`/`Lost` based on proceeds. No ExitWatcher, no resolver wait.

**Tech Stack:** Existing — tokio (`sleep_until` + monotonic deadline), rust_decimal, clap. No new deps.

**Spec source:** Backtest finding documented in `TODO.md` v1.7.5 section + `report-real-30d.html` (strategy 13 result).

## Build hygiene — STRICT

NEVER bare `cargo build`. Always scope:
- `cargo build --bin poly-trader`
- `cargo test --lib trader::`
- `cargo build --tests --test trader_e2e` (or similar)

DO NOT touch `src/backtest/`, `src/bin/poly-backtest.rs`, `src/positions.rs`, `src/bin/poly-redeem.rs`, `src/bin/poly-tui.rs`. v1.8 is **trader-only**.

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `src/trader/config.rs` | modify | Add `ExitRuleArg::HoldEarlyExit` variant. Add `--exit-at-secs <u32>` flag (Option<u32>). Validation: required only when `HoldEarlyExit`; must be in `1..=window_seconds-30`. Reject `--maker` with `HoldEarlyExit`. |
| `src/trader/window.rs` | modify | `WindowConfig` gains `exit_at_secs: Option<u32>`. New dispatch arm in `run_window`. New helper `run_hold_early_exit(...)`. |
| `src/bin/poly-trader.rs` | modify | Match new `ExitRuleArg::HoldEarlyExit` when building `WindowConfig`. |
| `tests/trader_hold_early_exit.rs` | **NEW** | Integration test driving full window with simulated executor + clock. |
| `features/trader_hold_early_exit.feature` | **NEW** (if BDD active) | BDD scenario: enter, wait, sell at t=270, no resolution. |
| `README.md` | modify | Document `--exit-rule hold-early-exit` + `--exit-at-secs`. |
| `TODO.md` | modify | Tick v1.8 ✅ COMPLETE; record live A/B plan. |

No new modules. No new dependencies.

---

## Task 0: Sanity baseline

**Files:** none.

- [ ] **Step 1: Working tree clean**

Run: `git status`
Expected: clean except `.claude/`, `tmp/`, untracked HTML reports.

- [ ] **Step 2: Trader tests green**

Run: `cargo test --lib trader::`
Expected: PASS. Record count for diff later.

- [ ] **Step 3: Trader binary builds**

Run: `cargo build --bin poly-trader`
Expected: clean build.

---

## Task 1: `ExitRuleArg::HoldEarlyExit` variant + `--exit-at-secs` flag

**Files:**
- Modify: `src/trader/config.rs`

Add the third variant and the new flag. Validation: `--exit-at-secs` is required when `--exit-rule hold-early-exit`, rejected otherwise (avoids confusion). Range check: must be ≥1 and ≤ `window_seconds - 30` (30s buffer to ensure market is still active for sell).

`--maker` remains rejected for everything except `tp-sl`.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/trader/config.rs`:

```rust
    #[test]
    fn parses_exit_rule_hold_early_exit_with_secs() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "hold-early-exit",
            "--exit-at-secs", "270",
        ]);
        assert_eq!(a.exit_rule, ExitRuleArg::HoldEarlyExit);
        assert_eq!(a.exit_at_secs, Some(270));
        assert!(a.validate().is_ok());
    }

    #[test]
    fn parses_exit_at_secs_default_is_none() {
        let a = parse(&["--direction", "up"]);
        assert_eq!(a.exit_at_secs, None);
    }

    #[test]
    fn validate_rejects_hold_early_exit_without_secs() {
        let mut a = parse(&["--direction", "up", "--exit-rule", "hold-early-exit"]);
        a.exit_at_secs = None;
        assert_eq!(a.validate(), Err(ConfigError::ExitAtSecsRequired));
    }

    #[test]
    fn validate_rejects_exit_at_secs_zero() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "hold-early-exit",
            "--exit-at-secs", "0",
        ]);
        assert_eq!(a.validate(), Err(ConfigError::ExitAtSecsOutOfRange));
    }

    #[test]
    fn validate_rejects_exit_at_secs_too_close_to_close() {
        // For 5-min window (300s), exit-at-secs must be <= 270 (300 - 30).
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "hold-early-exit",
            "--exit-at-secs", "290",
        ]);
        assert_eq!(a.validate(), Err(ConfigError::ExitAtSecsOutOfRange));
    }

    #[test]
    fn validate_rejects_exit_at_secs_for_non_hold_early_exit() {
        // exit-at-secs only valid with --exit-rule hold-early-exit
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "hold",
            "--exit-at-secs", "200",
        ]);
        assert_eq!(a.validate(), Err(ConfigError::ExitAtSecsWrongMode));
    }

    #[test]
    fn validate_rejects_maker_with_hold_early_exit() {
        let a = parse(&[
            "--direction", "up",
            "--exit-rule", "hold-early-exit",
            "--exit-at-secs", "270",
            "--maker",
        ]);
        assert_eq!(a.validate(), Err(ConfigError::MakerRequiresTpSl));
    }

    #[test]
    fn validate_hold_early_exit_with_15min_window() {
        // For 15-min window (900s), exit-at-secs must be <= 870 (900 - 30).
        let a = parse(&[
            "--direction", "up",
            "--window-minutes", "15",
            "--exit-rule", "hold-early-exit",
            "--exit-at-secs", "870",
        ]);
        assert!(a.validate().is_ok());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::config`
Expected: 7 NEW failures (variant missing, fields missing, error variants missing).

- [ ] **Step 3: Add `HoldEarlyExit` variant and `exit_at_secs` field**

Edit `src/trader/config.rs`.

Change:
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ExitRuleArg { Hold, TpSl }
```
to:
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum ExitRuleArg {
    Hold,
    TpSl,
    /// v1.8: Hold position, then market-sell at `--exit-at-secs`. Avoids
    /// resolution path entirely (no on-chain redeem; no MATIC needed).
    HoldEarlyExit,
}
```

In `TraderArgs`, after `sl_price`, add:
```rust
    /// Seconds into the window at which to market-sell. Required when
    /// --exit-rule is hold-early-exit. Rejected for other exit rules.
    /// Range: 1..=(window_seconds - 30) to ensure the orderbook is still
    /// active. Backtest-validated default: 270 (for 5-min windows).
    #[arg(long)]
    pub exit_at_secs: Option<u32>,
```

- [ ] **Step 4: Add new `ConfigError` variants**

In the `ConfigError` enum, add:
```rust
    #[error("--exit-rule hold-early-exit requires --exit-at-secs")]
    ExitAtSecsRequired,
    #[error("--exit-at-secs only valid with --exit-rule hold-early-exit")]
    ExitAtSecsWrongMode,
    #[error("--exit-at-secs must be in 1..=(window-seconds - 30)")]
    ExitAtSecsOutOfRange,
```

- [ ] **Step 5: Extend `validate()`**

After the existing `TpSl` validation block and before the `--maker` check, add:

```rust
        let window_seconds = (self.window_minutes as u32) * 60;
        match self.exit_rule {
            ExitRuleArg::HoldEarlyExit => {
                let secs = self.exit_at_secs.ok_or(ConfigError::ExitAtSecsRequired)?;
                if secs == 0 || secs > window_seconds.saturating_sub(30) {
                    return Err(ConfigError::ExitAtSecsOutOfRange);
                }
            }
            _ => {
                if self.exit_at_secs.is_some() {
                    return Err(ConfigError::ExitAtSecsWrongMode);
                }
            }
        }
```

Then update the `--maker` check so `HoldEarlyExit` is rejected too (already covered by `!matches!(..., TpSl)` — verify the existing line is correct).

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib trader::config`
Expected: all PASS — 7 new + existing.

- [ ] **Step 7: Commit**

```bash
git add src/trader/config.rs
git commit -m "feat(trader): add --exit-rule hold-early-exit and --exit-at-secs flag"
```

---

## Task 2: Thread `exit_at_secs` through `WindowConfig`

**Files:**
- Modify: `src/trader/window.rs`
- Modify: `src/bin/poly-trader.rs`

`WindowConfig` already has `exit: Option<ExitConfig>` for TP/SL. Add a parallel `exit_at_secs: Option<u32>` field that represents the new exit deadline. Only one of `exit` (TP/SL) and `exit_at_secs` is `Some` at runtime (mutually exclusive by config validation).

- [ ] **Step 1: Add `exit_at_secs` field to `WindowConfig`**

In `src/trader/window.rs`, modify the struct:

```rust
pub struct WindowConfig {
    pub band_min: Decimal,
    pub band_max: Decimal,
    pub exit: Option<ExitConfig>,
    /// v1.8: seconds into the window at which to market-sell.
    /// Mutually exclusive with `exit` — exactly zero or one is `Some`.
    pub exit_at_secs: Option<u32>,
    pub maker: bool,
    pub window_seconds: i64,
}
```

- [ ] **Step 2: Update `poly-trader.rs` to populate the field**

In `src/bin/poly-trader.rs`, in the `WindowConfig` construction:

```rust
    let window_cfg = WindowConfig {
        band_min: args.band_min,
        band_max: args.band_max,
        exit: exit_cfg,
        exit_at_secs: args.exit_at_secs,
        maker: args.maker,
        window_seconds,
    };
```

Also extend the existing `let exit_cfg = match args.exit_rule { ... }` to handle the new variant — `HoldEarlyExit` produces `None` for the `ExitConfig` (since it doesn't use ExitWatcher):

```rust
    let exit_cfg = match args.exit_rule {
        ExitRuleArg::Hold => None,
        ExitRuleArg::HoldEarlyExit => None,
        ExitRuleArg::TpSl => Some(ExitConfig {
            tp_price: args.tp_price.expect("validated: --tp-price required"),
            sl_price: args.sl_price.expect("validated: --sl-price required"),
            poll: std::time::Duration::from_secs(args.poll_secs as u64),
        }),
    };
```

- [ ] **Step 3: Update existing `WindowConfig { ... }` constructions in tests**

Run: `grep -rn 'WindowConfig {' src/ tests/`

For every match (likely in `tests/` integration tests), append `exit_at_secs: None,` to the literal.

- [ ] **Step 4: Verify build**

Run: `cargo build --bin poly-trader`
Expected: clean. The new field is unused in window.rs runtime yet (only constructed) — that's OK for now.

- [ ] **Step 5: Run tests**

Run: `cargo test --lib trader::`
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add src/trader/window.rs src/bin/poly-trader.rs tests/
git commit -m "feat(trader): thread exit_at_secs through WindowConfig"
```

(If `tests/` had no changes, drop from `git add`.)

---

## Task 3: `run_hold_early_exit` window path

**Files:**
- Modify: `src/trader/window.rs`

Implement the new dispatch path. After successful BUY:
1. Compute deadline = `window_open + exit_at_secs` (in unix seconds).
2. `tokio::time::sleep` until that deadline (using monotonic-clock-relative duration).
3. Call `executor.sell_market(token_id, buy_fill.shares).await`.
4. Compute proceeds; emit appropriate events; return `Won` / `Lost`.

Edge cases:
- Sleep duration < 0 (shouldn't happen if exit_at_secs valid, but guard with `.max(0)`): proceed to sell immediately.
- `sell_market` fails: emit `SellRejected` + `Alert`, return `Won { proceeds_usd: Decimal::ZERO }` (consistent with v1.1 winner-sweep failure behaviour — shares stuck, no PnL credit).

- [ ] **Step 1: Write the failing integration test**

Create `tests/trader_hold_early_exit.rs`:

```rust
use poly_tui::trader::config::ExitRuleArg;
use poly_tui::trader::event::TraderEventKind;
use poly_tui::trader::executor::FillResult;
use poly_tui::trader::ladder::{Direction, LadderState, WindowOutcome};
use poly_tui::trader::market::WindowMarket;
use poly_tui::trader::window::{run_window, WindowConfig, WindowDeps};
use rust_decimal_macros::dec;
use std::sync::Arc;

mod common;
use common::{
    FakeEmitter, FakeEvents, FakePrice, FakeResolver, SimulatedExecutorWrap,
    StubMarketDiscovery,
};

#[tokio::test(start_paused = true)]
async fn hold_early_exit_buys_waits_sells_at_deadline() {
    // Fixture: window opens at t=1000. Market ask=0.50, bid moves to 0.55
    // by t=270. We expect: BUY at 0.50, wait until t=1270, SELL at 0.55.
    let window_ts: i64 = 1000;
    let market = WindowMarket {
        slug: "test".into(),
        condition_id: "0xabc".into(),
        up_token_id: "u".into(),
        down_token_id: "d".into(),
        up_ask: dec!(0.50),
        up_bid: dec!(0.50),
        down_ask: dec!(0.50),
        down_bid: dec!(0.50),
    };
    let executor = SimulatedExecutorWrap::new(
        FillResult { fill_price: dec!(0.50), shares: dec!(10), dollars: dec!(5) },
        dec!(0.55), // sell bid at deadline
    );

    let deps = WindowDeps {
        market: Arc::new(StubMarketDiscovery::new(market)),
        executor: Arc::new(executor.clone()),
        resolver: Arc::new(FakeResolver::never()),
        emitter: Arc::new(FakeEmitter::new()),
        price: Arc::new(FakePrice::new(dec!(0.55))),
        events: Arc::new(FakeEvents::empty()),
    };

    let cfg = WindowConfig {
        band_min: dec!(0.45),
        band_max: dec!(0.55),
        exit: None,
        exit_at_secs: Some(270),
        maker: false,
        window_seconds: 300,
    };

    let ladder = LadderState::new(
        Direction::Up, dec!(5), 5, chrono::Utc::now(),
    );

    // Run, advancing tokio's paused clock past 270s.
    let task = tokio::spawn({
        let deps = deps_clone(&deps);
        let cfg = cfg.clone();
        let ladder = ladder.clone();
        async move { run_window(&deps, &cfg, &ladder, window_ts).await }
    });

    tokio::time::advance(std::time::Duration::from_secs(280)).await;
    let outcome = task.await.unwrap();

    match outcome {
        WindowOutcome::Won { proceeds_usd } => {
            // 10 shares × 0.55 = 5.50
            assert!(proceeds_usd >= dec!(5.45) && proceeds_usd <= dec!(5.55),
                    "got proceeds_usd={proceeds_usd}");
        }
        _ => panic!("expected Won, got {outcome:?}"),
    }

    // Confirm executor saw: 1 BUY, 1 sell_market (not sell_winner)
    assert_eq!(executor.buy_count(), 1);
    assert_eq!(executor.sell_market_count(), 1);
    assert_eq!(executor.sell_winner_count(), 0);
}
```

NOTE: This test uses `common::*` helpers. If the existing `tests/common.rs` module doesn't have `StubMarketDiscovery` or the executor wrapper with counters, mark this step as DONE_WITH_CONCERNS and provide a minimal stub inline. Don't refactor the existing test helpers in this task.

- [ ] **Step 2: Run test to verify it fails to compile**

Run: `cargo test --test trader_hold_early_exit`
Expected: compile error or test runtime failure — the dispatch arm doesn't exist yet.

- [ ] **Step 3: Add dispatch arm in `run_window`**

In `src/trader/window.rs`, locate the `match &cfg.exit { None => ..., Some(exit_cfg) => ... }` block. Restructure it to check `exit_at_secs` first:

```rust
    // Step 4: branch on exit rule
    if let Some(exit_at_secs) = cfg.exit_at_secs {
        return run_hold_early_exit(
            deps, ladder, &market, &token_id, &buy_fill,
            exit_at_secs, window_ts, cfg.window_seconds,
        ).await;
    }
    match &cfg.exit {
        None => {
            await_resolution_and_sweep(deps, ladder, &market, &token_id, &buy_fill).await
        }
        Some(exit_cfg) => {
            run_with_tp_sl(deps, ladder, &market, &token_id, &buy_fill, exit_cfg, window_ts).await
        }
    }
```

- [ ] **Step 4: Implement `run_hold_early_exit`**

Add the function below `await_resolution_and_sweep`:

```rust
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

    // Hard cap: don't sleep past window close. Defensive (validate() should
    // reject exit_at_secs > window_seconds - 30 already).
    let cap = (window_seconds - 30).max(0) as u64;
    let wait_secs = wait_secs.min(cap);

    if wait_secs > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
    }

    // Market-sell the entire position at the current bid.
    let sell_fill = match deps.executor.sell_market(token_id, buy_fill.shares).await {
        Ok(f) => f,
        Err(e) => {
            emit_kind(deps, ladder, TraderEventKind::SellRejected {
                reason: format!("{e}"),
            }).await;
            emit_kind(deps, ladder, TraderEventKind::Alert {
                message: format!("hold-early-exit sell failed; shares stuck for token {token_id}"),
            }).await;
            return WindowOutcome::Won { proceeds_usd: Decimal::ZERO };
        }
    };
    emit_kind(deps, ladder, TraderEventKind::SellFilled {
        proceeds_usd: sell_fill.dollars,
    }).await;

    // Determine win/lose by comparing proceeds vs cost.
    if sell_fill.dollars > buy_fill.dollars {
        WindowOutcome::Won { proceeds_usd: sell_fill.dollars }
    } else {
        WindowOutcome::Lost { spent_usd: buy_fill.dollars - sell_fill.dollars }
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --test trader_hold_early_exit`
Expected: PASS.

If the test still fails because `tests/common.rs` lacks helpers, document this in your report — the controller will decide whether to add helpers or test the path differently.

Also run unit tests to ensure no regressions:

Run: `cargo test --lib trader::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/trader/window.rs tests/trader_hold_early_exit.rs
git commit -m "feat(trader): run_hold_early_exit path — BUY, sleep, market-sell at deadline"
```

---

## Task 4: Sanity check — `--maker` still rejects HoldEarlyExit; `Hold` path untouched

**Files:**
- (Read-only; or add a smoke test)

Verify:
- `--exit-rule hold-early-exit --maker` rejected (Task 1 covered this).
- `--exit-rule hold` still routes through `await_resolution_and_sweep`.
- `--exit-rule tp-sl --maker` still routes through `run_maker`.

- [ ] **Step 1: Add a smoke unit test**

Append to `mod tests` in `src/trader/window.rs` (or test the dispatch via a thin integration test):

```rust
    #[test]
    fn window_config_can_hold_or_early_exit_not_both() {
        // Compile-time: exit (Option<ExitConfig>) and exit_at_secs are
        // independent fields. The validation lives in config.rs::validate().
        // This test just confirms WindowConfig accepts (None, None) and
        // (None, Some(270)) — the two valid configurations for non-tp-sl modes.
        let c1 = WindowConfig {
            band_min: dec!(0.45),
            band_max: dec!(0.55),
            exit: None,
            exit_at_secs: None,
            maker: false,
            window_seconds: 300,
        };
        let c2 = WindowConfig {
            exit_at_secs: Some(270),
            ..c1
        };
        assert!(c1.exit.is_none() && c1.exit_at_secs.is_none());
        assert!(c2.exit.is_none() && c2.exit_at_secs == Some(270));
    }
```

(NOTE: Add `use rust_decimal_macros::dec;` to the test `mod` if missing.)

- [ ] **Step 2: Verify**

Run: `cargo test --lib trader::window`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/trader/window.rs
git commit -m "test(trader): WindowConfig exit + exit_at_secs are independent fields"
```

---

## Task 5: README + TODO docs

**Files:**
- Modify: `README.md`
- Modify: `TODO.md`

- [ ] **Step 1: Update README.md**

Locate the trader documentation section (search for `--exit-rule`). Add the new option:

````markdown
### v1.8 — `hold-early-exit` (no redemption needed)

```bash
poly-trader --direction up \
  --exit-rule hold-early-exit \
  --exit-at-secs 270
```

Avoids the on-chain `redeemPositions` step entirely. BUY taker at entry, hold the position, then market-sell at `t = exit-at-secs` (default 270 for 5-min windows; max `window_seconds - 30`).

**Backtest:** 30-day real-trade replay (`report-real-30d.html`) shows **+$1,505 PnL** over 8503 windows for `13_hold_early_exit_270`, on par with the legacy `1_hold_martingale` baseline. Trade-data freshness check: 99.5% of windows have a SELL trade within the last 60s before t=270s — execution at the assumed bid is realistic.

**When to use:**
- EOA has USDC but no MATIC (can't `redeemPositions`).
- Want deterministic exit time independent of resolution latency.

**When NOT to use:**
- Strategies 8/9 (`TP=0.85, SL=0.30-0.35`) actually outperform on real data (+$1,696 to +$1,824) — use those instead if you can fund redeem with MATIC.

| Flag | Valid with | Notes |
|---|---|---|
| `--exit-rule hold-early-exit` | (new) | Requires `--exit-at-secs`. Rejects `--maker`. |
| `--exit-at-secs <u32>` | only with `hold-early-exit` | Range: 1..=(window_seconds - 30). Default: none — must specify explicitly. Backtest-validated: 270 for 5-min. |
````

- [ ] **Step 2: Update TODO.md**

Locate the v1.8 entry. Replace its body:

```markdown
## v1.8 — `--exit-rule hold-early-exit` ✅ COMPLETE

New exit mode: BUY taker → hold → market-sell at `--exit-at-secs` (default 270 for 5-min). No resolution wait. Avoids the MATIC redemption blocker.

**Backtest validation:** `13_hold_early_exit_270` returns +$1,505 over 8503 real-data windows (v1.7.5 backtest, `report-real-30d.html`). Trade-freshness check: 99.5% of windows have a SELL trade within last 60s before exit; median gap 0s. Strategy is execution-realistic.

- [x] `ExitRuleArg::HoldEarlyExit` variant + `--exit-at-secs <u32>` flag
- [x] Validation: required when `hold-early-exit`; rejected otherwise; range 1..=(window_seconds - 30)
- [x] `run_hold_early_exit` window path: BUY → sleep → sell_market
- [x] Integration test with paused tokio clock
- [x] README + TODO docs

**Live A/B test plan:** run `--direction up --exit-rule hold-early-exit --exit-at-secs 270` for ≥24 hours (~288 windows). Compare PnL vs simulated `1_hold_martingale` baseline. Decision after 200 windows:
- PnL > backtest projection (+$0.18/window × 288 = ~$52): ship as default.
- PnL within ±50%: continue monitoring.
- PnL substantially below: investigate (slippage? entry timing? band coverage?).

**Out of scope (defer to v1.8.1+):**
- Maker mode for `hold-early-exit` (saves entry fee but adds limit-order failure path).
- Strategies 8/9 (`TP=0.85, SL=0.30-0.35`) — higher backtest PnL but require MATIC funding + redeem integration.
- Dynamic exit time (data-driven choice of exit-at-secs based on intra-window volatility).
```

- [ ] **Step 3: Verify docs build cleanly**

Run: `cargo build --bin poly-trader`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add README.md TODO.md
git commit -m "docs(trader): v1.8 hold-early-exit operator guide"
```

---

## Final verification

- [ ] **Step 1: Full trader test suite**

Run: `cargo test --lib trader::`
Expected: all PASS.

Run: `cargo test --test trader_hold_early_exit`
Expected: PASS.

- [ ] **Step 2: Backtest suite unaffected**

Run: `cargo test --lib backtest::`
Expected: all PASS (we didn't touch backtest).

- [ ] **Step 3: `--help` lists the new flag**

Run: `target/debug/poly-trader --help | grep -A1 exit-rule`
Expected: shows `exit-rule [possible values: hold, tp-sl, hold-early-exit]`. And `--exit-at-secs <EXIT_AT_SECS>` present.

- [ ] **Step 4: Dry-run smoke**

Run: `cargo run --bin poly-trader -- --direction up --exit-rule hold-early-exit --exit-at-secs 270 --dry-run --max-windows 2`
Expected: starts cleanly, logs entries / exits / sell events; no resolution wait; completes after 2 windows.

(If the simulated executor and event flow surface unexpected issues, capture the log and report — this is the live-money path's first ground-truth check.)

---

## Out of scope (do NOT implement)

- Maker mode for `hold-early-exit` (limit BUY + market sell at deadline). Defer to v1.8.1.
- TP/SL combined with `hold-early-exit` (e.g., "exit at TP=0.75 OR at t=270, whichever first") — that's strategy 12 which already FAILED the backtest. Don't build it.
- Strategies 8/9 TP+SL variants (+$1,700-$1,800 backtest PnL). Need MATIC + redeem solved first. Separate plan.
- 15min / 60min market validation. v1.7.3 deferred; flag still accepts them but they're untested.
- Adaptive exit-at-secs based on intra-window volatility.

## Risk / known limitations

- **No price floor on sell.** If bid crashes to 0.10 before t=270s, we still sell at that bid. Acceptable per spec (this is the strategy 13 behaviour the backtest measured).
- **Network delay at deadline.** If the sell request takes 10s to reach Polymarket, actual fill time is t≈280s, with 20s buffer before resolution. Operator should monitor first few runs for any timeout/rejection clustering near the deadline.
- **Slippage from order size.** Backtest assumes our entire share count sells at observed bid. For $5 stakes vs typical $4-32K market depth, slippage is <0.5%. For larger stakes (post-Martingale step 5 ≈ $80), validate manually after the first ladder cap reset.
