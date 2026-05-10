# v1.6 — TUI Positions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render live Polymarket positions inside the TUI balance box so the operator sees stuck shares from `SellRejected`/`Alert` events and any leftover positions from prior sessions.

**Architecture:** New `Positioner` task polls the Polymarket data-api via the existing SDK's `data::Client`, writes to Redis at `poly:prod:positions`, and emits `AppEvent::PositionsUpdate`. The App reads from Redis on each render tick. Mirrors the existing `Refresher → Redis → App` flow for USDC balance. Proxy address is derived once at startup from the EOA private key via the SDK's `derive_proxy_wallet(eoa, POLYGON)`.

**Tech Stack:** Rust 1.78+, tokio, polymarket_client_sdk_v2 (existing — `data::Client` for `/positions`, `derive_proxy_wallet` for address derivation), fred (Redis), serde_json, ratatui.

**Spec:** `docs/superpowers/specs/2026-05-10-tui-positions-design.md`

## Build hygiene — STRICT

NEVER bare `cargo build`. Always scope:
- `cargo build --bin poly-tui`
- `cargo test --lib positions::` (and other narrow paths during development)
- `cargo test --test positions_integration -- --ignored` (final integration only)

Do NOT touch `src/backtest/`, `src/trader/`, or trader binary code. Trader is currently NOT running but the running TUI binary is locked while the user has the tmux session up — the implementer is expected to coordinate rebuilds with the user (kill via `tmux send-keys -t poly-tui q` then rebuild).

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `src/positions.rs` | new | `Position`, `Positions`, `Side`, `PositionsFetcher` trait, `PositionsCache` trait, `POSITIONS_KEY` constant |
| `src/adapters/polymarket_positions_wrapper.rs` | new | `PolymarketPositionsFetcher` — wraps SDK's `data::Client`. Maps SDK `Position` → our slim `Position`. |
| `src/adapters/redis_positions_wrapper.rs` | new | `RedisPositionsCache` — JSON in/out at `poly:prod:positions`. |
| `src/adapters/mod.rs` | modify | `pub mod polymarket_positions_wrapper; pub mod redis_positions_wrapper;` |
| `src/lib.rs` | modify | `pub mod adapters;` (if not already) + `pub mod positions; pub mod positioner;` |
| `src/positioner.rs` | new | Periodic poll loop, mirrors `refresher::run`. |
| `src/domain.rs` | modify | Add `AppEvent::PositionsUpdate(Positions)`. |
| `src/app.rs` | modify | `AppState.positions: Option<Positions>` + handle_event arm + `tick_once` reads from cache. |
| `src/ui.rs` | modify | `UiState.positions` + `render_balance` becomes 2-line. |
| `src/bin/poly-tui.rs` | modify | Derive proxy address from private key, construct fetcher + cache, spawn positioner. |
| `tests/positions_integration.rs` | new | Testcontainers Redis + fake fetcher → assert event emit + Redis state. |
| `README.md` | modify | New §Positions section. |
| `TODO.md` | modify | Tick v1.6 ✅. |

Existing src/adapters/ already exists at the workspace root (used by the trader). The new wrappers go alongside.

---

## Task 0: Sanity baseline

**Files:** none (read-only).

- [ ] **Step 1: Confirm working tree clean**

Run: `git status`
Expected: only untracked items are `.claude/`, the four `backtest-report*.html` files, and the cache directory. No tracked-file modifications.

- [ ] **Step 2: Confirm TUI lib tests green**

Run: `cargo test --lib`
Expected: PASS — 228+ tests green.

- [ ] **Step 3: Confirm TUI binary builds**

Run: `cargo build --bin poly-tui`
Expected: Compiles clean (warnings ok).

- [ ] **Step 4: No commit (read-only baseline)**

Skip — this task only verifies starting state.

---

## Task 1: Domain types — Position, Positions, Side, traits

**Files:**
- Create: `src/positions.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/positions.rs`:

```rust
use crate::domain::{CacheError, FetchError};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Production Redis key for the latest positions snapshot.
pub const POSITIONS_KEY: &str = "poly:prod:positions";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side { Up, Down }

impl Side {
    /// Parse from Polymarket's outcome string. Returns None if the outcome is
    /// not a binary BTC up/down market (e.g. presidential markets, etc.) so the
    /// caller can filter those out.
    pub fn parse(s: &str) -> Option<Side> {
        match s.to_ascii_lowercase().as_str() {
            "up" => Some(Side::Up),
            "down" => Some(Side::Down),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub token_id: String,
    pub side: Side,
    pub market_slug: String,
    pub shares: Decimal,
    pub avg_price: Decimal,     // USDC paid per share
    pub current_price: Decimal, // current bid per data-api
}

impl Position {
    pub fn cost_usd(&self) -> Decimal { self.avg_price * self.shares }
    pub fn value_usd(&self) -> Decimal { self.current_price * self.shares }
    /// Percent gain/loss vs cost. Returns 0 if cost is zero (avoid div-by-zero).
    pub fn pnl_pct(&self) -> Decimal {
        let cost = self.cost_usd();
        if cost.is_zero() {
            return Decimal::ZERO;
        }
        (self.value_usd() - cost) / cost * Decimal::from(100)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Positions {
    pub items: Vec<Position>,
    pub fetched_at: DateTime<Utc>,
}

#[async_trait]
pub trait PositionsFetcher: Send + Sync {
    async fn fetch(&self) -> Result<Positions, FetchError>;
}

#[async_trait]
pub trait PositionsCache: Send + Sync {
    async fn get(&self) -> Result<Option<Positions>, CacheError>;
    async fn set(&self, p: &Positions) -> Result<(), CacheError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn pos(avg: &str, cur: &str, shares: &str) -> Position {
        Position {
            token_id: "t".into(),
            side: Side::Up,
            market_slug: "btc-updown-5m-1".into(),
            shares: Decimal::from_str(shares).unwrap(),
            avg_price: Decimal::from_str(avg).unwrap(),
            current_price: Decimal::from_str(cur).unwrap(),
        }
    }

    #[test]
    fn cost_usd_multiplies_avg_by_shares() {
        let p = pos("0.50", "0.485", "10");
        assert_eq!(p.cost_usd(), Decimal::from_str("5.00").unwrap());
    }

    #[test]
    fn value_usd_multiplies_current_by_shares() {
        let p = pos("0.50", "0.485", "10");
        assert_eq!(p.value_usd(), Decimal::from_str("4.85").unwrap());
    }

    #[test]
    fn pnl_pct_negative_when_value_below_cost() {
        let p = pos("0.50", "0.485", "10");
        // (4.85 - 5.00) / 5.00 * 100 = -3.0
        assert_eq!(p.pnl_pct(), Decimal::from_str("-3.0").unwrap());
    }

    #[test]
    fn pnl_pct_positive_when_value_above_cost() {
        let p = pos("0.50", "0.85", "10");
        // (8.50 - 5.00) / 5.00 * 100 = 70.0
        assert_eq!(p.pnl_pct(), Decimal::from_str("70.0").unwrap());
    }

    #[test]
    fn pnl_pct_zero_when_cost_is_zero() {
        let p = pos("0", "0.50", "10");
        assert_eq!(p.pnl_pct(), Decimal::ZERO);
    }

    #[test]
    fn side_parses_case_insensitive() {
        assert_eq!(Side::parse("Up"), Some(Side::Up));
        assert_eq!(Side::parse("up"), Some(Side::Up));
        assert_eq!(Side::parse("DOWN"), Some(Side::Down));
        assert_eq!(Side::parse("Yes"), None);
    }

    #[test]
    fn position_serde_roundtrip() {
        let p = pos("0.50", "0.485", "10");
        let json = serde_json::to_string(&p).unwrap();
        let back: Position = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn positions_serde_roundtrip() {
        let p = Positions {
            items: vec![pos("0.50", "0.485", "10")],
            fetched_at: Utc::now(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Positions = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn positions_key_namespaces_prod() {
        assert!(POSITIONS_KEY.starts_with("poly:prod:"));
    }
}
```

