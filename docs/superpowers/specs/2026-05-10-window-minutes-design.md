# v1.7.1 — `--window-minutes` flag for trader (5 / 15 / 60)

**Goal:** Parameterize the hardcoded 5-minute window in `poly-trader` so the same strategy 4 logic can run on Polymarket's BTC 15min and 1hr markets. 15min was observed to have ~4× deeper liquidity than 5min — better fills for `--maker` mode and ~3× lower hourly exposure for the same `--max-step`.

**Non-goal:** Mid-session window-length changes, simultaneous multi-window trading, TUI showing more than one window at a time, backtest validation of 15min/60min.

## Context

Strategy 4's TP/SL thresholds (band 0.45–0.55, TP 0.85, SL 0.45) operate on the UP-token probability — they are invariant to total window length. What scales linearly with window length is the resolver timeout, the maker-mode end-of-window cancel deadline, and the gamma slug. v1.7.1 extracts these into derived helpers and exposes a single `--window-minutes` CLI flag. TUI auto-detects the trader's window via the existing event stream (no separate TUI flag).

15min market liquidity at the time of writing: $31,922 CLOB depth, vs 5min's $8,500 — substantially deeper. 60min markets are also supported as an opt-in but unverified by backtest; flagged risk in the operator notes.

## Architecture

```
   TraderArgs.window_minutes (5|15|60, default 5)
              │
              ├── written into LadderState.window_minutes at session start
              │
              ├── flows into:
              │     SchedulerConfig.window_seconds  (next-boundary sleep)
              │     WindowConfig.window_seconds     (resolver timeout = secs+300)
              │     MakerDeps                        (cancel_deadline = window_ts+secs−30)
              │     window_slug(ts, mins)            (gamma fetch slug)
              │
              └── persisted via Redis ladder state (poly:prod:trader:ladder)


   TUI:
   ─────  reads LadderState from event stream OR Redis ladder key
                                │
                                ▼
   AppState.window_minutes  ←── latest TraderEvent.ladder.window_minutes
                                (default 5 if no events yet)
                                │
                                ▼
   AppState pushes updates via mpsc::Sender<u32> to market_watch task
                                │
                                ▼
   market_watch holds current window_minutes; uses floor_window/next_window_boundary
                                │
                                ▼
   ui.rs render_market_strip — countdown uses state.window_minutes
```

The trader process is the source of truth. The TUI is a passive observer.

## Allowed values

`--window-minutes ∈ {5, 15, 60}`. Clap rejects others at parse time. Ranges chosen because:
- `5` — current default, full backtest coverage
- `15` — observed 4× deeper liquidity, same probability structure
- `60` — exists on Polymarket but **unvalidated by backtest** — operator note in README

## CLI surface

```bash
# v1.7 default (unchanged)
poly-trader --direction up --base 5 --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45

# 15min maker
poly-trader --window-minutes 15 --direction up --base 5 --maker \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45

# 60min (caveat: not backtested)
poly-trader --window-minutes 60 --direction up --base 5 \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45
```

TUI takes no new flag. Auto-tracks whatever the running trader writes to its ladder state.

## Components

### `src/trader/market.rs` *(modify)*

