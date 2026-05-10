# v1.7.1 — `--window-minutes` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--window-minutes 5|15|60` flag to `poly-trader`. TUI auto-detects the window length from the trader's ladder state via the existing event stream — no separate TUI flag.

**Architecture:** Replace hardcoded `300` / `5m` in `src/trader/market.rs` with `window_seconds(mins)` / `window_slug(ts, mins)` / `floor_window(ts, mins)` / `next_window_boundary(ts, mins)` helpers. Thread `window_minutes` through `LadderState → SchedulerConfig + WindowConfig → MakerDeps → resolver timeout`. TUI reads `window_minutes` from the latest `TraderEvent.ladder` and pushes updates to the `market_watch` task via mpsc channel.

**Tech Stack:** Rust 1.78+, tokio (existing), polymarket_client_sdk_v2 (unchanged), tokio mpsc (already used). No new deps.

**Spec:** `docs/superpowers/specs/2026-05-10-window-minutes-design.md`

## Build hygiene — STRICT

NEVER bare `cargo build`. Always scope:
- `cargo build --bin poly-trader`
- `cargo build --bin poly-tui` (for TUI changes)
- `cargo test --lib trader::`
- `cargo test --lib`  (only at end of task to verify full suite)

DO NOT touch `src/backtest/` — separate v1.4 module.
DO NOT touch `src/positions.rs`, `poly-redeem.rs`, or unrelated v1.6/v1.8 code.
DO NOT modify the `WindowResolver` trait — the per-call-timeout idea was rejected; resolver gets per-startup timeout based on `args.window_minutes`.

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `src/trader/market.rs` | modify | `window_seconds(mins)`, `window_slug(ts, mins)`, `floor_window(ts, mins)`, `next_window_boundary(ts, mins)`. Old fns become deprecated wrappers. |
| `src/trader/ladder.rs` | modify | `LadderState.window_minutes: u32`. `serde(default)` for backward compat. New `with_window_minutes(mins)` builder. `LadderState::new` signature unchanged. |
| `src/trader/config.rs` | modify | `--window-minutes` clap flag (default 5). `ConfigError::InvalidWindowMinutes`. |
| `src/trader/scheduler.rs` | modify | `SchedulerConfig.window_seconds`. Sleep computation uses `floor_window`/`next_window_boundary`. |
| `src/trader/window.rs` | modify | `WindowConfig.window_seconds`. Threaded into `run_maker`. (Resolver timeout is set at trader startup, not per-call.) |
| `src/trader/maker.rs` | modify | `sell_with_tp_sl` cancel_deadline = `window_ts + window_seconds - 30`. `MakerDeps` unchanged; `window_seconds` passed as fn arg. |
| `src/tui/market_watch.rs` | modify | `MarketState.window_minutes`. `run` accepts `mpsc::Receiver<u32>`. Gamma fetch uses `floor_window(now, current_mins)`. `seconds_to_next_boundary` takes `mins` param. |
| `src/app.rs` | modify | `AppState.window_minutes`. `handle_event` for `TraderEvent` updates from `ev.ladder.window_minutes`, sends to market_watch via mpsc::Sender. |
| `src/ui.rs` | modify | `UiState.window_minutes`. `render_market_strip` countdown calc uses dynamic mins. New 15m / 60m insta snapshots. |
| `src/bin/poly-trader.rs` | modify | Wire `args.window_minutes` into LadderState + SchedulerConfig + WindowConfig + resolver timeout. Reject on saved-state mismatch. |
| `src/bin/poly-tui.rs` | modify | mpsc channel for window_minutes between app and market_watch. AppState boots with `window_minutes=5` (or from Redis ladder if present). |
| `README.md` | modify | Document `--window-minutes` + 15m liquidity advantage + 60m unvalidated caveat. |
| `TODO.md` | modify | Mark v1.7.1 ✅ COMPLETE. |

---

## Task 0: Sanity baseline

**Files:** none (read-only).

- [ ] **Step 1: Confirm working tree clean**

Run: `git status`
Expected: only untracked items are `.claude/`, the four `backtest-report*.html` files, and `~/.poly-backtest-cache/`. No tracked-file modifications.

- [ ] **Step 2: Confirm trader unit tests green**

Run: `cargo test --lib trader::`
Expected: PASS. Note count.

- [ ] **Step 3: Confirm both binaries build**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean.

Run: `cargo build --bin poly-tui`
Expected: Compiles clean.

- [ ] **Step 4: No commit**

Read-only baseline. No commit.

---

## Task 1: market.rs — parameterized window helpers

**Files:**
- Modify: `src/trader/market.rs`

- [ ] **Step 1: Write the failing tests**

Add to `src/trader/market.rs` inside `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn window_seconds_5m() { assert_eq!(window_seconds(5), 300); }
    #[test]
    fn window_seconds_15m() { assert_eq!(window_seconds(15), 900); }
    #[test]
    fn window_seconds_60m() { assert_eq!(window_seconds(60), 3600); }

    #[test]
    fn window_slug_includes_minutes() {
        assert_eq!(window_slug(1747789200, 5), "btc-updown-5m-1747789200");
        assert_eq!(window_slug(1747789200, 15), "btc-updown-15m-1747789200");
        assert_eq!(window_slug(1747789200, 60), "btc-updown-60m-1747789200");
    }

    #[test]
    fn floor_window_5m_matches_legacy() {
        // 1700000100 % 300 == 0: at boundary
        assert_eq!(floor_window(1700000100, 5), 1700000100);
        assert_eq!(floor_window(1700000100, 5), floor_5min(1700000100));
        assert_eq!(floor_window(1700000200, 5), floor_5min(1700000200));
    }

    #[test]
    fn floor_window_15m() {
        // 15-minute boundary: 900-second buckets.
        assert_eq!(floor_window(1700000900, 15), 1700000100); // 1700000900 - (1700000900 % 900) = 1700000100
        assert_eq!(floor_window(1700001000, 15), 1700000100);
    }

    #[test]
    fn floor_window_60m() {
        // 60-minute boundary: 3600-second buckets.
        // 1700001500 % 3600 = 1500 → floor = 1700000000
        assert_eq!(floor_window(1700001500, 60), 1700000000);
    }

    #[test]
    fn next_window_boundary_5m_matches_legacy() {
        assert_eq!(next_window_boundary(1700000100, 5), next_5min_boundary(1700000100));
        assert_eq!(next_window_boundary(1700000200, 5), 1700000400);
    }

    #[test]
    fn next_window_boundary_15m() {
        assert_eq!(next_window_boundary(1700000200, 15), 1700001000); // floor=1700000100, +900=1700001000
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::market::tests::window_seconds_5m`
Expected: FAIL — `cannot find function 'window_seconds'`.