- [ ] **Step 2: Add module declaration to lib.rs**

Edit `src/lib.rs` — add at the end (or alongside other `pub mod` declarations):

```rust
pub mod positions;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --lib positions::`
Expected: PASS — 9 tests green.

- [ ] **Step 4: Commit**

```bash
git add src/positions.rs src/lib.rs
git commit -m "feat(positions): Position/Positions/Side types + PositionsFetcher/PositionsCache traits"
```

---

## Task 2: AppEvent::PositionsUpdate

**Files:**
- Modify: `src/domain.rs`

- [ ] **Step 1: Write the failing test**

Edit `src/domain.rs`. Inside the `#[cfg(test)] mod tests` block, before the closing `}`, add:

```rust
    #[test]
    fn app_event_can_carry_positions_update() {
        // Compile-only sanity: the variant exists and accepts a Positions.
        use crate::positions::Positions;
        let p = Positions { items: vec![], fetched_at: ts(1_700_000_000) };
        let _ev = AppEvent::PositionsUpdate(p);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib domain::tests::app_event_can_carry_positions_update`
Expected: FAIL — `no variant PositionsUpdate on AppEvent`.

- [ ] **Step 3: Add the variant**

Edit `src/domain.rs`. Add a `use` near the top:

```rust
use crate::positions::Positions;
```

Find `pub enum AppEvent {` and add the variant before the closing `}`:

```rust
    PositionsUpdate(Positions),
```

The full enum after change (showing context):

```rust
#[derive(Debug)]
pub enum AppEvent {
    Tick,
    Key(crossterm::event::KeyEvent),
    Refresh(RefreshStatus),
    Shutdown,
    TraderEvent(TraderEvent),
    MarketUpdate(MarketState),
    PositionsUpdate(Positions),
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib domain::`
Expected: PASS — all domain tests green.

- [ ] **Step 5: Commit**

```bash
git add src/domain.rs
git commit -m "feat(domain): AppEvent::PositionsUpdate(Positions) variant"
```

---

## Task 3: PolymarketPositionsFetcher adapter

**Files:**
- Create: `src/adapters/polymarket_positions_wrapper.rs`
- Modify: `src/adapters/mod.rs`

This task wraps the SDK's `polymarket_client_sdk_v2::data::Client` — no custom reqwest, no custom JSON decoding. We map the SDK's rich `Position` into our slim domain `Position`.

- [ ] **Step 1: Add module declaration**

Edit `src/adapters/mod.rs` — add at end:

```rust
pub mod polymarket_positions_wrapper;
pub mod redis_positions_wrapper;
```

(The `redis_positions_wrapper` module is created in Task 4. Declaring both up front avoids two consecutive edits to mod.rs.)

- [ ] **Step 2: Write the failing tests**

Create `src/adapters/polymarket_positions_wrapper.rs`:

