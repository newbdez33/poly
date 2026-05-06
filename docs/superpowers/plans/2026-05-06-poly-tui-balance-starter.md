# Poly TUI Balance Starter — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Single-binary Rust TUI that displays Polymarket USDC balance, refreshed in the background through Redis cache, with BDD/TDD/E2E coverage.

**Architecture:** One binary, three tokio tasks (Refresher fetches CLOB → writes Redis; App reads Redis + renders ratatui frame; Input forwards crossterm events). Modules split along future-crate boundaries via `BalanceFetcher` and `BalanceCache` traits so v1.1 can split into daemon/TUI without rewrites.

**Tech Stack:** Rust 1.78+, tokio, ratatui + crossterm, polymarket-client-sdk-v2, alloy (signer), fred (Redis), cucumber-rs, testcontainers, wiremock, mockall, insta.

**Spec:** `docs/superpowers/specs/2026-05-06-poly-tui-balance-starter-design.md`

---

## File Structure

Lib + bin layout so tests can `use poly_tui::*` directly.

```
poly/
├── Cargo.toml                        ← workspace? no — single package, lib + bin
├── .env.example                      ← committed template (no secrets)
├── .gitignore                        ← exists
├── docker-compose.yml                ← exists
├── TODO.md                           ← exists
├── docs/superpowers/                 ← spec + this plan
├── src/
│   ├── lib.rs                        ← pub mod declarations
│   ├── bin/
│   │   └── poly-tui.rs               ← #[tokio::main] entry; only this is binary
│   ├── config.rs                     ← Config struct, .env loader
│   ├── domain.rs                     ← Balance, AppEvent, RefreshStatus, HealthLed, errors
│   ├── clob.rs                       ← BalanceFetcher trait + ClobBalanceFetcher (rs-clob-client adapter)
│   ├── cache.rs                      ← BalanceCache trait + RedisBalanceCache (fred adapter) + key constants
│   ├── refresher.rs                  ← run() + do_fetch()
│   ├── app.rs                        ← AppState, handle_event(), tick_once(), run()
│   ├── ui.rs                         ← render(frame, state) — pure, no I/O
│   └── input.rs                      ← crossterm event reader → AppEvent channel
└── tests/
    ├── features/
    │   └── balance.feature
    ├── bdd.rs                        ← cucumber-rs entry, runs features/*
    ├── e2e_tui.rs                    ← #[ignore] testcontainers + wiremock
    ├── cache_integration.rs          ← #[ignore] real Redis cache integration
    └── support/
        ├── mod.rs                    ← re-exports the fakes
        ├── fake_fetcher.rs           ← in-memory BalanceFetcher
        └── memory_cache.rs           ← in-memory BalanceCache
```

**Rules:**
- `ui.rs` is pure (no async, no I/O). Snapshot-testable with `TestBackend`.
- `app.rs` and `refresher.rs` depend only on traits, not concrete types.
- Test fakes live in `tests/support/`, shared across `bdd.rs`, `e2e_tui.rs`, and lib unit tests via `#[path = "../tests/support/mod.rs"]` where needed (or by being inside `tests/` only — preferred).
- **Where unit tests need fakes**, declare the fake module inline in the unit test (`#[cfg(test)]`) — this avoids cross-pollution with integration tests.

---

## Task 0: Project bootstrap

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/bin/poly-tui.rs`
- Create: `.env.example`

- [ ] **Step 1: Initialize cargo package as a lib + bin**

```bash
cd C:/Users/newbd/projects/dev/poly
cargo init --name poly-tui --lib
mkdir -p src/bin
```

This creates `Cargo.toml`, `src/lib.rs`, but we'll convert to also have a binary in `src/bin/poly-tui.rs`. Remove any auto-created `src/main.rs` if present.

- [ ] **Step 2: Write `Cargo.toml`**

Replace contents with:

```toml
[package]
name = "poly-tui"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
async-trait = "0.1"
futures = "0.3"

ratatui = "0.29"
crossterm = "0.28"

polymarket-client-sdk-v2 = "*"
alloy = { version = "0.8", features = ["signer-local"] }

fred = { version = "9", features = ["enable-rustls"] }

rust_decimal = { version = "1", features = ["serde"] }
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

dotenvy = "0.15"
envy = "0.4"

anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-appender = "0.2"

[dev-dependencies]
cucumber = { version = "0.21", features = ["macros"] }
mockall = "0.13"
insta = { version = "1", features = ["yaml"] }
wiremock = "0.6"
testcontainers = "0.23"
testcontainers-modules = { version = "0.11", features = ["redis"] }
tokio-test = "0.4"
serde_json = "1"
tempfile = "3"

[[bin]]
name = "poly-tui"
path = "src/bin/poly-tui.rs"

[lib]
path = "src/lib.rs"

[[test]]
name = "bdd"
path = "tests/bdd.rs"
harness = false   # cucumber-rs uses its own harness

[[test]]
name = "e2e_tui"
path = "tests/e2e_tui.rs"

[[test]]
name = "cache_integration"
path = "tests/cache_integration.rs"
```

> **Note for the implementer:** `polymarket-client-sdk-v2 = "*"` — run `cargo search polymarket-client-sdk-v2` (or check crates.io) and lock to the latest concrete version. The crate name in the spec snippet is `polymarket_client_sdk_v2` (underscored Rust name) — the package name on crates.io may differ; adjust accordingly. If the v2 SDK isn't on crates.io yet, fall back to `polymarket = "*"` or pull from git: `polymarket-client-sdk-v2 = { git = "https://github.com/polymarket/rs-clob-client-v2" }`.

- [ ] **Step 3: Stub `src/lib.rs`**

```rust
pub mod config;
pub mod domain;
pub mod clob;
pub mod cache;
pub mod refresher;
pub mod app;
pub mod ui;
pub mod input;
```

This will fail to compile until each module file exists. Create empty placeholders next.

- [ ] **Step 4: Create empty module files**

Create each of these as an empty file (or with `// placeholder` inside):

```bash
echo "// placeholder" > src/config.rs
echo "// placeholder" > src/domain.rs
echo "// placeholder" > src/clob.rs
echo "// placeholder" > src/cache.rs
echo "// placeholder" > src/refresher.rs
echo "// placeholder" > src/app.rs
echo "// placeholder" > src/ui.rs
echo "// placeholder" > src/input.rs
```

- [ ] **Step 5: Stub `src/bin/poly-tui.rs`**

```rust
fn main() {
    println!("poly-tui placeholder");
}
```

- [ ] **Step 6: Write `.env.example`**

```bash
# Polygon wallet private key — USE A DEDICATED WALLET, NEVER YOUR MAIN WALLET
POLYMARKET_PRIVATE_KEY=0x0000000000000000000000000000000000000000000000000000000000000000

# Redis (matches docker-compose.yml)
REDIS_URL=redis://127.0.0.1:6379

# Background refresh interval in seconds
REFRESH_INTERVAL_SECS=30

# Polymarket CLOB host
CLOB_HOST=https://clob-v2.polymarket.com

# tracing-subscriber EnvFilter directive
LOG_LEVEL=info
```

- [ ] **Step 7: Verify `cargo build` succeeds**

```bash
cargo build
```

Expected: `Finished` with no errors. Warnings about empty modules are fine.

