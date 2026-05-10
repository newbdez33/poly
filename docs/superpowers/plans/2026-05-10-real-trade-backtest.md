# v1.7.5 — Real Polymarket Trade-History Backtest Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the v1.7.2 BS+noise oracle for two new strategy candidates (`12_tp75_early_exit_270`, `13_hold_early_exit_270`) with **real Polymarket trade prices**. Both candidates exit BEFORE window resolution at t=270s to avoid the on-chain redemption blocker. Output decides v1.8 direction.

**Architecture:** New `RealTradeOracle` joins `BlackScholesOracle` and `NoisyBlackScholesOracle` behind a new `--oracle bs|noisy|real` flag. Trade fetch pipeline (using SDK's `data::Client::trades()`) paginates per market, caches per-window JSON in `~/.poly-backtest-cache/trades/`. `WindowMeta` gains an optional `condition_id` for the trades fetch. Two new strategies appended to `strategy_set()`. New `ExitRule::TpOnlyOrEarlyExit { tp_price, exit_at_secs }` variant.

**Tech Stack:** Existing — `polymarket_client_sdk_v2` (`data` feature), `tokio`, `rust_decimal`, `serde`, `clap`. No new deps.

**Spec:** `docs/superpowers/specs/2026-05-10-real-trade-backtest-design.md`

## Build hygiene — STRICT

NEVER bare `cargo build`. Always scope:
- `cargo build --bin poly-backtest`
- `cargo test --lib backtest::`
- `cargo build --tests --test backtest_smoke`

DO NOT touch `src/trader/`, `src/positions.rs`, `src/bin/poly-tui.rs`, `src/bin/poly-trader.rs`, `src/bin/poly-redeem.rs`. v1.7.5 is **backtest-only**.

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `src/backtest/data/gamma_history.rs` | modify | Add optional `condition_id: Option<String>` field to `WindowMeta` (`#[serde(default)]` for back-compat). Decode `markets[0].conditionId` into it. |
| `src/backtest/data/trades.rs` | **NEW** | `Trade`, `TradeSide`, `Outcome` types; `TradeFetcher` trait; `PolymarketTradeFetcher` (SDK-backed paginator); `CachedTradeStore` (per-window JSON cache). |
| `src/backtest/data/mod.rs` | modify | Add `pub mod trades;`. |
| `src/backtest/oracle.rs` | modify | Add `RealTradeOracle` struct + `impl TokenPriceOracle`. `PRE_TRADE_FALLBACK = dec!(0.5)` constant. |
| `src/backtest/exit_rule.rs` | modify | Add arm for new `TpOnlyOrEarlyExit { tp_price, exit_at_secs }` variant in `simulate_window`. |
| `src/backtest/config.rs` | modify | Add `OracleKind { Bs, Noisy, Real }` enum + `--oracle` flag (default `bs`). Add new `ExitRule::TpOnlyOrEarlyExit` variant. Append strategies 12 + 13 to `strategy_set()`. |
| `src/bin/poly-backtest.rs` | modify | Dispatch on `args.oracle`. For `Real`: pre-fetch all trades (auto-cache), build `RealTradeOracle`. |
| `tests/real_trade_backtest.rs` | **NEW** (`#[ignore]`) | Fixture-driven end-to-end run with `--oracle real` against pre-populated cache. |
| `README.md` | modify | Document `--oracle` flag, new strategies, fetch operator workflow. |
| `TODO.md` | modify | Tick v1.7.5 ✅ COMPLETE; record decision criteria for v1.8. |

No new dependencies.

---

## Task 0: Sanity baseline

**Files:** none (read-only).

- [ ] **Step 1: Confirm working tree clean**

Run: `git status`
Expected: working tree matches HEAD; only untracked items are `.claude/` and any historical `backtest-report*.html`.

- [ ] **Step 2: Confirm backtest tests green**

Run: `cargo test --lib backtest::`
Expected: PASS. Record the test count — later tasks add tests on top.

- [ ] **Step 3: Confirm `data` feature of SDK is enabled**

Run: `grep 'polymarket_client_sdk_v2' Cargo.toml`
Expected: line includes `features = ["clob", "data", "ctf"]`. If not — STOP and escalate (the spec assumes the feature is on).

---

## Task 1: WindowMeta gains optional `condition_id`

**Files:**
- Modify: `src/backtest/data/gamma_history.rs`

The trade-history fetch path needs the per-window `conditionId` to filter trades by market. We add it as `Option<String>` with `#[serde(default)]` so existing cached `WindowMeta` JSON files continue to deserialize (they get `None`). The BS path ignores the field entirely; only the real-oracle path consults it.

- [ ] **Step 1: Write the failing test**

Append to `mod tests` in `src/backtest/data/gamma_history.rs`:

```rust
    #[test]
    fn decode_extracts_condition_id() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78, "finalPrice": 80450.0},
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"resolved",
                "conditionId":"0x16b6deeed0603035fe1fab25c868f60fc5e7ac5e761dd4a15d34eb897dbbfa49",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"1\",\"0\"]"
            }]
        }]"#;
        let m = decode_window_meta(json, 1700000000).unwrap().unwrap();
        assert_eq!(
            m.condition_id.as_deref(),
            Some("0x16b6deeed0603035fe1fab25c868f60fc5e7ac5e761dd4a15d34eb897dbbfa49")
        );
    }

    #[test]
    fn decode_missing_condition_id_returns_none() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78, "finalPrice": 80450.0},
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"resolved",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"1\",\"0\"]"
            }]
        }]"#;
        let m = decode_window_meta(json, 1700000000).unwrap().unwrap();
        assert!(m.condition_id.is_none());
    }

    #[test]
    fn windowmeta_deserializes_legacy_json_without_condition_id() {
        // Older cache files don't have condition_id — must default to None.
        let json = r#"{
            "window_ts": 1700000000,
            "price_to_beat": "80424.78",
            "final_price": "80450",
            "winner": "Up"
        }"#;
        let m: WindowMeta = serde_json::from_str(json).unwrap();
        assert!(m.condition_id.is_none());
        assert_eq!(m.window_ts, 1700000000);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backtest::data::gamma_history --no-fail-fast`
Expected: 3 NEW failures: `decode_extracts_condition_id`, `decode_missing_condition_id_returns_none`, `windowmeta_deserializes_legacy_json_without_condition_id` — fail to compile (`condition_id` field missing).

- [ ] **Step 3: Add the field to `WindowMeta`**

Edit `src/backtest/data/gamma_history.rs`. Locate the `WindowMeta` struct and add the field:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowMeta {
    pub window_ts: i64,
    pub price_to_beat: Decimal,
    pub final_price: Option<Decimal>,
    pub winner: Option<Direction>,
    /// Hex-prefixed market condition_id ("0x..."). Optional for back-compat
    /// with cached JSON written before v1.7.5 — those deserialize with None
    /// here. The `--oracle real` path skips windows where this is None.
    #[serde(default)]
    pub condition_id: Option<String>,
}
```

- [ ] **Step 4: Decode `conditionId` in `decode_window_meta`**

In the same file, inside the `let market = ...` block (after extracting `market.outcomes` etc.), extract the conditionId. Put it just before the `if winner.is_none()` early-return so we capture it whenever a market exists:

```rust
    let condition_id = market.and_then(|m| {
        m.get("conditionId")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
    });
```

Then update the final `Ok(Some(WindowMeta { ... }))` to include it:

```rust
    Ok(Some(WindowMeta {
        window_ts,
        price_to_beat,
        final_price,
        winner,
        condition_id,
    }))
```

Update existing test fixtures inside this file's `mod tests` so explicit `WindowMeta { ... }` literals (none exist in this file's tests today, but check) still compile.

- [ ] **Step 5: Update fixtures elsewhere**

Search for any test that constructs `WindowMeta { ... }` explicitly:

Run: `grep -rn 'WindowMeta {' src/ tests/`
For every match (e.g. `src/backtest/oracle.rs`, `src/backtest/exit_rule.rs`, `src/backtest/runner.rs`, possibly `tests/backtest_smoke.rs`), append `condition_id: None,` to the literal. Do NOT rename or restructure the surrounding code.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib backtest:: --no-fail-fast`
Expected: all previously-passing tests still PASS, plus 3 new tests in `gamma_history::tests` PASS.

- [ ] **Step 7: Commit**

```bash
git add src/backtest/data/gamma_history.rs src/backtest/oracle.rs src/backtest/exit_rule.rs src/backtest/runner.rs tests/
git commit -m "feat(backtest): WindowMeta gains optional condition_id field"
```

---

## Task 2: Trade types + `TradeFetcher` trait

**Files:**
- Create: `src/backtest/data/trades.rs`
- Modify: `src/backtest/data/mod.rs`

Define the local types we'll use throughout the real-oracle path. Keep them serde-friendly (the cache writes them as JSON). The fetcher is a trait so we can mock it in tests.

- [ ] **Step 1: Create the file with skeleton + tests**

Create `src/backtest/data/trades.rs`:

```rust
use anyhow::Result;
use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide { Buy, Sell }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome { Up, Down }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trade {
    /// Unix seconds when the trade executed.
    pub timestamp: i64,
    pub side: TradeSide,
    pub price: Decimal,
    pub size: Decimal,
    pub outcome: Outcome,
}

#[async_trait]
pub trait TradeFetcher: Send + Sync {
    /// Fetch all trades for the given market (one Polymarket binary market =
    /// one 5-min window). Paginates internally. Returns sorted by timestamp
    /// ascending. `condition_id` is the hex-prefixed string from gamma.
    async fn fetch_window(
        &self,
        condition_id: &str,
        window_ts: i64,
    ) -> Result<Vec<Trade>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn trade_round_trips_through_json() {
        let t = Trade {
            timestamp: 1778416810,
            side: TradeSide::Buy,
            price: dec!(0.4823),
            size: dec!(100),
            outcome: Outcome::Up,
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: Trade = serde_json::from_str(&s).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn outcome_round_trips_through_json() {
        for o in [Outcome::Up, Outcome::Down] {
            let s = serde_json::to_string(&o).unwrap();
            let back: Outcome = serde_json::from_str(&s).unwrap();
            assert_eq!(o, back);
        }
    }

    #[test]
    fn trade_side_round_trips_through_json() {
        for side in [TradeSide::Buy, TradeSide::Sell] {
            let s = serde_json::to_string(&side).unwrap();
            let back: TradeSide = serde_json::from_str(&s).unwrap();
            assert_eq!(side, back);
        }
    }
}
```

The file ends at the closing `}` of `mod tests`. The unused `PathBuf` import at the top will be removed automatically when Task 4 adds `CachedTradeStore`. If `cargo build` warns about an unused import in the meantime, suppress it with `#[allow(unused_imports)]` on the `use std::path::PathBuf;` line — it'll go live in Task 4.

- [ ] **Step 2: Wire into mod.rs**

Edit `src/backtest/data/mod.rs`:

```rust
pub mod cache;
pub mod binance;
pub mod gamma_history;
pub mod loader;
pub mod trades;
```

- [ ] **Step 3: Verify `async_trait` is available**

Run: `grep 'async-trait\|async_trait' Cargo.toml`
Expected: `async-trait` already declared (it's used elsewhere in the workspace). If not, the next step adds it.

If absent, edit `Cargo.toml`:

```toml
async-trait = "0.1"
```

(Most likely already present; confirm by build.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib backtest::data::trades`
Expected: 3 PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backtest/data/trades.rs src/backtest/data/mod.rs Cargo.toml Cargo.lock
git commit -m "feat(backtest): add Trade types and TradeFetcher trait"
```

---

## Task 3: `PolymarketTradeFetcher` (SDK-backed, paginated)

**Files:**
- Modify: `src/backtest/data/trades.rs`

Implement the real fetcher. The SDK exposes `data::Client::trades(&TradesRequest)` which returns up to 10 000 trades with offset paging. We paginate until a page returns fewer than the requested limit, sleeping `throttle_ms` between calls. Return value is sorted-ascending by timestamp.

The mapping from SDK `Trade` (field types: `proxy_wallet: Address`, `side: Side`, `condition_id: B256`, `price: Decimal`, `size: Decimal`, `timestamp: i64`, `outcome: String`, ...) to local `Trade`:
- `side` → `TradeSide::Buy` / `Sell` based on `Side::Buy` / `Side::Sell`. Reject `Side::Unknown(_)` (warn + skip).
- `outcome` (case-insensitive) → `Outcome::Up` / `Outcome::Down`. Reject anything else (warn + skip).
- `price`, `size`, `timestamp` map directly.

- [ ] **Step 1: Write the failing test (mock-based pagination)**

Append to `mod tests` in `src/backtest/data/trades.rs`:

```rust
    use std::sync::{Arc, Mutex};

    /// Mock that hands back canned page sequences. Each call to `fetch_window`
    /// drains pages from `pages` in order. The mock is one-shot per
    /// (condition_id, window_ts) combo; pages-drained semantics test pagination.
    struct MockFetcher {
        pages: Mutex<Vec<Vec<Trade>>>,
        calls: Arc<Mutex<u32>>,
    }
    impl MockFetcher {
        fn new(pages: Vec<Vec<Trade>>) -> (Self, Arc<Mutex<u32>>) {
            let calls = Arc::new(Mutex::new(0));
            (Self { pages: Mutex::new(pages), calls: calls.clone() }, calls)
        }
    }
    #[async_trait]
    impl TradeFetcher for MockFetcher {
        async fn fetch_window(&self, _cid: &str, _ts: i64) -> Result<Vec<Trade>> {
            *self.calls.lock().unwrap() += 1;
            let mut all = Vec::new();
            let mut pages = self.pages.lock().unwrap();
            while let Some(p) = pages.first() {
                let n = p.len();
                all.extend(pages.remove(0));
                if n < 500 { break; }
            }
            all.sort_by_key(|t: &Trade| t.timestamp);
            Ok(all)
        }
    }

    fn make_trade(ts: i64, side: TradeSide, price: rust_decimal::Decimal) -> Trade {
        Trade { timestamp: ts, side, price, size: rust_decimal_macros::dec!(10), outcome: Outcome::Up }
    }

    #[tokio::test]
    async fn mock_fetcher_concatenates_pages() {
        // Page 1 = 500 entries (full page), Page 2 = 12 entries (partial → stop).
        let p1: Vec<Trade> = (0..500)
            .map(|i| make_trade(1000 + i as i64, TradeSide::Buy, rust_decimal_macros::dec!(0.50)))
            .collect();
        let p2: Vec<Trade> = (0..12)
            .map(|i| make_trade(1500 + i as i64, TradeSide::Sell, rust_decimal_macros::dec!(0.51)))
            .collect();
        let (mock, calls) = MockFetcher::new(vec![p1, p2]);
        let out = mock.fetch_window("0xdeadbeef", 1000).await.unwrap();
        assert_eq!(out.len(), 512);
        assert!(out.windows(2).all(|w| w[0].timestamp <= w[1].timestamp), "sorted ascending");
        assert_eq!(*calls.lock().unwrap(), 1);
    }
```

- [ ] **Step 2: Run test to verify it fails to compile**

Run: `cargo test --lib backtest::data::trades`
Expected: compile fails (no implementor of `TradeFetcher` exists yet, but the test only uses MockFetcher → should compile and PASS the mock test). If it passes, move on.

If it passes, that's fine — the mock test alone passes without the production impl. We add the production impl next.

- [ ] **Step 3: Implement `PolymarketTradeFetcher`**

Append to `src/backtest/data/trades.rs` (above `mod tests`):

```rust
use polymarket_client_sdk_v2::data::Client as SdkClient;
use polymarket_client_sdk_v2::data::types::request::TradesRequest;
use polymarket_client_sdk_v2::data::types::{MarketFilter, Side};
use polymarket_client_sdk_v2::types::B256;
use std::str::FromStr;
use std::time::Duration;

const PAGE_LIMIT: i32 = 500;

pub struct PolymarketTradeFetcher {
    client: SdkClient,
    throttle: Duration,
}

impl PolymarketTradeFetcher {
    pub fn new(throttle_ms: u64) -> Self {
        Self {
            client: SdkClient::default(),
            throttle: Duration::from_millis(throttle_ms),
        }
    }
}

#[async_trait]
impl TradeFetcher for PolymarketTradeFetcher {
    async fn fetch_window(
        &self,
        condition_id: &str,
        window_ts: i64,
    ) -> Result<Vec<Trade>> {
        let cid = B256::from_str(condition_id)
            .map_err(|e| anyhow::anyhow!("invalid condition_id {}: {}", condition_id, e))?;
        let mut all = Vec::new();
        let mut offset: i32 = 0;

        loop {
            let req = TradesRequest::builder()
                .filter(MarketFilter::markets([cid]))
                .limit(PAGE_LIMIT)
                .map_err(|e| anyhow::anyhow!("limit out of range: {e}"))?
                .offset(offset)
                .map_err(|e| anyhow::anyhow!("offset out of range: {e}"))?
                .taker_only(false)
                .build();
            let page = self.client.trades(&req).await
                .map_err(|e| anyhow::anyhow!("data-api trades error: {e}"))?;
            let n = page.len();

            for sdk in page.into_iter() {
                let side = match sdk.side {
                    Side::Buy => TradeSide::Buy,
                    Side::Sell => TradeSide::Sell,
                    _ => continue,
                };
                let outcome = match sdk.outcome.to_ascii_lowercase().as_str() {
                    "up" => Outcome::Up,
                    "down" => Outcome::Down,
                    _ => continue,
                };
                all.push(Trade {
                    timestamp: sdk.timestamp,
                    side,
                    price: sdk.price,
                    size: sdk.size,
                    outcome,
                });
            }

            if n < PAGE_LIMIT as usize { break; }
            offset += PAGE_LIMIT;
            // Defensive: SDK enforces offset ≤ 10000; abort cleanly if a single
            // window has more than that (shouldn't happen for 5-min markets).
            if offset >= 10000 {
                eprintln!(
                    "[trades] WARNING window {} hit 10k offset cap; results truncated",
                    window_ts
                );
                break;
            }
            tokio::time::sleep(self.throttle).await;
        }

        all.sort_by_key(|t| t.timestamp);
        Ok(all)
    }
}
```

- [ ] **Step 4: Run tests to verify both pass**

Run: `cargo test --lib backtest::data::trades`
Expected: 4 PASS (3 round-trip + mock pagination). The production fetcher isn't unit-tested directly (network-bound — covered in Task 9 integration).

- [ ] **Step 5: Confirm the binary still builds**

Run: `cargo build --bin poly-backtest`
Expected: clean build (one new module compiled in).

- [ ] **Step 6: Commit**

```bash
git add src/backtest/data/trades.rs
git commit -m "feat(backtest): add PolymarketTradeFetcher with paginated SDK calls"
```

---

## Task 4: `CachedTradeStore` (per-window JSON cache)

**Files:**
- Modify: `src/backtest/data/trades.rs`

Per the spec, each window's trades go to `~/.poly-backtest-cache/trades/<window_ts>.json`. We reuse the directory pattern from `DiskCache` but write a focused store: trades are large per-window arrays, and we want a thin save/load API.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/backtest/data/trades.rs`:

```rust
    use tempfile::TempDir;

    fn fixture_trades() -> Vec<Trade> {
        vec![
            make_trade(1000, TradeSide::Buy, rust_decimal_macros::dec!(0.42)),
            make_trade(1001, TradeSide::Sell, rust_decimal_macros::dec!(0.43)),
        ]
    }

    #[test]
    fn cached_trade_store_save_then_load_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let store = CachedTradeStore::new(tmp.path()).unwrap();
        let trades = fixture_trades();
        store.save(1700000000, &trades).unwrap();
        let back = store.load(1700000000).unwrap();
        assert_eq!(back, trades);
    }

    #[test]
    fn cached_trade_store_load_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        let store = CachedTradeStore::new(tmp.path()).unwrap();
        assert!(store.load(99999).is_none());
    }

    #[test]
    fn cached_trade_store_save_creates_per_window_file() {
        let tmp = TempDir::new().unwrap();
        let store = CachedTradeStore::new(tmp.path()).unwrap();
        store.save(1700000000, &fixture_trades()).unwrap();
        assert!(tmp.path().join("1700000000.json").exists());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backtest::data::trades::tests::cached_trade_store`
Expected: FAIL (`CachedTradeStore` not defined yet).

- [ ] **Step 3: Implement `CachedTradeStore`**

Append to `src/backtest/data/trades.rs` (just below `PolymarketTradeFetcher` impl, above `mod tests`):

```rust
use std::path::Path;

pub struct CachedTradeStore {
    root: PathBuf,
}

impl CachedTradeStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|e| anyhow::anyhow!("creating cache dir {}: {e}", root.display()))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, window_ts: i64) -> PathBuf {
        self.root.join(format!("{window_ts}.json"))
    }

    pub fn load(&self, window_ts: i64) -> Option<Vec<Trade>> {
        let path = self.path_for(window_ts);
        if !path.exists() { return None; }
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self, window_ts: i64, trades: &[Trade]) -> Result<()> {
        let path = self.path_for(window_ts);
        let bytes = serde_json::to_vec(trades)?;
        std::fs::write(&path, bytes)
            .map_err(|e| anyhow::anyhow!("writing trades cache {}: {e}", path.display()))?;
        Ok(())
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib backtest::data::trades`
Expected: all PASS (round-trips + mock + cache).

- [ ] **Step 5: Commit**

```bash
git add src/backtest/data/trades.rs
git commit -m "feat(backtest): add CachedTradeStore for per-window trade JSON"
```

---

## Task 5: `RealTradeOracle` + `PRE_TRADE_FALLBACK` constant

**Files:**
- Modify: `src/backtest/oracle.rs`

This is the core oracle. Constructor takes `HashMap<window_ts, Vec<Trade>>`, pre-filters to UP-outcome intra-window trades, sorts ascending. `price_at(window, t_secs)` walks back from `abs_t = window_ts + t_secs` to find the most recent SELL (= bid) and BUY (= ask). Falls back to flat `0.5` constant when no qualifying trade exists.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/backtest/oracle.rs`:

```rust
    use crate::backtest::data::trades::{Trade, TradeSide, Outcome};
    use std::collections::HashMap;

    fn up_trade(ts: i64, side: TradeSide, price: &str) -> Trade {
        Trade {
            timestamp: ts,
            side,
            price: Decimal::from_str(price).unwrap(),
            size: dec!(10),
            outcome: Outcome::Up,
        }
    }

    fn down_trade(ts: i64, side: TradeSide, price: &str) -> Trade {
        Trade { outcome: Outcome::Down, ..up_trade(ts, side, price) }
    }

    #[test]
    fn real_oracle_returns_last_sell_as_bid() {
        let window = make_window(80000.0); // window_ts = 1000
        let trades = vec![
            up_trade(1010, TradeSide::Sell, "0.50"),
            up_trade(1020, TradeSide::Sell, "0.45"),
        ];
        let mut by_window = HashMap::new();
        by_window.insert(1000_i64, trades);
        let oracle = RealTradeOracle::new(by_window);

        // At t=15s (abs_t=1015) → last SELL ≤ 1015 is 0.50
        let (bid, _) = oracle.price_at(&window, 15);
        assert_eq!(bid, dec!(0.50));

        // At t=25s (abs_t=1025) → last SELL ≤ 1025 is 0.45
        let (bid, _) = oracle.price_at(&window, 25);
        assert_eq!(bid, dec!(0.45));
    }

    #[test]
    fn real_oracle_returns_last_buy_as_ask() {
        let window = make_window(80000.0);
        let trades = vec![
            up_trade(1005, TradeSide::Buy, "0.51"),
            up_trade(1100, TradeSide::Buy, "0.55"),
        ];
        let mut by_window = HashMap::new();
        by_window.insert(1000_i64, trades);
        let oracle = RealTradeOracle::new(by_window);

        let (_, ask) = oracle.price_at(&window, 50);
        assert_eq!(ask, dec!(0.51));
        let (_, ask) = oracle.price_at(&window, 200);
        assert_eq!(ask, dec!(0.55));
    }

    #[test]
    fn real_oracle_falls_back_when_no_qualifying_trade() {
        let window = make_window(80000.0);
        // All trades after t=200s
        let trades = vec![
            up_trade(1210, TradeSide::Sell, "0.40"),
            up_trade(1220, TradeSide::Buy, "0.42"),
        ];
        let mut by_window = HashMap::new();
        by_window.insert(1000_i64, trades);
        let oracle = RealTradeOracle::new(by_window);

        // At t=10s → no trade yet → fallback both sides
        let (bid, ask) = oracle.price_at(&window, 10);
        assert_eq!(bid, dec!(0.5));
        assert_eq!(ask, dec!(0.5));
    }

    #[test]
    fn real_oracle_handles_empty_window() {
        let window = make_window(80000.0);
        let mut by_window = HashMap::new();
        by_window.insert(1000_i64, vec![]);
        let oracle = RealTradeOracle::new(by_window);
        let (bid, ask) = oracle.price_at(&window, 100);
        assert_eq!(bid, dec!(0.5));
        assert_eq!(ask, dec!(0.5));
    }

    #[test]
    fn real_oracle_handles_missing_window() {
        let window = make_window(80000.0); // window_ts = 1000
        let oracle = RealTradeOracle::new(HashMap::new());
        let (bid, ask) = oracle.price_at(&window, 100);
        assert_eq!(bid, dec!(0.5));
        assert_eq!(ask, dec!(0.5));
    }

    #[test]
    fn real_oracle_filters_post_close_trades() {
        let window = make_window(80000.0); // window_ts = 1000, close at 1300
        let trades = vec![
            up_trade(1010, TradeSide::Sell, "0.40"),
            up_trade(1400, TradeSide::Sell, "0.99"),  // post-close — must be filtered
        ];
        let mut by_window = HashMap::new();
        by_window.insert(1000_i64, trades);
        let oracle = RealTradeOracle::new(by_window);

        // At t=290s (abs_t=1290), post-close trade at ts=1400 must NOT influence bid.
        let (bid, _) = oracle.price_at(&window, 290);
        assert_eq!(bid, dec!(0.40));
    }

    #[test]
    fn real_oracle_filters_down_outcome_trades() {
        let window = make_window(80000.0);
        let trades = vec![
            down_trade(1010, TradeSide::Sell, "0.10"),  // DOWN side — irrelevant to UP token
            up_trade(1015, TradeSide::Sell, "0.55"),
        ];
        let mut by_window = HashMap::new();
        by_window.insert(1000_i64, trades);
        let oracle = RealTradeOracle::new(by_window);

        let (bid, _) = oracle.price_at(&window, 20);
        assert_eq!(bid, dec!(0.55));
    }

    #[test]
    fn real_oracle_filters_pre_window_trades() {
        let window = make_window(80000.0); // window_ts = 1000
        let trades = vec![
            up_trade(900, TradeSide::Sell, "0.99"),  // pre-window — must be filtered
            up_trade(1010, TradeSide::Sell, "0.50"),
        ];
        let mut by_window = HashMap::new();
        by_window.insert(1000_i64, trades);
        let oracle = RealTradeOracle::new(by_window);
        let (bid, _) = oracle.price_at(&window, 20);
        assert_eq!(bid, dec!(0.50));
    }
```

Add at the top of the existing `mod tests` (or fold into existing imports) to satisfy `Decimal::from_str`:

```rust
    use std::str::FromStr;
```

(if not already imported; check first).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backtest::oracle::tests::real_oracle`
Expected: 8 NEW failures (all `real_oracle_*` tests) — `RealTradeOracle` doesn't exist.

- [ ] **Step 3: Implement `RealTradeOracle`**

Edit `src/backtest/oracle.rs`. At the top, add the new imports:

```rust
use crate::backtest::data::trades::{Outcome, Trade, TradeSide};
use std::collections::HashMap;
```

Below `NoisyBlackScholesOracle`'s `impl TokenPriceOracle`, append:

```rust
const PRE_TRADE_FALLBACK: Decimal = dec!(0.5);

/// Oracle backed by recorded Polymarket trade history.
///
/// Constructor pre-filters trades to outcome=Up + intra-window only, then
/// sorts ascending. `price_at` does a reverse linear scan to find the most
/// recent SELL (bid) and BUY (ask) at or before `t_secs`. When no qualifying
/// trade exists, returns `PRE_TRADE_FALLBACK` (0.50) — the only no-info-leak
/// option for the pre-trade portion of a window.
pub struct RealTradeOracle {
    up_trades_by_window: HashMap<i64, Vec<Trade>>,
}

impl RealTradeOracle {
    pub fn new(all_trades: HashMap<i64, Vec<Trade>>) -> Self {
        let up_trades = all_trades
            .into_iter()
            .map(|(ts, trades)| {
                let mut up: Vec<Trade> = trades
                    .into_iter()
                    .filter(|t| {
                        t.outcome == Outcome::Up
                            && t.timestamp >= ts
                            && t.timestamp < ts + 300
                    })
                    .collect();
                up.sort_by_key(|t| t.timestamp);
                (ts, up)
            })
            .collect();
        Self { up_trades_by_window: up_trades }
    }
}

impl TokenPriceOracle for RealTradeOracle {
    fn price_at(&self, window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal) {
        let abs_t = window.window_ts + t_secs as i64;
        let trades = match self.up_trades_by_window.get(&window.window_ts) {
            Some(t) => t,
            None => return (PRE_TRADE_FALLBACK, PRE_TRADE_FALLBACK),
        };

        let bid = trades.iter().rev()
            .find(|t| t.side == TradeSide::Sell && t.timestamp <= abs_t)
            .map(|t| t.price)
            .unwrap_or(PRE_TRADE_FALLBACK);

        let ask = trades.iter().rev()
            .find(|t| t.side == TradeSide::Buy && t.timestamp <= abs_t)
            .map(|t| t.price)
            .unwrap_or(PRE_TRADE_FALLBACK);

        (bid, ask)
    }
}
```

- [ ] **Step 4: Run tests to verify all pass**

Run: `cargo test --lib backtest::oracle`
Expected: all PASS — pre-existing BS + Noisy tests still green; 8 new `real_oracle_*` tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/backtest/oracle.rs
git commit -m "feat(backtest): add RealTradeOracle backed by trade history"
```

---

## Task 6: `ExitRule::TpOnlyOrEarlyExit` variant + simulator arm

**Files:**
- Modify: `src/backtest/config.rs`
- Modify: `src/backtest/exit_rule.rs`

Strategy 12 needs a new exit rule semantically: "sell at `tp_price` if reached during the window; otherwise market-sell at the current bid at second `exit_at_secs`." This is distinct from `TpOnlyOrHold` (which falls through to resolution) and `FixedTime` (no TP at all).

- [ ] **Step 1: Write the failing simulator tests**

Append to `mod tests` in `src/backtest/exit_rule.rs`:

```rust
    #[test]
    fn tp_only_or_early_exit_fills_at_tp_before_exit_time() {
        // ask=0.50 at t=0; from t=1 onwards bid=0.80 → TP=0.75 triggers immediately
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        prices.extend(std::iter::repeat((dec!(0.80), dec!(0.80))).take(300));
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Down), // even with Down winner, TP triggers first
            &config_with_exit(ExitRule::TpOnlyOrEarlyExit {
                tp_price: dec!(0.75), exit_at_secs: 270
            }),
            &oracle, dec!(5),
        );
        assert!(matches!(outcome, WindowOutcome::Won { .. }));
    }

    #[test]
    fn tp_only_or_early_exit_falls_through_to_market_sell_when_no_tp() {
        // ask=0.50 at t=0, bid stays 0.50 forever → TP never hits.
        // At t=270s, market-sell at 0.50: proceeds = 10 × 0.50 = 5.00 == cost → break-even.
        // Per the >cost branch: proceeds (5.00) is NOT > cost (5.00) → Lost { spent_usd: 0 }.
        let oracle = flat_window("0.50");
        let outcome = simulate_window(
            &make_window(Direction::Up),
            &config_with_exit(ExitRule::TpOnlyOrEarlyExit {
                tp_price: dec!(0.75), exit_at_secs: 270
            }),
            &oracle, dec!(5),
        );
        match outcome {
            WindowOutcome::Lost { spent_usd } => {
                // Net loss is zero when proceeds equal cost
                assert_eq!(spent_usd, dec!(0));
            }
            _ => panic!("expected Lost (break-even = no profit), got {outcome:?}"),
        }
    }

    #[test]
    fn tp_only_or_early_exit_falls_through_to_loss() {
        // ask=0.50 entry, bid drifts to 0.40 → no TP, exit at 270s with loss
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        prices.extend(std::iter::repeat((dec!(0.40), dec!(0.40))).take(300));
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Up),
            &config_with_exit(ExitRule::TpOnlyOrEarlyExit {
                tp_price: dec!(0.75), exit_at_secs: 270
            }),
            &oracle, dec!(5),
        );
        match outcome {
            WindowOutcome::Lost { spent_usd } => {
                // 10 shares × (0.50 - 0.40) = 1.00 net loss
                assert!(spent_usd >= dec!(0.95) && spent_usd <= dec!(1.05),
                        "spent_usd={spent_usd}");
            }
            _ => panic!("expected Lost, got {outcome:?}"),
        }
    }

    #[test]
    fn tp_only_or_early_exit_falls_through_to_profit() {
        // ask=0.50 entry, bid drifts to 0.60 → no TP at 0.75, but exit at 270s with profit
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        prices.extend(std::iter::repeat((dec!(0.60), dec!(0.60))).take(300));
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Down), // direction irrelevant: we exit early
            &config_with_exit(ExitRule::TpOnlyOrEarlyExit {
                tp_price: dec!(0.75), exit_at_secs: 270
            }),
            &oracle, dec!(5),
        );
        match outcome {
            WindowOutcome::Won { proceeds_usd } => {
                // 10 shares × 0.60 = 6.00
                assert!(proceeds_usd >= dec!(5.95) && proceeds_usd <= dec!(6.05),
                        "proceeds_usd={proceeds_usd}");
            }
            _ => panic!("expected Won, got {outcome:?}"),
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backtest::exit_rule`
Expected: 4 NEW failures — `TpOnlyOrEarlyExit` variant doesn't exist.

- [ ] **Step 3: Add the variant to `ExitRule`**

Edit `src/backtest/config.rs`. In the `ExitRule` enum, add:

```rust
#[derive(Clone, Debug)]
pub enum ExitRule {
    HoldToResolution,
    TpOnlyOrHold { tp_price: Decimal },
    TpSlOrHold { tp_price: Decimal, sl_price: Decimal },
    FixedTime { seconds: u32 },
    /// v1.7.5: Try TP at `tp_price`; if not filled by `exit_at_secs`,
    /// market-sell at the current bid. Avoids resolution path entirely.
    TpOnlyOrEarlyExit { tp_price: Decimal, exit_at_secs: u32 },
}
```

- [ ] **Step 4: Add the simulator arm**

Edit `src/backtest/exit_rule.rs`. In the `for t in 1..=300u32` loop's `match &config.exit { ... }` block, add a new arm BEFORE the `_ => {}` catch-all:

```rust
            ExitRule::TpOnlyOrEarlyExit { tp_price, exit_at_secs } => {
                if bid >= *tp_price {
                    return WindowOutcome::Won { proceeds_usd: proceeds };
                }
                if t >= *exit_at_secs {
                    return if proceeds > dollars_spent {
                        WindowOutcome::Won { proceeds_usd: proceeds }
                    } else {
                        WindowOutcome::Lost { spent_usd: dollars_spent - proceeds }
                    };
                }
            }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib backtest::exit_rule`
Expected: 4 new tests PASS, all existing tests still green.

- [ ] **Step 6: Run the full backtest test suite to catch any regressions**

Run: `cargo test --lib backtest::`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/backtest/config.rs src/backtest/exit_rule.rs
git commit -m "feat(backtest): add TpOnlyOrEarlyExit exit rule variant"
```

---

## Task 7: `OracleKind` enum + `--oracle` flag + strategies 12 + 13

**Files:**
- Modify: `src/backtest/config.rs`

Add the dispatch enum and flag, plus the two new strategies. The flag defaults to `bs`, preserving v1.7.2 behavior.

- [ ] **Step 1: Write the failing tests**

Append to `mod tests` in `src/backtest/config.rs`:

```rust
    #[test]
    fn parses_oracle_default_bs() {
        let a = parse(&["--start", "2026-04-09", "--end", "2026-05-09"]);
        assert_eq!(a.oracle, OracleKind::Bs);
    }

    #[test]
    fn parses_oracle_real() {
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--oracle", "real",
        ]);
        assert_eq!(a.oracle, OracleKind::Real);
    }

    #[test]
    fn parses_oracle_noisy() {
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--oracle", "noisy",
        ]);
        assert_eq!(a.oracle, OracleKind::Noisy);
    }

    #[test]
    fn strategy_set_has_thirteen_strategies() {
        let s = strategy_set();
        assert_eq!(s.len(), 13);
    }

    #[test]
    fn strategy_12_is_tp75_early_exit_270() {
        let s = strategy_set();
        let s12 = s.iter().find(|c| c.name == "12_tp75_early_exit_270")
            .expect("strategy 12 missing");
        match &s12.exit {
            ExitRule::TpOnlyOrEarlyExit { tp_price, exit_at_secs } => {
                assert_eq!(*tp_price, dec!(0.75));
                assert_eq!(*exit_at_secs, 270);
            }
            _ => panic!("strategy 12 should be TpOnlyOrEarlyExit"),
        }
        assert!(matches!(s12.stake, StakeRule::Martingale { .. }));
    }

    #[test]
    fn strategy_13_is_hold_early_exit_270() {
        let s = strategy_set();
        let s13 = s.iter().find(|c| c.name == "13_hold_early_exit_270")
            .expect("strategy 13 missing");
        match &s13.exit {
            ExitRule::FixedTime { seconds } => {
                assert_eq!(*seconds, 270);
            }
            _ => panic!("strategy 13 should be FixedTime { seconds: 270 }"),
        }
        assert!(matches!(s13.stake, StakeRule::Martingale { .. }));
    }
```

Also update the existing `strategy_set_has_eleven_strategies` test:

```rust
    // Replace this old test:
    // #[test]
    // fn strategy_set_has_eleven_strategies() { ... assert_eq!(s.len(), 11); ... }
    // (It is REPLACED by `strategy_set_has_thirteen_strategies` above.
    // Delete it from the file.)

    // Also update `strategy_set_uniqueness` to expect 13.
    // Update `filter_all_returns_everything` to expect 13.
```

Concretely, edit:
- `strategy_set_has_eleven_strategies` → DELETE.
- In `strategy_set_uniqueness`: change `assert_eq!(names.len(), 11);` to `assert_eq!(names.len(), 13);`.
- In `filter_all_returns_everything`: change both `11`s to `13`s.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backtest::config`
Expected: 6 NEW failures (`OracleKind` doesn't exist + strategy count off + strategy 12/13 missing).

- [ ] **Step 3: Add `OracleKind` enum + flag**

Edit `src/backtest/config.rs`.

At the top, update imports:

```rust
use clap::{Parser, ValueEnum};
```

Add the enum (above `BacktestArgs`):

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum OracleKind {
    Bs,
    Noisy,
    Real,
}
```

Add the flag to `BacktestArgs` (after `noise_seed`):

```rust
    /// Oracle to use for token price simulation.
    /// `bs` = Black-Scholes theoretical (default; v1.4 behavior).
    /// `noisy` = BS + Gaussian noise (v1.7.2; respects --oracle-noise).
    /// `real` = Real Polymarket trade history (v1.7.5; auto-fetches uncached).
    #[arg(long, value_enum, default_value = "bs")]
    pub oracle: OracleKind,
```

- [ ] **Step 4: Append strategies 12 + 13**

In `strategy_set()`, after the line for `11_tp85_sl20`, append:

```rust
        common("12_tp75_early_exit_270",
            ExitRule::TpOnlyOrEarlyExit { tp_price: dec!(0.75), exit_at_secs: 270 },
            mart()),
        common("13_hold_early_exit_270",
            ExitRule::FixedTime { seconds: 270 },
            mart()),
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib backtest::config`
Expected: all PASS — including 6 new tests + the updated count tests.

Run: `cargo test --lib backtest::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/backtest/config.rs
git commit -m "feat(backtest): add --oracle flag and 2 early-exit strategies (12, 13)"
```

---

## Task 8: `poly-backtest` binary dispatch on `args.oracle`

**Files:**
- Modify: `src/bin/poly-backtest.rs`

Wire the new `OracleKind` into oracle construction. The `Real` arm must:
1. Walk `loaded.windows`. Skip any window without `condition_id` (warn).
2. For each remaining window: try cache; if miss, fetch via `PolymarketTradeFetcher` and save. Throttle 100ms.
3. Build `RealTradeOracle::new(HashMap<i64, Vec<Trade>>)`.

- [ ] **Step 1: Write the failing build check**

Run: `cargo build --bin poly-backtest`
Expected: still builds (we haven't broken it yet). Note: the binary doesn't have unit tests; correctness is verified end-to-end via Task 9 integration test.

- [ ] **Step 2: Edit `src/bin/poly-backtest.rs`**

Replace the existing oracle construction block:

```rust
    let oracle: Box<dyn TokenPriceOracle> = if args.oracle_noise > 0.0 {
        eprintln!(
            "[poly-backtest] oracle noise σ={:.4} seed={}",
            args.oracle_noise, args.noise_seed
        );
        Box::new(NoisyBlackScholesOracle::new(
            base_oracle,
            args.oracle_noise,
            args.noise_seed,
        ))
    } else {
        Box::new(base_oracle)
    };
```

with:

```rust
    let oracle: Box<dyn TokenPriceOracle> = match args.oracle {
        OracleKind::Bs => Box::new(BlackScholesOracle::new(btc_arc.clone(), sigma, args.friction)),
        OracleKind::Noisy => {
            eprintln!(
                "[poly-backtest] oracle noise σ={:.4} seed={}",
                args.oracle_noise, args.noise_seed
            );
            Box::new(NoisyBlackScholesOracle::new(
                BlackScholesOracle::new(btc_arc.clone(), sigma, args.friction),
                args.oracle_noise,
                args.noise_seed,
            ))
        }
        OracleKind::Real => {
            eprintln!(
                "[poly-backtest] loading real trade history (auto-fetching uncached)..."
            );
            let trades_dir = cache_root.join("trades");
            let store = CachedTradeStore::new(trades_dir)?;
            let fetcher = PolymarketTradeFetcher::new(100); // 100ms throttle
            let mut all_trades: HashMap<i64, Vec<Trade>> = HashMap::new();
            let mut fetched = 0usize;
            let mut cached = 0usize;
            let mut skipped = 0usize;
            for (i, w) in loaded.windows.iter().enumerate() {
                let cid = match &w.condition_id {
                    Some(c) => c.clone(),
                    None => {
                        skipped += 1;
                        continue;
                    }
                };
                let trades = match store.load(w.window_ts) {
                    Some(t) => { cached += 1; t }
                    None => {
                        let t = fetcher.fetch_window(&cid, w.window_ts).await
                            .with_context(|| format!(
                                "fetching trades for window {} ({})", w.window_ts, cid
                            ))?;
                        store.save(w.window_ts, &t)?;
                        fetched += 1;
                        if (cached + fetched) % 50 == 0 {
                            eprintln!(
                                "[poly-backtest]   trades: {} cached, {} fetched, {}/{} windows",
                                cached, fetched, i + 1, loaded.windows.len()
                            );
                        }
                        t
                    }
                };
                all_trades.insert(w.window_ts, trades);
            }
            eprintln!(
                "[poly-backtest] trades load complete: {} cached, {} fetched, {} skipped (no condition_id)",
                cached, fetched, skipped
            );
            Box::new(RealTradeOracle::new(all_trades))
        }
    };
```

Also update imports at the top of the file:

```rust
use poly_tui::backtest::{
    config::{filter_strategies, strategy_set, BacktestArgs, OracleKind},
    data::{
        cache::DiskCache,
        loader::DataLoader,
        trades::{CachedTradeStore, PolymarketTradeFetcher, Trade, TradeFetcher},
    },
    oracle::{estimate_sigma, BlackScholesOracle, NoisyBlackScholesOracle, RealTradeOracle, TokenPriceOracle},
    report::{render_html, ReportMeta},
    runner::run_strategy,
    stats::compute_stats,
};
use std::collections::HashMap;
```

- [ ] **Step 3: Re-export `RealTradeOracle` from oracle module**

Verify `RealTradeOracle` is `pub` in `src/backtest/oracle.rs`. (It already should be from Task 5; sanity-check.)

- [ ] **Step 4: Re-export `Trade`, `TradeFetcher`, etc. from data module**

The existing `pub mod trades;` in Task 2 already exposes the inner items via `data::trades::Trade` etc. The import path used above (`data::trades::{...}`) is correct.

- [ ] **Step 5: Verify the build**

Run: `cargo build --bin poly-backtest`
Expected: clean build. If it fails on `OracleKind` not being in scope inside `BacktestArgs`'s clap derive, ensure `OracleKind` is `pub`.

- [ ] **Step 6: Quick smoke run with `--oracle bs` (must be unchanged)**

Run: `cargo run --bin poly-backtest -- --start 2026-04-08 --end 2026-04-09 --oracle bs --output /tmp/bs-smoke.html`
Expected: completes successfully, prints stats. (Cache may already exist from earlier runs — fine.)

- [ ] **Step 7: Commit**

```bash
git add src/bin/poly-backtest.rs
git commit -m "feat(backtest): wire --oracle flag and real-oracle pre-fetch"
```

---

## Task 9: Integration test (fixture-driven)

**Files:**
- Create: `tests/real_trade_backtest.rs`

The integration test bypasses the network: it pre-populates the `trades/` cache with a hand-built fixture, then runs the backtest pipeline end-to-end with `--oracle real`. It's `#[ignore]` because it touches the real fs (writes a temp cache and can be slow).

- [ ] **Step 1: Write the integration test**

Create `tests/real_trade_backtest.rs`:

```rust
use poly_tui::backtest::data::trades::{CachedTradeStore, Outcome, Trade, TradeSide};
use poly_tui::backtest::data::gamma_history::WindowMeta;
use poly_tui::backtest::oracle::{RealTradeOracle, TokenPriceOracle};
use poly_tui::trader::ladder::Direction;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tempfile::TempDir;

#[test]
fn real_oracle_pipeline_fixture_round_trip() {
    let tmp = TempDir::new().unwrap();
    let store = CachedTradeStore::new(tmp.path()).unwrap();

    let window_ts: i64 = 1778416800;
    // Hand-crafted realistic trade sequence:
    //   t=10s ask=0.52 (BUY)
    //   t=20s bid=0.48 (SELL)
    //   t=120s bid=0.78 (SELL) — should trigger TP=0.75 in strategy 12
    //   t=250s bid=0.65 (SELL)
    let trades = vec![
        Trade { timestamp: window_ts + 10,  side: TradeSide::Buy,  price: dec!(0.52), size: dec!(50), outcome: Outcome::Up },
        Trade { timestamp: window_ts + 20,  side: TradeSide::Sell, price: dec!(0.48), size: dec!(50), outcome: Outcome::Up },
        Trade { timestamp: window_ts + 120, side: TradeSide::Sell, price: dec!(0.78), size: dec!(50), outcome: Outcome::Up },
        Trade { timestamp: window_ts + 250, side: TradeSide::Sell, price: dec!(0.65), size: dec!(50), outcome: Outcome::Up },
    ];
    store.save(window_ts, &trades).unwrap();

    let loaded = store.load(window_ts).expect("cache hit");
    assert_eq!(loaded, trades);

    let mut by_window = HashMap::new();
    by_window.insert(window_ts, loaded);
    let oracle = RealTradeOracle::new(by_window);

    let window = WindowMeta {
        window_ts,
        price_to_beat: dec!(80000),
        final_price: Some(dec!(80050)),
        winner: Some(Direction::Up),
        condition_id: Some(
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into(),
        ),
    };

    // At t=5s → no trade yet → fallback 0.5 / 0.5
    assert_eq!(oracle.price_at(&window, 5), (dec!(0.5), dec!(0.5)));

    // At t=15s → ask=0.52 (last BUY), bid=0.5 (no SELL yet)
    assert_eq!(oracle.price_at(&window, 15), (dec!(0.5), dec!(0.52)));

    // At t=125s → ask=0.52, bid=0.78 (TP would have triggered at t=120s)
    assert_eq!(oracle.price_at(&window, 125), (dec!(0.78), dec!(0.52)));

    // At t=295s → still ask=0.52, bid=0.65 (most recent SELL ≤ 295)
    assert_eq!(oracle.price_at(&window, 295), (dec!(0.65), dec!(0.52)));
}

#[test]
#[ignore = "writes to temp dir, runs full simulator pipeline"]
fn real_oracle_strategy_12_triggers_tp() {
    use poly_tui::backtest::config::{ExitRule, StakeRule, StrategyConfig};
    use poly_tui::backtest::exit_rule::simulate_window;
    use poly_tui::trader::ladder::WindowOutcome;

    let window_ts: i64 = 1778416800;
    let trades = vec![
        Trade { timestamp: window_ts + 10, side: TradeSide::Buy,  price: dec!(0.52), size: dec!(50), outcome: Outcome::Up },
        Trade { timestamp: window_ts + 20, side: TradeSide::Sell, price: dec!(0.48), size: dec!(50), outcome: Outcome::Up },
        Trade { timestamp: window_ts + 100, side: TradeSide::Sell, price: dec!(0.78), size: dec!(50), outcome: Outcome::Up },
    ];
    let mut by_window = HashMap::new();
    by_window.insert(window_ts, trades);
    let oracle = RealTradeOracle::new(by_window);

    let window = WindowMeta {
        window_ts,
        price_to_beat: dec!(80000),
        final_price: Some(dec!(80050)),
        winner: Some(Direction::Down),  // would lose at resolution but TP triggers first
        condition_id: Some("0x00".into()),
    };

    let cfg = StrategyConfig {
        name: "12_tp75_early_exit_270".into(),
        direction: Direction::Up,
        band_min: dec!(0.45),
        band_max: dec!(0.55),
        stake: StakeRule::Martingale { base: dec!(5), max_step: 5 },
        exit: ExitRule::TpOnlyOrEarlyExit { tp_price: dec!(0.75), exit_at_secs: 270 },
    };

    let outcome = simulate_window(&window, &cfg, &oracle, dec!(5));
    assert!(matches!(outcome, WindowOutcome::Won { .. }),
            "strategy 12 should have hit TP at t=100s; got {outcome:?}");
}
```

- [ ] **Step 2: Run the integration tests**

Run: `cargo test --test real_trade_backtest`
Expected: `real_oracle_pipeline_fixture_round_trip` PASS. The `#[ignore]`d `real_oracle_strategy_12_triggers_tp` skipped.

Run: `cargo test --test real_trade_backtest -- --include-ignored`
Expected: both PASS.

- [ ] **Step 3: Commit**

```bash
git add tests/real_trade_backtest.rs
git commit -m "test(backtest): add real-oracle fixture-driven integration test"
```

---

## Task 10: README + TODO docs

**Files:**
- Modify: `README.md`
- Modify: `TODO.md`

Document the new flag, strategies, and fetch workflow. Tick v1.7.5 ✅ on the TODO and record decision criteria for v1.8.

- [ ] **Step 1: Update README.md**

Locate the backtest section. Append a subsection:

````markdown
### v1.7.5 — Real Polymarket trade-history oracle

```bash
# First run on a date range — auto-fetches trades from data-api/trades.
# Stored in ~/.poly-backtest-cache/trades/<window_ts>.json. Subsequent runs
# reuse cache. Throttled to ~100ms between requests; ~17 min for 30 days.
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle real \
  --output report-real.html

# Once cached, focused re-runs are fast:
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle real \
  --strategies 12_tp75_early_exit_270,13_hold_early_exit_270 \
  --output report-real-candidates.html
```

| `--oracle` | Source | Friction | Determinism |
|---|---|---|---|
| `bs` (default) | Black-Scholes mid + symmetric friction | `--friction` (default 1.5%) | exact |
| `noisy` | BS + per-tick Gaussian noise | `--friction` + `--oracle-noise` | seeded reproducible |
| `real` | Last in-window SELL/BUY trade | already embedded in observed prices | exact (data-driven) |

**New strategies (added v1.7.5):**

- `12_tp75_early_exit_270`: BUY in band → limit TP @ 0.75 → at t=270s, market-sell residual at bid. No resolution path.
- `13_hold_early_exit_270`: BUY in band → hold → at t=270s, market-sell at bid. No resolution path.

Both candidates avoid post-resolution redemption (which currently requires MATIC the EOA doesn't have).
````

- [ ] **Step 2: Update TODO.md**

Locate v1.7.5 entry. Replace its body with:

```markdown
### v1.7.5 — Real Polymarket trade-history backtest ✅ COMPLETE

Validated two early-exit candidates against ground-truth trade data:

- `12_tp75_early_exit_270` — TP=0.75 → fall through to t=270s market-sell
- `13_hold_early_exit_270` — hold → t=270s market-sell

Both avoid the redemption path entirely (no MATIC needed).

**Decision rule for v1.8:**

- If strategy 12 PnL > 0: implement v1.8 trader with `--exit-rule tp-only`, `--tp-price 0.75`, `--exit-at-secs 270`.
- If strategy 13 PnL > 0 (and 12 ≤ 0): implement v1.8b with `FixedTime { seconds: 270 }` semantics.
- If both PnL ≤ 0: abandon Polymarket 5min market.

Run: `poly-backtest --start <30-day-window> --end <today> --oracle real --strategies 12_tp75_early_exit_270,13_hold_early_exit_270`. Inspect `backtest-report.html` PnL column.
```

- [ ] **Step 3: Verify the docs build cleanly**

Run: `cargo build --bin poly-backtest`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add README.md TODO.md
git commit -m "docs(backtest): v1.7.5 real trade history operator workflow"
```

---

## Final verification

- [ ] **Step 1: Full test suite**

Run: `cargo test --lib backtest::`
Expected: all PASS.

Run: `cargo test --test real_trade_backtest -- --include-ignored`
Expected: both PASS.

- [ ] **Step 2: BS oracle output unchanged**

Run: `cargo run --bin poly-backtest -- --start 2026-04-08 --end 2026-04-09 --oracle bs --output /tmp/bs-smoke.html`
Expected: completes; HTML written. Compare strategy stats line-by-line against a known-good v1.7.2 run for the same date range — should be identical (same RNG-free strategies, same windows).

- [ ] **Step 3: Verify clap accepts `--oracle real` end-to-end (dry compile-only check)**

Run: `cargo run --bin poly-backtest -- --help | grep -A2 oracle`
Expected: shows `--oracle <ORACLE>` with values `bs`, `noisy`, `real`.

---

## Out of scope (do NOT implement)

- 15min / 60min market variants — deferred (v1.7.3).
- Real-oracle parallel/concurrent fetch — sequential is fine for one-time 17-min hit.
- Trade size weighting — last-price regardless of size.
- Smart "fetch only new days" optimization — per-window cache already de-dups.
- v1.8 trader implementation — that's a separate plan after seeing the v1.7.5 report.