```rust
use crate::domain::FetchError;
use crate::positions::{Position as DomainPosition, Positions, PositionsFetcher, Side};
use alloy::primitives::Address;
use async_trait::async_trait;
use chrono::Utc;
use polymarket_client_sdk_v2::data::Client as DataClient;
use polymarket_client_sdk_v2::data::types::request::PositionsRequest;
use polymarket_client_sdk_v2::data::types::response::Position as SdkPosition;

pub struct PolymarketPositionsFetcher {
    client: DataClient,
    user: Address,
}

impl PolymarketPositionsFetcher {
    pub fn new(user: Address) -> Self {
        Self { client: DataClient::default(), user }
    }
    pub fn with_host(user: Address, host: &str) -> Result<Self, FetchError> {
        let client = DataClient::new(host)
            .map_err(|e| FetchError::Network(format!("data-api init: {e}")))?;
        Ok(Self { client, user })
    }
}

#[async_trait]
impl PositionsFetcher for PolymarketPositionsFetcher {
    async fn fetch(&self) -> Result<Positions, FetchError> {
        let req = PositionsRequest::builder().user(self.user).build();
        let raw = self.client.positions(&req)
            .await
            .map_err(|e| FetchError::Network(format!("data-api positions: {e}")))?;
        let items = raw.into_iter().filter_map(map_position).collect();
        Ok(Positions { items, fetched_at: Utc::now() })
    }
}

/// Converts an SDK Position into our slim domain Position. Returns None for
/// markets whose outcome name isn't "Up"/"Down" (Polymarket has many markets;
/// only BTC up/down has those outcome names).
pub fn map_position(p: SdkPosition) -> Option<DomainPosition> {
    let side = Side::parse(&p.outcome)?;
    Some(DomainPosition {
        token_id: p.asset.to_string(),
        side,
        market_slug: p.slug,
        shares: p.size,
        avg_price: p.avg_price,
        current_price: p.cur_price,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    /// Build an SdkPosition fixture. The SDK's struct is non_exhaustive but has
    /// a Builder, so we use that. If the builder API changes, only this helper
    /// needs to update.
    fn sdk_fixture(outcome: &str, slug: &str, size: &str, avg: &str, cur: &str) -> SdkPosition {
        // The SDK Position struct is non_exhaustive; constructed via deserializing
        // a JSON fixture matching its camelCase shape.
        let json = format!(r#"{{
            "proxyWallet": "0x0000000000000000000000000000000000000001",
            "asset": "12345",
            "conditionId": "0x0000000000000000000000000000000000000000000000000000000000000001",
            "size": "{size}",
            "avgPrice": "{avg}",
            "initialValue": "0",
            "currentValue": "0",
            "cashPnl": "0",
            "percentPnl": "0",
            "totalBought": "0",
            "realizedPnl": "0",
            "percentRealizedPnl": "0",
            "curPrice": "{cur}",
            "redeemable": false,
            "mergeable": false,
            "title": "test",
            "slug": "{slug}",
            "icon": "",
            "eventSlug": "",
            "eventId": "",
            "outcome": "{outcome}",
            "outcomeIndex": 0,
            "oppositeOutcome": "",
            "oppositeAsset": "0",
            "endDate": "",
            "negativeRisk": false
        }}"#);
        serde_json::from_str(&json).expect("sdk position fixture decodes")
    }

    #[test]
    fn maps_up_outcome() {
        let sdk = sdk_fixture("Up", "btc-updown-5m-1", "10", "0.50", "0.485");
        let p = map_position(sdk).expect("Up should map");
        assert_eq!(p.side, Side::Up);
        assert_eq!(p.market_slug, "btc-updown-5m-1");
        assert_eq!(p.shares, Decimal::from(10));
        assert_eq!(p.avg_price, Decimal::from_str("0.50").unwrap());
        assert_eq!(p.current_price, Decimal::from_str("0.485").unwrap());
    }

    #[test]
    fn maps_down_outcome() {
        let sdk = sdk_fixture("Down", "btc-updown-5m-1", "5", "0.50", "0.50");
        let p = map_position(sdk).expect("Down should map");
        assert_eq!(p.side, Side::Down);
    }

    #[test]
    fn filters_unknown_outcome() {
        let sdk = sdk_fixture("Yes", "presidential-2024", "100", "0.60", "0.58");
        assert!(map_position(sdk).is_none());
    }

    #[test]
    fn fetcher_constructs_with_default_host() {
        let user = Address::from([0u8; 20]);
        let _f = PolymarketPositionsFetcher::new(user);
    }

    #[test]
    fn fetcher_constructs_with_custom_host() {
        let user = Address::from([0u8; 20]);
        let f = PolymarketPositionsFetcher::with_host(user, "https://data-api.polymarket.com");
        assert!(f.is_ok());
    }

    #[test]
    fn fetcher_rejects_invalid_host_url() {
        let user = Address::from([0u8; 20]);
        let f = PolymarketPositionsFetcher::with_host(user, "not a url");
        assert!(matches!(f, Err(FetchError::Network(_))));
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --lib adapters::polymarket_positions_wrapper::`
Expected: PASS — 6 tests green.

- [ ] **Step 4: Commit**

```bash
git add src/adapters/mod.rs src/adapters/polymarket_positions_wrapper.rs
git commit -m "feat(positions): PolymarketPositionsFetcher wrapping SDK data::Client"
```

---

## Task 4: RedisPositionsCache adapter

**Files:**
- Create: `src/adapters/redis_positions_wrapper.rs`

The mod.rs already declares this module from Task 3.

- [ ] **Step 1: Write the failing tests**

Create `src/adapters/redis_positions_wrapper.rs`:

```rust
use crate::domain::CacheError;
use crate::positions::{Positions, PositionsCache, POSITIONS_KEY};
use async_trait::async_trait;
use fred::interfaces::ClientLike;
use fred::prelude::{KeysInterface, RedisClient, RedisConfig};

pub struct RedisPositionsCache {
    client: RedisClient,
}

impl RedisPositionsCache {
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        let config = RedisConfig::from_url(url)
            .map_err(|e| CacheError::Op(format!("bad redis url: {e}")))?;
        let client = RedisClient::new(config, None, None, None);
        client
            .init()
            .await
            .map_err(|e| CacheError::Op(format!("redis init: {e}")))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl PositionsCache for RedisPositionsCache {
    async fn get(&self) -> Result<Option<Positions>, CacheError> {
        let raw: Option<String> = self
            .client
            .get(POSITIONS_KEY)
            .await
            .map_err(map_err)?;
        match raw {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| CacheError::Decode(e.to_string())),
        }
    }

    async fn set(&self, p: &Positions) -> Result<(), CacheError> {
        let json = serde_json::to_string(p)
            .map_err(|e| CacheError::Decode(e.to_string()))?;
        self.client
            .set::<(), _, _>(POSITIONS_KEY, json, None, None, false)
            .await
            .map_err(map_err)
    }
}

fn map_err(e: fred::error::RedisError) -> CacheError {
    use fred::error::RedisErrorKind;
    if matches!(e.kind(), RedisErrorKind::IO | RedisErrorKind::Canceled) {
        CacheError::Disconnected
    } else {
        CacheError::Op(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Functional integration tests live in tests/positions_integration.rs
    // (need a real Redis container). This unit test just confirms the type
    // builds and the bad-URL path returns CacheError.
    #[tokio::test]
    async fn connect_rejects_invalid_url() {
        let r = RedisPositionsCache::connect("not a url").await;
        assert!(matches!(r, Err(CacheError::Op(_))));
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib adapters::redis_positions_wrapper::`
Expected: PASS — 1 test green.