- [ ] **Step 3: Add the new helpers**

Edit `src/trader/market.rs`. Find the existing `window_slug`, `floor_5min`, `next_5min_boundary` functions. Add the parameterized versions and convert old fns to wrappers:

```rust
/// Total seconds in a `window_minutes`-long window.
pub fn window_seconds(window_minutes: u32) -> i64 { window_minutes as i64 * 60 }

/// Slug for the BTC up/down market at a given window boundary.
pub fn window_slug(window_ts: i64, window_minutes: u32) -> String {
    format!("btc-updown-{}m-{}", window_minutes, window_ts)
}

/// Floor `now_ts` to the start of its window of length `window_minutes`.
pub fn floor_window(now_ts: i64, window_minutes: u32) -> i64 {
    let secs = window_seconds(window_minutes);
    now_ts - now_ts.rem_euclid(secs)
}

/// Next window boundary strictly after `now_ts`.
pub fn next_window_boundary(now_ts: i64, window_minutes: u32) -> i64 {
    floor_window(now_ts, window_minutes) + window_seconds(window_minutes)
}

// Backward-compat wrappers — internal callers migrate gradually.
pub fn floor_5min(now_ts: i64) -> i64 { floor_window(now_ts, 5) }
pub fn next_5min_boundary(now_ts: i64) -> i64 { next_window_boundary(now_ts, 5) }
```

The original `window_slug(ts) -> String` (no minutes arg) is being replaced with the 2-arg version. Find every existing internal caller and add `, 5` as the second arg. Search:

```bash
grep -n "window_slug(" src/ tests/
```

Expected callers:
- `src/trader/market.rs::tests::slug_format` — change to `window_slug(1747789200, 5)`
- `src/trader/adapters/gamma_wrapper.rs:23` — change to `window_slug(window_ts, 5)`
- Any test files referencing the old 1-arg form

Update each to pass `5` for now (Task 4 + Task 7 will replace with the actual window_minutes).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::market::`
Expected: PASS — all 9+ market tests green.

- [ ] **Step 5: Verify trader binary still compiles**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean.

- [ ] **Step 6: Commit**

```bash
git add src/trader/market.rs src/trader/adapters/gamma_wrapper.rs
git commit -m "feat(trader): window_seconds/window_slug/floor_window/next_window_boundary helpers"
```

---

## Task 2: LadderState gains window_minutes field

**Files:**
- Modify: `src/trader/ladder.rs`

- [ ] **Step 1: Write the failing tests**

Append to `#[cfg(test)] mod tests` in `src/trader/ladder.rs`:

```rust
    #[test]
    fn ladder_default_window_minutes_is_5() {
        let s = LadderState::new(Direction::Up, Decimal::from(5), 5, ts());
        assert_eq!(s.window_minutes, 5);
    }

    #[test]
    fn ladder_with_window_minutes_builder() {
        let s = LadderState::new(Direction::Up, Decimal::from(5), 5, ts())
            .with_window_minutes(15);
        assert_eq!(s.window_minutes, 15);
    }

    #[test]
    fn ladder_serde_roundtrip_includes_window_minutes() {
        let s = LadderState::new(Direction::Up, Decimal::from(5), 5, ts())
            .with_window_minutes(15);
        let json = serde_json::to_string(&s).unwrap();
        let back: LadderState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.window_minutes, 15);
    }

    #[test]
    fn ladder_legacy_json_without_window_minutes_defaults_to_5() {
        // Pre-v1.7.1 ladder JSON has no window_minutes field.
        let legacy = r#"{
            "session_id": "00000000-0000-0000-0000-000000000000",
            "direction": "up",
            "base_usd": "5",
            "max_step": 5,
            "current_step": 1,
            "session_started_at": "2026-05-10T00:00:00Z",
            "realized_pnl_usd": "0",
            "windows_won": 0,
            "windows_lost": 0,
            "windows_skipped": 0,
            "stopped": null
        }"#;
        let s: LadderState = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.window_minutes, 5);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::ladder::tests::ladder_default_window_minutes_is_5`
Expected: FAIL — `field 'window_minutes' does not exist on LadderState`.

- [ ] **Step 3: Add the field, default fn, and builder**

Edit `src/trader/ladder.rs`. Update `LadderState`:

```rust
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
    /// Trading window length in minutes. {5, 15, 60}. Pre-v1.7.1 ladder JSON
    /// omits this field; serde(default) restores 5min behavior on legacy state.
    #[serde(default = "default_window_minutes")]
    pub window_minutes: u32,
}

fn default_window_minutes() -> u32 { 5 }
```

Update `LadderState::new` to set `window_minutes: 5`:

```rust
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
            window_minutes: 5,
        }
    }

    /// Builder-style override for `window_minutes`. Use after `new()`.
    pub fn with_window_minutes(mut self, mins: u32) -> Self {
        self.window_minutes = mins;
        self
    }

    pub fn current_bet_usd(&self) -> Decimal {
        let multiplier = 2_u64.pow((self.current_step - 1) as u32);
        self.base_usd * Decimal::from(multiplier)
    }

    pub fn is_stopped(&self) -> bool { self.stopped.is_some() }
}
```

Update the `fn fresh(step: u8)` test helper to include `window_minutes: 5`:

```rust
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
            window_minutes: 5,
        }
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::ladder::`
Expected: PASS — all ladder tests green (existing + 4 new).

- [ ] **Step 5: Verify lib still compiles**

Run: `cargo build --lib`
Expected: Compiles clean.

- [ ] **Step 6: Commit**

```bash
git add src/trader/ladder.rs
git commit -m "feat(trader): LadderState.window_minutes (default 5, serde back-compat) + with_window_minutes builder"
```

---

## Task 3: --window-minutes CLI flag + validation

**Files:**
- Modify: `src/trader/config.rs`

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `src/trader/config.rs`:

```rust
    #[test]
    fn parses_window_minutes_default_5() {
        let a = parse(&["--direction", "up"]);
        assert_eq!(a.window_minutes, 5);
    }

    #[test]
    fn parses_window_minutes_15() {
        let a = parse(&["--direction", "up", "--window-minutes", "15"]);
        assert_eq!(a.window_minutes, 15);
    }

    #[test]
    fn parses_window_minutes_60() {
        let a = parse(&["--direction", "up", "--window-minutes", "60"]);
        assert_eq!(a.window_minutes, 60);
    }

    #[test]
    fn validate_rejects_window_minutes_7() {
        let mut a = parse(&["--direction", "up"]);
        a.window_minutes = 7;
        assert_eq!(a.validate(), Err(ConfigError::InvalidWindowMinutes));
    }

    #[test]
    fn validate_rejects_window_minutes_0() {
        let mut a = parse(&["--direction", "up"]);
        a.window_minutes = 0;
        assert_eq!(a.validate(), Err(ConfigError::InvalidWindowMinutes));
    }

    #[test]
    fn validate_accepts_window_minutes_15() {
        let a = parse(&["--direction", "up", "--window-minutes", "15"]);
        assert!(a.validate().is_ok());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib trader::config::tests::parses_window_minutes_default_5`
Expected: FAIL — `field 'window_minutes' does not exist`.

- [ ] **Step 3: Add the flag and validation**

Edit `src/trader/config.rs`. Add a field to `TraderArgs` (near other args, before `--dry-run`):

```rust
    /// Trading window length in minutes. {5, 15, 60}. 5 has full backtest
    /// coverage; 15 has observed deeper liquidity but is unvalidated; 60 is
    /// unvalidated. Default 5.
    #[arg(long, default_value = "5")]
    pub window_minutes: u32,
```

Add a variant to `ConfigError`:

```rust
    #[error("window-minutes must be 5, 15, or 60")]
    InvalidWindowMinutes,
```

Add the validation rule inside `validate()`:

```rust
        if !matches!(self.window_minutes, 5 | 15 | 60) {
            return Err(ConfigError::InvalidWindowMinutes);
        }
```

Place after the existing band validation, before exit-rule validation.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::config::`
Expected: PASS — all config tests green.

- [ ] **Step 5: Commit**

```bash
git add src/trader/config.rs
git commit -m "feat(trader): --window-minutes CLI flag (5|15|60)"
```

---

## Task 4: SchedulerConfig.window_seconds + dynamic boundary

**Files:**
- Modify: `src/trader/scheduler.rs`

- [ ] **Step 1: Read existing scheduler**

Read `src/trader/scheduler.rs` to find `SchedulerConfig` and the `floor_5min`/`next_5min_boundary` callsites.

- [ ] **Step 2: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/trader/scheduler.rs`:

```rust
    #[test]
    fn scheduler_config_carries_window_seconds() {
        let c = SchedulerConfig { max_windows: None, window_seconds: 900 };
        assert_eq!(c.window_seconds, 900);
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib trader::scheduler::tests::scheduler_config_carries_window_seconds`
Expected: FAIL — `field 'window_seconds' does not exist`.

- [ ] **Step 4: Add window_seconds to SchedulerConfig**

Edit `src/trader/scheduler.rs`. Update the struct:

```rust
pub struct SchedulerConfig {
    pub max_windows: Option<u32>,
    pub window_seconds: i64,
}
```

Find every internal usage of `floor_5min(now)` / `next_5min_boundary(now)` in this file. Replace with `floor_window(now, mins)` / `next_window_boundary(now, mins)` derived from `cfg.window_seconds`. Conversion: `mins = (cfg.window_seconds / 60) as u32`. Or pass `window_seconds` directly:

```rust
let secs = cfg.window_seconds;
let next = (now / secs + 1) * secs;
let sleep_until_secs = next - now;
```

(Inline math, no helper required, since SchedulerConfig already has window_seconds and the formula is trivial.)

Find the existing scheduler test fixtures that build `SchedulerConfig { max_windows: ... }`. Add `window_seconds: 300` to each. There are likely 3-5 such fixtures across `e2e_trader.rs` and `scheduler.rs::tests`.

Search:

```bash
grep -rn "SchedulerConfig {" src/ tests/
```

Update each match.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib trader::scheduler::`
Expected: PASS.

Run: `cargo build --tests`
Expected: All test crates compile.

- [ ] **Step 6: Commit**

```bash
git add src/trader/scheduler.rs tests/ src/
git commit -m "feat(trader): SchedulerConfig.window_seconds drives next-boundary sleep"
```

---

## Task 5: WindowConfig.window_seconds (used by maker)

**Files:**
- Modify: `src/trader/window.rs`

The resolver timeout will continue to be set at trader startup (Task 10) — not per-call. So `WindowConfig.window_seconds` is only used to pass through to `run_maker`'s cancel deadline.

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/trader/window.rs` (where existing `cfg()` and other helpers live):

```rust
    #[test]
    fn window_config_carries_window_seconds() {
        let c = WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: None,
            maker: false,
            window_seconds: 900,
        };
        assert_eq!(c.window_seconds, 900);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib trader::window::tests::window_config_carries_window_seconds`
Expected: FAIL — `field 'window_seconds' does not exist`.

- [ ] **Step 3: Add window_seconds to WindowConfig**

Edit `src/trader/window.rs`. Update the struct:

```rust
pub struct WindowConfig {
    pub band_min: Decimal,
    pub band_max: Decimal,
    pub exit: Option<ExitConfig>,
    pub maker: bool,
    pub window_seconds: i64,
}
```

Update every existing test fixture that builds `WindowConfig { ... }`. There are 12+ in `window.rs::tests`. Add `window_seconds: 300` to each. Helper:

```rust
    fn cfg() -> WindowConfig {
        WindowConfig {
            band_min: Decimal::from_str("0.45").unwrap(),
            band_max: Decimal::from_str("0.55").unwrap(),
            exit: None,
            maker: false,
            window_seconds: 300,
        }
    }
```

Same for `cfg_with_exit(...)`. Inspect the file for all `WindowConfig { ... }` uses and update each.

In `run_window`, where it dispatches to `run_maker`, thread `cfg.window_seconds`:

```rust
    if cfg.maker && cfg.exit.is_some() {
        let exit_cfg = cfg.exit.as_ref().unwrap();
        let maker_deps = crate::trader::maker::MakerDeps { /* ... */ };
        return crate::trader::maker::run_maker(
            &maker_deps, ladder, &market, &token_id, dollars, ask, exit_cfg,
            window_ts,
            cfg.window_seconds,  // NEW
            tokio_util::sync::CancellationToken::new(),
        ).await;
    }
```

The `run_maker` signature change happens in Task 6.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::window::`
Expected: PASS — all window tests green (with updated fixtures).

- [ ] **Step 5: Commit**

```bash
git add src/trader/window.rs
git commit -m "feat(trader): WindowConfig.window_seconds threaded into run_maker"
```

---

## Task 6: maker.rs cancel_deadline scales

**Files:**
- Modify: `src/trader/maker.rs`

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/trader/maker.rs`. The existing tests pass `window_ts = chrono::Utc::now().timestamp()` and rely on `+270` cancel. We need a new test that verifies the +870 cancel for 15min:

```rust
    #[tokio::test(start_paused = true)]
    async fn cancel_deadline_scales_with_window_seconds_15m() {
        // 15min window: cancel deadline is window_ts + 900 - 30 = window_ts + 870.
        // We can't directly observe the deadline, but we can check that for a
        // window 600s in the past, the deadline (window_ts + 870) is ~270s
        // in the future and the watcher behaves consistently.
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
        // TP never fills; SL never fires.
        let price = StubPrice::const_bid("0.55");
        let emitter = CapturingEmitter::new();
        let deps = MakerDeps {
            executor: exec.clone(), events: events.clone(),
            price: price.clone(), emitter: emitter.clone(),
        };

        // 600s ago window_ts means window_seconds=900 → cancel at window_ts+870 = 270s in future.
        let window_ts = chrono::Utc::now().timestamp() - 600;
        let outcome = run_maker(
            &deps, &fresh_ladder(), &fake_market(), "tok-up",
            Decimal::from(5), Decimal::from_str("0.50").unwrap(),
            &cfg(), window_ts,
            900,  // window_seconds = 15min
            CancellationToken::new(),
        ).await;
        // Deadline reached — cancel TP, market sell residual at constant 0.55 bid.
        // 10 shares × 0.55 × 0.99 (slippage) = 5.445 vs 4.90 cost → Won.
        assert!(matches!(outcome, WindowOutcome::Won { .. }),
                "outcome was: {outcome:?}");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib trader::maker::tests::cancel_deadline_scales_with_window_seconds_15m`
Expected: FAIL — `run_maker` doesn't accept a `window_seconds` arg yet.

- [ ] **Step 3: Update run_maker + sell_with_tp_sl signatures**

Edit `src/trader/maker.rs`. Update `run_maker`:

```rust
pub async fn run_maker(
    deps: &MakerDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    dollars: Decimal,
    ask: Decimal,
    exit_cfg: &ExitConfig,
    window_ts: i64,
    window_seconds: i64,  // NEW
    shutdown: CancellationToken,
) -> WindowOutcome {
    // Phase 1 unchanged
    let buy_fill = match buy_with_sweep(deps, ladder, token_id, dollars, ask, &shutdown).await {
        BuyOutcome::Filled { shares, dollars_spent, fill_price } => {
            BuyFill { shares, dollars: dollars_spent, fill_price }
        }
        BuyOutcome::Skipped => return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed },
        BuyOutcome::ShutdownDuringBuy => return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed },
    };
    sell_with_tp_sl(deps, ladder, market, token_id, &buy_fill, exit_cfg, window_ts, window_seconds, shutdown).await
}
```

Update `sell_with_tp_sl`:

```rust
async fn sell_with_tp_sl(
    deps: &MakerDeps,
    ladder: &LadderState,
    market: &WindowMarket,
    token_id: &str,
    buy_fill: &BuyFill,
    exit_cfg: &ExitConfig,
    window_ts: i64,
    window_seconds: i64,  // NEW
    shutdown: CancellationToken,
) -> WindowOutcome {
    // ... existing setup ...

    // Cancel-at-window_seconds-30 absolute deadline.
    let cancel_unix = window_ts + window_seconds - 30;
    let now_unix = chrono::Utc::now().timestamp();
    let cancel_after = (cancel_unix - now_unix).max(0) as u64;
    let cancel_deadline = tokio::time::Instant::now() + Duration::from_secs(cancel_after);

    // ... rest unchanged
}
```

Update existing call sites in `run_maker` tests (3 of them already exist — `buy_fills_immediately_then_tp_fills_returns_won`, `buy_never_fills_three_steps_then_skipped`, `sl_triggers_during_hold_phase`). Add `300` for `window_seconds` to each test's `run_maker` call so 5min behavior is preserved.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib trader::maker::`
Expected: PASS — 4 tests green (3 existing + 1 new).

- [ ] **Step 5: Commit**

```bash
git add src/trader/maker.rs
git commit -m "feat(trader): cancel_deadline scales with window_seconds (15m → t+870, 60m → t+3570)"
```

---

## Task 7: market_watch — dynamic window_minutes

**Files:**
- Modify: `src/tui/market_watch.rs`

Adds a `window_minutes` field to `MarketState`, accepts `mpsc::Receiver<u32>` in `run`, uses `floor_window`/`next_window_boundary` instead of `floor_5min`/`next_5min_boundary`. `seconds_to_next_boundary` gains a `mins` arg.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `src/tui/market_watch.rs`:

```rust
    #[test]
    fn seconds_to_next_boundary_15m() {
        // 1700000600 % 900 = 200 → 700s remaining
        let s = state_with(None, None);
        assert_eq!(s.seconds_to_next_boundary(1700000600, 15), 700);
    }

    #[test]
    fn seconds_to_next_boundary_5m_unchanged() {
        let s = state_with(None, None);
        assert_eq!(s.seconds_to_next_boundary(1700000200, 5), 200);
    }

    #[tokio::test]
    async fn market_state_carries_window_minutes() {
        let s = MarketState {
            window_ts: Some(1700000000),
            price_to_beat: None,
            current_price: None,
            last_rpc_ok_at: None,
            last_gamma_ok_at: None,
            window_minutes: 15,
        };
        assert_eq!(s.window_minutes, 15);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib tui::market_watch::tests::seconds_to_next_boundary_15m`
