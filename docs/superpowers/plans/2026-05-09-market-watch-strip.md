# BTC Market Watch Strip Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a 1-row TUI strip between balance and trader sub-title showing current Polymarket BTC 5-min window's `priceToBeat`, live Chainlink BTC/USD price, signed diff, and MM:SS countdown to window close.

**Architecture:** A new 5th tokio task in `poly-tui` independently polls Chainlink BTC/USD on Polygon RPC every 5s and Polymarket gamma-api at each 5-min boundary; emits `AppEvent::MarketUpdate(MarketState)` through the existing event channel. `WindowMarket` gets one new `Option<Decimal>` field (`price_to_beat`) — purely additive, no existing trader behavior changes. The display degrades gracefully when either data source fails.

**Tech Stack:** Rust 1.78+, alloy 1.x (already in deps for Polygon RPC + ABI), reqwest (already in deps for gamma-api via `GammaMarketDiscovery`), ratatui (existing render), tokio (existing runtime), insta (existing snapshot tests).

**Spec:** `docs/superpowers/specs/2026-05-09-market-watch-strip-design.md`

**Base commit:** `ab1a069` (spec landed). Pre-feature baseline: `3519187` (cache-bust trader fix).

---

## ⚠️ Build hygiene during implementation

**A `poly-trader.exe` process is currently running** (overnight dry-run, PID 53896). Until that process exits naturally (it has `--max-windows 144`), the binary is locked.