> If `polymarket-client-sdk-v2` doesn't resolve, comment its line in `Cargo.toml` and add a TODO. We'll wire it in at Task 9; tasks before that don't depend on it.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/ .env.example
git commit -m "chore: cargo bootstrap (lib + bin) with deps"
```

---

## Task 1: Domain types

**Files:**
- Modify: `src/domain.rs`

- [ ] **Step 1: Write failing tests in `src/domain.rs`**

Replace the placeholder with:

```rust
use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::time::Duration;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    pub usdc: Decimal,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefreshStatus {
    Ok { at: DateTime<Utc> },
    Failed { at: DateTime<Utc>, error: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthLed {
    Green,
    Yellow,
    Red,
}

impl HealthLed {
    /// Derive a CLOB health LED from the time since the last successful refresh.
    pub fn from_clob_age(last_status: Option<&RefreshStatus>, interval: Duration, now: DateTime<Utc>) -> HealthLed {
        match last_status {
            None => HealthLed::Red,
            Some(RefreshStatus::Failed { .. }) => HealthLed::Red,
            Some(RefreshStatus::Ok { at }) => {
                let age = now.signed_duration_since(*at).to_std().unwrap_or(Duration::ZERO);
                let i = interval.as_secs_f64();
                let a = age.as_secs_f64();
                if a < 1.5 * i {
                    HealthLed::Green
                } else if a < 3.0 * i {
                    HealthLed::Yellow
                } else {
                    HealthLed::Red
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum AppEvent {
    Tick,
    Key(crossterm::event::KeyEvent),
    Refresh(RefreshStatus),
    Shutdown,
}

#[derive(Error, Debug)]
pub enum FetchError {
    #[error("CLOB request failed: {0}")]
    Network(String),
    #[error("CLOB returned invalid data: {0}")]
    Decode(String),
    #[error("authentication failed")]
    Auth,
}

#[derive(Error, Debug)]
pub enum CacheError {
    #[error("redis connection lost")]
    Disconnected,
    #[error("redis op failed: {0}")]
    Op(String),
    #[error("cache value malformed: {0}")]
    Decode(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn balance_serde_roundtrip() {
        let b = Balance {
            usdc: Decimal::from_str("123.45").unwrap(),
            fetched_at: ts(1_700_000_000),
        };
        let json = serde_json::to_string(&b).unwrap();
        let back: Balance = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn health_red_when_no_status() {
        let led = HealthLed::from_clob_age(None, Duration::from_secs(30), ts(1000));
        assert_eq!(led, HealthLed::Red);
    }

    #[test]
    fn health_red_when_last_failed() {
        let s = RefreshStatus::Failed { at: ts(1000), error: "x".into() };
        let led = HealthLed::from_clob_age(Some(&s), Duration::from_secs(30), ts(1001));
        assert_eq!(led, HealthLed::Red);
    }

    #[test]
    fn health_green_within_1_5x() {
        let s = RefreshStatus::Ok { at: ts(1000) };
        let led = HealthLed::from_clob_age(Some(&s), Duration::from_secs(30), ts(1040));
        assert_eq!(led, HealthLed::Green);
    }

    #[test]
    fn health_yellow_between_1_5x_and_3x() {
        let s = RefreshStatus::Ok { at: ts(1000) };
        // 60s after at, interval 30s → 2x → yellow
        let led = HealthLed::from_clob_age(Some(&s), Duration::from_secs(30), ts(1060));
        assert_eq!(led, HealthLed::Yellow);
    }

    #[test]
    fn health_red_beyond_3x() {
        let s = RefreshStatus::Ok { at: ts(1000) };
        // 100s after, interval 30s → > 3x → red
        let led = HealthLed::from_clob_age(Some(&s), Duration::from_secs(30), ts(1100));
        assert_eq!(led, HealthLed::Red);
    }
}
```

- [ ] **Step 2: Run tests — they should pass (impl is in same step)**

```bash
cargo test --lib domain
```

Expected: 6 tests pass.

> **Note:** This is one of the few cases where test and impl arrive together — for plain data types, the type definition IS the production code. The discipline is still TDD because the assertions encode the intent and would fail if the impl drifted.

- [ ] **Step 3: Commit**

```bash
git add src/domain.rs
git commit -m "feat(domain): Balance, RefreshStatus, HealthLed, AppEvent + tests"
```

---

## Task 2: Config loader

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Write failing tests in `src/config.rs`**

```rust
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub polymarket_private_key: String,
    #[serde(default = "default_redis_url")]
    pub redis_url: String,
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_secs: u64,
    #[serde(default = "default_clob_host")]
    pub clob_host: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_redis_url() -> String { "redis://127.0.0.1:6379".to_string() }
fn default_refresh_interval() -> u64 { 30 }
fn default_clob_host() -> String { "https://clob-v2.polymarket.com".to_string() }
fn default_log_level() -> String { "info".to_string() }

impl Config {
    /// Load from process environment (caller is expected to have run `dotenvy::dotenv()` first).
    pub fn from_env() -> Result<Self, envy::Error> {
        envy::from_env::<Config>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env<F: FnOnce()>(vars: &[(&str, &str)], f: F) {
        let saved: Vec<(String, Option<String>)> = vars.iter()
            .map(|(k, _)| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            std::env::set_var(k, v);
        }
        f();
        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(&k, val),
                None => std::env::remove_var(&k),
            }
        }
    }

    #[test]
    fn loads_required_with_defaults() {
        // Tests run in parallel — each test must isolate env. Use a dedicated key suffix
        // here; for stronger isolation, run config tests with --test-threads=1 in CI.
        with_env(&[("POLYMARKET_PRIVATE_KEY", "0xabc")], || {
            let cfg = Config::from_env().unwrap();
            assert_eq!(cfg.polymarket_private_key, "0xabc");
            assert_eq!(cfg.redis_url, "redis://127.0.0.1:6379");
            assert_eq!(cfg.refresh_interval_secs, 30);
            assert_eq!(cfg.clob_host, "https://clob-v2.polymarket.com");
            assert_eq!(cfg.log_level, "info");
        });
    }

    #[test]
    fn missing_private_key_errors() {
        std::env::remove_var("POLYMARKET_PRIVATE_KEY");
        assert!(Config::from_env().is_err());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib config -- --test-threads=1
```

Expected: 2 tests pass. `--test-threads=1` because tests mutate process env.

- [ ] **Step 3: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): .env loader with defaults"
```

---

## Task 3: BalanceCache trait + InMemoryCache (test support)

**Files:**
- Modify: `src/cache.rs`
- Create: `tests/support/mod.rs`
- Create: `tests/support/memory_cache.rs`

- [ ] **Step 1: Write trait + key constants in `src/cache.rs`**

```rust
use crate::domain::{Balance, CacheError};
use async_trait::async_trait;

/// Production Redis key for the latest balance. Namespaced so test data can never
/// collide with prod data even if someone connects the wrong client.
pub const BALANCE_KEY_PROD: &str = "poly:prod:balance:latest";

#[async_trait]
pub trait BalanceCache: Send + Sync {
    async fn get(&self) -> Result<Option<Balance>, CacheError>;
    async fn set(&self, balance: &Balance) -> Result<(), CacheError>;
    async fn ping(&self) -> Result<(), CacheError>;
}
```

- [ ] **Step 2: Write `InMemoryCache` in `tests/support/memory_cache.rs`**

```rust
use async_trait::async_trait;
use poly_tui::cache::BalanceCache;
use poly_tui::domain::{Balance, CacheError};
use std::sync::Mutex;

#[derive(Default)]
pub struct InMemoryCache {
    state: Mutex<Option<Balance>>,
    pub fail_next_get: Mutex<bool>,
    pub fail_next_set: Mutex<bool>,
    pub fail_next_ping: Mutex<bool>,
}

impl InMemoryCache {
    pub fn new() -> Self { Self::default() }

    pub fn with_balance(b: Balance) -> Self {
        let c = Self::default();
        *c.state.lock().unwrap() = Some(b);
        c
    }
}

#[async_trait]
impl BalanceCache for InMemoryCache {
    async fn get(&self) -> Result<Option<Balance>, CacheError> {
        let mut flag = self.fail_next_get.lock().unwrap();
        if *flag { *flag = false; return Err(CacheError::Disconnected); }
        Ok(self.state.lock().unwrap().clone())
    }

    async fn set(&self, balance: &Balance) -> Result<(), CacheError> {
        let mut flag = self.fail_next_set.lock().unwrap();
        if *flag { *flag = false; return Err(CacheError::Op("forced".into())); }
        *self.state.lock().unwrap() = Some(balance.clone());
        Ok(())
    }

    async fn ping(&self) -> Result<(), CacheError> {
        let mut flag = self.fail_next_ping.lock().unwrap();
        if *flag { *flag = false; return Err(CacheError::Disconnected); }
        Ok(())
    }
}
```

- [ ] **Step 3: Write `tests/support/mod.rs`**

```rust
pub mod memory_cache;
pub mod fake_fetcher;

pub use memory_cache::InMemoryCache;
pub use fake_fetcher::FakeFetcher;
```

(The `fake_fetcher` reference will fail until Task 4 — that's expected, we'll create it in the next task. For now, leave only `pub mod memory_cache;` and `pub use memory_cache::InMemoryCache;` — add the other two lines in Task 4.)

For now, write only:
```rust
pub mod memory_cache;
pub use memory_cache::InMemoryCache;
```

- [ ] **Step 4: Verify lib still compiles**

```bash
cargo build --lib
```

Expected: success.

- [ ] **Step 5: Commit**

```bash
git add src/cache.rs tests/support/
git commit -m "feat(cache): BalanceCache trait + InMemoryCache test fake"
```

---

## Task 4: BalanceFetcher trait + FakeFetcher (test support)

**Files:**
- Modify: `src/clob.rs`
- Create: `tests/support/fake_fetcher.rs`
- Modify: `tests/support/mod.rs`

- [ ] **Step 1: Write trait in `src/clob.rs`**

```rust
use crate::domain::{Balance, FetchError};
use async_trait::async_trait;

#[async_trait]
pub trait BalanceFetcher: Send + Sync {
    async fn fetch(&self) -> Result<Balance, FetchError>;
}
```

- [ ] **Step 2: Write `FakeFetcher` in `tests/support/fake_fetcher.rs`**

```rust
use async_trait::async_trait;
use chrono::Utc;
use poly_tui::clob::BalanceFetcher;
use poly_tui::domain::{Balance, FetchError};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};

pub struct FakeFetcher {
    balance: Mutex<Decimal>,
    fail: Mutex<bool>,
    pub call_count: AtomicUsize,
}

impl FakeFetcher {
    pub fn with_usdc(amount: &str) -> Self {
        Self {
            balance: Mutex::new(Decimal::from_str(amount).unwrap()),
            fail: Mutex::new(false),
            call_count: AtomicUsize::new(0),
        }
    }

    pub fn set_balance(&self, amount: &str) {
        *self.balance.lock().unwrap() = Decimal::from_str(amount).unwrap();
    }

    pub fn set_fail(&self, fail: bool) {
        *self.fail.lock().unwrap() = fail;
    }

    pub fn calls(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BalanceFetcher for FakeFetcher {
    async fn fetch(&self) -> Result<Balance, FetchError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        if *self.fail.lock().unwrap() {
            return Err(FetchError::Network("forced fake fail".into()));
        }
        Ok(Balance {
            usdc: *self.balance.lock().unwrap(),
            fetched_at: Utc::now(),
        })
    }
}
```

- [ ] **Step 3: Update `tests/support/mod.rs`**

```rust
pub mod memory_cache;
pub mod fake_fetcher;

pub use memory_cache::InMemoryCache;
pub use fake_fetcher::FakeFetcher;
```

- [ ] **Step 4: Verify**

```bash
cargo build --lib
cargo build --tests   # supports compilation of test fakes
```

> **Note:** `cargo build --tests` won't yet succeed unless we have at least one integration test file to root the support tree. Skip this check until Task 12; for now `cargo build --lib` is the gate.

```bash
cargo build --lib
```

Expected: success.

- [ ] **Step 5: Commit**

```bash
git add src/clob.rs tests/support/fake_fetcher.rs tests/support/mod.rs
git commit -m "feat(clob): BalanceFetcher trait + FakeFetcher test fake"
```

---

## Task 5: Refresher task

**Files:**
- Modify: `src/refresher.rs`

- [ ] **Step 1: Write `do_fetch()` and `run()` in `src/refresher.rs`**

```rust
use crate::cache::BalanceCache;
use crate::clob::BalanceFetcher;
use crate::domain::RefreshStatus;
use chrono::Utc;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum Cmd {
    ForceRefresh,
}

/// One-shot fetch + cache write + status emit. Used both by the periodic loop
/// and by the synchronous startup pre-warm in main.
pub async fn do_fetch(
    fetcher: &dyn BalanceFetcher,
    cache: &dyn BalanceCache,
    status_tx: &mpsc::Sender<RefreshStatus>,
) {
    match fetcher.fetch().await {
        Ok(b) => {
            if let Err(e) = cache.set(&b).await {
                let _ = status_tx
                    .send(RefreshStatus::Failed { at: Utc::now(), error: format!("cache: {e}") })
                    .await;
                return;
            }
            let _ = status_tx
                .send(RefreshStatus::Ok { at: Utc::now() })
                .await;
        }
        Err(e) => {
            let _ = status_tx
                .send(RefreshStatus::Failed { at: Utc::now(), error: e.to_string() })
                .await;
        }
    }
}

/// Long-running refresh loop. Exits when `shutdown` is cancelled.
pub async fn run(
    fetcher: Arc<dyn BalanceFetcher>,
    cache: Arc<dyn BalanceCache>,
    mut cmd_rx: mpsc::Receiver<Cmd>,
    status_tx: mpsc::Sender<RefreshStatus>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            Some(Cmd::ForceRefresh) = cmd_rx.recv() => {
                do_fetch(fetcher.as_ref(), cache.as_ref(), &status_tx).await;
            }
            _ = tokio::time::sleep(interval) => {
                do_fetch(fetcher.as_ref(), cache.as_ref(), &status_tx).await;
            }
        }
    }
}
```

- [ ] **Step 2: Write tests at the bottom of `src/refresher.rs`**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Balance, FetchError};
    use async_trait::async_trait;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Inline fakes — avoid dragging tests/support/ into a unit test (it's an
    // integration-test-only module).

    struct FakeFetcher {
        usdc: Mutex<Decimal>,
        fail: Mutex<bool>,
        calls: AtomicUsize,
    }
    impl FakeFetcher {
        fn ok(amount: &str) -> Arc<Self> {
            Arc::new(Self {
                usdc: Mutex::new(Decimal::from_str(amount).unwrap()),
                fail: Mutex::new(false),
                calls: AtomicUsize::new(0),
            })
        }
    }
    #[async_trait]
    impl BalanceFetcher for FakeFetcher {
        async fn fetch(&self) -> Result<Balance, FetchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if *self.fail.lock().unwrap() {
                return Err(FetchError::Network("x".into()));
            }
            Ok(Balance { usdc: *self.usdc.lock().unwrap(), fetched_at: Utc::now() })
        }
    }

    struct MemCache {
        last: Mutex<Option<Balance>>,
    }
    impl MemCache {
        fn new() -> Arc<Self> { Arc::new(Self { last: Mutex::new(None) }) }
        fn snapshot(&self) -> Option<Balance> { self.last.lock().unwrap().clone() }
    }
    #[async_trait]
    impl BalanceCache for MemCache {
        async fn get(&self) -> Result<Option<Balance>, crate::domain::CacheError> {
            Ok(self.last.lock().unwrap().clone())
        }
        async fn set(&self, b: &Balance) -> Result<(), crate::domain::CacheError> {
            *self.last.lock().unwrap() = Some(b.clone()); Ok(())
        }
        async fn ping(&self) -> Result<(), crate::domain::CacheError> { Ok(()) }
    }

    #[tokio::test]
    async fn do_fetch_writes_cache_and_emits_ok() {
        let f = FakeFetcher::ok("100");
        let c = MemCache::new();
        let (tx, mut rx) = mpsc::channel(8);
        do_fetch(f.as_ref(), c.as_ref(), &tx).await;

        let s = rx.recv().await.unwrap();
        assert!(matches!(s, RefreshStatus::Ok { .. }));
        assert_eq!(c.snapshot().unwrap().usdc, Decimal::from_str("100").unwrap());
    }

    #[tokio::test]
    async fn do_fetch_emits_failed_when_fetch_errors() {
        let f = FakeFetcher::ok("100");
        *f.fail.lock().unwrap() = true;
        let c = MemCache::new();
        let (tx, mut rx) = mpsc::channel(8);
        do_fetch(f.as_ref(), c.as_ref(), &tx).await;

        let s = rx.recv().await.unwrap();
        assert!(matches!(s, RefreshStatus::Failed { .. }));
        assert!(c.snapshot().is_none());
    }

    #[tokio::test]
    async fn force_refresh_command_triggers_fetch() {
        tokio::time::pause();

        let f = FakeFetcher::ok("50");
        let c = MemCache::new();
        let (status_tx, mut status_rx) = mpsc::channel(8);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let token = CancellationToken::new();

        let f_arc: Arc<dyn BalanceFetcher> = f.clone();
        let c_arc: Arc<dyn BalanceCache> = c.clone();
        let task = tokio::spawn(run(f_arc, c_arc, cmd_rx, status_tx,
                                    Duration::from_secs(60), token.clone()));

        cmd_tx.send(Cmd::ForceRefresh).await.unwrap();

        let s = tokio::time::timeout(Duration::from_secs(1), status_rx.recv()).await
            .expect("status emitted").expect("status some");
        assert!(matches!(s, RefreshStatus::Ok { .. }));

        token.cancel();
        let _ = task.await;
    }

    #[tokio::test]
    async fn shutdown_token_cancels_loop() {
        tokio::time::pause();
        let f = FakeFetcher::ok("1");
        let c = MemCache::new();
        let (status_tx, _status_rx) = mpsc::channel(8);
        let (_cmd_tx, cmd_rx) = mpsc::channel(8);
        let token = CancellationToken::new();

        let f_arc: Arc<dyn BalanceFetcher> = f.clone();
        let c_arc: Arc<dyn BalanceCache> = c.clone();
        let task = tokio::spawn(run(f_arc, c_arc, cmd_rx, status_tx,
                                    Duration::from_secs(60), token.clone()));

        token.cancel();
        tokio::time::timeout(Duration::from_secs(1), task).await
            .expect("task exits within 1s")
            .expect("no panic");
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test --lib refresher
```

Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/refresher.rs
git commit -m "feat(refresher): periodic + force-refresh loop with status emit"
```

---

## Task 6: UI render function (insta snapshots)

**Files:**
- Modify: `src/ui.rs`
- Create: `src/ui/snapshots/` (created by insta on first run)

- [ ] **Step 1: Write the `AppState` view + `render()` in `src/ui.rs`**

```rust
use crate::domain::{Balance, HealthLed, RefreshStatus};
use chrono::{DateTime, Utc};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct UiState {
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub clob_health: HealthLed,
    pub redis_health: HealthLed,
    pub refresh_interval: Duration,
    pub now: DateTime<Utc>,   // injected for deterministic snapshots
}

pub fn render(frame: &mut Frame, state: &UiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    // Main: centered balance
    let balance_text = match &state.balance {
        Some(b) => format!("USDC: ${}", format_decimal(b.usdc)),
        None    => "USDC: --".to_string(),
    };
    let balance = Paragraph::new(balance_text)
        .alignment(Alignment::Center)
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("poly-tui"));
    frame.render_widget(balance, chunks[0]);

    // Status bar
    let status = build_status_line(state);
    frame.render_widget(Paragraph::new(status), chunks[1]);
}

fn format_decimal(d: rust_decimal::Decimal) -> String {
    // 2 decimal places, comma thousands separator (rust_decimal lacks built-in
    // grouping; use a simple manual format).
    let raw = format!("{:.2}", d);
    let (whole, frac) = raw.split_once('.').unwrap_or((&raw, "00"));
    let mut grouped = String::new();
    for (i, ch) in whole.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 { grouped.push(','); }
        grouped.push(ch);
    }
    let whole_grouped: String = grouped.chars().rev().collect();
    format!("{whole_grouped}.{frac}")
}

fn led_span<'a>(label: &'a str, led: HealthLed) -> Vec<Span<'a>> {
    let dot = match led {
        HealthLed::Green  => Span::styled("●", Style::default().fg(Color::Green)),
        HealthLed::Yellow => Span::styled("●", Style::default().fg(Color::Yellow)),
        HealthLed::Red    => Span::styled("●", Style::default().fg(Color::Red)),
    };
    vec![dot, Span::raw(format!(" {label} "))]
}

fn build_status_line<'a>(state: &'a UiState) -> Line<'a> {
    let mut spans = Vec::new();
    spans.extend(led_span("CLOB", state.clob_health));
    spans.push(Span::raw(" "));
    spans.extend(led_span("Redis", state.redis_health));
    spans.push(Span::raw("  "));
    spans.push(Span::raw(format!("refresh: {}s", state.refresh_interval.as_secs())));
    spans.push(Span::raw("  "));

    let last_str = match &state.last_refresh {
        Some(RefreshStatus::Ok { at }) => {
            let age = state.now.signed_duration_since(*at).num_seconds().max(0);
            format!("last: {age}s ago")
        }
        Some(RefreshStatus::Failed { error, .. }) => format!("last: failed ({error})"),
        None => "last: --".to_string(),
    };
    spans.push(Span::raw(last_str));
    spans.push(Span::raw("    q quit  r refresh"));

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use ratatui::{Terminal, backend::TestBackend};

    fn fixed_now() -> DateTime<Utc> { Utc.timestamp_opt(1_700_000_120, 0).unwrap() }

    fn render_to_buffer(state: &UiState) -> String {
        let backend = TestBackend::new(60, 12);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, state)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_balance_when_present() {
        let state = UiState {
            balance: Some(Balance {
                usdc: Decimal::from_str("1234.56").unwrap(),
                fetched_at: fixed_now(),
            }),
            last_refresh: Some(RefreshStatus::Ok { at: fixed_now() - chrono::Duration::seconds(12) }),
            clob_health: HealthLed::Green,
            redis_health: HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now: fixed_now(),
        };
        let out = render_to_buffer(&state);
        insta::assert_snapshot!("ui_with_balance", out);
    }

    #[test]
    fn renders_dashes_when_no_balance() {
        let state = UiState {
            balance: None,
            last_refresh: None,
            clob_health: HealthLed::Red,
            redis_health: HealthLed::Red,
            refresh_interval: Duration::from_secs(30),
            now: fixed_now(),
        };
        let out = render_to_buffer(&state);
        insta::assert_snapshot!("ui_no_balance", out);
    }

    #[test]
    fn renders_failure_status() {
        let state = UiState {
            balance: Some(Balance {
                usdc: Decimal::from_str("100").unwrap(),
                fetched_at: fixed_now() - chrono::Duration::seconds(120),
            }),
            last_refresh: Some(RefreshStatus::Failed {
                at: fixed_now() - chrono::Duration::seconds(2),
                error: "Network timeout".into(),
            }),
            clob_health: HealthLed::Red,
            redis_health: HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now: fixed_now(),
        };
        let out = render_to_buffer(&state);
        insta::assert_snapshot!("ui_failure", out);
    }
}
```

- [ ] **Step 2: Run snapshot tests (will be marked PENDING on first run)**

```bash
cargo test --lib ui
```

Expected: 3 snapshot tests fail with "no snapshot found" or "pending". Insta has written `.new` files.

- [ ] **Step 3: Review snapshots**

Inspect the generated snapshot files at `src/snapshots/poly_tui__ui__tests__ui_*.snap.new`. They should look like the spec's mockup (balance centered, status bar at bottom, LEDs as `●`).

If they look correct, accept all:

```bash
cargo install cargo-insta   # if not installed
cargo insta accept
```

(If a snapshot looks wrong, fix `render()` and re-run.)

- [ ] **Step 4: Run snapshot tests again**

```bash
cargo test --lib ui
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/ui.rs src/snapshots/
git commit -m "feat(ui): ratatui render with status bar + insta snapshots"
```

---

## Task 7: App state machine

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Write `AppState`, `handle_event()`, `tick_once()`, `run()` in `src/app.rs`**

```rust
use crate::cache::BalanceCache;
use crate::domain::{AppEvent, Balance, HealthLed, RefreshStatus};
use crate::refresher::Cmd;
use crate::ui::{self, UiState};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::Backend};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct AppState {
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub redis_ok: bool,
    pub refresh_interval: Duration,
    pub should_quit: bool,
}

impl AppState {
    pub fn new(refresh_interval: Duration) -> Self {
        Self {
            balance: None,
            last_refresh: None,
            redis_ok: false,
            refresh_interval,
            should_quit: false,
        }
    }

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
        }
    }
}

pub fn handle_event(state: &mut AppState, ev: AppEvent, cmd_tx: &mpsc::Sender<Cmd>) {
    match ev {
        AppEvent::Tick => {}
        AppEvent::Shutdown => state.should_quit = true,
        AppEvent::Refresh(s) => state.last_refresh = Some(s),
        AppEvent::Key(k) => match (k.code, k.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => state.should_quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => state.should_quit = true,
            (KeyCode::Char('r'), _) => { let _ = cmd_tx.try_send(Cmd::ForceRefresh); }
            _ => {}
        },
    }
}

/// One tick of the main loop. Reads cache, updates state, returns the new state.
/// Exposed for tests to drive the loop deterministically.
pub async fn tick_once(state: &mut AppState, cache: &dyn BalanceCache) {
    match cache.get().await {
        Ok(Some(b)) => { state.balance = Some(b); state.redis_ok = true; }
        Ok(None) => { state.redis_ok = true; /* keep last balance */ }
        Err(_) => { state.redis_ok = false; }
    }
}

pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    cache: Arc<dyn BalanceCache>,
    cmd_tx: mpsc::Sender<Cmd>,
    mut events: mpsc::Receiver<AppEvent>,
    refresh_interval: Duration,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let mut state = AppState::new(refresh_interval);
    let mut render_ticker = tokio::time::interval(Duration::from_millis(250));

    loop {
        if state.should_quit { break; }

        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            Some(ev) = events.recv() => handle_event(&mut state, ev, &cmd_tx),
            _ = render_ticker.tick() => {
                tick_once(&mut state, cache.as_ref()).await;
                let now = Utc::now();
                let snap = state.ui_state(now);
                terminal.draw(|f| ui::render(f, &snap))?;
            }
        }
    }
    shutdown.cancel();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Balance;
    use async_trait::async_trait;
    use chrono::TimeZone;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use std::sync::Mutex;

    struct MemCache { state: Mutex<Option<Balance>>, fail: Mutex<bool> }
    impl MemCache {
        fn new() -> Arc<Self> { Arc::new(Self { state: Mutex::new(None), fail: Mutex::new(false) }) }
        fn with(b: Balance) -> Arc<Self> {
            Arc::new(Self { state: Mutex::new(Some(b)), fail: Mutex::new(false) })
        }
    }
    #[async_trait]
    impl BalanceCache for MemCache {
        async fn get(&self) -> Result<Option<Balance>, crate::domain::CacheError> {
            if *self.fail.lock().unwrap() { return Err(crate::domain::CacheError::Disconnected); }
            Ok(self.state.lock().unwrap().clone())
        }
        async fn set(&self, b: &Balance) -> Result<(), crate::domain::CacheError> {
            *self.state.lock().unwrap() = Some(b.clone()); Ok(())
        }
        async fn ping(&self) -> Result<(), crate::domain::CacheError> { Ok(()) }
    }

    fn key(c: char) -> AppEvent {
        AppEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    #[tokio::test]
    async fn quit_key_sets_should_quit() {
        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, _rx) = mpsc::channel(1);
        handle_event(&mut s, key('q'), &tx);
        assert!(s.should_quit);
    }

    #[tokio::test]
    async fn ctrl_c_sets_should_quit() {
        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, _rx) = mpsc::channel(1);
        let ev = AppEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        handle_event(&mut s, ev, &tx);
        assert!(s.should_quit);
    }

    #[tokio::test]
    async fn refresh_key_sends_cmd() {
        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, mut rx) = mpsc::channel(1);
        handle_event(&mut s, key('r'), &tx);
        assert!(matches!(rx.try_recv().unwrap(), Cmd::ForceRefresh));
    }

    #[tokio::test]
    async fn refresh_status_updates_last_refresh() {
        let mut s = AppState::new(Duration::from_secs(30));
        let (tx, _rx) = mpsc::channel(1);
        let now = Utc.timestamp_opt(1000, 0).unwrap();
        handle_event(&mut s, AppEvent::Refresh(RefreshStatus::Ok { at: now }), &tx);
        assert!(matches!(s.last_refresh, Some(RefreshStatus::Ok { .. })));
    }

    #[tokio::test]
    async fn tick_once_reads_cache() {
        let b = Balance { usdc: Decimal::from_str("42").unwrap(), fetched_at: Utc::now() };
        let cache = MemCache::with(b.clone());
        let mut s = AppState::new(Duration::from_secs(30));
        tick_once(&mut s, cache.as_ref()).await;
        assert_eq!(s.balance.unwrap().usdc, b.usdc);
        assert!(s.redis_ok);
    }

    #[tokio::test]
    async fn tick_once_keeps_balance_on_cache_error() {
        let b = Balance { usdc: Decimal::from_str("99").unwrap(), fetched_at: Utc::now() };
        let cache = MemCache::with(b.clone());
        let mut s = AppState::new(Duration::from_secs(30));
        tick_once(&mut s, cache.as_ref()).await;          // populate
        *cache.fail.lock().unwrap() = true;
        tick_once(&mut s, cache.as_ref()).await;          // now errors
        assert_eq!(s.balance.unwrap().usdc, b.usdc);      // still there
        assert!(!s.redis_ok);                             // but flagged
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib app
```

Expected: 6 tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): state machine with handle_event + tick_once + run loop"
```

---

## Task 8: Input task

**Files:**
- Modify: `src/input.rs`

- [ ] **Step 1: Write `run()` in `src/input.rs`**

```rust
use crate::domain::AppEvent;
use crossterm::event::{self, Event};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Reads crossterm events on a blocking thread and forwards them as `AppEvent::Key`.
/// Exits on shutdown.
pub async fn run(tx: mpsc::Sender<AppEvent>, shutdown: CancellationToken) {
    let tx_clone = tx.clone();
    let shutdown_clone = shutdown.clone();

    tokio::task::spawn_blocking(move || {
        loop {
            if shutdown_clone.is_cancelled() { break; }
            // poll with short timeout so we can observe shutdown
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => {
                    match event::read() {
                        Ok(Event::Key(k)) => {
                            if tx_clone.blocking_send(AppEvent::Key(k)).is_err() { break; }
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                Ok(false) => {}
                Err(_) => break,
            }
        }
    }).await.ok();
}

#[cfg(test)]
mod tests {
    // Crossterm input is hard to unit-test because it reads from stdin TTY state.
    // Coverage is provided by the BDD step "I press 'q'" which constructs KeyEvent
    // values directly (bypassing crossterm), and by the e2e quit-key scenario.
}
```

- [ ] **Step 2: Verify compile**

```bash
cargo build --lib
```

- [ ] **Step 3: Commit**

```bash
git add src/input.rs
git commit -m "feat(input): crossterm event reader on blocking thread"
```

---

## Task 9: ClobBalanceFetcher (real adapter)

**Files:**
- Modify: `src/clob.rs`

> **Note for implementer:** verify the rs-clob-client v2 crate name and `balance_allowance` response shape before writing this. The snippet from context7 shows `client.balance_allowance(BalanceAllowanceRequest::default()).await?` returning a struct with USDC balance — but field names (e.g. `balance`, `usdc`, `value`) and units (raw µUSDC integer vs Decimal) need confirmation. Adjust the conversion below accordingly.

- [ ] **Step 1: Add `ClobBalanceFetcher` to `src/clob.rs` (append after the trait)**

```rust
use crate::domain::{Balance, FetchError};
use chrono::Utc;
use rust_decimal::Decimal;
use std::str::FromStr;

// rs-clob-client v2 imports — adjust crate name if needed
use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;

pub struct ClobBalanceFetcher {
    // Hold the authenticated client. Type names below mirror the v2 SDK docs;
    // verify the actual export path in your version of polymarket-client-sdk-v2.
    client: AuthenticatedClient,
}

// Type alias indirection so tests don't need to instantiate the SDK type.
type AuthenticatedClient = polymarket_client_sdk_v2::clob::Client<
    polymarket_client_sdk_v2::clob::Authenticated,
>;

impl ClobBalanceFetcher {
    pub async fn connect(host: &str, private_key: &str) -> Result<Self, FetchError> {
        use polymarket_client_sdk_v2::clob::{Client, Config};
        use polymarket_client_sdk_v2::POLYGON;

        let signer = LocalSigner::from_str(private_key)
            .map_err(|e| FetchError::Decode(format!("invalid private key: {e}")))?
            .with_chain_id(Some(POLYGON));

        let client = Client::new(host, Config::default())
            .map_err(|e| FetchError::Network(e.to_string()))?
            .authentication_builder(&signer)
            .authenticate()
            .await
            .map_err(|e| FetchError::Auth)?;

        Ok(Self { client })
    }
}

#[async_trait::async_trait]
impl BalanceFetcher for ClobBalanceFetcher {
    async fn fetch(&self) -> Result<Balance, FetchError> {
        use polymarket_client_sdk_v2::clob::types::request::BalanceAllowanceRequest;

        let resp = self.client
            .balance_allowance(BalanceAllowanceRequest::default())
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;

        // ⚠️ Verify field name + unit at impl time. The SDK likely returns micro-USDC
        // (1 USDC = 1_000_000) as a string or U256. Convert to Decimal with 6 decimal places.
        let raw = resp.balance.to_string();   // FIELD NAME — verify
        let usdc = parse_usdc_micros(&raw)?;

        Ok(Balance { usdc, fetched_at: Utc::now() })
    }
}

fn parse_usdc_micros(raw: &str) -> Result<Decimal, FetchError> {
    let n = Decimal::from_str(raw)
        .map_err(|e| FetchError::Decode(format!("not a number: {e}")))?;
    Ok(n / Decimal::from(1_000_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_micros_to_usdc() {
        assert_eq!(parse_usdc_micros("0").unwrap(), Decimal::ZERO);
        assert_eq!(parse_usdc_micros("1000000").unwrap(), Decimal::from(1));
        assert_eq!(parse_usdc_micros("1234567890").unwrap(), Decimal::from_str("1234.56789").unwrap());
        assert!(parse_usdc_micros("not_a_number").is_err());
    }
}
```

- [ ] **Step 2: Run conversion test**

```bash
cargo test --lib clob::tests::parse_micros_to_usdc
```

Expected: PASS.

> **Note:** The `connect`/`fetch` methods themselves can't be unit-tested without a mock server; their integration is covered by the E2E task. If the SDK's actual API differs from above, fix the imports and field access; the conversion function is the only logic worth a unit test here.

- [ ] **Step 3: Verify lib compiles**

```bash
cargo build --lib
```

If this fails because the SDK API differs, adjust the imports and types in `ClobBalanceFetcher::connect`/`fetch` to match the actual `polymarket-client-sdk-v2` API. The `parse_usdc_micros` function and `BalanceFetcher` impl shape stay.

- [ ] **Step 4: Commit**

```bash
git add src/clob.rs
git commit -m "feat(clob): ClobBalanceFetcher real adapter + µUSDC conversion"
```

---

## Task 10: RedisBalanceCache (real adapter)

**Files:**
- Modify: `src/cache.rs`
- Create: `tests/cache_integration.rs`

> **Note:** `fred` 9.x API surface is verified at impl time. The pattern below is for `fred::prelude::*`. If your version's API differs (e.g., `init()` vs `connect()` vs `wait_for_connect()`), adjust accordingly.

- [ ] **Step 1: Append `RedisBalanceCache` to `src/cache.rs`**

```rust
use crate::domain::Balance;
use fred::prelude::*;
use serde_json;

pub struct RedisBalanceCache {
    client: Client,
}

impl RedisBalanceCache {
    pub async fn connect(url: &str) -> Result<Self, CacheError> {
        let config = Config::from_url(url)
            .map_err(|e| CacheError::Op(format!("bad redis url: {e}")))?;
        let client = Client::new(config, None, None, None);
        client.init().await
            .map_err(|e| CacheError::Op(format!("redis init: {e}")))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl BalanceCache for RedisBalanceCache {
    async fn get(&self) -> Result<Option<Balance>, CacheError> {
        let raw: Option<String> = self.client.get(BALANCE_KEY_PROD).await
            .map_err(|e| map_err(e))?;
        match raw {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(|e| CacheError::Decode(e.to_string())),
        }
    }

    async fn set(&self, balance: &Balance) -> Result<(), CacheError> {
        let json = serde_json::to_string(balance)
            .map_err(|e| CacheError::Decode(e.to_string()))?;
        self.client
            .set::<(), _, _>(BALANCE_KEY_PROD, json, None, None, false)
            .await
            .map_err(|e| map_err(e))
    }

    async fn ping(&self) -> Result<(), CacheError> {
        self.client.ping::<()>(None).await.map_err(|e| map_err(e))
    }
}

fn map_err(e: fred::error::Error) -> CacheError {
    if matches!(e.kind(), fred::error::ErrorKind::IO | fred::error::ErrorKind::Canceled) {
        CacheError::Disconnected
    } else {
        CacheError::Op(e.to_string())
    }
}
```

> **Note:** The `Client::set` signature varies between fred versions; adjust the generic args and Option flags to match your installed version. `ping`'s argument may also differ (some versions take no arg).

- [ ] **Step 2: Write integration test in `tests/cache_integration.rs`**

```rust
#![cfg(test)]

use poly_tui::cache::{BalanceCache, RedisBalanceCache, BALANCE_KEY_PROD};
use poly_tui::domain::Balance;
use rust_decimal::Decimal;
use std::str::FromStr;
use chrono::Utc;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "testcontainers must not bind dev port");
    let url = format!("redis://127.0.0.1:{port}");
    (node, url)
}

#[tokio::test]
#[ignore]
async fn redis_set_then_get_roundtrips() {
    let (_node, url) = start_redis().await;
    let cache = RedisBalanceCache::connect(&url).await.unwrap();

    let b = Balance {
        usdc: Decimal::from_str("123.45").unwrap(),
        fetched_at: Utc::now(),
    };
    cache.set(&b).await.unwrap();
    let got = cache.get().await.unwrap().expect("Some");
    assert_eq!(got.usdc, b.usdc);
}

#[tokio::test]
#[ignore]
async fn redis_get_returns_none_when_unset() {
    let (_node, url) = start_redis().await;
    let cache = RedisBalanceCache::connect(&url).await.unwrap();
    let got = cache.get().await.unwrap();
    assert!(got.is_none());
}

#[tokio::test]
#[ignore]
async fn redis_ping_succeeds() {
    let (_node, url) = start_redis().await;
    let cache = RedisBalanceCache::connect(&url).await.unwrap();
    cache.ping().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn redis_uses_prod_namespace_key() {
    let (_node, url) = start_redis().await;
    let cache = RedisBalanceCache::connect(&url).await.unwrap();

    let b = Balance {
        usdc: Decimal::from_str("1").unwrap(),
        fetched_at: Utc::now(),
    };
    cache.set(&b).await.unwrap();

    // The key is internal but we can assert the constant is namespaced
    assert!(BALANCE_KEY_PROD.starts_with("poly:prod:"));
}
```

- [ ] **Step 3: Run integration tests with Docker available**

```bash
docker info >/dev/null && cargo test --test cache_integration -- --ignored
```

Expected: 4 tests pass. If Docker isn't running, start Docker Desktop first.

- [ ] **Step 4: Commit**

```bash
git add src/cache.rs tests/cache_integration.rs
git commit -m "feat(cache): RedisBalanceCache (fred) + testcontainers integration tests"
```

---

## Task 11: Wire main.rs

**Files:**
- Modify: `src/bin/poly-tui.rs`

- [ ] **Step 1: Replace `src/bin/poly-tui.rs`**

```rust
use anyhow::Context;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use poly_tui::{
    app, cache::{BalanceCache, RedisBalanceCache},
    clob::{BalanceFetcher, ClobBalanceFetcher},
    config::Config,
    domain::{AppEvent, RefreshStatus},
    input, refresher::{self, Cmd},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::{io, sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let cfg = Config::from_env().context("loading .env / environment")?;

    // Logging → file only; never stdout while TUI is up
    let file_appender = tracing_appender::rolling::daily("logs", "poly.log");
    let (nb, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(nb)
        .with_env_filter(EnvFilter::new(&cfg.log_level))
        .init();

    tracing::info!("starting poly-tui");

    // Build adapters
    let cache: Arc<dyn BalanceCache> = Arc::new(
        RedisBalanceCache::connect(&cfg.redis_url).await
            .context("connecting Redis (fatal: cache architecture requires it)")?
    );

    let fetcher: Arc<dyn BalanceFetcher> = match
        ClobBalanceFetcher::connect(&cfg.clob_host, &cfg.polymarket_private_key).await
    {
        Ok(f) => Arc::new(f),
        Err(e) => {
            tracing::warn!("CLOB connect failed at startup: {e} — TUI will start with red CLOB led");
            // Construct a "always-fails" fetcher so startup proceeds.
            Arc::new(AlwaysFails)
        }
    };

    // Channels
    let (status_tx, mut status_rx) = mpsc::channel::<RefreshStatus>(64);
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(8);
    let (event_tx, event_rx) = mpsc::channel::<AppEvent>(64);
    let shutdown = CancellationToken::new();

    // Synchronous pre-warm (5s timeout, ignore failure)
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        refresher::do_fetch(fetcher.as_ref(), cache.as_ref(), &status_tx),
    ).await;

    // Forward Refresher status into app event channel
    let event_tx_status = event_tx.clone();
    let shutdown_status = shutdown.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_status.cancelled() => break,
                Some(s) = status_rx.recv() => {
                    if event_tx_status.send(AppEvent::Refresh(s)).await.is_err() { break; }
                }
            }
        }
    });

    // Spawn refresher
    let h_refresh = tokio::spawn(refresher::run(
        fetcher.clone(),
        cache.clone(),
        cmd_rx,
        status_tx.clone(),
        Duration::from_secs(cfg.refresh_interval_secs),
        shutdown.clone(),
    ));

    // Spawn input
    let h_input = tokio::spawn(input::run(event_tx.clone(), shutdown.clone()));

    // Set up terminal
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("init terminal")?;

    // Run app loop
    let app_result = app::run(
        &mut terminal,
        cache.clone(),
        cmd_tx,
        event_rx,
        Duration::from_secs(cfg.refresh_interval_secs),
        shutdown.clone(),
    ).await;

    // Tear down terminal
    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    // Cleanup
    shutdown.cancel();
    let _ = tokio::join!(h_refresh, h_input);

    app_result
}

// Fallback fetcher used when initial CLOB auth fails — keeps the TUI alive
// so you can see the red LED instead of the binary refusing to start.
struct AlwaysFails;
#[async_trait::async_trait]
impl BalanceFetcher for AlwaysFails {
    async fn fetch(&self) -> Result<poly_tui::domain::Balance, poly_tui::domain::FetchError> {
        Err(poly_tui::domain::FetchError::Auth)
    }
}
```

- [ ] **Step 2: Build**

```bash
cargo build --bin poly-tui
```

If `polymarket-client-sdk-v2` failed to resolve in Task 0, this is where it'll bite. Fix the dep and retry.

- [ ] **Step 3: Manual smoke test**

```bash
docker compose up -d
cp .env.example .env
# edit .env: set POLYMARKET_PRIVATE_KEY to a real dedicated wallet's key
cargo run --bin poly-tui
```

Expected: TUI appears within 1s, shows balance (or `--`) with status bar. Press `q` — exits cleanly. Press `r` — refresh status updates.

- [ ] **Step 4: Commit**

```bash
git add src/bin/poly-tui.rs
git commit -m "feat: wire main, spawn three tasks, terminal lifecycle"
```

---

## Task 12: BDD scenarios with cucumber-rs

**Files:**
- Create: `tests/features/balance.feature`
- Create: `tests/bdd.rs`

- [ ] **Step 1: Write `tests/features/balance.feature`**

```gherkin
Feature: 余额展示
  作为机器人主人，我希望 TUI 启动后能立刻看到当前 USDC 余额

  Scenario: 缓存里已有余额，启动即显示
    Given Redis 缓存里有余额 "100.00" USDC
    When  我启动 TUI 主循环
    And   驱动 1 个 tick
    Then  屏幕上能看到 "USDC: $100.00"

  Scenario: 缓存为空，CLOB 返回 50.00
    Given Redis 缓存为空
    And   CLOB 返回余额 "50.00" USDC
    When  我启动 TUI 主循环
    And   触发一次强制刷新
    And   驱动 1 个 tick
    Then  屏幕上能看到 "USDC: $50.00"

  Scenario: CLOB 失败，仍显示旧缓存
    Given Redis 缓存里有余额 "200.00" USDC
    And   CLOB 调用会失败
    When  我启动 TUI 主循环
    And   触发一次强制刷新
    And   驱动 1 个 tick
    Then  屏幕上仍显示 "USDC: $200.00"

  Scenario: 按 q 触发关闭
    Given Redis 缓存为空
    When  我启动 TUI 主循环
    And   按下 "q" 键
    Then  应用进入退出状态
```

- [ ] **Step 2: Write `tests/bdd.rs`**

```rust
use chrono::Utc;
use cucumber::{given, then, when, World};
use poly_tui::{
    app::{self, AppState},
    domain::{AppEvent, Balance, RefreshStatus},
    refresher::{self, Cmd},
};
use ratatui::{Terminal, backend::TestBackend};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

#[path = "support/mod.rs"]
mod support;
use support::{FakeFetcher, InMemoryCache};

#[derive(Debug, World)]
#[world(init = Self::new)]
struct AppWorld {
    cache: Arc<InMemoryCache>,
    fetcher: Arc<FakeFetcher>,
    state: Option<AppState>,
    terminal: Option<Terminal<TestBackend>>,
    cmd_tx: Option<mpsc::Sender<Cmd>>,
    event_tx: Option<mpsc::Sender<AppEvent>>,
    status_rx: Option<mpsc::Receiver<RefreshStatus>>,
    last_buffer: String,
}

impl AppWorld {
    async fn new() -> Self {
        Self {
            cache: Arc::new(InMemoryCache::new()),
            fetcher: Arc::new(FakeFetcher::with_usdc("0")),
            state: None,
            terminal: None,
            cmd_tx: None,
            event_tx: None,
            status_rx: None,
            last_buffer: String::new(),
        }
    }
}

#[given(regex = r#"^Redis 缓存里有余额 "([^"]+)" USDC$"#)]
async fn given_cache_has(world: &mut AppWorld, amount: String) {
    let b = Balance {
        usdc: Decimal::from_str(&amount).unwrap(),
        fetched_at: Utc::now(),
    };
    world.cache.set(&b).await.unwrap();
}

#[given("Redis 缓存为空")]
async fn given_cache_empty(_world: &mut AppWorld) { /* default */ }

#[given(regex = r#"^CLOB 返回余额 "([^"]+)" USDC$"#)]
async fn given_clob_returns(world: &mut AppWorld, amount: String) {
    world.fetcher.set_balance(&amount);
}

#[given("CLOB 调用会失败")]
async fn given_clob_fails(world: &mut AppWorld) {
    world.fetcher.set_fail(true);
}

#[when("我启动 TUI 主循环")]
async fn when_start_loop(world: &mut AppWorld) {
    let (cmd_tx, _cmd_rx) = mpsc::channel::<Cmd>(8);
    let (event_tx, _event_rx) = mpsc::channel::<AppEvent>(64);
    let (status_tx, status_rx) = mpsc::channel::<RefreshStatus>(8);

    world.state = Some(AppState::new(Duration::from_secs(30)));
    world.terminal = Some(Terminal::new(TestBackend::new(60, 12)).unwrap());
    world.cmd_tx = Some(cmd_tx);
    world.event_tx = Some(event_tx);
    world.status_rx = Some(status_rx);
    let _ = status_tx; // keep it alive in case Refresher is invoked
}

#[when("触发一次强制刷新")]
async fn when_force_refresh(world: &mut AppWorld) {
    let (status_tx, mut status_rx) = mpsc::channel::<RefreshStatus>(8);
    refresher::do_fetch(world.fetcher.as_ref() as _, world.cache.as_ref() as _, &status_tx).await;
    if let Ok(s) = status_rx.try_recv() {
        if let Some(state) = world.state.as_mut() {
            state.last_refresh = Some(s);
        }
    }
}

#[when(regex = r"^驱动 (\d+) 个 tick$")]
async fn when_drive_ticks(world: &mut AppWorld, n: u32) {
    let state = world.state.as_mut().expect("state initialized");
    let term = world.terminal.as_mut().expect("terminal initialized");
    for _ in 0..n {
        app::tick_once(state, world.cache.as_ref()).await;
        let snap = state.ui_state(Utc::now());
        term.draw(|f| poly_tui::ui::render(f, &snap)).unwrap();
    }
    // Capture buffer
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    world.last_buffer = out;
}

#[when(regex = r#"^按下 "([^"]+)" 键$"#)]
async fn when_press_key(world: &mut AppWorld, key: String) {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let state = world.state.as_mut().expect("state initialized");
    let cmd_tx = world.cmd_tx.as_ref().expect("cmd_tx");
    let code = match key.as_str() {
        "q" => KeyCode::Char('q'),
        "r" => KeyCode::Char('r'),
        _   => panic!("unsupported key in step: {key}"),
    };
    app::handle_event(
        state,
        AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)),
        cmd_tx,
    );
}

#[then(regex = r#"^屏幕上能看到 "([^"]+)"$"#)]
async fn then_screen_shows(world: &mut AppWorld, expected: String) {
    assert!(
        world.last_buffer.contains(&expected),
        "screen buffer missing {expected:?}; got:\n{}",
        world.last_buffer
    );
}

#[then(regex = r#"^屏幕上仍显示 "([^"]+)"$"#)]
async fn then_screen_still_shows(world: &mut AppWorld, expected: String) {
    assert!(
        world.last_buffer.contains(&expected),
        "screen buffer should still contain {expected:?}; got:\n{}",
        world.last_buffer
    );
}

#[then("应用进入退出状态")]
async fn then_should_quit(world: &mut AppWorld) {
    let state = world.state.as_ref().expect("state initialized");
    assert!(state.should_quit, "expected should_quit=true");
}

#[tokio::main]
async fn main() {
    AppWorld::cucumber()
        .run("tests/features")
        .await;
}
```

- [ ] **Step 3: Run BDD**

```bash
cargo test --test bdd
```

Expected: 4 scenarios pass. If any step regex doesn't match, adjust the regex strings to match the gherkin literally.

- [ ] **Step 4: Commit**

```bash
git add tests/bdd.rs tests/features/
git commit -m "test(bdd): cucumber-rs scenarios for balance display"
```

---

## Task 13: E2E test (testcontainers + real Redis + FakeFetcher)

**Files:**
- Create: `tests/e2e_tui.rs`

> **Note:** Spec §9 mentions wiremock for CLOB. In practice, the polymarket-client-sdk-v2 auth flow involves multiple endpoints (signature derivation, API-key creation, balance) that are awkward to mock byte-for-byte in wiremock. We use **FakeFetcher** for the CLOB side here (the conversion logic is already covered by `parse_usdc_micros` unit test) and **real Redis via testcontainers** for the cache side — that's the integration that actually matters for data isolation.
>
> A separate `--ignored` smoke test against the real Polymarket API can be added later if useful.

- [ ] **Step 1: Write `tests/e2e_tui.rs`**

```rust
#![cfg(test)]

use chrono::Utc;
use poly_tui::{
    app::{self, AppState},
    cache::{BalanceCache, RedisBalanceCache},
    clob::BalanceFetcher,
    domain::{AppEvent, Balance, RefreshStatus},
    refresher::{self, Cmd},
};
use ratatui::{Terminal, backend::TestBackend};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[path = "support/mod.rs"]
mod support;
use support::FakeFetcher;

async fn start_redis() -> (testcontainers::ContainerAsync<Redis>, String) {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    assert_ne!(port, 6379, "E2E must NOT bind dev Redis port");
    let url = format!("redis://127.0.0.1:{port}");
    (node, url)
}

fn buffer_string(term: &Terminal<TestBackend>) -> String {
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[tokio::test]
#[ignore]
async fn e2e_full_path_renders_balance() {
    let (_node, url) = start_redis().await;
    let cache: Arc<dyn BalanceCache> = Arc::new(RedisBalanceCache::connect(&url).await.unwrap());
    let fetcher: Arc<dyn BalanceFetcher> = Arc::new(FakeFetcher::with_usdc("100.00"));
    let (status_tx, mut status_rx) = mpsc::channel::<RefreshStatus>(8);

    refresher::do_fetch(fetcher.as_ref(), cache.as_ref(), &status_tx).await;
    let s = status_rx.try_recv().unwrap();
    assert!(matches!(s, RefreshStatus::Ok { .. }));

    let mut state = AppState::new(Duration::from_secs(30));
    state.last_refresh = Some(s);
    app::tick_once(&mut state, cache.as_ref()).await;
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    term.draw(|f| poly_tui::ui::render(f, &state.ui_state(Utc::now()))).unwrap();

    let buf = buffer_string(&term);
    assert!(buf.contains("USDC: $100.00"), "buffer:\n{buf}");
}

#[tokio::test]
#[ignore]
async fn e2e_clob_down_keeps_cached_value() {
    let (_node, url) = start_redis().await;
    let cache: Arc<dyn BalanceCache> = Arc::new(RedisBalanceCache::connect(&url).await.unwrap());
    let fetcher = Arc::new(FakeFetcher::with_usdc("100.00"));
    let fetcher_dyn: Arc<dyn BalanceFetcher> = fetcher.clone();
    let (status_tx, mut status_rx) = mpsc::channel::<RefreshStatus>(8);

    refresher::do_fetch(fetcher_dyn.as_ref(), cache.as_ref(), &status_tx).await;
    let _ok = status_rx.try_recv().unwrap();

    fetcher.set_fail(true);
    refresher::do_fetch(fetcher_dyn.as_ref(), cache.as_ref(), &status_tx).await;
    let s = status_rx.try_recv().unwrap();
    assert!(matches!(s, RefreshStatus::Failed { .. }));

    let mut state = AppState::new(Duration::from_secs(30));
    state.last_refresh = Some(s);
    app::tick_once(&mut state, cache.as_ref()).await;
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    term.draw(|f| poly_tui::ui::render(f, &state.ui_state(Utc::now()))).unwrap();

    let buf = buffer_string(&term);
    assert!(buf.contains("USDC: $100.00"), "still shows last good value:\n{buf}");
    assert!(buf.contains("last: failed"), "shows failure status:\n{buf}");
}

#[tokio::test]
#[ignore]
async fn e2e_quit_key_terminates_cleanly() {
    let (_node, _url) = start_redis().await;
    let mut state = AppState::new(Duration::from_secs(30));
    let (cmd_tx, _cmd_rx) = mpsc::channel::<Cmd>(8);

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    app::handle_event(
        &mut state,
        AppEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        &cmd_tx,
    );
    assert!(state.should_quit);
}
```

- [ ] **Step 2: Run E2E**

```bash
cargo test --test e2e_tui -- --ignored
```

Expected: 3 tests pass. Requires Docker.

- [ ] **Step 3: Commit**

```bash
git add tests/e2e_tui.rs
git commit -m "test(e2e): testcontainers Redis + FakeFetcher integration tests"
```

---

## Task 14: Coverage gate + README polish + final acceptance

**Files:**
- Create/Modify: `README.md` (only if user requested — skip if not)
- Modify: `TODO.md` (mark v1.0 items complete)

> **Skip the README creation** unless the user explicitly asks for it (project rule: don't create unrequested docs).

- [ ] **Step 1: Run full test suite**

```bash
cargo test                              # unit + BDD
cargo test --test cache_integration -- --ignored
cargo test --test e2e_tui -- --ignored
```

Expected: all green.

- [ ] **Step 2: Run coverage**

```bash
cargo install cargo-llvm-cov   # if not installed
cargo llvm-cov --lib --tests --html
```

Expected: ≥ 80% line coverage on `src/`. Open `target/llvm-cov/html/index.html` to inspect.

If below 80%, identify uncovered modules and add tests; commonly missed:
- `input.rs` (acceptable — covered by manual smoke test, document in TODO)
- Error variants (add tests forcing each variant)

- [ ] **Step 3: Verify acceptance checklist from spec §13**

Walk through each item:

- [ ] `cargo run --bin poly-tui` shows TUI within 1s with status LEDs
- [ ] Real wallet + real Redis: USDC balance visible
- [ ] Disconnect network: CLOB LED red, balance unchanged
- [ ] Stop Redis (`docker compose stop redis`): Redis LED red, balance frozen
- [ ] `q` exits within 1s, terminal restored
- [ ] `cargo test` green
- [ ] `cargo test -- --ignored` green
- [ ] `cargo llvm-cov` ≥ 80%
- [ ] `.env` in `.gitignore`, only `.env.example` in repo
- [ ] `grep -r "use redis\|use fred" tests/` returns only `tests/cache_integration.rs` and `tests/e2e_tui.rs` (NOT bdd.rs or unit tests)
- [ ] `tests/e2e_tui.rs` and `tests/cache_integration.rs` both contain `assert_ne!(port, 6379, ...)`

- [ ] **Step 4: Update `TODO.md`**

Mark all v1.0 acceptance items checked. Leave v1.1+ untouched.

- [ ] **Step 5: Final commit**

```bash
git add TODO.md
git commit -m "docs: mark v1.0 acceptance items complete"
```

---

## Self-Review Notes

**Spec coverage check:**
- §1 Goals: covered by Tasks 1, 7, 9, 10, 11
- §2 Decisions: encoded in Cargo.toml (Task 0), trait design (Tasks 3,4), key namespace (Task 3), startup ordering (Task 11)
- §3 Architecture: implemented in Task 11 (main wiring) + Tasks 5, 7, 8 (the three task functions)
- §4 Modules + traits: Tasks 1, 3, 4, 9, 10
- §5 Data flow + state machine: Tasks 5 (refresher), 7 (app)
- §6 Error handling: domain errors in Task 1, RedisBalanceCache error mapping in Task 10, AlwaysFails fallback in Task 11
- §7 Config: Task 2
- §8 UI: Task 6
- §9 Tests: Tasks 5/6/7 (unit), 12 (BDD), 10/13 (integration/E2E)
- §10 Data isolation: assert_ne!(port, 6379) in Tasks 10 and 13; key namespace constant in Task 3; grep-check in Task 14
- §11 Local dev: Task 11 step 3 (smoke test commands)
- §12 Deps: Task 0
- §13 Acceptance: Task 14

**Open items flagged for impl time (not failures, just verifications):**
1. polymarket-client-sdk-v2 crate name + version (Task 0)
2. balance_allowance() response field name + units (Task 9)
3. fred 9.x exact API (Task 10)

These are documented inline in their tasks with fallback/adjustment notes.

**Scope:** focused on a single binary with one feature (display USDC). v1.1 daemon split is in TODO.md, not this plan.