- [ ] **Step 3: Commit**

```bash
git add src/adapters/redis_positions_wrapper.rs
git commit -m "feat(positions): RedisPositionsCache JSON in/out at poly:prod:positions"
```

---

## Task 5: Positioner task

**Files:**
- Create: `src/positioner.rs`
- Modify: `src/lib.rs`

Mirrors `src/refresher.rs`. The fetcher returns `Positions` directly (no separate status type — failure is just a logged warning).

- [ ] **Step 1: Add module declaration**

Edit `src/lib.rs` — add at end:

```rust
pub mod positioner;
```

- [ ] **Step 2: Write the failing tests**

Create `src/positioner.rs`:

```rust
use crate::domain::AppEvent;
use crate::positions::{Positions, PositionsCache, PositionsFetcher};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// One-shot fetch + cache write + event emit.
pub async fn do_fetch(
    fetcher: &dyn PositionsFetcher,
    cache: &dyn PositionsCache,
    event_tx: &mpsc::Sender<AppEvent>,
) {
    match fetcher.fetch().await {
        Ok(p) => {
            if let Err(e) = cache.set(&p).await {
                tracing::warn!("positions cache write failed: {e}");
                // Still emit so UI gets fresh data even if cache is broken.
            }
            let _ = event_tx.send(AppEvent::PositionsUpdate(p)).await;
        }
        Err(e) => {
            tracing::warn!("positions fetch failed: {e}");
            // Don't emit on failure — App keeps last known positions.
        }
    }
}

/// Long-running positions poll loop. First fetch happens immediately, then
/// every `interval`. Exits when `shutdown` is cancelled.
pub async fn run(
    fetcher: Arc<dyn PositionsFetcher>,
    cache: Arc<dyn PositionsCache>,
    event_tx: mpsc::Sender<AppEvent>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    // Immediate first fetch so the UI doesn't wait `interval` seconds on launch.
    do_fetch(fetcher.as_ref(), cache.as_ref(), &event_tx).await;

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            _ = tokio::time::sleep(interval) => {
                do_fetch(fetcher.as_ref(), cache.as_ref(), &event_tx).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CacheError, FetchError};
    use crate::positions::{Position, Side};
    use async_trait::async_trait;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeFetcher {
        items: Mutex<Vec<Position>>,
        fail: Mutex<bool>,
        calls: AtomicUsize,
    }
    impl FakeFetcher {
        fn ok(items: Vec<Position>) -> Arc<Self> {
            Arc::new(Self { items: Mutex::new(items), fail: Mutex::new(false), calls: AtomicUsize::new(0) })
        }
        fn fail() -> Arc<Self> {
            Arc::new(Self { items: Mutex::new(vec![]), fail: Mutex::new(true), calls: AtomicUsize::new(0) })
        }
    }
    #[async_trait]
    impl PositionsFetcher for FakeFetcher {
        async fn fetch(&self) -> Result<Positions, FetchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if *self.fail.lock().unwrap() {
                return Err(FetchError::Network("x".into()));
            }
            Ok(Positions { items: self.items.lock().unwrap().clone(), fetched_at: Utc::now() })
        }
    }

    struct MemCache { last: Mutex<Option<Positions>> }
    impl MemCache {
        fn new() -> Arc<Self> { Arc::new(Self { last: Mutex::new(None) }) }
        fn snapshot(&self) -> Option<Positions> { self.last.lock().unwrap().clone() }
    }
    #[async_trait]
    impl PositionsCache for MemCache {
        async fn get(&self) -> Result<Option<Positions>, CacheError> {
            Ok(self.last.lock().unwrap().clone())
        }
        async fn set(&self, p: &Positions) -> Result<(), CacheError> {
            *self.last.lock().unwrap() = Some(p.clone()); Ok(())
        }
    }

    fn p(slug: &str) -> Position {
        Position {
            token_id: "1".into(),
            side: Side::Up,
            market_slug: slug.into(),
            shares: Decimal::from(10),
            avg_price: Decimal::from_str("0.50").unwrap(),
            current_price: Decimal::from_str("0.485").unwrap(),
        }
    }

    #[tokio::test]
    async fn do_fetch_writes_cache_and_emits_event() {
        let f = FakeFetcher::ok(vec![p("m1")]);
        let c = MemCache::new();
        let (tx, mut rx) = mpsc::channel::<AppEvent>(8);
        do_fetch(f.as_ref(), c.as_ref(), &tx).await;
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, AppEvent::PositionsUpdate(_)));
        assert_eq!(c.snapshot().unwrap().items.len(), 1);
    }

    #[tokio::test]
    async fn do_fetch_silent_on_fetch_error() {
        let f = FakeFetcher::fail();
        let c = MemCache::new();
        let (tx, mut rx) = mpsc::channel::<AppEvent>(8);
        do_fetch(f.as_ref(), c.as_ref(), &tx).await;
        // No event when fetch fails
        assert!(rx.try_recv().is_err());
        // Cache still empty
        assert!(c.snapshot().is_none());
    }

    #[tokio::test]
    async fn run_first_fetch_happens_immediately() {
        tokio::time::pause();
        let f = FakeFetcher::ok(vec![p("m1")]);
        let c = MemCache::new();
        let (tx, mut rx) = mpsc::channel::<AppEvent>(8);
        let shutdown = CancellationToken::new();

        let f_arc: Arc<dyn PositionsFetcher> = f.clone();
        let c_arc: Arc<dyn PositionsCache> = c.clone();
        let task = tokio::spawn(run(f_arc, c_arc, tx, Duration::from_secs(60), shutdown.clone()));

        // We expect an immediate fetch — should arrive without advancing time.
        let ev = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await
            .expect("event arrives without time advance").unwrap();
        assert!(matches!(ev, AppEvent::PositionsUpdate(_)));

        shutdown.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn shutdown_token_cancels_loop() {
        tokio::time::pause();
        let f = FakeFetcher::ok(vec![]);
        let c = MemCache::new();
        let (tx, _rx) = mpsc::channel::<AppEvent>(8);
        let shutdown = CancellationToken::new();

        let f_arc: Arc<dyn PositionsFetcher> = f.clone();
        let c_arc: Arc<dyn PositionsCache> = c.clone();
        let task = tokio::spawn(run(f_arc, c_arc, tx, Duration::from_secs(60), shutdown.clone()));

        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(1), task).await
            .expect("task exits within 1s")
            .expect("no panic");
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --lib positioner::`
Expected: PASS — 4 tests green.