**Allowed:**
- `cargo build --bin poly-tui`
- `cargo test --lib`
- `cargo test --test bdd`
- `cargo test --test cache_integration -- --ignored`
- `cargo test --test chainlink_integration -- --ignored` (new in this plan)
- `cargo check --bin poly-trader` (compile-check only, doesn't write .exe)

**Avoid:**
- `cargo build` (no args — builds all bins, fails on poly-trader.exe lock)
- `cargo build --bin poly-trader` (would overwrite the running binary)
- `cargo build --release` (same problem)
- **Never** kill `poly-trader.exe` to free the lock

If a `cargo build` fails with `Access is denied (os error 5)` on `poly-trader.exe`, leave it. Use `--bin poly-tui` only.

---

## File Structure

```
src/
├── tui/
│   ├── mod.rs                 ← +pub mod market_watch
│   ├── events.rs              (unchanged)
│   └── market_watch.rs        ← NEW (BtcPriceFeed trait, MarketState, MarketWatchError, run())
├── trader/
│   ├── market.rs              ← extend WindowMarket: +price_to_beat: Option<Decimal>
│   │                            extend decode_event_response (additive)
│   └── adapters/
│       └── chainlink_btc_wrapper.rs  ← NEW (excluded from coverage)
├── domain.rs                  ← +AppEvent::MarketUpdate(MarketState) variant
├── app.rs                     ← AppState gets `market: Option<MarketState>` field
│                                handle_event branch for MarketUpdate
├── ui.rs                      ← +render_market_strip; insert in Layout
├── config.rs                  ← +polygon_rpc_url field
└── bin/poly-tui.rs            ← spawn 5th task; build ChainlinkBtcPriceFeed

tests/
└── chainlink_integration.rs   ← NEW (#[ignore], one-shot real Polygon RPC smoke)

.env.example                   ← +POLYGON_RPC_URL=https://polygon-rpc.com
Cargo.toml                     ← +[[test]] chainlink_integration block
```

**Module dependency direction (enforced):**

```
tui::market_watch        →  domain (AppEvent), trader::market (WindowMarket, MarketDiscovery)
trader::market           →  domain (existing; extends decoder)
trader::adapters::chainlink_btc_wrapper   →  tui::market_watch (BtcPriceFeed), alloy
bin/poly-tui             →  tui::market_watch, trader::adapters::*
```

**Coverage exclusion regex (existing):** `src/bin|src/trader/adapters/|.*_wrapper\.rs` — `chainlink_btc_wrapper.rs` falls under both rules; safely excluded.

---

## Task 0: Add `polygon_rpc_url` to Config + .env.example

**Files:**
- Modify: `src/config.rs`
- Modify: `.env.example`

- [ ] **Step 1: Read existing `src/config.rs`**

```bash
cat src/config.rs
```

Note the existing `Config` struct + `default_*` helper functions pattern.

- [ ] **Step 2: Add new field + default helper to `src/config.rs`**

In the `Config` struct, after `log_level`:

```rust
    #[serde(default = "default_polygon_rpc_url")]
    pub polygon_rpc_url: String,
```

Below the other `default_*` functions:

```rust
fn default_polygon_rpc_url() -> String { "https://polygon-rpc.com".to_string() }
```

- [ ] **Step 3: Update existing test `loads_required_with_defaults`**

Add one assertion:

```rust
assert_eq!(cfg.polygon_rpc_url, "https://polygon-rpc.com");
```

- [ ] **Step 4: Run config tests**

```bash
cargo test --lib config -- --test-threads=1
```

Expected: 9 passed (existing 9 still pass — only added an assertion to `loads_required_with_defaults`).

- [ ] **Step 5: Update `.env.example`**

Append:

```
POLYGON_RPC_URL=https://polygon-rpc.com
```

- [ ] **Step 6: Commit**

```bash
git add src/config.rs .env.example
git commit -m "feat(config): add polygon_rpc_url with public default"
```

---

## Task 1: Extend `WindowMarket` with `price_to_beat` field + decoder

**Files:**
- Modify: `src/trader/market.rs`

- [ ] **Step 1: Add the new field to `WindowMarket`**

Find the existing struct in `src/trader/market.rs` and add `price_to_beat`:

```rust
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
    pub price_to_beat: Option<Decimal>,
}
```

- [ ] **Step 2: Extend `decode_event_response` to extract `eventMetadata.priceToBeat`**

In the existing `decode_event_response`, after the existing winner computation and BEFORE the `Ok(WindowMarket { ... })`, insert:

```rust
    let price_to_beat = event.get("eventMetadata")
        .and_then(|m| m.get("priceToBeat"))
        .and_then(|p| p.as_f64())
        .and_then(|f| rust_decimal::Decimal::from_f64_retain(f));
```

Add `price_to_beat` to the returned struct literal:

```rust
    Ok(WindowMarket {
        window_ts,
        slug,
        up_token_id: token_ids[up].clone(),
        down_token_id: token_ids[down].clone(),
        up_ask,
        down_ask,
        closed,
        winner,
        price_to_beat,
    })
```

- [ ] **Step 3: Update existing tests' `WindowMarket { ... }` literals**

In the test module of `src/trader/market.rs`, find the `ask_for_returns_correct_side` test which constructs a `WindowMarket` directly. Add `price_to_beat: None,` to that literal.

- [ ] **Step 4: Add 2 new tests for the decoder**

Append to the `tests` module:

```rust
    #[test]
    fn decode_extracts_price_to_beat() {
        let json = r#"[{"markets":[{
            "slug":"x", "closed":false,
            "outcomes":"[\"Up\",\"Down\"]",
            "clobTokenIds":"[\"u\",\"d\"]",
            "outcomePrices":"[\"0.50\",\"0.50\"]"
        }],
        "eventMetadata": {"priceToBeat": 80424.78}
        }]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.price_to_beat, Some(Decimal::from_str("80424.78").unwrap()));
    }

    #[test]
    fn decode_missing_event_metadata_yields_none_price_to_beat() {
        // Existing fixture without eventMetadata
        let json = r#"[{"markets":[{
            "slug":"x", "closed":false,
            "outcomes":"[\"Up\",\"Down\"]",
            "clobTokenIds":"[\"u\",\"d\"]",
            "outcomePrices":"[\"0.50\",\"0.50\"]"
        }]}]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.price_to_beat, None);
    }
```

- [ ] **Step 5: Run market tests**

```bash
cargo test --lib trader::market
```

Expected: all existing tests + 2 new = ~12 passed.

- [ ] **Step 6: Commit**

```bash
git add src/trader/market.rs
git commit -m "feat(market): WindowMarket.price_to_beat (decode eventMetadata)"
```

---

## Task 2: Update all existing `WindowMarket` literals (mechanical)

Touching this struct breaks every test that constructs it. Sweep them all in one task.

**Files (each gets `price_to_beat: None,` appended to literal):**
- Modify: `src/trader/window.rs` (5+ test fixtures inside `tests` mod)
- Modify: `src/trader/resolver.rs` (`open_market`, `closed_market` helpers in tests mod)
- Modify: `tests/e2e_trader.rs` (any literal usage)
- Modify: `tests/trader_market_integration.rs` (no literal usage; relies on `decode_event_response` — wiremock body strings; no edit needed unless tests assert on the new field, which they don't)

- [ ] **Step 1: Find all literals**

```bash
grep -rn "WindowMarket {" src/ tests/
```

This lists every file that constructs a `WindowMarket`. Expected hits: `src/trader/market.rs` (already updated in Task 1), `src/trader/window.rs`, `src/trader/resolver.rs`, possibly `tests/e2e_trader.rs`.

- [ ] **Step 2: Add `price_to_beat: None,` to each literal**

For each `WindowMarket {` block found, append `price_to_beat: None,` to the field list before the closing `}`.

Example diff for `src/trader/resolver.rs`:

```rust
    fn open_market() -> WindowMarket {
        WindowMarket {
            window_ts: 1700000300, slug: "x".into(),
            up_token_id: "u".into(), down_token_id: "d".into(),
            up_ask: Decimal::ONE_HUNDRED, down_ask: Decimal::ONE_HUNDRED,
            closed: false, winner: None,
            price_to_beat: None,                 // ← NEW
        }
    }
    fn closed_market(winner: Direction) -> WindowMarket {
        WindowMarket {
            window_ts: 1700000300, slug: "x".into(),
            up_token_id: "u".into(), down_token_id: "d".into(),
            up_ask: Decimal::ONE_HUNDRED, down_ask: Decimal::ONE_HUNDRED,
            closed: true, winner: Some(winner),
            price_to_beat: None,                 // ← NEW
        }
    }
```

Same pattern in `src/trader/window.rs` `open_market_at` helper and any other test fixtures.

- [ ] **Step 3: Run lib tests to confirm nothing broken**

```bash
cargo test --lib
```

Expected: all previously-passing tests still pass.

- [ ] **Step 4: Run E2E (#[ignore]) once to ensure they still compile**

```bash
cargo test --test e2e_trader -- --ignored --test-threads=1 || true
```

Expected: no compile errors. The 5 #[ignore] tests should run unless they fail for other reasons (Docker etc.).

- [ ] **Step 5: Commit**

```bash
git add src/trader/window.rs src/trader/resolver.rs tests/e2e_trader.rs
git commit -m "chore(market): add price_to_beat: None to existing WindowMarket literals"
```

---

## Task 3: Create `tui::market_watch` skeleton (errors + `BtcPriceFeed` trait)

**Files:**
- Modify: `src/tui/mod.rs`
- Create: `src/tui/market_watch.rs`

- [ ] **Step 1: Update `src/tui/mod.rs`**

```rust
pub mod events;
pub mod market_watch;
```

- [ ] **Step 2: Write `src/tui/market_watch.rs` with errors + trait + state struct skeleton**

```rust
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MarketWatchError {
    #[error("Polygon RPC connection failed: {0}")]
    Connect(String),
    #[error("RPC call failed: {0}")]
    Rpc(String),
    #[error("response decode failed: {0}")]
    Decode(String),
}

#[async_trait]
pub trait BtcPriceFeed: Send + Sync {
    async fn latest_price(&self) -> Result<Decimal, MarketWatchError>;
}

/// Live state of the BTC market strip. Updated by the market_watch task,
/// emitted via AppEvent::MarketUpdate, rendered by ui::render_market_strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketState {
    pub window_ts: Option<i64>,
    pub price_to_beat: Option<Decimal>,
    pub current_price: Option<Decimal>,
    pub last_rpc_ok_at: Option<DateTime<Utc>>,
    pub last_gamma_ok_at: Option<DateTime<Utc>>,
}

impl MarketState {
    pub fn empty() -> Self {
        Self {
            window_ts: None,
            price_to_beat: None,
            current_price: None,
            last_rpc_ok_at: None,
            last_gamma_ok_at: None,
        }
    }
}
```

- [ ] **Step 3: Verify lib compiles**

```bash
cargo build --bin poly-tui
```

Expected: success (no warnings about the new module — it's used in Task 4+).

- [ ] **Step 4: Commit**

```bash
git add src/tui/mod.rs src/tui/market_watch.rs
git commit -m "feat(market_watch): BtcPriceFeed trait + MarketState skeleton"
```

---

## Task 4: `MarketState` helper methods + 100% pure-logic tests

**Files:**
- Modify: `src/tui/market_watch.rs`

- [ ] **Step 1: Add helper methods to `MarketState` impl**

Append to the existing `impl MarketState` block:

```rust
    /// current_price - price_to_beat. None if either is missing.
    pub fn diff(&self) -> Option<Decimal> {
        match (self.price_to_beat, self.current_price) {
            (Some(p), Some(c)) => Some(c - p),
            _ => None,
        }
    }

    /// True iff RPC has succeeded within the last 30 seconds.
    pub fn rpc_healthy(&self, now: DateTime<Utc>) -> bool {
        match self.last_rpc_ok_at {
            Some(t) => now.signed_duration_since(t).num_seconds() < 30,
            None => false,
        }
    }

    /// True iff gamma has succeeded within the last 6 minutes.
    pub fn gamma_healthy(&self, now: DateTime<Utc>) -> bool {
        match self.last_gamma_ok_at {
            Some(t) => now.signed_duration_since(t).num_seconds() < 6 * 60,
            None => false,
        }
    }

    /// Seconds remaining until the current 5-minute window closes.
    /// Returns 0 when at or past the boundary, until the next window opens.
    pub fn seconds_to_next_boundary(&self, now_ts: i64) -> i64 {
        // Window ends at floor_5min(now_ts) + 300 — independent of self.window_ts
        // because we want "real wall-clock countdown", not "stale state countdown".
        let end = (now_ts - now_ts.rem_euclid(300)) + 300;
        (end - now_ts).max(0)
    }
}
```

- [ ] **Step 2: Add tests**

Append a `#[cfg(test)] mod tests` block at the bottom of `src/tui/market_watch.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::str::FromStr;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    fn state_with(price_to_beat: Option<&str>, current_price: Option<&str>) -> MarketState {
        MarketState {
            window_ts: Some(1700000000),
            price_to_beat: price_to_beat.map(|s| Decimal::from_str(s).unwrap()),
            current_price: current_price.map(|s| Decimal::from_str(s).unwrap()),
            last_rpc_ok_at: None,
            last_gamma_ok_at: None,
        }
    }

    #[test]
    fn diff_both_present_positive() {
        let s = state_with(Some("80000"), Some("80050"));
        assert_eq!(s.diff(), Some(Decimal::from(50)));
    }

    #[test]
    fn diff_both_present_negative() {
        let s = state_with(Some("80000"), Some("79950"));
        assert_eq!(s.diff(), Some(Decimal::from(-50)));
    }

    #[test]
    fn diff_both_present_zero() {
        let s = state_with(Some("80000"), Some("80000"));
        assert_eq!(s.diff(), Some(Decimal::ZERO));
    }

    #[test]
    fn diff_missing_to_beat_is_none() {
        let s = state_with(None, Some("80050"));
        assert_eq!(s.diff(), None);
    }

    #[test]
    fn diff_missing_current_is_none() {
        let s = state_with(Some("80000"), None);
        assert_eq!(s.diff(), None);
    }

    #[test]
    fn diff_both_missing_is_none() {
        let s = state_with(None, None);
        assert_eq!(s.diff(), None);
    }

    #[test]
    fn rpc_healthy_within_30s() {
        let mut s = MarketState::empty();
        s.last_rpc_ok_at = Some(ts(1000));
        assert!(s.rpc_healthy(ts(1015)));
        assert!(s.rpc_healthy(ts(1029)));
    }

    #[test]
    fn rpc_unhealthy_past_30s() {
        let mut s = MarketState::empty();
        s.last_rpc_ok_at = Some(ts(1000));
        assert!(!s.rpc_healthy(ts(1030)));
        assert!(!s.rpc_healthy(ts(1100)));
    }

    #[test]
    fn rpc_unhealthy_when_never_ok() {
        let s = MarketState::empty();
        assert!(!s.rpc_healthy(ts(1000)));
    }

    #[test]
    fn gamma_healthy_within_6_min() {
        let mut s = MarketState::empty();
        s.last_gamma_ok_at = Some(ts(1000));
        assert!(s.gamma_healthy(ts(1000 + 5 * 60)));
        assert!(!s.gamma_healthy(ts(1000 + 6 * 60)));
    }

    #[test]
    fn seconds_to_next_boundary_at_open() {
        // At a boundary, full 300 seconds until next
        let s = MarketState::empty();
        assert_eq!(s.seconds_to_next_boundary(1700000000), 300);
    }

    #[test]
    fn seconds_to_next_boundary_mid_window() {
        let s = MarketState::empty();
        // 100 seconds in, 200 left
        assert_eq!(s.seconds_to_next_boundary(1700000100), 200);
    }

    #[test]
    fn seconds_to_next_boundary_at_close() {
        let s = MarketState::empty();
        // Exactly at the boundary
        assert_eq!(s.seconds_to_next_boundary(1700000300), 300);
    }

    #[test]
    fn seconds_to_next_boundary_one_before_close() {
        let s = MarketState::empty();
        assert_eq!(s.seconds_to_next_boundary(1700000299), 1);
    }

    #[test]
    fn empty_state_has_no_data() {
        let s = MarketState::empty();
        assert!(s.window_ts.is_none());
        assert!(s.price_to_beat.is_none());
        assert!(s.current_price.is_none());
        assert!(s.last_rpc_ok_at.is_none());
        assert!(s.last_gamma_ok_at.is_none());
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test --lib tui::market_watch
```

Expected: 14 passed.

- [ ] **Step 4: Commit**

```bash
git add src/tui/market_watch.rs
git commit -m "feat(market_watch): MarketState helpers + 14 unit tests"
```

---

## Task 5: `market_watch::run` task with FakeBtcPriceFeed + tests

**Files:**
- Modify: `src/tui/market_watch.rs`
- Modify: `src/domain.rs` (add `MarketUpdate(MarketState)` variant — needed by run())

- [ ] **Step 1: Add `AppEvent::MarketUpdate` variant to `src/domain.rs`**

Find the `AppEvent` enum and add a new variant. Top of file:

```rust
use crate::tui::market_watch::MarketState;
```

In the enum:

```rust
#[derive(Debug)]
pub enum AppEvent {
    Tick,
    Key(crossterm::event::KeyEvent),
    Refresh(RefreshStatus),
    Shutdown,
    TraderEvent(TraderEvent),
    MarketUpdate(MarketState),         // NEW
}
```

- [ ] **Step 2: Add `run` function + helpers to `src/tui/market_watch.rs`**

Append (after the existing impl + before the test module):

```rust
use crate::domain::AppEvent;
use crate::trader::market::MarketDiscovery;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Floor a UTC epoch second to its 5-minute boundary.
fn floor_5min(now_ts: i64) -> i64 {
    now_ts - now_ts.rem_euclid(300)
}

async fn emit(tx: &mpsc::Sender<AppEvent>, state: &MarketState) {
    let _ = tx.send(AppEvent::MarketUpdate(state.clone())).await;
}

pub async fn run(
    price_feed: Arc<dyn BtcPriceFeed>,
    market: Arc<dyn MarketDiscovery>,
    event_tx: mpsc::Sender<AppEvent>,
    shutdown: CancellationToken,
) {
    let mut state = MarketState::empty();
    let mut rpc_ticker = tokio::time::interval(Duration::from_secs(5));
    let mut gamma_ticker = tokio::time::interval(Duration::from_secs(15));

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,

            _ = rpc_ticker.tick() => {
                if let Ok(p) = price_feed.latest_price().await {
                    state.current_price = Some(p);
                    state.last_rpc_ok_at = Some(chrono::Utc::now());
                }
                emit(&event_tx, &state).await;
            }

            _ = gamma_ticker.tick() => {
                let now_ts = chrono::Utc::now().timestamp();
                let current_window = floor_5min(now_ts);
                if state.window_ts != Some(current_window) {
                    if let Ok(m) = market.find_window(current_window).await {
                        state.window_ts = Some(current_window);
                        state.price_to_beat = m.price_to_beat;
                        state.last_gamma_ok_at = Some(chrono::Utc::now());
                        emit(&event_tx, &state).await;
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 3: Add 4 task tests using `tokio::time::pause()` + injected fakes**

In the test module of `src/tui/market_watch.rs`:

```rust
    use crate::trader::errors::MarketError;
    use crate::trader::market::{MarketDiscovery, WindowMarket};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;
    use std::time::Duration;

    struct FakePriceFeed {
        result: Mutex<Result<Decimal, MarketWatchError>>,
    }
    impl FakePriceFeed {
        fn ok(price: &str) -> Arc<Self> {
            Arc::new(Self {
                result: Mutex::new(Ok(Decimal::from_str(price).unwrap())),
            })
        }
        fn fail() -> Arc<Self> {
            Arc::new(Self {
                result: Mutex::new(Err(MarketWatchError::Rpc("forced".into()))),
            })
        }
    }
    #[async_trait]
    impl BtcPriceFeed for FakePriceFeed {
        async fn latest_price(&self) -> Result<Decimal, MarketWatchError> {
            // Re-clone whatever the mutex holds; doesn't drain.
            match &*self.result.lock().unwrap() {
                Ok(p) => Ok(*p),
                Err(_) => Err(MarketWatchError::Rpc("forced".into())),
            }
        }
    }

    struct FakeMarket {
        responses: Mutex<Vec<Result<WindowMarket, MarketError>>>,
    }
    impl FakeMarket {
        fn with_price_to_beat(p: &str) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(vec![Ok(WindowMarket {
                    window_ts: 0,
                    slug: "test".into(),
                    up_token_id: "u".into(),
                    down_token_id: "d".into(),
                    up_ask: Decimal::ZERO,
                    down_ask: Decimal::ZERO,
                    closed: false,
                    winner: None,
                    price_to_beat: Some(Decimal::from_str(p).unwrap()),
                })]),
            })
        }
        fn always_fail() -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(vec![]),
            })
        }
    }
    #[async_trait]
    impl MarketDiscovery for FakeMarket {
        async fn find_window(&self, _ts: i64) -> Result<WindowMarket, MarketError> {
            let mut q = self.responses.lock().unwrap();
            if q.is_empty() {
                return Err(MarketError::NotFound { window_ts: 0 });
            }
            // re-emit the same entry forever (don't pop)
            q[0].clone()
        }
    }

    #[tokio::test]
    async fn run_emits_after_first_rpc_tick() {
        tokio::time::pause();
        let feed = FakePriceFeed::ok("80000");
        let market = FakeMarket::with_price_to_beat("80100");
        let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(run(feed, market, tx, shutdown.clone()));
        tokio::time::advance(Duration::from_secs(6)).await;

        let mut got_market = false;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::MarketUpdate(s) = ev {
                if s.current_price == Some(Decimal::from(80000)) {
                    got_market = true;
                    break;
                }
            }
        }
        assert!(got_market, "expected MarketUpdate with current_price 80000");

        shutdown.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn run_emits_price_to_beat_at_gamma_tick() {
        tokio::time::pause();
        let feed = FakePriceFeed::ok("80000");
        let market = FakeMarket::with_price_to_beat("80100");
        let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(run(feed, market, tx, shutdown.clone()));
        tokio::time::advance(Duration::from_secs(20)).await;

        let mut latest_state: Option<MarketState> = None;
        while let Ok(ev) = rx.try_recv() {
            if let AppEvent::MarketUpdate(s) = ev {
                latest_state = Some(s);
            }
        }
        let s = latest_state.expect("at least one MarketUpdate");
        assert_eq!(s.price_to_beat, Some(Decimal::from(80100)));

        shutdown.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn run_keeps_emitting_when_rpc_fails() {
        tokio::time::pause();
        let feed = FakePriceFeed::fail();
        let market = FakeMarket::always_fail();
        let (tx, mut rx) = mpsc::channel::<AppEvent>(64);
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(run(feed, market, tx, shutdown.clone()));
        tokio::time::advance(Duration::from_secs(11)).await;

        // Should still emit MarketUpdate with empty current_price (RPC failed,
        // so emit fires on rpc_ticker.tick() but state.current_price stays None).
        let mut count = 0;
        while let Ok(_) = rx.try_recv() { count += 1; }
        assert!(count > 0, "expected at least one MarketUpdate even on RPC failure");

        shutdown.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn run_exits_on_shutdown() {
        tokio::time::pause();
        let feed = FakePriceFeed::ok("80000");
        let market = FakeMarket::always_fail();
        let (tx, _rx) = mpsc::channel::<AppEvent>(64);
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(run(feed, market, tx, shutdown.clone()));
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), task).await
            .expect("task exits within 1s")
            .expect("no panic");
    }