Expected: FAIL — `seconds_to_next_boundary` takes only 1 arg.

- [ ] **Step 3: Update MarketState + seconds_to_next_boundary**

Edit `src/tui/market_watch.rs`. Update `MarketState`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketState {
    pub window_ts: Option<i64>,
    pub price_to_beat: Option<Decimal>,
    pub current_price: Option<Decimal>,
    pub last_rpc_ok_at: Option<DateTime<Utc>>,
    pub last_gamma_ok_at: Option<DateTime<Utc>>,
    pub window_minutes: u32,
}
```

Update `MarketState::empty()` to include `window_minutes: 5`. Same for the `state_with` test helper — add `window_minutes: 5` field.

Update `seconds_to_next_boundary`:

```rust
    /// Seconds remaining until the current `window_minutes`-minute window closes.
    pub fn seconds_to_next_boundary(&self, now_ts: i64, window_minutes: u32) -> i64 {
        let secs = window_minutes as i64 * 60;
        let r = now_ts.rem_euclid(secs);
        if r == 0 { secs } else { secs - r }
    }
```

(Note the existing fn took just `now_ts`; new fn takes `now_ts` AND `window_minutes`.) Update existing tests of this fn to pass `5` explicitly:

```rust
    #[test]
    fn seconds_to_next_boundary_at_open() {
        let s = state_with(None, None);
        // 1700000100 % 300 == 0
        assert_eq!(s.seconds_to_next_boundary(1700000100, 5), 300);
    }
```

(All existing tests of this method need `, 5` added.)

- [ ] **Step 4: Update `run` to accept window_minutes channel**

Replace the `run` function:

```rust
pub async fn run(
    price_feed: Arc<dyn BtcPriceFeed>,
    market: Arc<dyn MarketDiscovery>,
    event_tx: mpsc::Sender<AppEvent>,
    mut window_minutes_rx: mpsc::Receiver<u32>,
    shutdown: CancellationToken,
) {
    use crate::trader::market::floor_window;

    let mut state = MarketState::empty();
    let mut window_minutes: u32 = 5;
    let mut rpc_ticker = tokio::time::interval(Duration::from_secs(5));
    let mut gamma_ticker = tokio::time::interval(Duration::from_secs(15));

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,

            Some(new_mins) = window_minutes_rx.recv() => {
                if window_minutes != new_mins {
                    window_minutes = new_mins;
                    state.window_minutes = new_mins;
                    state.window_ts = None;  // force re-fetch on next gamma tick
                    emit(&event_tx, &state).await;
                }
            }

            _ = rpc_ticker.tick() => {
                if let Ok(p) = price_feed.latest_price().await {
                    state.current_price = Some(p);
                    state.last_rpc_ok_at = Some(chrono::Utc::now());
                    let now_ts = chrono::Utc::now().timestamp();
                    let current_window = floor_window(now_ts, window_minutes);
                    if state.window_ts != Some(current_window) {
                        state.window_ts = Some(current_window);
                        state.price_to_beat = Some(p);
                    }
                }
                emit(&event_tx, &state).await;
            }

            _ = gamma_ticker.tick() => {
                let now_ts = chrono::Utc::now().timestamp();
                let current_window = floor_window(now_ts, window_minutes);
                if let Ok(m) = market.find_window(current_window).await {
                    if let Some(p) = m.price_to_beat {
                        state.price_to_beat = Some(p);
                    }
                    state.last_gamma_ok_at = Some(chrono::Utc::now());
                    emit(&event_tx, &state).await;
                }
            }
        }
    }
}
```

Note: gamma's `find_window` currently uses `window_slug(ts)` 1-arg from Task 1's deprecated wrapper. We need to update gamma_wrapper.rs to use `window_slug(ts, mins)` and accept window_minutes. But that would touch the gamma adapter. Simpler v1.7.1: pass an `mins` field via the `MarketDiscovery` trait? No, that's invasive.

Alternative: introduce a `find_window_with_mins(window_ts, mins)` on `MarketDiscovery`, OR change `find_window` to take both args. Cleanest: change `find_window` to `find_window(window_ts, window_minutes)`. This is a trait change but well-bounded.

Actually the simplest path: `GammaMarketDiscovery::new(base_url, window_minutes)` — store window_minutes at adapter construction. Then `find_window(ts)` builds the slug correctly. The trader binary creates the adapter once with the configured `args.window_minutes`. The TUI's market_watch creates its adapter at startup with `5` (default), but since the adapter uses 5min slugs even if the trader is doing 15min, the TUI would query the wrong gamma endpoint.

Better path: `MarketDiscovery::find_window(ts: i64, mins: u32)` — add the param to the trait. All callers pass through their known `mins`. Slightly more invasive but correct.

For this task (Task 7), update the trait. For market_watch, pass `window_minutes` into the gamma fetch:

```rust
            _ = gamma_ticker.tick() => {
                let now_ts = chrono::Utc::now().timestamp();
                let current_window = floor_window(now_ts, window_minutes);
                if let Ok(m) = market.find_window(current_window, window_minutes).await {
                    // ... unchanged
                }
            }
```

Update the trait definition (in `src/trader/market.rs`):

```rust
#[async_trait]
pub trait MarketDiscovery: Send + Sync {
    async fn find_window(&self, window_ts: i64, window_minutes: u32) -> Result<WindowMarket, MarketError>;
}
```

Update `GammaMarketDiscovery::find_window` (in `src/trader/adapters/gamma_wrapper.rs`) to use `window_slug(window_ts, window_minutes)` instead of the deprecated 1-arg form.

Update every test stub that implements `MarketDiscovery` to accept the new arg. There are several:
- `src/trader/window.rs::tests::StubMarket`
- `src/trader/scheduler.rs::tests::?` (probably doesn't have one)
- `src/tui/market_watch.rs::tests::FakeMarket`
- `tests/trader_market_integration.rs::?` (may have stub)

Each impl just needs `_window_minutes: u32` ignored or used as appropriate.

This is a moderately invasive trait change but correct. Apply to all callers in this task to keep the diff atomic.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib`
Expected: PASS — all tests across lib green.

- [ ] **Step 6: Commit**