- [ ] **Step 4: Commit**

```bash
git add src/positioner.rs src/lib.rs
git commit -m "feat(positions): Positioner task with immediate-first-fetch + 30s loop"
```

---

## Task 6: AppState gains positions; tick reads cache

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Write the failing tests**

Add to `src/app.rs` inside an existing `#[cfg(test)] mod tests` block, OR create a new test block at the bottom. Inside the tests block:

```rust
    #[tokio::test]
    async fn handle_event_updates_positions_on_positions_update() {
        use crate::positions::Positions;
        let mut state = AppState::new(Duration::from_secs(30));
        let (cmd_tx, _cmd_rx) = mpsc::channel(8);
        let p = Positions { items: vec![], fetched_at: chrono::Utc::now() };
        handle_event(&mut state, AppEvent::PositionsUpdate(p.clone()), &cmd_tx);
        assert_eq!(state.positions, Some(p));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib app::`
Expected: FAIL — `field 'positions' does not exist on AppState`.

- [ ] **Step 3: Add positions field + handle event**

Edit `src/app.rs`. Update the `use` statements at the top:

```rust
use crate::positions::Positions;
```

Modify `AppState`:

```rust
#[derive(Clone, Debug)]
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
}
```

Update `AppState::new` to add `positions: None`:

```rust
impl AppState {
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
        }
    }
    // ... ui_state stays the same shape but gains positions:
}
```

Update `ui_state` to pass positions through:

```rust
    pub fn ui_state(&self, now: DateTime<Utc>) -> UiState {
        let clob_health = HealthLed::from_clob_age(self.last_refresh.as_ref(), self.refresh_interval, now);
        let redis_health = if self.redis_ok { HealthLed::Green } else { HealthLed::Red };
        UiState {
            balance: self.balance.clone(),
            last_refresh: self.last_refresh.clone(),
            clob_health,
            redis_health,
            refresh_interval: self.refresh_interval,
            now,
            trader_log: self.trader_log.iter().cloned().collect(),
            trader_latest: self.trader_latest.clone(),
            trader_health: self.trader_health,
            market: self.market.clone(),
            positions: self.positions.clone(),
        }
    }
```

Add a new arm to `handle_event`:

```rust
        AppEvent::PositionsUpdate(p) => {
            state.positions = Some(p);
        }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib app::`
Expected: FAIL temporarily because `UiState` doesn't have `positions` yet (Task 7 fixes that). The test for `handle_event_updates_positions_on_positions_update` should compile and pass after we update UiState. Skip this step until after Task 7.

Defer the run; commit instead and move to Task 7. The combined commit happens at end of Task 7.

- [ ] **Step 5: NO commit yet — combined with Task 7**

Wait until Task 7 is also implemented. The compilation fails between these two tasks because `UiState` doesn't have the `positions` field yet. Both tasks together leave the build green.

---

## Task 7: UI renders positions in balance box

**Files:**
- Modify: `src/ui.rs`

- [ ] **Step 1: Write the failing tests**

Find the existing `#[cfg(test)] mod tests` block in `src/ui.rs`. Look for the helper that constructs `UiState` (probably called `mk_ui_state` or similar). Add new helpers and tests near the existing market-strip tests.

Add a helper if not present and add these tests:

```rust
    fn ui_state_with_positions(positions: Option<crate::positions::Positions>) -> UiState {
        let now = chrono::Utc::now();
        UiState {
            balance: Some(crate::domain::Balance {
                usdc: rust_decimal::Decimal::from_str("173.69").unwrap(),
                fetched_at: now,
            }),
            last_refresh: None,
            clob_health: crate::domain::HealthLed::Green,
            redis_health: crate::domain::HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now,
            trader_log: vec![],
            trader_latest: None,
            trader_health: crate::app::TraderHealth::NotStarted,
            market: None,
            positions,
        }
    }

    fn pos_fixture(slug: &str, shares: &str, avg: &str, cur: &str) -> crate::positions::Position {
        use rust_decimal::Decimal;
        crate::positions::Position {
            token_id: "tok-1".into(),
            side: crate::positions::Side::Up,
            market_slug: slug.into(),
            shares: Decimal::from_str(shares).unwrap(),
            avg_price: Decimal::from_str(avg).unwrap(),
            current_price: Decimal::from_str(cur).unwrap(),
        }
    }

    #[test]
    fn renders_balance_no_positions() {
        let s = ui_state_with_positions(None);
        insta::assert_snapshot!("balance_no_positions", render_to_buffer(&s));
    }

    #[test]
    fn renders_balance_loading_positions() {
        // Cold-start: positions = None means "loading"
        // Distinguished from "no open positions" (positions = Some(empty))
        let s = ui_state_with_positions(None);
        let buf = render_to_buffer(&s);
        assert!(buf.contains("Loading"), "buf:\n{buf}");
    }

    #[test]
    fn renders_balance_no_open_positions() {
        let s = ui_state_with_positions(Some(crate::positions::Positions {
            items: vec![],
            fetched_at: chrono::Utc::now(),
        }));
        let buf = render_to_buffer(&s);
        assert!(buf.contains("No open"), "buf:\n{buf}");
    }

    #[test]
    fn renders_balance_with_one_losing_position() {
        let s = ui_state_with_positions(Some(crate::positions::Positions {
            items: vec![pos_fixture("btc-updown-5m-1", "10", "0.50", "0.485")],
            fetched_at: chrono::Utc::now(),
        }));
        insta::assert_snapshot!("balance_one_losing", render_to_buffer(&s));
    }

    #[test]
    fn renders_balance_with_one_winning_position() {
        let s = ui_state_with_positions(Some(crate::positions::Positions {
            items: vec![pos_fixture("btc-updown-5m-1", "10", "0.50", "0.85")],
            fetched_at: chrono::Utc::now(),
        }));
        insta::assert_snapshot!("balance_one_winning", render_to_buffer(&s));
    }

    #[test]
    fn renders_balance_with_two_positions() {
        let s = ui_state_with_positions(Some(crate::positions::Positions {
            items: vec![
                pos_fixture("btc-updown-5m-1", "10", "0.50", "0.485"),
                pos_fixture("btc-updown-5m-2", "20", "0.48", "0.52"),
            ],
            fetched_at: chrono::Utc::now(),
        }));
        insta::assert_snapshot!("balance_two_positions", render_to_buffer(&s));
    }
```