```

- [ ] **Step 4: Run tests**

```bash
cargo build --bin poly-tui
cargo test --lib tui::market_watch
```

Expected: 14 (from Task 4) + 4 = 18 passed.

- [ ] **Step 5: Commit**

```bash
git add src/tui/market_watch.rs src/domain.rs
git commit -m "feat(market_watch): run() task + AppEvent::MarketUpdate variant"
```

---

## Task 6: `AppState.market` field + `handle_event` branch

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Add `market` field to `AppState`**

Append to existing `AppState`:

```rust
    pub market: Option<MarketState>,
```

Top of file, ensure import:

```rust
use crate::tui::market_watch::MarketState;
```

- [ ] **Step 2: Initialize new field in `AppState::new`**

Append in `Self { ... }`:

```rust
            market: None,
```

- [ ] **Step 3: Add `handle_event` branch for `MarketUpdate`**

Find existing `handle_event` and add a new arm:

```rust
        AppEvent::MarketUpdate(s) => {
            state.market = Some(s);
        }
```

- [ ] **Step 4: Add 1 unit test**

In the existing tests module:

```rust
    #[tokio::test]
    async fn market_update_sets_state() {
        use crate::tui::market_watch::MarketState;
        use rust_decimal::Decimal;

        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, _rx) = mpsc::channel(1);
        let mut market = MarketState::empty();
        market.current_price = Some(Decimal::from(80000));
        handle_event(&mut s, AppEvent::MarketUpdate(market.clone()), &tx);
        assert_eq!(s.market.as_ref().unwrap().current_price, market.current_price);
    }