```bash
git add src/tui/market_watch.rs src/trader/market.rs src/trader/adapters/gamma_wrapper.rs src/trader/window.rs
git commit -m "feat(tui): market_watch dynamic window_minutes via mpsc; MarketDiscovery::find_window takes mins"
```

---

## Task 8: AppState.window_minutes + handle_event update

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Write the failing test**

Add to `src/app.rs::tests`:

```rust
    #[tokio::test]
    async fn handle_event_updates_window_minutes_from_trader_event() {
        use crate::trader::ladder::{LadderState, Direction};
        use crate::trader::event::{TraderEvent, TraderEventKind};

        let mut state = AppState::new(Duration::from_secs(30));
        let (cmd_tx, _cmd_rx) = mpsc::channel(8);
        let (mins_tx, mut mins_rx) = mpsc::channel::<u32>(8);
        state.window_minutes_tx = Some(mins_tx);

        let ladder = LadderState::new(Direction::Up, Decimal::from(5), 5, chrono::Utc::now())
            .with_window_minutes(15);
        let ev = TraderEvent {
            ts: chrono::Utc::now(),
            session_id: ladder.session_id,
            kind: TraderEventKind::SessionStarted,
            ladder,
        };
        handle_event(&mut state, AppEvent::TraderEvent(ev), &cmd_tx);

        assert_eq!(state.window_minutes, 15);
        let pushed = mins_rx.try_recv().unwrap();
        assert_eq!(pushed, 15);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib app::tests::handle_event_updates_window_minutes_from_trader_event`
Expected: FAIL — `field 'window_minutes' / 'window_minutes_tx' does not exist`.

- [ ] **Step 3: Add fields and update handle_event**

Edit `src/app.rs`. Update `AppState`:

```rust
pub struct AppState {
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub redis_ok: bool,
    pub refresh_interval: Duration,
    pub should_quit: bool,
    pub trader_log: VecDeque<TraderEvent>,
    pub trader_latest: Option<TraderEvent>,
    pub trader_health: TraderHealth,
    pub market: Option<MarketState>,
    pub positions: Option<Positions>,
    pub window_minutes: u32,
    pub window_minutes_tx: Option<mpsc::Sender<u32>>,
}
```

Update `AppState::new`:

```rust
    pub fn new(refresh_interval: Duration) -> Self {
        Self {
            balance: None,
            last_refresh: None,
            redis_ok: false,
            refresh_interval,
            should_quit: false,
            trader_log: VecDeque::with_capacity(64),
            trader_latest: None,
            trader_health: TraderHealth::NotStarted,
            market: None,
            positions: None,
            window_minutes: 5,
            window_minutes_tx: None,
        }
    }
```

Update `ui_state` to pass `window_minutes` through:

```rust
            // ... existing fields
            positions: self.positions.clone(),
            window_minutes: self.window_minutes,
```

Update the `handle_event` arm for `TraderEvent`:

```rust
        AppEvent::TraderEvent(ev) => {
            // Detect window_minutes change from trader, push to market_watch.
            if state.window_minutes != ev.ladder.window_minutes {
                state.window_minutes = ev.ladder.window_minutes;
                if let Some(tx) = &state.window_minutes_tx {
                    let _ = tx.try_send(ev.ladder.window_minutes);
                }
            }
            if state.trader_log.len() >= 64 {
                state.trader_log.pop_front();
            }
            state.trader_log.push_back(ev.clone());
            state.trader_latest = Some(ev);
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib app::`
Expected: PASS — all app tests green.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs
git commit -m "feat(tui): AppState.window_minutes auto-updates from trader events"
```

---

## Task 9: ui.rs — UiState.window_minutes + countdown uses dynamic mins

**Files:**
- Modify: `src/ui.rs`

- [ ] **Step 1: Write the failing test**

Add to `#[cfg(test)] mod tests` in `src/ui.rs`:

```rust
    #[test]
    fn ui_state_carries_window_minutes() {
        let s = UiState {
            balance: None,
            last_refresh: None,
            clob_health: HealthLed::Green,
            redis_health: HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now: chrono::Utc::now(),
            trader_log: vec![],
            trader_latest: None,
            trader_health: crate::app::TraderHealth::NotStarted,
            market: None,
            positions: None,
            window_minutes: 15,
        };
        assert_eq!(s.window_minutes, 15);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib ui::tests::ui_state_carries_window_minutes`
Expected: FAIL — `field 'window_minutes' does not exist on UiState`.

- [ ] **Step 3: Add the field**

Edit `src/ui.rs`. Update `UiState`:

```rust
pub struct UiState {
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub clob_health: HealthLed,
    pub redis_health: HealthLed,
    pub refresh_interval: Duration,
    pub now: DateTime<Utc>,
    pub trader_log: Vec<TraderEvent>,
    pub trader_latest: Option<TraderEvent>,
    pub trader_health: TraderHealth,
    pub market: Option<MarketState>,
    pub positions: Option<Positions>,
    pub window_minutes: u32,
}
```

Update `render_market_strip` to use it. Find the existing call:

```rust
    let secs = m.seconds_to_next_boundary(now_ts);
```

Change to:

```rust
    let secs = m.seconds_to_next_boundary(now_ts, state.window_minutes);
```

Note: `seconds_to_next_boundary` was already updated in Task 7 to take `mins`.

Update every existing `UiState { ... }` test fixture in `src/ui.rs` and `tests/` to add `window_minutes: 5` (default 5min behavior preserved).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib ui::`
Expected: PASS — all ui tests green. Existing snapshots unchanged because countdown for 5m is unchanged.

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs
git commit -m "feat(tui): UiState.window_minutes drives market-strip countdown"
```

---

## Task 10: poly-trader.rs — wire args.window_minutes everywhere

**Files:**
- Modify: `src/bin/poly-trader.rs`

- [ ] **Step 1: Read existing wiring**

Read `src/bin/poly-trader.rs` lines 50-200 to understand the wiring.

- [ ] **Step 2: Update the wiring**

Edit `src/bin/poly-trader.rs`. After `args.validate()` and before resolver construction, compute `window_seconds`:

```rust
    let window_seconds = poly_tui::trader::market::window_seconds(args.window_minutes);
```

Update resolver timeout — currently fixed 600s:

```rust
    let resolver: Arc<dyn WindowResolver> =
        Arc::new(PolymarketResolver::new(market.clone(),
            Duration::from_secs((window_seconds + 300) as u64)));
```