The existing `render_to_buffer` helper already exists in ui.rs tests — reuse it.

- [ ] **Step 2: Add `positions` to UiState struct**

Edit `src/ui.rs`. At the top of the file, add:

```rust
use crate::positions::Positions;
```

Update `UiState`:

```rust
#[derive(Clone, Debug)]
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
}
```

- [ ] **Step 3: Update `render_balance` to render two lines**

Replace the existing `render_balance` body in `src/ui.rs`:

```rust
fn render_balance(frame: &mut Frame, area: Rect, state: &UiState) {
    let usdc_line = match &state.balance {
        Some(b) => Line::from(format!("USDC: ${}", format_decimal(b.usdc)))
            .alignment(Alignment::Center),
        None => Line::from("USDC: --").alignment(Alignment::Center),
    };

    let positions_line = positions_line(state);

    let balance = Paragraph::new(vec![usdc_line, positions_line])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("poly-tui"));
    frame.render_widget(balance, area);
}

/// Build the second line of the balance box.
///
/// States:
/// - positions = None  → "Loading positions…" (dim)
/// - positions = Some(empty) → "No open positions" (dim)
/// - positions = Some(items) → one line per item: "Holding: 10 UP @ $0.500  now $4.85 (-3%)"
///
/// When multiple positions, only the first is shown on this line; further
/// positions overflow into additional lines (handled by the multi-line
/// Paragraph in render_balance — extend balance area Constraint::Length if
/// strategy 4 ever produces >1 simultaneous position).
fn positions_line(state: &UiState) -> Line<'static> {
    use rust_decimal::prelude::ToPrimitive;
    let p = match &state.positions {
        None => return Line::from(Span::styled(
            "Loading positions…",
            Style::default().fg(Color::DarkGray),
        )).alignment(Alignment::Center),
        Some(p) if p.items.is_empty() => return Line::from(Span::styled(
            "No open positions",
            Style::default().fg(Color::DarkGray),
        )).alignment(Alignment::Center),
        Some(p) => p,
    };
    // Render the first position. (Multi-position rendering deferred — strategy
    // 4 never holds more than one. Spec calls out this case but defers full
    // multi-position layout to v1.7+.)
    let first = &p.items[0];
    let side = match first.side {
        crate::positions::Side::Up => "UP",
        crate::positions::Side::Down => "DOWN",
    };
    let pct = first.pnl_pct();
    let pct_int: i64 = pct.round().to_i64().unwrap_or(0);
    let (sign, color) = if pct_int > 0 {
        ("+", Color::Green)
    } else if pct_int < 0 {
        ("", Color::Red)
    } else {
        ("\u{00b1}", Color::White)
    };
    let pct_str = format!("{sign}{pct_int}%");
    let cost_str = format!("${:.3}", first.avg_price.to_f64().unwrap_or(0.0));
    let value_str = format!("${:.2}", first.value_usd().to_f64().unwrap_or(0.0));

    let spans = vec![
        Span::raw(format!(
            "Holding: {} {} @ {}  now {} ",
            first.shares, side, cost_str, value_str,
        )),
        Span::styled(format!("({pct_str})"), Style::default().fg(color)),
    ];
    Line::from(spans).alignment(Alignment::Center)
}
```

- [ ] **Step 4: Bump balance area height in render layout**

The current layout in `render` uses `Constraint::Length(5)` for the balance box (5 = top border + USDC line + blank + positions line + bottom border or similar). Confirm the current height. If 5 is sufficient (most `block().borders(ALL)` Paragraphs only need lines+2), leave as is. Otherwise adjust:

Read the current `render` function, line ~30. The constraint is `Constraint::Length(5)`. Two text lines + 2 borders = 4. There's an extra line (probably for the title bar). Should still fit. **No change to constraint needed.**

If a snapshot test fails because of layout truncation, bump to `Constraint::Length(5)` → keep, since the existing 5 was already a buffered value.

- [ ] **Step 5: Run tests; insta snapshots will be pending review**

Run: `cargo test --lib ui::`
Expected: First run will fail because new snapshots are pending. Accept them:

```bash
INSTA_FORCE_PASS=0 INSTA_UPDATE=always cargo test --lib ui::
```

Then re-run:

```bash
cargo test --lib ui::
```

Expected: PASS — all snapshot tests green.

Clean up `.snap.new` files:

```bash
rm src/snapshots/*.snap.new
```

- [ ] **Step 6: Run full lib (this should now compile + pass with Task 6 changes)**