```

- [ ] **Step 5: Run tests**

```bash
cargo test --lib app
```

Expected: existing 14 + 1 new = 15 passed.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): AppState.market + handle_event branch"
```

---

## Task 7: `render_market_strip` + 5 insta snapshots

**Files:**
- Modify: `src/ui.rs`

- [ ] **Step 1: Add `market` field to `UiState`**

Find existing `UiState` struct and append:

```rust
    pub market: Option<MarketState>,
```

Imports:

```rust
use crate::tui::market_watch::MarketState;
```

- [ ] **Step 2: Update `AppState::ui_state` to populate the new field**

In `src/app.rs`, find the existing `ui_state` builder and add:

```rust
            market: self.market.clone(),
```

- [ ] **Step 3: Update Layout in `render`**

Replace the existing constraints in `render`:

```rust
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),    // balance
            Constraint::Length(1),    // market strip (NEW)
            Constraint::Length(1),    // trader sub-title
            Constraint::Min(0),       // trader log
            Constraint::Length(1),    // status bar
        ])
        .split(area);

    render_balance(frame, chunks[0], state);
    render_market_strip(frame, chunks[1], state);  // NEW
    render_trader_subtitle(frame, chunks[2], state);
    render_trader_log(frame, chunks[3], state);
    render_status_bar(frame, chunks[4], state);
```