Update `restore_or_init` to pass through `args.window_minutes`. Find the function and update:

```rust
async fn restore_or_init(
    store: &dyn TraderStateStore,
    args: &TraderArgs,
) -> Result<LadderState> {
    let existing = store.load().await?;
    match (existing, args.reset) {
        (Some(s), false) if !s.is_stopped() => {
            // Detect mid-session window-length switch — refuse, instruct --reset.
            if s.window_minutes != args.window_minutes {
                anyhow::bail!(
                    "saved ladder is for {}min windows; trader configured for {}min. \
                     Pass --reset to start a fresh session.",
                    s.window_minutes, args.window_minutes
                );
            }
            tracing::info!("resuming ladder: step={} pnl={} window_minutes={}",
                s.current_step, s.realized_pnl_usd, s.window_minutes);
            Ok(s)
        }
        (Some(s), false) if s.is_stopped() => {
            anyhow::bail!("previous session stopped: {:?}; pass --reset to start fresh", s.stopped)
        }
        _ => {
            store.clear().await?;
            let direction: Direction = args.direction.into();
            Ok(LadderState::new(direction, args.base, args.max_step, chrono::Utc::now())
                .with_window_minutes(args.window_minutes))
        }
    }
}
```

Update `WindowConfig` and `SchedulerConfig` constructions to include `window_seconds`:

```rust
    let window_cfg = WindowConfig {
        band_min: args.band_min,
        band_max: args.band_max,
        exit: exit_cfg,
        maker: args.maker,
        window_seconds,
    };

    let sched_cfg = SchedulerConfig {
        max_windows: args.max_windows,
        window_seconds,
    };
```