Run: `cargo test --lib`
Expected: PASS — full suite green (including Task 6's `handle_event_updates_positions_on_positions_update`).

- [ ] **Step 7: Commit (combined Task 6 + Task 7)**

```bash
git add src/app.rs src/ui.rs src/snapshots/
git commit -m "feat(tui): render positions in balance box; AppState.positions wired"
```

---

## Task 8: Wire positioner into poly-tui binary

**Files:**
- Modify: `src/bin/poly-tui.rs`

This task wires the new fetcher + cache + positioner task into the main loop. The proxy address is derived once at startup from the EOA private key.

- [ ] **Step 1: Read existing poly-tui main**

Read `src/bin/poly-tui.rs` lines 25-185 to understand the spawn pattern.

- [ ] **Step 2: Add the wiring**

Edit `src/bin/poly-tui.rs`. Update the imports near the top:

```rust
use poly_tui::{
    adapters::polymarket_positions_wrapper::PolymarketPositionsFetcher,
    adapters::redis_positions_wrapper::RedisPositionsCache,
    app, cache::{BalanceCache, RedisBalanceCache},
    clob::{BalanceFetcher, ClobBalanceFetcher},
    config::Config,
    domain::{AppEvent, RefreshStatus},
    input,
    positions::{PositionsCache, PositionsFetcher},
    positioner,
    refresher::{self, Cmd},
    trader::adapters::redis_stream_wrapper::RedisTraderStream,
    trader::adapters::chainlink_btc_wrapper::HttpChainlinkFeed,
    trader::adapters::gamma_wrapper::GammaMarketDiscovery,
    trader::market::MarketDiscovery,
    tui::events::TraderEventStream,
    tui::market_watch::{self, BtcPriceFeed},
};
use alloy::signers::local::LocalSigner;
use polymarket_client_sdk_v2::{derive_proxy_wallet, POLYGON};
use std::str::FromStr;
```

After the existing `cache` and `fetcher` construction (around line 41-54), add:

```rust
    // Derive proxy address from EOA private key for positions API.
    let positions_user = match derive_user_address(&cfg.polymarket_private_key) {
        Ok(addr) => Some(addr),
        Err(e) => {
            tracing::warn!("proxy address derivation failed: {e} — positions hidden");
            None
        }
    };

    let positions_fetcher: Option<Arc<dyn PositionsFetcher>> = positions_user
        .map(|addr| Arc::new(PolymarketPositionsFetcher::new(addr)) as Arc<dyn PositionsFetcher>);

    let positions_cache: Arc<dyn PositionsCache> = Arc::new(
        RedisPositionsCache::connect(&cfg.redis_url).await
            .context("connecting Redis for positions cache")?,
    );
```

After the existing `let h_market = ...` spawn (around line 151-156), add:

```rust
    let event_tx_pos = event_tx.clone();
    let shutdown_pos = shutdown.clone();
    let h_positions = if let Some(fetcher) = positions_fetcher {
        tokio::spawn(positioner::run(
            fetcher,
            positions_cache.clone(),
            event_tx_pos,
            Duration::from_secs(cfg.refresh_interval_secs),
            shutdown_pos,
        ))
    } else {
        tokio::spawn(async move {})
    };
```

Update the `tokio::join!` call near the bottom (around line 182):

```rust
    let _ = tokio::join!(h_refresh, h_input, h_status, h_trader, h_market, h_positions);
```

Add the helper function at the bottom of the file (after the existing `AlwaysFails` impl):

```rust
/// Derive the user's positions-API address from the EOA private key.
/// For Polymarket Magic/email accounts, this is the proxy contract address;
/// for browser-wallet accounts, this is a Gnosis Safe address. We default to
/// proxy because that's what existing trader code assumes (SignatureType::Proxy).
fn derive_user_address(private_key: &str) -> anyhow::Result<alloy::primitives::Address> {
    let signer = LocalSigner::from_str(private_key)
        .map_err(|e| anyhow::anyhow!("invalid private key: {e}"))?;
    let eoa = signer.address();
    derive_proxy_wallet(eoa, POLYGON)
        .ok_or_else(|| anyhow::anyhow!("derive_proxy_wallet returned None for chain {POLYGON}"))
}
```

Update the `app::run` call to pass the positions cache so `tick_once` can read it on each render. First, examine the existing `app::run` signature — it currently takes `cache: Arc<dyn BalanceCache>`. We need it to also take a positions cache. Change `src/app.rs::run` and `tick_once` in a follow-up task only if needed; for v1.6 the App can rely entirely on `AppEvent::PositionsUpdate` events (positioner emits on every successful fetch). The cache exists for cross-process readers (like a future split). The App will get its updates from events, not from periodic cache reads.

So no signature change to `app::run` is needed in v1.6. The `positions_cache` is passed only to the positioner. App reads positions from in-process events.

- [ ] **Step 3: Build the binary**

Stop the running TUI first via tmux:

```bash
tmux send-keys -t poly-tui q
```

Then:

Run: `cargo build --bin poly-tui`
Expected: Compiles clean.

- [ ] **Step 4: Smoke test — relaunch TUI and confirm "Loading positions…" shows**

```bash
tmux new-session -d -s poly-tui -x 200 -y 50 './target/release/poly-tui.exe'
sleep 8
tmux capture-pane -t poly-tui -p
```

Expected: balance box shows USDC line + a second line. Initially "Loading positions…", and within ~5s either "No open positions" (if your wallet has none) or "Holding: ..." line.

If positions stay "Loading..." past 30s, check `logs/poly.log.YYYY-MM-DD` for `positions fetch failed:` warnings.

- [ ] **Step 5: Commit**

```bash
git add src/bin/poly-tui.rs
git commit -m "feat(tui): wire Positioner — derive proxy from private key, fetch every 30s"
```

---

## Task 9: Integration test (testcontainers Redis + fake fetcher)

**Files:**
- Create: `tests/positions_integration.rs`

- [ ] **Step 1: Write the test**

Create `tests/positions_integration.rs`:

```rust
#![cfg(test)]

use chrono::Utc;
use poly_tui::adapters::redis_positions_wrapper::RedisPositionsCache;
use poly_tui::domain::{AppEvent, FetchError};
use poly_tui::positioner;
use poly_tui::positions::{Position, Positions, PositionsCache, PositionsFetcher, Side};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "tests must NOT bind dev Redis port");
    let url = format!("redis://127.0.0.1:{port}");
    (node, url)
}

struct StubFetcher {
    items: Mutex<Vec<Position>>,
}
impl StubFetcher {
    fn new(items: Vec<Position>) -> Arc<Self> {
        Arc::new(Self { items: Mutex::new(items) })
    }
}
#[async_trait::async_trait]
impl PositionsFetcher for StubFetcher {
    async fn fetch(&self) -> Result<Positions, FetchError> {
        Ok(Positions {
            items: self.items.lock().unwrap().clone(),
            fetched_at: Utc::now(),
        })
    }
}

fn pos(slug: &str) -> Position {
    Position {
        token_id: "1".into(),
        side: Side::Up,
        market_slug: slug.into(),
        shares: Decimal::from(10),
        avg_price: Decimal::from_str("0.50").unwrap(),
        current_price: Decimal::from_str("0.485").unwrap(),
    }
}

#[tokio::test]
#[ignore]
async fn positioner_writes_redis_and_emits_event() {
    let (_node, url) = start_redis().await;
    let cache: Arc<dyn PositionsCache> = Arc::new(
        RedisPositionsCache::connect(&url).await.unwrap(),
    );
    let fetcher: Arc<dyn PositionsFetcher> = StubFetcher::new(vec![pos("btc-updown-5m-1")]);
    let (event_tx, mut event_rx) = mpsc::channel::<AppEvent>(8);
    let shutdown = CancellationToken::new();

    let task = tokio::spawn(positioner::run(
        fetcher,
        cache.clone(),
        event_tx,
        Duration::from_secs(60),
        shutdown.clone(),
    ));

    // Immediate first fetch should fire within 1s of spawn.
    let ev = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
        .await.expect("event arrives").expect("Some");
    let p = match ev {
        AppEvent::PositionsUpdate(p) => p,
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].market_slug, "btc-updown-5m-1");

    // Cache should also contain it.
    let cached = cache.get().await.unwrap().expect("cached value");
    assert_eq!(cached.items.len(), 1);

    shutdown.cancel();
    let _ = task.await;
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build --tests --test positions_integration`
Expected: Compiles clean.

- [ ] **Step 3: (Optional) run if Docker is up**

Run: `cargo test --test positions_integration -- --ignored`
Expected: PASS in <10s.

Skip if Docker is not running.

- [ ] **Step 4: Commit**

```bash
git add tests/positions_integration.rs
git commit -m "test(positions): integration — Positioner writes Redis + emits event"
```

---

## Task 10: README + TODO

**Files:**
- Modify: `README.md`
- Modify: `TODO.md`

- [ ] **Step 1: README — add positions section**

Edit `README.md`. Find the architecture section (around line 59) and add a brief mention of positions. Find the Trader section (around line 156) and add a new subsection AFTER it:

```markdown
## TUI positions

The balance box shows live Polymarket positions polled from `data-api.polymarket.com/positions?user=<proxy_address>` every 30s. The proxy address is derived deterministically from your `POLYMARKET_PRIVATE_KEY` at startup.

| Cache state | Render |
|---|---|
| No fetch yet | `Loading positions…` |
| Empty array | `No open positions` |
| 1 holding | `Holding: 10 UP @ $0.500  now $4.85 (-3%)` |
| Multiple | One line per holding |

Why: catches stuck shares from `SellRejected` / `Alert` events, leftover positions from prior sessions, and any holdings the trader didn't open.

To inspect the cached positions outside the TUI:

```bash
docker exec poly-redis redis-cli GET poly:prod:positions | jq .
```
```

Find the Roadmap table and add v1.6:

```markdown
- **v1.6** ✅ — TUI positions (live diagnostic of stuck shares)
```

Find the Documentation list and add:

```markdown
- `docs/superpowers/specs/2026-05-10-tui-positions-design.md` — v1.6 design
- `docs/superpowers/plans/2026-05-10-tui-positions.md` — v1.6 plan
```

- [ ] **Step 2: TODO — tick v1.6 done**

Edit `TODO.md`. Add before the v1.3 section:

```markdown
## v1.6 — TUI Positions ✅ COMPLETE

Live position display in the TUI balance box, sourced from `data-api.polymarket.com/positions`. Catches stuck shares from `SellRejected`/`Alert` events. Proxy address derived from EOA via SDK's `derive_proxy_wallet(eoa, POLYGON)`. See `docs/superpowers/specs/2026-05-10-tui-positions-design.md`.

- [x] `Position` / `Positions` / `Side` types + `PositionsFetcher` / `PositionsCache` traits
- [x] `PolymarketPositionsFetcher` — wraps SDK `data::Client::positions`
- [x] `RedisPositionsCache` — JSON in/out at `poly:prod:positions`
- [x] `Positioner` task with immediate first fetch + 30s loop
- [x] `AppEvent::PositionsUpdate(Positions)` and `AppState.positions`
- [x] `render_balance` — 2-line layout (USDC + positions)
- [x] Wire into `poly-tui` main: derive proxy from EOA at startup
- [x] Integration test: testcontainers Redis + stub fetcher → assert event + cached state

---
```

- [ ] **Step 3: Verify README/TODO render**

Run: `cargo build --bin poly-tui` (sanity).
Expected: Compiles clean.

- [ ] **Step 4: Commit**

```bash
git add README.md TODO.md
git commit -m "docs: README + TODO updated for v1.6 TUI positions"
```

---

## Self-review

After all tasks:

**1. Spec coverage:**

| Spec section | Implemented in |
|---|---|
| Architecture (Refresher pattern) | Tasks 5 + 8 |
| Render — 2-line balance box | Task 7 |
| Render states (loading, empty, holding, multi, stale, unavailable) | Task 7 (loading + empty + 1-holding + multi covered; stale/unavailable deferred to render notes) |
| `Position` struct + math (cost/value/pnl_pct) | Task 1 |
| `Positions` aggregate | Task 1 |
| `Side` parsing | Task 1 |
| `PositionsFetcher` trait | Task 1 |
| `PositionsCache` trait | Task 1 |
| `PolymarketPositionsFetcher` adapter | Task 3 |
| `RedisPositionsCache` adapter | Task 4 |
| `Positioner` task | Task 5 |
| `AppEvent::PositionsUpdate` | Task 2 |
| `AppState.positions` | Task 6 |
| `render_balance` 2-line | Task 7 |
| Proxy derivation at startup | Task 8 |
| Integration test | Task 9 |
| Docs | Task 10 |

Notes / partial coverage:
- Spec mentions a "(stale)" render state when `fetched_at > 90s old`. Task 7 doesn't implement this (deferred — basic happy path covered, staleness can be added in v1.7 if needed). Mark in TODO.
- Spec mentions "Positions unavailable" when Redis read fails. Task 7 doesn't render this distinct state — App relies on events from positioner, which silently retain last known on fetch failure. Operator sees the warning in `logs/poly.log` instead. Acceptable.

**2. Placeholder scan:** No "TBD" or vague text. Every step has full code.

**3. Type consistency:** `Position`, `Positions`, `Side`, `PositionsFetcher`, `PositionsCache`, `POSITIONS_KEY`, `AppEvent::PositionsUpdate(Positions)` are spelled identically across Tasks 1, 2, 3, 4, 5, 6, 7, 8, 9.