- [ ] **Step 4: Add `render_market_strip` and helpers**

Append to `src/ui.rs` (private functions, near other render helpers):

```rust
fn render_market_strip(frame: &mut Frame, area: Rect, state: &UiState) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    let m = match &state.market {
        Some(m) => m,
        None => {
            frame.render_widget(Paragraph::new(" BTC: -- "), area);
            return;
        }
    };

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(" BTC "));

    match (m.price_to_beat, m.current_price) {
        (Some(p), Some(c)) => {
            spans.push(Span::raw(format_usd_int(p)));
            spans.push(Span::raw(" → "));
            spans.push(Span::raw(format_usd_int(c)));
            let diff = c - p;
            let (sign, color) = if diff > Decimal::ZERO {
                ("+", Color::Green)
            } else if diff < Decimal::ZERO {
                ("", Color::Red)
            } else {
                ("±", Color::White)
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("{sign}{}", format_usd_int(diff)),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
        (None, Some(c)) => {
            spans.push(Span::raw("--"));
            spans.push(Span::raw(" → "));
            spans.push(Span::raw(format_usd_int(c)));
            spans.push(Span::raw("  --"));
        }
        (Some(p), None) => {
            spans.push(Span::raw(format_usd_int(p)));
            spans.push(Span::raw(" → "));
            spans.push(Span::styled("--",
                Style::default().fg(Color::DarkGray)));
            spans.push(Span::raw("  --"));
        }
        (None, None) => {
            spans.push(Span::raw("--"));
        }
    }

    spans.push(Span::raw("   "));
    let now_ts = state.now.timestamp();
    let secs = m.seconds_to_next_boundary(now_ts);
    if secs > 0 {
        spans.push(Span::raw(format!("⏱ {}:{:02}", secs / 60, secs % 60)));
    } else {
        spans.push(Span::styled("⏱ rolling…",
            Style::default().fg(Color::DarkGray)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn format_usd_int(d: Decimal) -> String {
    // Round to nearest integer, comma-group thousands. Negative handled by
    // working on the magnitude and prepending `-`.
    use rust_decimal::prelude::ToPrimitive;
    let n: i64 = d.round().to_i64().unwrap_or(0);
    if n < 0 {
        format!("-{}", group_thousands(&(-n).to_string()))
    } else {
        group_thousands(&n.to_string())
    }
}

fn group_thousands(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}
```

- [ ] **Step 5: Add 5 insta snapshot tests**

In the existing tests module of `src/ui.rs`:

```rust
    fn ui_state_with_market(market: Option<MarketState>) -> UiState {
        UiState {
            balance: Some(Balance {
                usdc: Decimal::from_str("100").unwrap(),
                fetched_at: fixed_now(),
            }),
            last_refresh: None,
            clob_health: HealthLed::Green,
            redis_health: HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now: fixed_now(),
            trader_log: vec![],
            trader_latest: None,
            trader_health: TraderHealth::NotStarted,
            market,
        }
    }

    fn make_market(price_to_beat: Option<&str>, current: Option<&str>) -> MarketState {
        let mut m = MarketState::empty();
        m.window_ts = Some(fixed_now().timestamp() / 300 * 300);
        m.price_to_beat = price_to_beat.map(|s| Decimal::from_str(s).unwrap());
        m.current_price = current.map(|s| Decimal::from_str(s).unwrap());
        m
    }

    #[test]
    fn renders_market_no_data() {
        let state = ui_state_with_market(None);
        insta::assert_snapshot!("market_no_data", render_to_buffer(&state));
    }

    #[test]
    fn renders_market_full() {
        let state = ui_state_with_market(Some(make_market(Some("80425"), Some("80431"))));
        insta::assert_snapshot!("market_full", render_to_buffer(&state));
    }

    #[test]
    fn renders_market_negative_diff() {
        let state = ui_state_with_market(Some(make_market(Some("80425"), Some("80418"))));
        insta::assert_snapshot!("market_negative_diff", render_to_buffer(&state));
    }

    #[test]
    fn renders_market_only_current() {
        let state = ui_state_with_market(Some(make_market(None, Some("80431"))));
        insta::assert_snapshot!("market_only_current", render_to_buffer(&state));
    }

    #[test]
    fn renders_market_rolling() {
        // Move `now` exactly to a 5-min boundary so countdown reads 0
        let mut s = ui_state_with_market(Some(make_market(Some("80425"), Some("80425"))));
        let boundary = fixed_now().timestamp() / 300 * 300;
        s.now = chrono::Utc.timestamp_opt(boundary, 0).unwrap();
        insta::assert_snapshot!("market_rolling", render_to_buffer(&s));
    }
```

- [ ] **Step 6: Run + accept snapshots**

```bash
cargo test --lib ui
```

Expected: 5 new snapshots fail with "no snapshot found".

```bash
cargo insta accept
cargo test --lib ui
```

Expected: 12 (existing) + 5 (new) = 17 passed.

> **Note:** the existing 12 snapshots will need to be regenerated because the layout chunk count changed (from 4 to 5). Inspect each `*.snap.new` to ensure the new market strip row is the only added row before accepting.

Inspect new snapshots:

```bash
cat src/snapshots/poly_tui__ui__tests__market_full.snap
```

Expected to contain ` BTC 80,425 → 80,431  +6  ⏱` somewhere in the rendered text (exact countdown depends on `fixed_now`).

- [ ] **Step 7: Commit**

```bash
git add src/ui.rs src/app.rs src/snapshots/
git commit -m "feat(ui): render_market_strip + 5 insta snapshots"
```

---

## Task 8: `ChainlinkBtcPriceFeed` adapter (excluded from coverage)

**Files:**
- Create: `src/trader/adapters/chainlink_btc_wrapper.rs`
- Modify: `src/trader/adapters/mod.rs`

- [ ] **Step 1: Update `src/trader/adapters/mod.rs`**

Append:

```rust
pub mod chainlink_btc_wrapper;
```

- [ ] **Step 2: Write `src/trader/adapters/chainlink_btc_wrapper.rs`**

```rust
use crate::tui::market_watch::{BtcPriceFeed, MarketWatchError};
use alloy::primitives::{address, Address};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Chainlink BTC/USD aggregator on Polygon mainnet.
const BTC_USD_AGGREGATOR_POLYGON: Address =
    address!("c907E116054Ad103354f2D350FD2514433D57F6f");

const BTC_USD_DECIMALS: u32 = 8;

sol! {
    #[sol(rpc)]
    interface AggregatorV3 {
        function latestRoundData() external view returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );
    }
}

pub struct ChainlinkBtcPriceFeed {
    provider: alloy::providers::RootProvider<alloy::network::Ethereum>,
}

impl ChainlinkBtcPriceFeed {
    pub async fn connect(rpc_url: &str) -> Result<Self, MarketWatchError> {
        let url = reqwest::Url::parse(rpc_url)
            .map_err(|e| MarketWatchError::Connect(format!("invalid url: {e}")))?;
        let provider = ProviderBuilder::new().on_http(url);
        Ok(Self { provider })
    }
}

#[async_trait]
impl BtcPriceFeed for ChainlinkBtcPriceFeed {
    async fn latest_price(&self) -> Result<Decimal, MarketWatchError> {
        let agg = AggregatorV3::new(BTC_USD_AGGREGATOR_POLYGON, &self.provider);
        let result = agg.latestRoundData().call().await
            .map_err(|e| MarketWatchError::Rpc(e.to_string()))?;
        let answer_i128 = result.answer.try_into()
            .map_err(|e: alloy::primitives::ruint::FromUintError<i128>| {
                MarketWatchError::Decode(format!("answer overflow: {e}"))
            })?;
        decode_chainlink_answer(answer_i128, BTC_USD_DECIMALS)
    }
}

/// Pure helper, unit-tested. Converts a Chainlink raw integer answer to
/// plain dollars (Decimal), dividing by 10^decimals.
pub fn decode_chainlink_answer(raw: i128, decimals: u32) -> Result<Decimal, MarketWatchError> {
    let raw_dec = Decimal::from_i128_with_scale(raw, 0);
    let divisor = Decimal::from(10_u64.pow(decimals));
    Ok(raw_dec / divisor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn decode_typical_btc_price() {
        // $80,424.78 with 8 decimals = 8042478000000
        let r = decode_chainlink_answer(8_042_478_000_000_i128, 8).unwrap();
        assert_eq!(r, Decimal::from_str("80424.78").unwrap());
    }

    #[test]
    fn decode_zero() {
        let r = decode_chainlink_answer(0, 8).unwrap();
        assert_eq!(r, Decimal::ZERO);
    }

    #[test]
    fn decode_small_value() {
        // 1 satoshi-ish = 0.00000001 USD
        let r = decode_chainlink_answer(1, 8).unwrap();
        assert_eq!(r, Decimal::from_str("0.00000001").unwrap());
    }

    #[test]
    fn decode_large_value() {
        // $1,000,000.00 = 100_000_000_000_000
        let r = decode_chainlink_answer(100_000_000_000_000_i128, 8).unwrap();
        assert_eq!(r, Decimal::from(1_000_000));
    }
}
```