Replace hardcoded `300` and `5m` with parameterized helpers. Old `floor_5min` / `next_5min_boundary` / `window_slug` become wrappers calling the new generic versions with `mins=5` for backward compat (existing callers remain compatible until they're migrated).

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

// Backward-compat (delete after callers migrate; both internal-only):
#[deprecated]
pub fn window_slug_5m(window_ts: i64) -> String { window_slug(window_ts, 5) }
#[deprecated]
pub fn floor_5min(now_ts: i64) -> i64 { floor_window(now_ts, 5) }
#[deprecated]
pub fn next_5min_boundary(now_ts: i64) -> i64 { next_window_boundary(now_ts, 5) }
```

All internal callers migrate immediately to the new helpers (deprecation warnings forbidden in `-D warnings` builds; callers replaced in same diff). The `#[deprecated]` annotation is signaling to anyone reading older code paths that these are gone.

### `src/trader/ladder.rs` *(modify)*

Add a field to `LadderState`:

```rust
pub struct LadderState {
    // ... existing fields
    /// Length of one trading window in minutes. {5, 15, 60}. Defaults to 5
    /// when deserializing pre-v1.7.1 state via serde(default).
    #[serde(default = "default_window_minutes")]
    pub window_minutes: u32,
}

fn default_window_minutes() -> u32 { 5 }

impl LadderState {
    pub fn new(direction: Direction, base: Decimal, max_step: u8, started_at: DateTime<Utc>, window_minutes: u32) -> Self { ... }
}
```

Migration: existing in-Redis ladder JSON without the field will deserialize with `window_minutes = 5`. Saved sessions transparently keep working in 5-minute mode.

### `src/trader/config.rs` *(modify)*

```rust
#[derive(Parser, ...)]
pub struct TraderArgs {
    // ... existing fields
    /// Trading window length in minutes. Strategy 4 backtest covers 5 only;
    /// 15 is observed deeper liquidity but unvalidated; 60 is unvalidated.
    #[arg(long, default_value = "5", value_parser = clap::value_parser!(u32))]
    pub window_minutes: u32,
}

impl TraderArgs {
    fn validate(&self) -> Result<(), ConfigError> {
        // ... existing rules
        if !matches!(self.window_minutes, 5 | 15 | 60) {
            return Err(ConfigError::InvalidWindowMinutes);
        }
        Ok(())
    }
}

#[derive(thiserror::Error, ...)]
pub enum ConfigError {
    // ... existing variants
    #[error("window-minutes must be 5, 15, or 60")]
    InvalidWindowMinutes,
}
```

### `src/trader/scheduler.rs::SchedulerConfig` *(modify)*

```rust
pub struct SchedulerConfig {
    pub max_windows: Option<u32>,
    pub window_seconds: i64,  // NEW — drives the next-boundary sleep
}
```

Scheduler's per-loop sleep computation switches from `floor_5min` to `floor_window(now, mins)`. Field added; existing tests that build `SchedulerConfig { max_windows: ... }` need updating to add `window_seconds: 300` (5min default).

### `src/trader/window.rs::WindowConfig` *(modify)*

```rust
pub struct WindowConfig {
    pub band_min: Decimal,
    pub band_max: Decimal,
    pub exit: Option<ExitConfig>,
    pub maker: bool,
    pub window_seconds: i64,  // NEW
}
```

Resolver timeout in `await_resolution_and_sweep`:

```rust
let timeout = std::time::Duration::from_secs((cfg.window_seconds + 300) as u64);
let resolver_with_timeout = ...;
```

Currently `PolymarketResolver::new` takes a fixed `Duration::from_secs(600)` at startup. We move the timeout to per-call (passed through `WindowConfig`) so it can scale.

### `src/trader/maker.rs::sell_with_tp_sl` *(modify)*

Replace hardcoded `window_ts + 270`:

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
    // ...
    let cancel_unix = window_ts + window_seconds - 30;  // 30s before close
    // ... rest unchanged
}
```

`run_maker` adds `window_seconds: i64` to its signature; passes through.

### `src/bin/poly-trader.rs` *(modify)*

Wire `args.window_minutes` everywhere:

```rust
let window_seconds = poly_tui::trader::market::window_seconds(args.window_minutes);

let ladder = restore_or_init(state_store.as_ref(), &args).await?;
// restore_or_init now constructs LadderState with window_minutes from args

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