Find the existing `MarketDiscovery::find_window` callers in poly-trader.rs (likely none — the trait is consumed by `run_window` and `await_resolution`, both of which now need the mins arg passed. Check `await_resolution` which calls `find_window` internally — that's via `PolymarketResolver`'s impl. The resolver's internal `find_window` call needs the mins. Update `PolymarketResolver` impl in `src/trader/resolver.rs` to remember `window_minutes` and pass it):

Actually, the resolver also needs window_minutes. Update `PolymarketResolver`:

```rust
pub struct PolymarketResolver {
    market: Arc<dyn MarketDiscovery>,
    tick: Duration,
    timeout: Duration,
    window_minutes: u32,
}

impl PolymarketResolver {
    pub fn new(market: Arc<dyn MarketDiscovery>, timeout: Duration, window_minutes: u32) -> Self {
        Self { market, tick: Duration::from_secs(2), timeout, window_minutes }
    }
    pub fn with_tick(market: Arc<dyn MarketDiscovery>, tick: Duration, timeout: Duration, window_minutes: u32) -> Self {
        Self { market, tick, timeout, window_minutes }
    }
}

#[async_trait]
impl WindowResolver for PolymarketResolver {
    async fn await_resolution(&self, market: &WindowMarket) -> Result<Resolution, ResolveError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            match self.market.find_window(market.window_ts, self.window_minutes).await {
                // ... unchanged
            }
        }
    }
}
```

Update existing `PolymarketResolver` test calls in `src/trader/resolver.rs::tests` to pass `5` as the new arg.

In poly-trader.rs:

```rust
    let resolver: Arc<dyn WindowResolver> =
        Arc::new(PolymarketResolver::new(market.clone(),
            Duration::from_secs((window_seconds + 300) as u64),
            args.window_minutes));
```

- [ ] **Step 3: Build the binary**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean.

- [ ] **Step 4: Smoke test --help**

Run: `./target/debug/poly-trader.exe --help`
Expected: Output shows `--window-minutes <WINDOW_MINUTES>` flag.

- [ ] **Step 5: Smoke test parse**

Run: `./target/debug/poly-trader.exe --direction up --window-minutes 15 --dry-run --max-windows 0 --help`
Expected: Help prints. No clap parse errors.

- [ ] **Step 6: Commit**

```bash
git add src/bin/poly-trader.rs src/trader/resolver.rs
git commit -m "feat(trader): wire --window-minutes into resolver timeout + ladder + configs"
```

---

## Task 11: poly-tui.rs — mpsc channel for window_minutes

**Files:**
- Modify: `src/bin/poly-tui.rs`

- [ ] **Step 1: Read existing wiring**

Read `src/bin/poly-tui.rs` to find where `market_watch::run` is spawned.

- [ ] **Step 2: Add the channel**

Edit `src/bin/poly-tui.rs`. Before the `market_watch` spawn, add:

```rust
    let (window_minutes_tx, window_minutes_rx) = mpsc::channel::<u32>(8);
```

Update the `market_watch::run` call to include the receiver:

```rust
    let h_market = match (price_feed, market_for_watch) {
        (Some(feed), Some(market)) => {
            tokio::spawn(market_watch::run(feed, market, event_tx_market, window_minutes_rx, shutdown_market))
        }
        _ => {
            // Drop the rx to avoid leaving an unfilled channel.
            drop(window_minutes_rx);
            tokio::spawn(async move {})
        }
    };
```

`AppState` is constructed inside `app::run`. To inject `window_minutes_tx`, the simplest path: extend `app::run`'s signature to accept an `Option<mpsc::Sender<u32>>` and store it on AppState.

Update `app::run` (in `src/app.rs`):

```rust
pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    cache: Arc<dyn BalanceCache>,
    cmd_tx: mpsc::Sender<Cmd>,
    mut event_rx: mpsc::Receiver<AppEvent>,
    refresh_interval: Duration,
    window_minutes_tx: Option<mpsc::Sender<u32>>,  // NEW
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let mut state = AppState::new(refresh_interval);
    state.window_minutes_tx = window_minutes_tx;
    // ... rest unchanged
}
```

Update poly-tui.rs's call to `app::run` to pass the sender:

```rust
    let app_result = app::run(
        &mut terminal,
        cache.clone(),
        cmd_tx,
        event_rx,
        Duration::from_secs(cfg.refresh_interval_secs),
        Some(window_minutes_tx),  // NEW
        shutdown.clone(),
    ).await;
```

Update existing tests of `app::run` in `src/app.rs::tests` (and any in `tests/`) to pass `None` for the new arg.

- [ ] **Step 3: Build TUI binary**

Stop the running TUI first via tmux:

```bash
tmux send-keys -t poly-tui q
```

Then:

Run: `cargo build --bin poly-tui`
Expected: Compiles clean.

- [ ] **Step 4: Run full lib tests**

Run: `cargo test --lib`
Expected: PASS — full suite.

- [ ] **Step 5: Smoke test relaunch**

```bash
tmux kill-session -t poly-tui 2>/dev/null
tmux new-session -d -s poly-tui -x 200 -y 50 './target/release/poly-tui.exe'
sleep 8
tmux capture-pane -t poly-tui -p | head -10
```

(release rebuild via `cargo build --release --bin poly-tui` first — adds about 15s but rebuilds.)

Expected: TUI shows USDC + No open positions + BTC strip with 5min countdown (existing behavior, no flag passed).

- [ ] **Step 6: Commit**

```bash
git add src/bin/poly-tui.rs src/app.rs
git commit -m "feat(tui): pipe window_minutes_tx from app to market_watch task"
```

---

## Task 12: README + TODO

**Files:**
- Modify: `README.md`
- Modify: `TODO.md`

- [ ] **Step 1: README — add window-minutes section**

Edit `README.md`. After the v1.7 maker mode section, insert:

````markdown
### Window length (v1.7.1, 5/15/60 min)

Default `--window-minutes 5` reproduces v1.7 behavior. Polymarket also offers 15-minute and 60-minute BTC up/down markets:

```bash
# 15min — 4× deeper liquidity than 5min, better for --maker
poly-trader --window-minutes 15 --direction up --base 5 --maker \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45

# 60min — supported but unvalidated by backtest
poly-trader --window-minutes 60 --direction up --base 5 \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45
```

| Window | Backtest | Liquidity | Hourly exposure (--max-step 5) |
|---|---|---|---|
| 5min  | validated +$7.5K/30d | ~$8K | $60/hr base |
| 15min | unvalidated         | ~$32K | $20/hr base |
| 60min | unvalidated         | varies | $5/hr base |

Probability structure (band, TP, SL on UP token) is window-length invariant; the strategy's positive expectancy on 5min should carry to 15/60 in theory, but real-money A/B is the only proof.

The TUI auto-detects the trader's window length from its event stream — no separate flag needed. If the trader switches windows mid-state (without `--reset`), it refuses to start with a clear error.
````

Find the Roadmap and add v1.7.1:

```markdown
- **v1.7.1** ✅ — `--window-minutes 5|15|60` flag (TUI auto-detects)
```

Find the Documentation list and add:

```markdown
- `docs/superpowers/specs/2026-05-10-window-minutes-design.md` — v1.7.1 design
- `docs/superpowers/plans/2026-05-10-window-minutes.md` — v1.7.1 plan
```

- [ ] **Step 2: TODO — tick v1.7.1**

Edit `TODO.md`. Insert before v1.7 section:

```markdown
## v1.7.1 — `--window-minutes` flag ✅ COMPLETE

Adds `--window-minutes 5|15|60` to `poly-trader`. TUI auto-tracks via `LadderState.window_minutes` from the event stream. See `docs/superpowers/specs/2026-05-10-window-minutes-design.md`.

- [x] `market.rs` — `window_seconds`/`window_slug`/`floor_window`/`next_window_boundary` parameterized helpers
- [x] `LadderState.window_minutes` field with serde back-compat default
- [x] CLI: `--window-minutes` flag with {5,15,60} validation
- [x] `SchedulerConfig.window_seconds` + `WindowConfig.window_seconds` + `MakerDeps` threading
- [x] Resolver timeout = `window_seconds + 300s` set at trader startup
- [x] Maker `cancel_deadline = window_ts + window_seconds - 30`
- [x] `MarketDiscovery::find_window(ts, mins)` trait change
- [x] TUI auto-detect via `mpsc::Sender<u32>` from app to market_watch
- [x] `restore_or_init` refuses mid-session window-length switch
- [x] README + TODO docs

**Open items / next:**
- v1.7.2: extend backtest binary to 15m/60m (currently 5m hardcoded)
- v1.8: real-money A/B test 5m vs 15m with --maker

---
```

- [ ] **Step 3: Verify build still clean**

Run: `cargo build --bin poly-trader`
Expected: Compiles clean.

Run: `cargo build --bin poly-tui`
Expected: Compiles clean.

- [ ] **Step 4: Commit**

```bash
git add README.md TODO.md
git commit -m "docs: README + TODO updated for v1.7.1 --window-minutes"
```

---

## Self-review

After all tasks:

**1. Spec coverage:**

| Spec section | Implemented in |
|---|---|
| Architecture (helpers in market.rs) | Task 1 |
| `--window-minutes` flag + validation | Task 3 |
| `LadderState.window_minutes` + serde back-compat | Task 2 |
| `SchedulerConfig.window_seconds` | Task 4 |
| `WindowConfig.window_seconds` | Task 5 |
| Maker cancel_deadline scales | Task 6 |
| `MarketDiscovery::find_window(ts, mins)` | Task 7 |
| `market_watch` dynamic via mpsc | Task 7 |
| `AppState.window_minutes` + handle_event | Task 8 |
| `UiState.window_minutes` + countdown | Task 9 |
| Resolver timeout scales | Task 10 (set at startup) |
| `restore_or_init` mismatch refusal | Task 10 |
| TUI mpsc wiring | Task 11 |
| README + TODO | Task 12 |

**2. Placeholder scan:** No "TBD" or vague text. All code blocks complete.

**3. Type consistency:** `window_minutes`, `window_seconds`, `MarketDiscovery::find_window(ts, mins)`, `LadderState.with_window_minutes`, `MakerDeps`, `MarketState.window_minutes`, `AppState.window_minutes`, `AppState.window_minutes_tx`, `UiState.window_minutes` spelled identically across files.

**4. Notes:**
- `MarketDiscovery::find_window` trait change touches every impl. Search-and-update in Task 7 covers all known impls; keep an eye out for any in `tests/` that the search misses.
- Backward-compatible: pre-v1.7.1 ladder state JSON deserializes with `window_minutes=5` via serde default. Old behavior preserved bit-for-bit.
- The `window_slug` 1-arg form is removed (replaced with 2-arg). Internal callers updated. No public API breakage outside this crate.