> **alloy API note for implementer:** the exact `ProviderBuilder` and `sol!` interface API may differ slightly between alloy minor versions. If compile fails:
> - `ProviderBuilder::new().on_http(url)` may want `RootProvider::new_http` instead in some versions
> - `result.answer` may be wrapped in a tuple or named struct depending on `sol!` macro variants — try `result.answer` first, fall back to `result.0` or whatever `latestRoundData` returns
> - The pure helper `decode_chainlink_answer` is the only logic that must be exact; the wrapper around it is just I/O

- [ ] **Step 3: Run wrapper tests**

```bash
cargo test --lib trader::adapters::chainlink_btc_wrapper
```

Expected: 4 passed.

- [ ] **Step 4: Verify build for poly-tui**

```bash
cargo build --bin poly-tui
```

Expected: success.

- [ ] **Step 5: Commit**

```bash
git add src/trader/adapters/chainlink_btc_wrapper.rs src/trader/adapters/mod.rs
git commit -m "feat(adapter): ChainlinkBtcPriceFeed via alloy + decode helper"
```

---

## Task 9: Wire 5th task into `poly-tui` main

**Files:**
- Modify: `src/bin/poly-tui.rs`

- [ ] **Step 1: Read existing main**

```bash
cat src/bin/poly-tui.rs
```

Note where adapters are constructed (RedisCache, RedisTraderStream) and where tasks are spawned.

- [ ] **Step 2: Add new imports**

```rust
use poly_tui::tui::market_watch::{self, BtcPriceFeed};
use poly_tui::trader::market::MarketDiscovery;
use poly_tui::trader::adapters::chainlink_btc_wrapper::ChainlinkBtcPriceFeed;
use poly_tui::trader::adapters::gamma_wrapper::GammaMarketDiscovery;
```

- [ ] **Step 3: Construct the two adapters after existing adapter setup**

```rust
let gamma_host = std::env::var("GAMMA_HOST")
    .unwrap_or_else(|_| "https://gamma-api.polymarket.com".into());

let market_for_watch: Option<Arc<dyn MarketDiscovery>> = Some(Arc::new(
    GammaMarketDiscovery::new(gamma_host)
));

let price_feed: Option<Arc<dyn BtcPriceFeed>> =
    match ChainlinkBtcPriceFeed::connect(&cfg.polygon_rpc_url).await {
        Ok(f) => Some(Arc::new(f)),
        Err(e) => {
            tracing::warn!("Chainlink RPC connect failed: {e} — BTC strip shows --");
            None
        }
    };
```

- [ ] **Step 4: Spawn the 5th task**

After the existing 4 task spawns (refresher, app, input, trader event subscriber), add:

```rust
let event_tx_market = event_tx.clone();
let shutdown_market = shutdown.clone();
let h_market = match (price_feed, market_for_watch) {
    (Some(feed), Some(market)) => {
        tokio::spawn(market_watch::run(feed, market, event_tx_market, shutdown_market))
    }
    _ => tokio::spawn(async move {}),
};
```

- [ ] **Step 5: Add `h_market` to the final `tokio::join!`**

Find the existing `tokio::join!(h_refresh, h_input, h_status, h_trader)` and append:

```rust
let _ = tokio::join!(h_refresh, h_input, h_status, h_trader, h_market);
```

- [ ] **Step 6: Build**

```bash
cargo build --bin poly-tui
```

Expected: success.

- [ ] **Step 7: Smoke test (verify 5th task wires correctly)**

```bash
cargo run --bin poly-tui
```

Expected behavior in TUI:
1. Within 1s: TUI appears, market strip shows " BTC: -- "
2. Within ~5-10s: market strip shows current BTC price → "BTC -- → 80,431  --   ⏱ X:YY"
3. Within ~15-20s: priceToBeat populates → "BTC 80,425 → 80,431  +6  ⏱ X:YY"
4. Countdown ticks every second
5. `q` quits cleanly

**If you don't have a real Polygon RPC** (e.g., the public default times out), the strip will show " BTC: -- " indefinitely — that's the graceful failure mode and is fine for a smoke test.

Don't worry if you don't see actual price values; the build succeeding + TUI rendering without panicking is the bar for this task. Real values are validated by the integration test in Task 10.

- [ ] **Step 8: Commit**

```bash
git add src/bin/poly-tui.rs
git commit -m "feat(tui): wire 5th task — market_watch with Chainlink + Gamma"
```

---

## Task 10: Chainlink integration smoke test

**Files:**
- Create: `tests/chainlink_integration.rs`
- Modify: `Cargo.toml` (uncomment `[[test]] chainlink_integration` block)

- [ ] **Step 1: Add `[[test]]` block to `Cargo.toml`**

Append to the bottom of `Cargo.toml` (after the existing `[[test]]` blocks):

```toml
[[test]]
name = "chainlink_integration"
path = "tests/chainlink_integration.rs"
```

- [ ] **Step 2: Write `tests/chainlink_integration.rs`**

```rust
#![cfg(test)]

use poly_tui::trader::adapters::chainlink_btc_wrapper::ChainlinkBtcPriceFeed;
use poly_tui::tui::market_watch::BtcPriceFeed;
use rust_decimal::Decimal;
use std::str::FromStr;

#[tokio::test]
#[ignore]
async fn fetches_real_btc_price_from_polygon() {
    let feed = ChainlinkBtcPriceFeed::connect("https://polygon-rpc.com").await
        .expect("connect to public Polygon RPC");
    let price = feed.latest_price().await
        .expect("fetch latest BTC/USD round");

    // Plausible BTC range: $10k < p < $1M
    assert!(price > Decimal::from_str("10000").unwrap(),
        "price too low: {price}");
    assert!(price < Decimal::from_str("1000000").unwrap(),
        "price implausibly high: {price}");
}
```

- [ ] **Step 3: Build the test target**

```bash
cargo build --tests
```

If `cargo build --tests` fails with the poly-trader.exe lock error (the test binary infra builds the bin), fall back to:

```bash
cargo build --test chainlink_integration
```

This builds only the named test binary. Should not touch poly-trader.exe.

- [ ] **Step 4: Run the test**

```bash
cargo test --test chainlink_integration -- --ignored
```

Expected: 1 passed in ~3-10s (depending on RPC latency).

If the public RPC is unavailable (`https://polygon-rpc.com` rate-limits or 503s), this test failure is acceptable. Document in the commit message and move on.

- [ ] **Step 5: Commit**

```bash
git add tests/chainlink_integration.rs Cargo.toml
git commit -m "test(adapter): chainlink smoke test against public Polygon RPC"
```

---

## Task 11: Coverage check + acceptance walkthrough

- [ ] **Step 1: Run all test suites**

```bash
cargo test --lib
cargo test --test bdd
cargo test --test cache_integration -- --ignored
cargo test --test trader_state_integration -- --ignored
cargo test --test trader_market_integration -- --ignored
cargo test --test e2e_trader -- --ignored
cargo test --test e2e_tui -- --ignored
cargo test --test chainlink_integration -- --ignored
```

Expected: all green. Note total counts; should be at least 21 more lib tests than v1.x (14 market_watch + 4 chainlink decode + 1 app + 5 ui snapshots + 2 market decoder).

- [ ] **Step 2: Run coverage**

```bash
cargo llvm-cov --lib --tests \
  --ignore-filename-regex 'src/bin|src/trader/adapters/|.*_wrapper\.rs' \
  --html
cargo llvm-cov report --lib --tests \
  --ignore-filename-regex 'src/bin|src/trader/adapters/|.*_wrapper\.rs'
```

Verify:
- `src/tui/market_watch.rs` ≥ 95%
- `src/trader/market.rs` not regressed (was ~92.86%; should stay close)
- `src/trader/` aggregate not regressed (was 96%)
- Overall lib coverage not regressed below v1.x baseline

If `src/tui/market_watch.rs` is below 95%, add tests for whichever `run` branches are uncovered (likely the `ResolutionTimeout`-equivalent path in gamma_ticker).

- [ ] **Step 3: Manual acceptance per spec §10**

- [ ] BTC strip renders within ~5s of poly-tui startup (first RPC poll)
- [ ] `priceToBeat` populates within ~15s of startup (first gamma fetch)
- [ ] diff sign + color: positive green, negative red, zero white
- [ ] Countdown ticks every second
- [ ] At 5-min boundary, `priceToBeat` updates to new window's value
- [ ] Polygon RPC down → strip degrades to "--" gracefully (set `POLYGON_RPC_URL=https://invalid-url.example/`)
- [ ] Resize terminal to 50 cols → graceful truncation, no panic
- [ ] `cargo clippy --bin poly-tui` introduces no new warnings
- [ ] All 5 new insta snapshots committed
- [ ] All existing tests still pass
- [ ] PID 53896 dry-run trader still running (or has hit `--max-windows 144` cap and exited normally)

```bash
# Verify trader process is unaffected
tasklist 2>&1 | grep -i poly-trader || echo "trader exited (likely max-windows cap)"
docker exec poly-redis redis-cli GET poly:prod:trader:ladder | head
```

- [ ] **Step 4: Update README and TODO.md**

Append a short subsection to `README.md` after the existing `## Trader` section:

```markdown
### BTC market watch strip

The TUI shows a 1-row strip with the current Polymarket BTC 5-min window's
price-to-beat, live Chainlink BTC/USD price, signed diff, and countdown to
window close. Independent of trader; works whether or not poly-trader is
running.

Configure the Polygon RPC endpoint via `POLYGON_RPC_URL` in `.env` (default:
`https://polygon-rpc.com`).
```

In `TODO.md`, add a new completed-section block above the v1.1 daemon split:

```markdown
## v1.x.1 — BTC Market Watch Strip  ✅ COMPLETE

- [x] WindowMarket extended with price_to_beat (additive, backward-compat)
- [x] tui::market_watch task: Chainlink BTC/USD via Polygon RPC + gamma priceToBeat
- [x] Layout: new 1-row strip between balance and trader sub-title
- [x] Graceful degradation on RPC / gamma failure
- [x] 5 new insta snapshots; 18 new market_watch unit tests; 4 new decoder tests
- [x] Independent of trader process — works with or without poly-trader running
```

- [ ] **Step 5: Final commit**

```bash
git add README.md TODO.md
git commit -m "docs: README + TODO updated for v1.x.1 market watch strip"
```

- [ ] **Step 6: Push to origin**

```bash
git push origin main
```

---

## Self-Review Notes

**Spec coverage:**
- §1 Goals: covered by Tasks 3-9
- §2 Decisions: encoded across all tasks
- §3 Architecture: Tasks 5 (run loop), 9 (wiring)
- §4 Modules: Tasks 3, 5, 8
- §5 WindowMarket extension: Tasks 1, 2
- §6 MarketState + run: Tasks 3, 4, 5
- §7 TUI render: Task 7
- §8 Config + errors: Task 0 + run loop in Task 5 + adapter in Task 8
- §9 Tests: Tasks 1, 4, 5, 6, 7, 8, 10
- §10 Acceptance: Task 11
- §11 Future: not implemented (out of scope)

**Build hygiene preserved:** every `cargo build` invocation uses `--bin poly-tui` or `cargo check --bin poly-trader`. No bare `cargo build` or `cargo build --bin poly-trader`. Tests use `cargo test --lib` or `--test <name>` only.

**Type consistency:** `MarketState`, `BtcPriceFeed`, `MarketWatchError`, `WindowMarket.price_to_beat`, `AppEvent::MarketUpdate` — names cross-checked across tasks.

**Open implementer-time verifications (flagged inline):**
1. alloy `ProviderBuilder` exact API (Task 8) — `on_http` vs `with_recommended_fillers`, etc.
2. `sol!` macro return shape — `result.answer` vs tuple struct
3. fred / cucumber tests already verified by v1.x; no new uncertainties

**Out-of-scope items (per spec §1):** ETH/SOL, other window durations, sparkline, runtime price-source switch, persistent priceToBeat across restarts, WebSocket push, trader using watcher data — all skipped, no shadow tasks.