// resolver: timeout still scales per-window via WindowConfig now
```

### `src/tui/market_watch.rs` *(modify)*

`MarketState` gains a `window_minutes: u32` field. The `run` task accepts an `mpsc::Receiver<u32>` for live updates from the TUI's app loop. On each gamma tick it uses `floor_window(now, current_mins)` to identify the active window.

```rust
pub async fn run(
    price_feed: Arc<dyn BtcPriceFeed>,
    market: Arc<dyn MarketDiscovery>,
    event_tx: mpsc::Sender<AppEvent>,
    mut window_minutes_rx: mpsc::Receiver<u32>,  // NEW
    shutdown: CancellationToken,
) {
    let mut window_minutes: u32 = 5;
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            Some(new_mins) = window_minutes_rx.recv() => {
                window_minutes = new_mins;
            }
            // ... existing rpc_ticker / gamma_ticker arms use floor_window(now, window_minutes)
        }
    }
}
```

`MarketState::seconds_to_next_boundary` already takes the timestamp; gains a `window_minutes` parameter.

### `src/app.rs` *(modify)*

`AppState` tracks `window_minutes`:

```rust
pub struct AppState {
    // ... existing
    pub window_minutes: u32,  // default 5
}
```

`handle_event` for `TraderEvent` updates from `ev.ladder.window_minutes`, sending a value to the market_watch task only when it changes:

```rust
AppEvent::TraderEvent(ev) => {
    if state.window_minutes != ev.ladder.window_minutes {
        state.window_minutes = ev.ladder.window_minutes;
        let _ = window_minutes_tx.try_send(ev.ladder.window_minutes);
    }
    // ... existing log push
}
```

### `src/ui.rs` *(modify)*

`UiState` carries `window_minutes`. `render_market_strip` uses it for the countdown:

```rust
let secs = m.seconds_to_next_boundary(now_ts, state.window_minutes);
```

Existing snapshot tests for 5m render are unchanged. New 15m and 60m snapshot tests added.

### `src/bin/poly-tui.rs` *(modify)*

No new CLI flag. Wire the `mpsc::channel::<u32>(8)` for window_minutes between `app::run` and `market_watch::run`. AppState's initial `window_minutes` reads from `state_store.load()` if a ladder exists in Redis; defaults to 5 otherwise.

## Sweep schedule (unchanged)

| Time | 5m | 15m | 60m |
|---|---|---|---|
| t=0 | limit BUY @ ask−0.01 | same | same |
| t=30s | sweep to ask | same | same |
| t=60s | sweep to ask+0.01 | same | same |
| t=90s | give up, skip window | same | same |
| **t=cancel** | t=270 | t=870 | t=3570 |
| **resolver timeout** | 600s | 1200s | 3900s |

Buy-formation is wall-clock noise — first 90s of any window is "prices forming." We don't scale sweep timing.

## Errors and edge cases

| Scenario | Handling |
|---|---|
| `--window-minutes 7` | Clap rejects with `InvalidWindowMinutes`. |
| Ladder state in Redis missing `window_minutes` field | `serde(default)` deserializes to 5. Trader logs a warning if argument's `window_minutes != 5`, advising `--reset`. |
| Trader switched from 5m to 15m mid-session (without `--reset`) | Trader detects mismatch in `restore_or_init` (`saved.window_minutes != args.window_minutes`) and refuses to start. Operator instructed to `--reset`. |
| TUI receives a TraderEvent with `window_minutes=15` mid-session | AppState updates, market_watch retargets to 15min slug on its next gamma tick. Brief render glitch (one tick) acceptable. |
| TUI cold start, no events yet, no ladder in Redis | Defaults to 5min until first event arrives. |
| Older trader process running 5m + new process configured 15m, sharing same Redis ladder | Lock prevents concurrent run. The first to acquire wins; the other dies cleanly with "lock held". |

## Testing

### Unit (`market.rs`)

| Test | Assertion |
|---|---|
| `window_seconds_5m` | `window_seconds(5) == 300` |
| `window_seconds_15m` | `window_seconds(15) == 900` |
| `window_seconds_60m` | `window_seconds(60) == 3600` |
| `floor_window_5m_matches_old` | `floor_window(t, 5) == floor_5min(t)` (legacy) |
| `floor_window_15m` | floors to 15-minute boundary correctly |
| `next_window_boundary_60m` | next boundary +3600s from floor |
| `window_slug_5m` | `window_slug(t, 5) == "btc-updown-5m-{t}"` |
| `window_slug_15m` | `window_slug(t, 15) == "btc-updown-15m-{t}"` |
| `window_slug_60m` | `window_slug(t, 60) == "btc-updown-60m-{t}"` |

### Unit (`config.rs`)

| Test | Assertion |
|---|---|
| `parses_window_minutes_default` | omitted → 5 |
| `parses_window_minutes_15` | accepts 15 |
| `parses_window_minutes_60` | accepts 60 |
| `validate_rejects_window_minutes_7` | `InvalidWindowMinutes` |
| `validate_rejects_window_minutes_0` | `InvalidWindowMinutes` |

### Unit (`ladder.rs`)

| Test | Assertion |
|---|---|
| `ladder_default_serde_window_minutes` | deserializing legacy JSON (no field) sets `window_minutes = 5` |
| `ladder_serde_roundtrip_window_minutes_15` | new state with 15 round-trips correctly |

### Unit (`maker.rs`)

| Test | Assertion |
|---|---|
| `cancel_deadline_scales_5m` | for `window_seconds=300`, deadline = `window_ts + 270` |
| `cancel_deadline_scales_15m` | for `window_seconds=900`, deadline = `window_ts + 870` |

### Unit (`window.rs`)

| Test | Assertion |
|---|---|
| `resolver_timeout_5m` | `cfg.window_seconds=300` → resolver timeout 600s |
| `resolver_timeout_15m` | `cfg.window_seconds=900` → resolver timeout 1200s |

### Unit (`market_watch.rs`)

| Test | Assertion |
|---|---|
| `seconds_to_next_boundary_5m_unchanged` | existing tests pass with explicit `window_minutes=5` |
| `seconds_to_next_boundary_15m` | for `now_ts` mid-15min-window, returns correct seconds |
| `window_minutes_update_via_channel` | feeding 15 via the channel switches the gamma slug target on next tick |

### Snapshot (`ui.rs`)

| Test | Assertion |
|---|---|
| existing 5m snapshots | unchanged |
| `renders_market_15m_countdown` | new snapshot for 15min countdown rendering |

### Integration

No new integration test; existing `e2e_trader.rs` and `maker_integration.rs` remain valid (default 5m). Manual smoke for 15m happens during real-money A/B.

## Backward compatibility

- Default behavior (`--window-minutes` omitted) is byte-identical to v1.7.
- Pre-v1.7.1 LadderState in Redis loads with `window_minutes=5` via serde default. Operator sees no surprise.
- Existing tests pass without modification (all scaffold values default to 5min).
- TUI continues showing 5min countdown until a trader writes a different `window_minutes`.

## Risk / out-of-scope

- **60-min market unvalidated by backtest.** Strategy 4 was tested on 5min only. The probability-structure-invariance argument applies but is theoretical for 60m. Real-money A/B test required. README documents this.
- **Mid-session switch refused.** No support for changing `window_minutes` without `--reset`. Operator must explicitly clear ladder state.
- **TUI can only track ONE window length at a time.** Running both a 5m and a 15m trader simultaneously would confuse the TUI; out-of-scope for v1.7.1.
- **Backtest binary is unchanged.** Backtest still hardcodes 5min. Adding 15m/60m to backtest is v1.7.2 scope.

## Migration / rollback

- Adds one new optional CLI flag and one optional ladder field. No breaking schema changes.
- Rollback: revert; old binaries deserialize new ladder JSON ignoring the field (Rust serde drops unknown fields by default). Forward and backward compatible at the JSON level.

## Related documents

- v1.5 trader spec: `docs/superpowers/specs/2026-05-10-trader-tp-sl-design.md`
- v1.7 maker spec: `docs/superpowers/specs/2026-05-10-trader-maker-mode-design.md`
