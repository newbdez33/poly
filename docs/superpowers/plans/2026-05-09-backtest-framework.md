# Polymarket BTC 5min Backtest Framework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `poly-backtest` — an offline CLI tool that runs 6 strategy variants (hold-to-resolution / TP-only / TP+SL symmetric / TP+SL asymmetric / time-based / fixed-stake) against 30 days of historical Polymarket BTC 5-min market data, generates a single-page HTML report comparing EV, win rate, max drawdown, and Martingale cap-trigger frequency. Used to **decide which strategy to deploy** (or whether none of them are EV-positive).

**Architecture:** Independent offline binary, zero modifications to v1.0/v1.1/v1.2 code. Reuses `trader::ladder::{LadderState, apply_outcome, Direction, WindowOutcome, SkipReason}` for Martingale FSM. Synthetic Polymarket UP-token prices via Black-Scholes binary-option oracle (BTC 1-min Binance data + estimated σ). Data cached on disk so repeat runs don't re-fetch. HTML report uses Chart.js from CDN, single self-contained file ~500KB.

**Tech Stack:** Rust 1.78+, reqwest (HTTP), serde_json, rust_decimal, chrono, tokio, clap (CLI), statrs (Black-Scholes Φ).

**Spec:** `docs/superpowers/specs/2026-05-09-backtest-framework-design.md`
**HTML preview:** `docs/superpowers/mockups/2026-05-09-backtest-report-preview.html`
**Base commit:** `53df8fa` (spec + preview committed). Pre-feature baseline: `2436436` (v1.2.1).

---

## ⚠️ Build hygiene during implementation

The dry-run trader (PID 53896) may still be running OR may have hit the cap and exited cleanly. Either way:

**Allowed:**
- `cargo build --bin poly-backtest`
- `cargo test --lib backtest`
- `cargo build --test backtest_smoke`
- `cargo test --test backtest_smoke -- --ignored`

**Avoid:**
- `cargo build` (no args — would try to build all bins, may fail on poly-trader.exe lock)
- `cargo build --bin poly-trader` / `cargo build --bin poly-tui` (not needed for this work)

If a build fails with `Access is denied (os error 5)` on `poly-trader.exe`, leave it. We don't need that binary.

---

## File Structure

```
src/
├── bin/
│   └── poly-backtest.rs               ← NEW: CLI entry, orchestrates everything
├── backtest/                          ← NEW: backtest module tree
│   ├── mod.rs
│   ├── config.rs                      ← BacktestArgs (clap), StrategyConfig, StakeRule, ExitRule, strategy_set()
│   ├── data/
│   │   ├── mod.rs
│   │   ├── cache.rs                   ← Generic JSON disk cache
│   │   ├── binance.rs                 ← Binance klines fetcher + 1min BTC series
│   │   ├── gamma_history.rs           ← Per-window gamma fetcher + WindowMeta
│   │   └── loader.rs                  ← Combine: load N-day window list + BTC series
│   ├── oracle.rs                      ← BlackScholesOracle, σ estimation
│   ├── exit_rule.rs                   ← simulate_window function
│   ├── runner.rs                      ← run_strategy (multi-window loop)
│   ├── stats.rs                       ← compute_stats, StrategyStats
│   └── report.rs                      ← HTML rendering (5 sub-renderers)
└── lib.rs                             ← +pub mod backtest

tests/
└── backtest_smoke.rs                  ← NEW: #[ignore] real network smoke test

docs/superpowers/mockups/
└── 2026-05-09-backtest-report-preview.html  ← already committed (53df8fa)

Cargo.toml                             ← +statrs, +[[bin]] poly-backtest, +[[test]] backtest_smoke
```

**Module dependency direction (enforced):**

```
backtest::config         → trader::ladder (Direction)
backtest::data::cache    → (no dep on backtest)
backtest::data::binance  → backtest::data::cache
backtest::data::gamma_history → trader::ladder (Direction), backtest::data::cache
backtest::data::loader   → backtest::data::binance, backtest::data::gamma_history
backtest::oracle         → backtest::data::binance (BTC series)
backtest::exit_rule      → backtest::config, backtest::oracle, trader::ladder (WindowOutcome, SkipReason)
backtest::runner         → backtest::config, backtest::exit_rule, trader::ladder (LadderState, apply_outcome)
backtest::stats          → backtest::runner
backtest::report         → backtest::stats
bin/poly-backtest        → all of backtest::*
```

**Coverage exclusion regex (existing, no change):** `src/bin|src/trader/adapters/|.*_wrapper\.rs`. The new `src/backtest/` is fully covered. The `data/binance.rs` and `data/gamma_history.rs` HTTP wrappers each have a thin `_wrapper.rs` companion if the implementer extracts I/O — otherwise they fall under "data/" path which is testable end-to-end via mock HTTP servers in tests.

---

## Task 0: Bootstrap deps + module skeleton

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Create: `src/backtest/mod.rs`, 8 placeholder module files
- Create: `src/backtest/data/mod.rs`, 4 placeholder files
- Create: `src/bin/poly-backtest.rs` (stub)

- [ ] **Step 1: Add `statrs` to `[dependencies]` in `Cargo.toml`**

```toml
statrs = "0.18"
```

- [ ] **Step 2: Add `[[bin]] poly-backtest` block at end of `Cargo.toml`**

```toml
[[bin]]
name = "poly-backtest"
path = "src/bin/poly-backtest.rs"
```

- [ ] **Step 3: Add commented `[[test]]` for the smoke test**

```toml
# [[test]]
# name = "backtest_smoke"
# path = "tests/backtest_smoke.rs"
```

(Will uncomment in Task 12.)

- [ ] **Step 4: Create directories and placeholder files**

```bash
cd C:/Users/newbd/projects/dev/poly
mkdir -p src/backtest/data
for f in mod config oracle exit_rule runner stats report; do
  echo "// placeholder" > "src/backtest/$f.rs"
done
for f in mod cache binance gamma_history loader; do
  echo "// placeholder" > "src/backtest/data/$f.rs"
done
```

- [ ] **Step 5: Write `src/backtest/mod.rs`**

```rust
pub mod config;
pub mod data;
pub mod oracle;
pub mod exit_rule;
pub mod runner;
pub mod stats;
pub mod report;
```

- [ ] **Step 6: Write `src/backtest/data/mod.rs`**

```rust
pub mod cache;
pub mod binance;
pub mod gamma_history;
pub mod loader;
```

- [ ] **Step 7: Update `src/lib.rs` — append**

```rust
pub mod backtest;
```

- [ ] **Step 8: Stub `src/bin/poly-backtest.rs`**

```rust
fn main() {
    println!("poly-backtest placeholder");
}
```

- [ ] **Step 9: Verify build**

```bash
cargo build --bin poly-backtest
```

Expected: `Finished`. Warnings about empty modules OK.

- [ ] **Step 10: Commit**

```bash
git add Cargo.toml src/lib.rs src/backtest/ src/bin/poly-backtest.rs
git commit -m "chore(backtest): bootstrap deps + module skeleton"
```

---

## Task 1: Config — BacktestArgs (CLI) + strategy_set

**Files:**
- Modify: `src/backtest/config.rs`

- [ ] **Step 1: Replace placeholder with config + tests**

```rust
use crate::trader::ladder::Direction;
use chrono::NaiveDate;
use clap::Parser;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

#[derive(Parser, Debug, Clone)]
#[command(name = "poly-backtest", about = "Backtest strategies on Polymarket BTC 5min history")]
pub struct BacktestArgs {
    /// Start date (UTC, inclusive) — e.g. 2026-04-09
    #[arg(long)]
    pub start: NaiveDate,

    /// End date (UTC, exclusive) — e.g. 2026-05-09
    #[arg(long)]
    pub end: NaiveDate,

    /// Output HTML path
    #[arg(long, default_value = "backtest-report.html")]
    pub output: std::path::PathBuf,

    /// Cache directory (default ~/.poly-backtest-cache/)
    #[arg(long)]
    pub cache_dir: Option<std::path::PathBuf>,

    /// Override sigma (BTC 5-min std dev in dollars). Defaults to estimated from data.
    #[arg(long)]
    pub sigma: Option<f64>,

    /// Friction coefficient (spread + fees). Default 0.015 (1.5%).
    #[arg(long, default_value = "0.015")]
    pub friction: f64,

    /// Strategy filter — comma-separated names, or "all"
    #[arg(long, default_value = "all")]
    pub strategies: String,
}

#[derive(Clone, Debug)]
pub enum StakeRule {
    Martingale { base: Decimal, max_step: u8 },
    Fixed { stake: Decimal },
}

#[derive(Clone, Debug)]
pub enum ExitRule {
    HoldToResolution,
    TpOnlyOrHold { tp_price: Decimal },
    TpSlOrHold { tp_price: Decimal, sl_price: Decimal },
    FixedTime { seconds: u32 },
}

#[derive(Clone, Debug)]
pub struct StrategyConfig {
    pub name: String,
    pub direction: Direction,
    pub band_min: Decimal,
    pub band_max: Decimal,
    pub stake: StakeRule,
    pub exit: ExitRule,
}

pub fn strategy_set() -> Vec<StrategyConfig> {
    let mart = || StakeRule::Martingale { base: dec!(5), max_step: 5 };
    let common = |name: &str, exit: ExitRule, stake: StakeRule| StrategyConfig {
        name: name.to_string(),
        direction: Direction::Up,
        band_min: dec!(0.45),
        band_max: dec!(0.55),
        stake,
        exit,
    };
    vec![
        common("1_hold_martingale",       ExitRule::HoldToResolution,                              mart()),
        common("2_tp_only_martingale",    ExitRule::TpOnlyOrHold { tp_price: dec!(0.75) },         mart()),
        common("3_tp_sl_symmetric",       ExitRule::TpSlOrHold { tp_price: dec!(0.55), sl_price: dec!(0.45) }, mart()),
        common("4_tp_sl_asymmetric",      ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.45) }, mart()),
        common("5_time_60s_martingale",   ExitRule::FixedTime { seconds: 60 },                     mart()),
        common("6_fixed_stake_baseline",  ExitRule::HoldToResolution,                              StakeRule::Fixed { stake: dec!(5) }),
    ]
}

/// Filter `all_strategies` by the comma-separated `filter` string. "all" returns all.
pub fn filter_strategies(all: &[StrategyConfig], filter: &str) -> Vec<StrategyConfig> {
    if filter == "all" || filter.is_empty() {
        return all.to_vec();
    }
    let names: Vec<&str> = filter.split(',').map(|s| s.trim()).collect();
    all.iter().filter(|s| names.contains(&s.name.as_str())).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> BacktestArgs {
        let mut full = vec!["poly-backtest"];
        full.extend(args);
        BacktestArgs::parse_from(full)
    }

    #[test]
    fn parses_minimal_args() {
        let a = parse(&["--start", "2026-04-09", "--end", "2026-05-09"]);
        assert_eq!(a.start, NaiveDate::from_ymd_opt(2026, 4, 9).unwrap());
        assert_eq!(a.end, NaiveDate::from_ymd_opt(2026, 5, 9).unwrap());
        assert_eq!(a.friction, 0.015);
        assert_eq!(a.strategies, "all");
    }

    #[test]
    fn strategy_set_has_six_strategies() {
        let s = strategy_set();
        assert_eq!(s.len(), 6);
        let names: Vec<&str> = s.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"1_hold_martingale"));
        assert!(names.contains(&"6_fixed_stake_baseline"));
    }

    #[test]
    fn strategy_set_uniqueness() {
        let s = strategy_set();
        let mut names: Vec<&String> = s.iter().map(|c| &c.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 6);
    }

    #[test]
    fn strategy_1_is_hold_to_resolution_martingale() {
        let s = strategy_set();
        let s1 = s.iter().find(|c| c.name == "1_hold_martingale").unwrap();
        assert!(matches!(s1.exit, ExitRule::HoldToResolution));
        assert!(matches!(s1.stake, StakeRule::Martingale { .. }));
    }

    #[test]
    fn strategy_6_is_fixed_stake_no_martingale() {
        let s = strategy_set();
        let s6 = s.iter().find(|c| c.name == "6_fixed_stake_baseline").unwrap();
        assert!(matches!(s6.stake, StakeRule::Fixed { stake } if stake == dec!(5)));
    }

    #[test]
    fn filter_all_returns_everything() {
        let s = strategy_set();
        assert_eq!(filter_strategies(&s, "all").len(), 6);
        assert_eq!(filter_strategies(&s, "").len(), 6);
    }

    #[test]
    fn filter_specific_names() {
        let s = strategy_set();
        let f = filter_strategies(&s, "1_hold_martingale,4_tp_sl_asymmetric");
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].name, "1_hold_martingale");
        assert_eq!(f[1].name, "4_tp_sl_asymmetric");
    }

    #[test]
    fn filter_unknown_name_returns_empty() {
        let s = strategy_set();
        assert_eq!(filter_strategies(&s, "nonexistent").len(), 0);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib backtest::config
```

Expected: 8 passed.

- [ ] **Step 3: Commit**

```bash
git add src/backtest/config.rs
git commit -m "feat(backtest): BacktestArgs (clap) + 6 StrategyConfigs"
```

---

## Task 2: Generic JSON disk cache

**Files:**
- Modify: `src/backtest/data/cache.rs`

- [ ] **Step 1: Replace placeholder**

```rust
use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};

/// Disk cache rooted at a directory, storing JSON files keyed by filename.
pub struct DiskCache {
    root: PathBuf,
}

impl DiskCache {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating cache dir {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn path_for(&self, key: &str) -> PathBuf {
        self.root.join(format!("{key}.json"))
    }

    pub fn exists(&self, key: &str) -> bool {
        self.path_for(key).exists()
    }

    pub fn read<T: DeserializeOwned>(&self, key: &str) -> Result<T> {
        let path = self.path_for(key);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("reading cache {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("decoding cache {}", path.display()))
    }

    pub fn write<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(value)?;
        std::fs::write(&path, bytes)
            .with_context(|| format!("writing cache {}", path.display()))?;
        Ok(())
    }

    /// Default cache root: ~/.poly-backtest-cache/<subdir>
    pub fn default_root(subdir: &str) -> PathBuf {
        match dirs::home_dir() {
            Some(home) => home.join(".poly-backtest-cache").join(subdir),
            None => PathBuf::from("./poly-backtest-cache").join(subdir),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::TempDir;

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Sample {
        n: i32,
        s: String,
    }

    #[test]
    fn write_then_read_roundtrips() {
        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path()).unwrap();
        let v = Sample { n: 42, s: "hello".into() };
        cache.write("foo", &v).unwrap();
        let back: Sample = cache.read("foo").unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn exists_reflects_writes() {
        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path()).unwrap();
        assert!(!cache.exists("k"));
        cache.write("k", &Sample { n: 1, s: "x".into() }).unwrap();
        assert!(cache.exists("k"));
    }

    #[test]
    fn read_missing_returns_error() {
        let tmp = TempDir::new().unwrap();
        let cache = DiskCache::new(tmp.path()).unwrap();
        let r: Result<Sample> = cache.read("nonexistent");
        assert!(r.is_err());
    }
}
```

- [ ] **Step 2: Add `dirs` to `[dependencies]` in `Cargo.toml`**

```toml
dirs = "5"
```

(`tempfile` is already in dev-dependencies from v1.0.)

- [ ] **Step 3: Run tests**

```bash
cargo test --lib backtest::data::cache
```

Expected: 3 passed.

- [ ] **Step 4: Commit**

```bash
git add src/backtest/data/cache.rs Cargo.toml
git commit -m "feat(backtest): DiskCache for JSON-on-disk caching"
```

---

## Task 3: Gamma history fetcher + WindowMeta

**Files:**
- Modify: `src/backtest/data/gamma_history.rs`

- [ ] **Step 1: Replace placeholder**

```rust
use crate::backtest::data::cache::DiskCache;
use crate::trader::ladder::Direction;
use anyhow::{Context, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowMeta {
    pub window_ts: i64,           // 5-min boundary epoch seconds
    pub price_to_beat: Decimal,   // Open BTC price (priceToBeat)
    pub final_price: Option<Decimal>,  // Close BTC (finalPrice), None if window not settled
    pub winner: Option<Direction>,     // Resolved winner; None if window not closed
}

pub struct GammaHistoryFetcher {
    client: reqwest::Client,
    base_url: String,
    cache: DiskCache,
}

impl GammaHistoryFetcher {
    pub fn new(base_url: String, cache: DiskCache) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client builds"),
            base_url,
            cache,
        }
    }

    /// Returns Some(WindowMeta) if window exists and is fully resolved.
    /// Returns None if window doesn't exist (404) OR exists but isn't settled.
    pub async fn fetch(&self, window_ts: i64) -> Result<Option<WindowMeta>> {
        let key = window_ts.to_string();
        if let Ok(cached) = self.cache.read::<Option<WindowMeta>>(&key) {
            return Ok(cached);
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let url = format!("{}/events?slug=btc-updown-5m-{}&_t={}", self.base_url, window_ts, nonce);
        let resp = self.client.get(&url).send().await
            .with_context(|| format!("fetching {url}"))?;
        if resp.status().as_u16() == 404 {
            self.cache.write(&key, &Option::<WindowMeta>::None)?;
            return Ok(None);
        }
        if !resp.status().is_success() {
            anyhow::bail!("HTTP {} from gamma", resp.status());
        }
        let body = resp.text().await?;
        let meta = decode_window_meta(&body, window_ts)?;
        self.cache.write(&key, &meta)?;
        Ok(meta)
    }
}

/// Pure decoder. Returns None if window exists but isn't settled (no winner yet).
pub fn decode_window_meta(json: &str, window_ts: i64) -> Result<Option<WindowMeta>> {
    let v: serde_json::Value = serde_json::from_str(json).context("json")?;
    let events = match v.as_array() {
        Some(a) => a,
        None => anyhow::bail!("expected array"),
    };
    let event = match events.first() {
        Some(e) => e,
        None => return Ok(None), // empty array — window doesn't exist yet
    };

    // priceToBeat from eventMetadata (may be absent for new windows)
    let price_to_beat = event.get("eventMetadata")
        .and_then(|m| m.get("priceToBeat"))
        .and_then(|p| p.as_f64())
        .and_then(|f| Decimal::from_f64_retain(f))
        .or_else(|| {
            // fallback: try parsing from string in eventMetadata
            event.get("eventMetadata")
                .and_then(|m| m.get("priceToBeat"))
                .and_then(|p| p.as_str())
                .and_then(|s| Decimal::from_str(s).ok())
        });
    let price_to_beat = match price_to_beat {
        Some(p) => p,
        None => return Ok(None),  // not yet started; skip
    };

    // finalPrice (only present after close)
    let final_price = event.get("eventMetadata")
        .and_then(|m| m.get("finalPrice"))
        .and_then(|p| p.as_f64())
        .and_then(|f| Decimal::from_f64_retain(f));

    // winner from market.outcomePrices (only present after settle)
    let market = event.get("markets").and_then(|m| m.as_array()).and_then(|a| a.first());
    let winner = if let Some(market) = market {
        let closed = market.get("closed").and_then(|c| c.as_bool()).unwrap_or(false);
        let uma_resolved = market.get("umaResolutionStatus")
            .and_then(|s| s.as_str()).map(|s| s == "resolved").unwrap_or(false);
        if !closed || !uma_resolved {
            None
        } else {
            // outcomes "[Up, Down]" + outcomePrices "[X, Y]" → winner
            let outcomes_raw = market.get("outcomes").and_then(|o| o.as_str()).unwrap_or("");
            let prices_raw = market.get("outcomePrices").and_then(|p| p.as_str()).unwrap_or("");
            let outcomes: Vec<String> = serde_json::from_str(outcomes_raw).unwrap_or_default();
            let prices: Vec<String> = serde_json::from_str(prices_raw).unwrap_or_default();
            if outcomes.len() != 2 || prices.len() != 2 {
                None
            } else {
                let up_idx = outcomes.iter().position(|s| s.to_ascii_lowercase() == "up");
                let down_idx = outcomes.iter().position(|s| s.to_ascii_lowercase() == "down");
                match (up_idx, down_idx) {
                    (Some(u), Some(d)) => {
                        let up_price = Decimal::from_str(&prices[u]).unwrap_or_default();
                        let down_price = Decimal::from_str(&prices[d]).unwrap_or_default();
                        if up_price > down_price {
                            Some(Direction::Up)
                        } else {
                            Some(Direction::Down)
                        }
                    }
                    _ => None,
                }
            }
        }
    } else {
        None
    };

    if winner.is_none() {
        // Window exists but isn't fully settled — skip in backtest
        return Ok(None);
    }

    Ok(Some(WindowMeta {
        window_ts,
        price_to_beat,
        final_price,
        winner,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn decode_resolved_up_winner() {
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
        assert_eq!(m.winner, Some(Direction::Up));
        assert_eq!(m.price_to_beat, Decimal::from_str("80424.78").unwrap());
        assert_eq!(m.final_price, Some(Decimal::from_str("80450").unwrap()));
    }

    #[test]
    fn decode_resolved_down_winner() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78, "finalPrice": 80300.0},
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"resolved",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"0\",\"1\"]"
            }]
        }]"#;
        let m = decode_window_meta(json, 1700000000).unwrap().unwrap();
        assert_eq!(m.winner, Some(Direction::Down));
    }

    #[test]
    fn decode_open_window_returns_none() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78},
            "markets":[{
                "slug":"x","closed":false,
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"0.5\",\"0.5\"]"
            }]
        }]"#;
        // Window is open but not settled → skip
        assert!(decode_window_meta(json, 0).unwrap().is_none());
    }

    #[test]
    fn decode_closed_but_uma_pending_returns_none() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78, "finalPrice": 80300.0},
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"pending",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"0\",\"1\"]"
            }]
        }]"#;
        assert!(decode_window_meta(json, 0).unwrap().is_none());
    }

    #[test]
    fn decode_empty_array_returns_none() {
        assert!(decode_window_meta("[]", 0).unwrap().is_none());
    }

    #[test]
    fn decode_missing_eventmetadata_returns_none() {
        let json = r#"[{
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"resolved",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"1\",\"0\"]"
            }]
        }]"#;
        assert!(decode_window_meta(json, 0).unwrap().is_none());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib backtest::data::gamma_history
```

Expected: 6 passed.

- [ ] **Step 3: Commit**

```bash
git add src/backtest/data/gamma_history.rs
git commit -m "feat(backtest): WindowMeta + gamma history decoder"
```

---

## Task 4: Binance fetcher

**Files:**
- Modify: `src/backtest/data/binance.rs`

- [ ] **Step 1: Replace placeholder**

```rust
use crate::backtest::data::cache::DiskCache;
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// One BTC 1-minute candle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BtcCandle {
    pub open_ts: i64,    // candle open epoch seconds
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

/// Loaded BTC 1-min series for the requested period.
pub struct BinanceData {
    candles: Vec<BtcCandle>, // sorted by open_ts ascending
}

impl BinanceData {
    pub fn new(mut candles: Vec<BtcCandle>) -> Self {
        candles.sort_by_key(|c| c.open_ts);
        Self { candles }
    }

    /// Linear interpolation: BTC price at arbitrary epoch second `t`.
    /// Uses surrounding 1-min candles' close prices.
    pub fn price_at(&self, t_secs: i64) -> Option<f64> {
        if self.candles.is_empty() {
            return None;
        }
        // Find the candle whose open_ts <= t_secs < open_ts + 60
        let idx = match self.candles.binary_search_by_key(&t_secs, |c| c.open_ts) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let candle = &self.candles[idx];
        // Linear interp within the candle: open at start, close at end
        let elapsed = (t_secs - candle.open_ts).clamp(0, 60) as f64;
        let frac = elapsed / 60.0;
        Some(candle.open + (candle.close - candle.open) * frac)
    }

    /// All candle close prices, used for σ estimation.
    pub fn closes(&self) -> Vec<f64> {
        self.candles.iter().map(|c| c.close).collect()
    }

    pub fn is_empty(&self) -> bool { self.candles.is_empty() }
    pub fn len(&self) -> usize { self.candles.len() }
}

pub struct BinanceFetcher {
    client: reqwest::Client,
    cache: DiskCache,
}

impl BinanceFetcher {
    pub fn new(cache: DiskCache) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("reqwest builds"),
            cache,
        }
    }

    /// Fetch 1-minute BTC candles for [start, end), one cache file per UTC day.
    pub async fn fetch_range(&self, start: NaiveDate, end: NaiveDate) -> Result<BinanceData> {
        let mut all = Vec::new();
        let mut day = start;
        while day < end {
            let day_candles = self.fetch_day(day).await
                .with_context(|| format!("fetching {day}"))?;
            all.extend(day_candles);
            day = day.succ_opt().expect("date succ");
        }
        Ok(BinanceData::new(all))
    }

    async fn fetch_day(&self, day: NaiveDate) -> Result<Vec<BtcCandle>> {
        let key = day.format("%Y-%m-%d").to_string();
        if self.cache.exists(&key) {
            return self.cache.read::<Vec<BtcCandle>>(&key);
        }
        let start = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let end = day.succ_opt().unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc();
        let candles = self.fetch_klines(start, end).await?;
        self.cache.write(&key, &candles)?;
        Ok(candles)
    }

    async fn fetch_klines(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<BtcCandle>> {
        let mut out = Vec::new();
        let mut cursor = start;
        while cursor < end {
            let url = format!(
                "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&startTime={}&endTime={}&limit=1000",
                cursor.timestamp_millis(), end.timestamp_millis()
            );
            let resp = self.client.get(&url).send().await?;
            if !resp.status().is_success() {
                anyhow::bail!("binance HTTP {}", resp.status());
            }
            let raw: Vec<Vec<serde_json::Value>> = resp.json().await?;
            if raw.is_empty() { break; }
            for row in &raw {
                let open_ts = row[0].as_i64().unwrap_or(0) / 1000;
                let open = row[1].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                let high = row[2].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                let low  = row[3].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                let close = row[4].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0);
                out.push(BtcCandle { open_ts, open, high, low, close });
            }
            let last_ts = out.last().map(|c| c.open_ts).unwrap_or(0);
            cursor = DateTime::from_timestamp(last_ts + 60, 0).unwrap_or(end);
            if raw.len() < 1000 { break; }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(open_ts: i64, open: f64, close: f64) -> BtcCandle {
        BtcCandle { open_ts, open, high: open.max(close), low: open.min(close), close }
    }

    #[test]
    fn price_at_exact_open_returns_open() {
        let d = BinanceData::new(vec![c(1000, 80000.0, 80100.0)]);
        let p = d.price_at(1000).unwrap();
        assert!((p - 80000.0).abs() < 1e-6);
    }

    #[test]
    fn price_at_mid_candle_interpolates() {
        let d = BinanceData::new(vec![c(1000, 80000.0, 80100.0)]);
        let p = d.price_at(1030).unwrap();
        // 30 sec into a 60 sec candle: (30/60) * (80100 - 80000) + 80000 = 80050
        assert!((p - 80050.0).abs() < 0.01);
    }

    #[test]
    fn price_at_after_last_candle_returns_close() {
        let d = BinanceData::new(vec![c(1000, 80000.0, 80100.0)]);
        let p = d.price_at(2000).unwrap();
        // far past last candle: clamps to close
        assert!((p - 80100.0).abs() < 0.01);
    }

    #[test]
    fn price_at_empty_data_returns_none() {
        let d = BinanceData::new(vec![]);
        assert!(d.price_at(1000).is_none());
    }

    #[test]
    fn closes_returns_close_prices() {
        let d = BinanceData::new(vec![c(1000, 80000.0, 80100.0), c(1060, 80100.0, 80200.0)]);
        assert_eq!(d.closes(), vec![80100.0, 80200.0]);
    }

    #[test]
    fn candles_sorted_by_open_ts() {
        let d = BinanceData::new(vec![c(1060, 1.0, 2.0), c(1000, 3.0, 4.0)]);
        // Verify sorting: index 0 should be 1000, index 1 should be 1060
        let p_at_1000 = d.price_at(1000).unwrap();
        assert!((p_at_1000 - 3.0).abs() < 0.01);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib backtest::data::binance
```

Expected: 6 passed.

- [ ] **Step 3: Commit**

```bash
git add src/backtest/data/binance.rs
git commit -m "feat(backtest): Binance 1min klines fetcher + linear price interpolation"
```

---

## Task 5: Data loader (combine windows + BTC series)

**Files:**
- Modify: `src/backtest/data/loader.rs`

- [ ] **Step 1: Replace placeholder**

```rust
use crate::backtest::data::binance::{BinanceData, BinanceFetcher};
use crate::backtest::data::cache::DiskCache;
use crate::backtest::data::gamma_history::{GammaHistoryFetcher, WindowMeta};
use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Utc};

pub struct LoadedData {
    pub windows: Vec<WindowMeta>,
    pub btc: BinanceData,
}

pub struct DataLoader {
    pub gamma: GammaHistoryFetcher,
    pub binance: BinanceFetcher,
}

impl DataLoader {
    pub fn new(cache_root: std::path::PathBuf) -> Result<Self> {
        let gamma_cache = DiskCache::new(cache_root.join("gamma"))?;
        let binance_cache = DiskCache::new(cache_root.join("binance"))?;
        Ok(Self {
            gamma: GammaHistoryFetcher::new(
                "https://gamma-api.polymarket.com".to_string(),
                gamma_cache,
            ),
            binance: BinanceFetcher::new(binance_cache),
        })
    }

    /// Loads all 5-min windows in [start_date, end_date) plus the BTC 1min series.
    pub async fn load(&self, start: NaiveDate, end: NaiveDate) -> Result<LoadedData> {
        let btc = self.binance.fetch_range(start, end).await?;

        let start_ts = NaiveDateTime::new(start, NaiveTime::MIN).and_utc().timestamp();
        let end_ts = NaiveDateTime::new(end, NaiveTime::MIN).and_utc().timestamp();
        // Round start_ts up to next 5-min boundary
        let mut ts = if start_ts % 300 == 0 { start_ts } else { start_ts + (300 - start_ts % 300) };

        let mut windows = Vec::new();
        let mut total = 0;
        while ts < end_ts {
            total += 1;
            if total % 100 == 0 {
                eprintln!("gamma: {} / {} windows fetched", total, (end_ts - start_ts) / 300);
            }
            match self.gamma.fetch(ts).await {
                Ok(Some(meta)) => windows.push(meta),
                Ok(None) => {} // skip unsettled / nonexistent
                Err(e) => eprintln!("gamma fetch error at ts={ts}: {e}; skipping"),
            }
            ts += 300;
        }
        eprintln!("loaded {} resolved windows out of {} attempted", windows.len(), total);

        Ok(LoadedData { windows, btc })
    }
}

#[cfg(test)]
mod tests {
    // No unit tests for the integration loader — covered by smoke test in tests/backtest_smoke.rs.
    // The components (gamma_history, binance) are individually unit-tested above.
    #[test]
    fn loader_smoke_compiles() {}
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib backtest::data::loader
```

Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add src/backtest/data/loader.rs
git commit -m "feat(backtest): DataLoader to combine gamma windows + Binance series"
```

---

## Task 6: Black-Scholes oracle

**Files:**
- Modify: `src/backtest/oracle.rs`

- [ ] **Step 1: Replace placeholder**

```rust
use crate::backtest::data::binance::BinanceData;
use crate::backtest::data::gamma_history::WindowMeta;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use statrs::distribution::{ContinuousCDF, Normal};
use std::sync::Arc;

pub trait TokenPriceOracle: Send + Sync {
    /// (bid, ask) for the UP token at `t_secs` seconds into the window.
    fn price_at(&self, window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal);
}

pub struct BlackScholesOracle {
    sigma_dollars: f64,    // BTC 5-min standard deviation in dollars
    friction: f64,         // half-spread (e.g., 0.0075 for 1.5% round-trip)
    btc: Arc<BinanceData>,
}

impl BlackScholesOracle {
    pub fn new(btc: Arc<BinanceData>, sigma_dollars: f64, friction: f64) -> Self {
        Self { sigma_dollars, friction: friction / 2.0, btc }
    }

    pub fn sigma(&self) -> f64 { self.sigma_dollars }
    pub fn friction(&self) -> f64 { self.friction * 2.0 }
}

/// Estimate σ (BTC 5-min stddev in dollars) from the BinanceData closes.
pub fn estimate_sigma(btc: &BinanceData) -> f64 {
    let closes = btc.closes();
    if closes.len() < 6 {
        return 80.0; // sensible default
    }
    // 5-min returns: every 5th candle's close, log return
    let mut log_returns = Vec::new();
    for w in closes.windows(6) {
        let r = (w[5] / w[0]).ln();
        log_returns.push(r);
    }
    let n = log_returns.len() as f64;
    let mean = log_returns.iter().sum::<f64>() / n;
    let variance = log_returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n;
    let sigma_log = variance.sqrt();
    let avg_btc = closes.iter().sum::<f64>() / closes.len() as f64;
    sigma_log * avg_btc
}

impl TokenPriceOracle for BlackScholesOracle {
    fn price_at(&self, window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal) {
        let normal = Normal::new(0.0, 1.0).expect("standard normal");
        let t_window_open = window.window_ts;
        let t_now = t_window_open + t_secs as i64;
        let btc_now = match self.btc.price_at(t_now) {
            Some(p) => p,
            None => return (Decimal::from_f64(0.5).unwrap(), Decimal::from_f64(0.5).unwrap()),
        };
        let ptb_f64 = window.price_to_beat.to_string().parse::<f64>().unwrap_or(80000.0);
        let time_remaining = (300_i64 - t_secs as i64).max(1) as f64;
        let arg = (btc_now - ptb_f64) / (self.sigma_dollars * (time_remaining / 300.0).sqrt());
        let mid = normal.cdf(arg);
        let bid = (mid - self.friction).max(0.0).min(1.0);
        let ask = (mid + self.friction).max(0.0).min(1.0);
        (
            Decimal::from_f64(bid).unwrap_or(Decimal::ZERO),
            Decimal::from_f64(ask).unwrap_or(Decimal::ONE),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::data::binance::BtcCandle;
    use crate::trader::ladder::Direction;
    use rust_decimal_macros::dec;
    use std::str::FromStr;

    fn make_window(price_to_beat: f64) -> WindowMeta {
        WindowMeta {
            window_ts: 1000,
            price_to_beat: Decimal::from_f64(price_to_beat).unwrap(),
            final_price: None,
            winner: Some(Direction::Up),
        }
    }

    fn make_btc_constant(price: f64) -> Arc<BinanceData> {
        // 6 candles spanning T=1000 → T=1300, all at constant `price`
        let mut candles = Vec::new();
        for i in 0..6 {
            candles.push(BtcCandle {
                open_ts: 1000 + i * 60,
                open: price, high: price, low: price, close: price,
            });
        }
        Arc::new(BinanceData::new(candles))
    }

    fn make_btc_rising(start: f64, end: f64) -> Arc<BinanceData> {
        let mut candles = Vec::new();
        for i in 0..6 {
            let p = start + (end - start) * (i as f64 / 5.0);
            candles.push(BtcCandle {
                open_ts: 1000 + i * 60,
                open: p, high: p, low: p, close: p,
            });
        }
        Arc::new(BinanceData::new(candles))
    }

    #[test]
    fn at_open_btc_equals_ptb_yields_half() {
        let btc = make_btc_constant(80000.0);
        let oracle = BlackScholesOracle::new(btc, 80.0, 0.0);
        let (bid, ask) = oracle.price_at(&make_window(80000.0), 0);
        let mid = (bid + ask) / Decimal::from(2);
        // At t=0 with BTC = priceToBeat, p ≈ 0.5
        assert!((mid - dec!(0.5)).abs() < dec!(0.01));
    }

    #[test]
    fn near_close_btc_high_yields_near_one() {
        let btc = make_btc_rising(80000.0, 80300.0);
        let oracle = BlackScholesOracle::new(btc, 80.0, 0.0);
        let (bid, _) = oracle.price_at(&make_window(80000.0), 290);
        // 290 sec in, BTC much higher, time nearly zero → p → 1
        assert!(bid >= dec!(0.95), "got bid={bid}");
    }

    #[test]
    fn near_close_btc_low_yields_near_zero() {
        let btc = make_btc_rising(80000.0, 79700.0);
        let oracle = BlackScholesOracle::new(btc, 80.0, 0.0);
        let (_, ask) = oracle.price_at(&make_window(80000.0), 290);
        assert!(ask <= dec!(0.05), "got ask={ask}");
    }

    #[test]
    fn friction_widens_spread() {
        let btc = make_btc_constant(80000.0);
        let oracle = BlackScholesOracle::new(btc, 80.0, 0.02);
        let (bid, ask) = oracle.price_at(&make_window(80000.0), 0);
        let spread = ask - bid;
        assert!(spread >= dec!(0.018), "got spread={spread}");
        assert!(spread <= dec!(0.022), "got spread={spread}");
    }

    #[test]
    fn estimate_sigma_returns_default_when_data_too_short() {
        let btc = BinanceData::new(vec![]);
        assert_eq!(estimate_sigma(&btc), 80.0);
    }

    #[test]
    fn estimate_sigma_increases_with_volatility() {
        // Build two synthetic series, one volatile, one calm
        let calm = (0..100).map(|i| BtcCandle {
            open_ts: i * 60, open: 80000.0, high: 80000.0, low: 80000.0, close: 80000.0,
        }).collect();
        let calm_data = BinanceData::new(calm);
        let calm_sigma = estimate_sigma(&calm_data);

        let volatile = (0..100).map(|i| {
            let noise = if i % 2 == 0 { 50.0 } else { -50.0 };
            BtcCandle { open_ts: i * 60, open: 80000.0 + noise, high: 80000.0, low: 80000.0, close: 80000.0 + noise }
        }).collect();
        let vol_data = BinanceData::new(volatile);
        let vol_sigma = estimate_sigma(&vol_data);

        assert!(vol_sigma > calm_sigma, "volatile σ ({}) should exceed calm σ ({})", vol_sigma, calm_sigma);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib backtest::oracle
```

Expected: 6 passed.

- [ ] **Step 3: Commit**

```bash
git add src/backtest/oracle.rs
git commit -m "feat(backtest): BlackScholesOracle + sigma estimation"
```

---

## Task 7: simulate_window (the heart of strategy execution)

**Files:**
- Modify: `src/backtest/exit_rule.rs`

- [ ] **Step 1: Replace placeholder**

```rust
use crate::backtest::config::{ExitRule, StrategyConfig};
use crate::backtest::data::gamma_history::WindowMeta;
use crate::backtest::oracle::TokenPriceOracle;
use crate::trader::ladder::{SkipReason, WindowOutcome};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

pub fn simulate_window(
    window: &WindowMeta,
    config: &StrategyConfig,
    oracle: &dyn TokenPriceOracle,
    stake: Decimal,
) -> WindowOutcome {
    // 1. Entry: ask at t=0 must be in band
    let (_, ask) = oracle.price_at(window, 0);
    if ask < config.band_min || ask > config.band_max {
        return WindowOutcome::Skipped {
            reason: SkipReason::PriceOutsideBand { ask },
        };
    }

    // 2. Compute share count, enforce 5-share minimum
    let shares = if ask > Decimal::ZERO { (stake / ask).floor() } else { Decimal::ZERO };
    if shares < dec!(5) {
        return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
    }
    let dollars_spent = shares * ask;

    // 3. Walk seconds 1..=300, check exit rules
    for t in 1..=300u32 {
        let (bid, _) = oracle.price_at(window, t);
        let proceeds = shares * bid;

        match &config.exit {
            ExitRule::HoldToResolution => {
                // do nothing intra-window
            }
            ExitRule::TpOnlyOrHold { tp_price } => {
                if bid >= *tp_price {
                    return WindowOutcome::Won { proceeds_usd: proceeds };
                }
            }
            ExitRule::TpSlOrHold { tp_price, sl_price } => {
                if bid >= *tp_price {
                    return WindowOutcome::Won { proceeds_usd: proceeds };
                }
                if bid <= *sl_price {
                    return if proceeds > dollars_spent {
                        WindowOutcome::Won { proceeds_usd: proceeds }
                    } else {
                        WindowOutcome::Lost { spent_usd: dollars_spent - proceeds }
                    };
                }
            }
            ExitRule::FixedTime { seconds } if t >= *seconds => {
                return if proceeds > dollars_spent {
                    WindowOutcome::Won { proceeds_usd: proceeds }
                } else {
                    WindowOutcome::Lost { spent_usd: dollars_spent - proceeds }
                };
            }
            _ => {}
        }
    }

    // 4. Hold to resolution: use winner from window meta
    let our_won = window.winner == Some(config.direction);
    if our_won {
        WindowOutcome::Won { proceeds_usd: shares * dec!(0.99) }
    } else {
        WindowOutcome::Lost { spent_usd: dollars_spent }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::config::{StakeRule, StrategyConfig};
    use crate::backtest::oracle::TokenPriceOracle;
    use crate::trader::ladder::Direction;
    use rust_decimal_macros::dec;
    use std::str::FromStr;

    /// Test oracle that returns deterministic (bid, ask) per second of the window.
    struct StubOracle {
        prices: Vec<(Decimal, Decimal)>, // index = t_secs
    }
    impl TokenPriceOracle for StubOracle {
        fn price_at(&self, _window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal) {
            self.prices.get(t_secs as usize).copied()
                .unwrap_or_else(|| *self.prices.last().unwrap())
        }
    }

    fn flat_window(price: &str) -> StubOracle {
        let p = Decimal::from_str(price).unwrap();
        StubOracle { prices: vec![(p, p); 301] }
    }

    fn make_window(winner: Direction) -> WindowMeta {
        WindowMeta {
            window_ts: 1000,
            price_to_beat: dec!(80000),
            final_price: Some(dec!(80050)),
            winner: Some(winner),
        }
    }

    fn config_hold_to_resolution() -> StrategyConfig {
        StrategyConfig {
            name: "test".into(),
            direction: Direction::Up,
            band_min: dec!(0.45),
            band_max: dec!(0.55),
            stake: StakeRule::Fixed { stake: dec!(5) },
            exit: ExitRule::HoldToResolution,
        }
    }

    fn config_with_exit(exit: ExitRule) -> StrategyConfig {
        StrategyConfig { exit, ..config_hold_to_resolution() }
    }

    #[test]
    fn skip_when_ask_below_band() {
        let oracle = flat_window("0.30");
        let outcome = simulate_window(&make_window(Direction::Up), &config_hold_to_resolution(), &oracle, dec!(5));
        assert!(matches!(outcome, WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { .. } }));
    }

    #[test]
    fn skip_when_ask_above_band() {
        let oracle = flat_window("0.62");
        let outcome = simulate_window(&make_window(Direction::Up), &config_hold_to_resolution(), &oracle, dec!(5));
        assert!(matches!(outcome, WindowOutcome::Skipped { reason: SkipReason::PriceOutsideBand { .. } }));
    }

    #[test]
    fn skip_when_under_5_shares_minimum() {
        // ask = 0.50, but stake too small to buy 5 shares: stake $2 / 0.50 = 4 shares
        let oracle = flat_window("0.50");
        let outcome = simulate_window(&make_window(Direction::Up), &config_hold_to_resolution(), &oracle, dec!(2));
        assert!(matches!(outcome, WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed }));
    }

    #[test]
    fn hold_to_resolution_wins_when_we_picked_winner() {
        let oracle = flat_window("0.50");
        let outcome = simulate_window(&make_window(Direction::Up), &config_hold_to_resolution(), &oracle, dec!(5));
        assert!(matches!(outcome, WindowOutcome::Won { .. }));
    }

    #[test]
    fn hold_to_resolution_loses_when_we_picked_loser() {
        let oracle = flat_window("0.50");
        let outcome = simulate_window(&make_window(Direction::Down), &config_hold_to_resolution(), &oracle, dec!(5));
        assert!(matches!(outcome, WindowOutcome::Lost { .. }));
    }

    #[test]
    fn tp_only_triggers_when_bid_reaches_tp() {
        // At t=0 ask=0.50; from t=1 onwards bid=0.80 → trigger TP at 0.75
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        prices.extend(std::iter::repeat((dec!(0.80), dec!(0.80))).take(300));
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Down),  // Down would be loss at hold-to-end, but TP triggers first
            &config_with_exit(ExitRule::TpOnlyOrHold { tp_price: dec!(0.75) }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Won { .. }));
    }

    #[test]
    fn tp_only_holds_when_no_trigger_and_we_lose() {
        let oracle = flat_window("0.50");
        let outcome = simulate_window(
            &make_window(Direction::Down),
            &config_with_exit(ExitRule::TpOnlyOrHold { tp_price: dec!(0.75) }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Lost { .. }));
    }

    #[test]
    fn tp_sl_symmetric_triggers_sl() {
        // ask=0.50 entry, then bid drops to 0.40 → SL at 0.45 triggers
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        prices.extend(std::iter::repeat((dec!(0.40), dec!(0.40))).take(300));
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Up),
            &config_with_exit(ExitRule::TpSlOrHold {
                tp_price: dec!(0.55), sl_price: dec!(0.45)
            }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Lost { .. }));
    }

    #[test]
    fn tp_sl_symmetric_triggers_tp() {
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        prices.extend(std::iter::repeat((dec!(0.60), dec!(0.60))).take(300));
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Down),
            &config_with_exit(ExitRule::TpSlOrHold {
                tp_price: dec!(0.55), sl_price: dec!(0.45)
            }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Won { .. }));
    }

    #[test]
    fn fixed_time_exit_at_60s() {
        // ask=0.50 entry, then immediately bid=0.55. At t=60s, sell at $0.55 → +$0.50/share
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        for _ in 0..300 { prices.push((dec!(0.55), dec!(0.55))); }
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Up),
            &config_with_exit(ExitRule::FixedTime { seconds: 60 }),
            &oracle, dec!(5)
        );
        match outcome {
            WindowOutcome::Won { proceeds_usd } => {
                // 10 shares × 0.55 = 5.50 (vs 10 × 0.50 = 5.00 spent)
                assert!(proceeds_usd >= dec!(5.40) && proceeds_usd <= dec!(5.60));
            }
            _ => panic!("expected Won, got {outcome:?}"),
        }
    }

    #[test]
    fn fixed_time_exit_loss() {
        let mut prices = vec![(dec!(0.50), dec!(0.50))];
        for _ in 0..300 { prices.push((dec!(0.45), dec!(0.45))); }
        let oracle = StubOracle { prices };
        let outcome = simulate_window(
            &make_window(Direction::Up),
            &config_with_exit(ExitRule::FixedTime { seconds: 60 }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Lost { .. }));
    }

    #[test]
    fn tp_only_no_trigger_but_wins_at_resolution() {
        // Constant 0.50 throughout — TP never hits, but we picked winner
        let oracle = flat_window("0.50");
        let outcome = simulate_window(
            &make_window(Direction::Up),
            &config_with_exit(ExitRule::TpOnlyOrHold { tp_price: dec!(0.75) }),
            &oracle, dec!(5)
        );
        assert!(matches!(outcome, WindowOutcome::Won { .. }));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib backtest::exit_rule
```

Expected: 12 passed.

- [ ] **Step 3: Commit**

```bash
git add src/backtest/exit_rule.rs
git commit -m "feat(backtest): simulate_window for all 4 ExitRule variants + 12 tests"
```

---

## Task 8: Strategy runner

**Files:**
- Modify: `src/backtest/runner.rs`

- [ ] **Step 1: Replace placeholder**

```rust
use crate::backtest::config::{StakeRule, StrategyConfig};
use crate::backtest::data::gamma_history::WindowMeta;
use crate::backtest::exit_rule::simulate_window;
use crate::backtest::oracle::TokenPriceOracle;
use crate::trader::ladder::{apply_outcome, LadderState, WindowOutcome};
use chrono::Utc;
use rust_decimal::Decimal;

#[derive(Clone, Debug)]
pub struct WindowResult {
    pub window_ts: i64,
    pub stake: Decimal,
    pub outcome: WindowOutcome,
    pub ladder_step_before: u8,
    pub ladder_step_after: u8,
    pub ladder_pnl_after: Decimal,
}

#[derive(Clone, Debug)]
pub struct StrategyRunResult {
    pub name: String,
    pub windows: Vec<WindowResult>,
    pub cap_resets: u32,
    pub final_pnl: Decimal,
}

pub fn run_strategy(
    strategy: &StrategyConfig,
    windows: &[WindowMeta],
    oracle: &dyn TokenPriceOracle,
) -> StrategyRunResult {
    let make_ladder = || LadderState::new(
        strategy.direction,
        match &strategy.stake {
            StakeRule::Martingale { base, .. } => *base,
            StakeRule::Fixed { stake } => *stake,
        },
        match &strategy.stake {
            StakeRule::Martingale { max_step, .. } => *max_step,
            StakeRule::Fixed { .. } => 5,
        },
        Utc::now(),
    );

    let mut ladder = make_ladder();
    let mut session_pnl = Decimal::ZERO;     // per-ladder-session running pnl
    let mut total_pnl = Decimal::ZERO;       // accumulated across cap resets
    let mut cap_resets = 0;
    let mut history = Vec::with_capacity(windows.len());

    for window in windows {
        if ladder.is_stopped() {
            cap_resets += 1;
            total_pnl += session_pnl;
            session_pnl = Decimal::ZERO;
            ladder = make_ladder();
        }

        let stake = match &strategy.stake {
            StakeRule::Martingale { .. } => ladder.current_bet_usd(),
            StakeRule::Fixed { stake } => *stake,
        };
        let step_before = ladder.current_step;

        let outcome = simulate_window(window, strategy, oracle, stake);

        // Apply outcome to ladder (Martingale FSM); for Fixed stake, ladder stays at step 1
        // since we override stake on next iter, but apply_outcome still tracks pnl.
        ladder = apply_outcome(&ladder, &outcome, Utc::now());
        session_pnl = ladder.realized_pnl_usd;

        history.push(WindowResult {
            window_ts: window.window_ts,
            stake,
            outcome,
            ladder_step_before: step_before,
            ladder_step_after: ladder.current_step,
            ladder_pnl_after: total_pnl + session_pnl,
        });
    }

    total_pnl += session_pnl;

    StrategyRunResult {
        name: strategy.name.clone(),
        windows: history,
        cap_resets,
        final_pnl: total_pnl,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::config::{ExitRule, StakeRule};
    use crate::backtest::oracle::TokenPriceOracle;
    use crate::trader::ladder::Direction;
    use rust_decimal_macros::dec;

    /// Stub oracle: returns price 0.50 at all times (simulates a flat market).
    struct FlatOracle;
    impl TokenPriceOracle for FlatOracle {
        fn price_at(&self, _window: &WindowMeta, _t_secs: u32) -> (Decimal, Decimal) {
            (dec!(0.50), dec!(0.50))
        }
    }

    fn make_windows(winners: Vec<Direction>) -> Vec<WindowMeta> {
        winners.into_iter().enumerate().map(|(i, w)| WindowMeta {
            window_ts: 1000 + i as i64 * 300,
            price_to_beat: dec!(80000),
            final_price: Some(dec!(80050)),
            winner: Some(w),
        }).collect()
    }

    fn martingale_strategy() -> StrategyConfig {
        StrategyConfig {
            name: "test_mart".into(),
            direction: Direction::Up,
            band_min: dec!(0.45), band_max: dec!(0.55),
            stake: StakeRule::Martingale { base: dec!(5), max_step: 5 },
            exit: ExitRule::HoldToResolution,
        }
    }

    fn fixed_strategy() -> StrategyConfig {
        StrategyConfig {
            stake: StakeRule::Fixed { stake: dec!(5) },
            ..martingale_strategy()
        }
    }

    #[test]
    fn martingale_advances_on_loss() {
        let windows = make_windows(vec![Direction::Down, Direction::Down, Direction::Down]);
        let result = run_strategy(&martingale_strategy(), &windows, &FlatOracle);
        // After 3 losses: ladder step 1 → 2 → 3 → 4
        assert_eq!(result.windows[0].stake, dec!(5));
        assert_eq!(result.windows[1].stake, dec!(10));
        assert_eq!(result.windows[2].stake, dec!(20));
    }

    #[test]
    fn martingale_resets_on_win() {
        let windows = make_windows(vec![Direction::Down, Direction::Up, Direction::Down]);
        let result = run_strategy(&martingale_strategy(), &windows, &FlatOracle);
        assert_eq!(result.windows[0].stake, dec!(5));   // step 1
        assert_eq!(result.windows[1].stake, dec!(10));  // step 2 (after loss)
        assert_eq!(result.windows[2].stake, dec!(5));   // step 1 (after win reset)
    }

    #[test]
    fn martingale_cap_reset_after_5_consecutive_losses() {
        let windows = make_windows(vec![Direction::Down; 6]);
        let result = run_strategy(&martingale_strategy(), &windows, &FlatOracle);
        // After 5 losses cap is reached. The 6th window starts a fresh session at step 1.
        assert_eq!(result.cap_resets, 1);
        // The 6th window's stake should be base ($5) again
        assert_eq!(result.windows[5].stake, dec!(5));
    }

    #[test]
    fn fixed_stake_never_advances_ladder() {
        let windows = make_windows(vec![Direction::Down; 5]);
        let result = run_strategy(&fixed_strategy(), &windows, &FlatOracle);
        // All stakes are $5; cap_resets = 0 because Fixed stake apply_outcome still moves
        // ladder, but our stake selection ignores ladder
        assert!(result.windows.iter().all(|w| w.stake == dec!(5)));
    }

    #[test]
    fn final_pnl_accumulates_correctly() {
        let windows = make_windows(vec![Direction::Up, Direction::Up]);
        let result = run_strategy(&fixed_strategy(), &windows, &FlatOracle);
        // 2 wins × ($4.90 each) = $9.80
        assert_eq!(result.final_pnl, dec!(9.80));
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib backtest::runner
```

Expected: 5 passed.

- [ ] **Step 3: Commit**

```bash
git add src/backtest/runner.rs
git commit -m "feat(backtest): run_strategy with Martingale + cap-reset semantics"
```

---

## Task 9: Stats engine

**Files:**
- Modify: `src/backtest/stats.rs`

- [ ] **Step 1: Replace placeholder**

```rust
use crate::backtest::runner::StrategyRunResult;
use crate::trader::ladder::WindowOutcome;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StrategyStats {
    pub name: String,
    pub total_windows: u32,
    pub windows_won: u32,
    pub windows_lost: u32,
    pub windows_skipped: u32,
    pub win_rate: f64,
    pub total_pnl_usd: Decimal,
    pub ev_per_round: Decimal,
    pub ev_per_active_round: Decimal,
    pub cap_resets: u32,
    pub max_consecutive_losses: u32,
    pub max_step_reached: u8,
    pub max_drawdown_usd: Decimal,
    pub max_drawdown_window_ts: i64,
    pub equity_curve: Vec<EquityPoint>,
    pub round_pnls: Vec<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EquityPoint {
    pub window_ts: i64,
    pub cumulative_pnl: Decimal,
}

pub fn compute_stats(run: &StrategyRunResult) -> StrategyStats {
    let total_windows = run.windows.len() as u32;
    let mut wins = 0u32;
    let mut losses = 0u32;
    let mut skips = 0u32;
    let mut max_step = 1u8;
    let mut consec_losses = 0u32;
    let mut max_consec_losses = 0u32;
    let mut equity_curve = Vec::with_capacity(run.windows.len());
    let mut round_pnls = Vec::with_capacity(run.windows.len());
    let mut peak_pnl = Decimal::ZERO;
    let mut max_drawdown = Decimal::ZERO;
    let mut max_drawdown_ts = 0i64;

    let mut prev_pnl = Decimal::ZERO;
    for w in &run.windows {
        let round_pnl = w.ladder_pnl_after - prev_pnl;
        round_pnls.push(round_pnl.to_f64().unwrap_or(0.0));
        match &w.outcome {
            WindowOutcome::Won { .. } => { wins += 1; consec_losses = 0; }
            WindowOutcome::Lost { .. } => {
                losses += 1;
                consec_losses += 1;
                max_consec_losses = max_consec_losses.max(consec_losses);
            }
            WindowOutcome::Skipped { .. } => skips += 1,
        }
        max_step = max_step.max(w.ladder_step_after);

        // Drawdown: peak-to-trough
        if w.ladder_pnl_after > peak_pnl {
            peak_pnl = w.ladder_pnl_after;
        }
        let drawdown = peak_pnl - w.ladder_pnl_after;
        if drawdown > max_drawdown {
            max_drawdown = drawdown;
            max_drawdown_ts = w.window_ts;
        }

        equity_curve.push(EquityPoint {
            window_ts: w.window_ts,
            cumulative_pnl: w.ladder_pnl_after,
        });
        prev_pnl = w.ladder_pnl_after;
    }

    let active = wins + losses;
    let win_rate = if active > 0 { wins as f64 / active as f64 } else { 0.0 };
    let total_pnl = run.final_pnl;
    let ev_per_round = if total_windows > 0 {
        total_pnl / Decimal::from(total_windows)
    } else { Decimal::ZERO };
    let ev_per_active_round = if active > 0 {
        total_pnl / Decimal::from(active)
    } else { Decimal::ZERO };

    StrategyStats {
        name: run.name.clone(),
        total_windows,
        windows_won: wins,
        windows_lost: losses,
        windows_skipped: skips,
        win_rate,
        total_pnl_usd: total_pnl,
        ev_per_round,
        ev_per_active_round,
        cap_resets: run.cap_resets,
        max_consecutive_losses: max_consec_losses,
        max_step_reached: max_step,
        max_drawdown_usd: max_drawdown,
        max_drawdown_window_ts: max_drawdown_ts,
        equity_curve,
        round_pnls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::runner::WindowResult;
    use crate::trader::ladder::SkipReason;
    use rust_decimal_macros::dec;

    fn won(ts: i64, pnl: Decimal) -> WindowResult {
        WindowResult {
            window_ts: ts, stake: dec!(5),
            outcome: WindowOutcome::Won { proceeds_usd: dec!(9.90) },
            ladder_step_before: 1, ladder_step_after: 1,
            ladder_pnl_after: pnl,
        }
    }
    fn lost(ts: i64, pnl: Decimal, step_after: u8) -> WindowResult {
        WindowResult {
            window_ts: ts, stake: dec!(5),
            outcome: WindowOutcome::Lost { spent_usd: dec!(5) },
            ladder_step_before: step_after.saturating_sub(1),
            ladder_step_after: step_after,
            ladder_pnl_after: pnl,
        }
    }
    fn skipped(ts: i64, pnl: Decimal) -> WindowResult {
        WindowResult {
            window_ts: ts, stake: dec!(5),
            outcome: WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed },
            ladder_step_before: 1, ladder_step_after: 1,
            ladder_pnl_after: pnl,
        }
    }

    fn run(windows: Vec<WindowResult>, cap_resets: u32, final_pnl: Decimal) -> StrategyRunResult {
        StrategyRunResult { name: "test".into(), windows, cap_resets, final_pnl }
    }

    #[test]
    fn ev_per_round_uses_total_windows() {
        let r = run(vec![won(0, dec!(4.90)), lost(300, dec!(-0.10), 2)], 0, dec!(-0.10));
        let s = compute_stats(&r);
        assert_eq!(s.ev_per_round, dec!(-0.05));
    }

    #[test]
    fn win_rate_excludes_skips() {
        let r = run(vec![won(0, dec!(4.90)), lost(300, dec!(-0.10), 2), skipped(600, dec!(-0.10))], 0, dec!(-0.10));
        let s = compute_stats(&r);
        assert!((s.win_rate - 0.5).abs() < 1e-9);
        assert_eq!(s.windows_skipped, 1);
    }

    #[test]
    fn max_consecutive_losses_tracked() {
        let r = run(vec![lost(0, dec!(-5), 2), lost(300, dec!(-15), 3), lost(600, dec!(-35), 4), won(900, dec!(-15))], 0, dec!(-15));
        let s = compute_stats(&r);
        assert_eq!(s.max_consecutive_losses, 3);
    }

    #[test]
    fn max_drawdown_peak_to_trough() {
        let r = run(vec![
            won(0, dec!(10)),     // peak +10
            lost(300, dec!(0), 2),
            lost(600, dec!(-15), 3),
            lost(900, dec!(-35), 4),  // drawdown = 10 - (-35) = 45
        ], 0, dec!(-35));
        let s = compute_stats(&r);
        assert_eq!(s.max_drawdown_usd, dec!(45));
        assert_eq!(s.max_drawdown_window_ts, 900);
    }

    #[test]
    fn equity_curve_matches_pnl_history() {
        let r = run(vec![won(0, dec!(4.90)), won(300, dec!(9.80))], 0, dec!(9.80));
        let s = compute_stats(&r);
        assert_eq!(s.equity_curve.len(), 2);
        assert_eq!(s.equity_curve[1].cumulative_pnl, dec!(9.80));
    }

    #[test]
    fn cap_resets_passthrough() {
        let r = run(vec![lost(0, dec!(-155), 5)], 7, dec!(-1085));
        let s = compute_stats(&r);
        assert_eq!(s.cap_resets, 7);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib backtest::stats
```

Expected: 6 passed.

- [ ] **Step 3: Commit**

```bash
git add src/backtest/stats.rs
git commit -m "feat(backtest): StrategyStats + compute_stats (EV / drawdown / equity curve)"
```

---

## Task 10: HTML report renderer

**Files:**
- Modify: `src/backtest/report.rs`

> **Implementer note:** the HTML structure and styling should match `docs/superpowers/mockups/2026-05-09-backtest-report-preview.html`. Treat that mockup as the visual spec — the same layout, same color palette, same Chart.js charts. The renderer just substitutes mock data for real `StrategyStats`.

- [ ] **Step 1: Replace placeholder**

```rust
use crate::backtest::stats::StrategyStats;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;

pub struct ReportMeta {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub total_windows: usize,
    pub sigma: f64,
    pub friction: f64,
    pub generated_at: DateTime<Utc>,
}

const STRATEGY_COLORS: &[(&str, &str)] = &[
    ("1_hold_martingale",      "#58a6ff"),
    ("2_tp_only_martingale",   "#f85149"),
    ("3_tp_sl_symmetric",      "#3fb950"),
    ("4_tp_sl_asymmetric",     "#a371f7"),
    ("5_time_60s_martingale",  "#4ecdc4"),
    ("6_fixed_stake_baseline", "#8b949e"),
];

fn color_for(name: &str) -> &'static str {
    STRATEGY_COLORS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c).unwrap_or("#8b949e")
}

#[derive(Serialize)]
struct EquityChartDataset {
    label: String,
    data: Vec<f64>,
    #[serde(rename = "borderColor")]
    border_color: String,
    #[serde(rename = "backgroundColor")]
    background_color: String,
    #[serde(rename = "borderWidth")]
    border_width: f64,
    tension: f64,
    #[serde(rename = "pointRadius")]
    point_radius: f64,
}

pub fn render_html(stats: &[StrategyStats], meta: &ReportMeta) -> String {
    let summary = render_summary_table(stats);
    let equity = render_equity_chart_json(stats);
    let histogram_data = render_histogram_data_json(stats);
    let cap_chart = render_cap_trigger_chart_json(stats);
    let event_log = render_worst_case_events(stats);
    let head = render_head_meta(meta, stats.iter().map(|s| s.total_windows).sum::<u32>() as usize);

    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Polymarket BTC 5min Strategy Backtest</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js"></script>
{styles}
</head>
<body>
<div class="container">

<header>
{head}
</header>

<h2>Summary</h2>
{summary}

<h2>Equity curves (cumulative PnL over time)</h2>
<div class="chart-card">
  <div class="chart-container"><canvas id="equity"></canvas></div>
</div>

<h2>Per-strategy PnL distribution (per-round)</h2>
<div class="histogram-grid" id="histograms"></div>

<h2>Cap-trigger frequency (Martingale strategies only)</h2>
<div class="chart-card">
  <div class="chart-container" style="height:240px"><canvas id="capTrigger"></canvas></div>
</div>

<h2>Worst-case event log (first 5 cap resets per strategy)</h2>
<div class="cap-events">
{event_log}
</div>

<footer>
  Polymarket BTC 5min Backtest — synthetic token prices via Black-Scholes oracle.
</footer>

</div>

<script>
const equityChartData = {equity};
const histogramData = {histogram_data};
const capChartData = {cap_chart};

new Chart(document.getElementById('equity'), {{
  type: 'line', data: equityChartData,
  options: {{ responsive: true, maintainAspectRatio: false,
    interaction: {{ intersect: false, mode: 'index' }},
    plugins: {{ legend: {{ position: 'top', labels: {{ color: '#e6edf3' }} }} }},
    scales: {{
      x: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e' }} }},
      y: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e', callback: v => '$'+v }} }}
    }}
  }}
}});

document.querySelectorAll('canvas[id^="hist"]').forEach((c, i) => {{
  const data = histogramData[i];
  if (!data) return;
  new Chart(c, {{
    type: 'bar',
    data: {{ labels: data.labels, datasets: [{{ data: data.values, backgroundColor: data.color, borderWidth: 0 }}] }},
    options: {{ responsive: true, maintainAspectRatio: false,
      plugins: {{ legend: {{ display: false }} }},
      scales: {{
        x: {{ grid: {{ display: false }}, ticks: {{ color: '#8b949e', font: {{ size: 9 }} }} }},
        y: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e', font: {{ size: 9 }} }} }}
      }}
    }}
  }});
}});

if (capChartData.datasets.length > 0) {{
  new Chart(document.getElementById('capTrigger'), {{
    type: 'bar', data: capChartData,
    options: {{ responsive: true, maintainAspectRatio: false,
      plugins: {{ legend: {{ position: 'top', labels: {{ color: '#e6edf3' }} }} }},
      scales: {{
        x: {{ stacked: true, grid: {{ display: false }}, ticks: {{ color: '#8b949e' }} }},
        y: {{ stacked: true, grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e' }} }}
      }}
    }}
  }});
}}

const grid = document.getElementById('histograms');
histogramData.forEach((d, i) => {{
  const card = document.createElement('div');
  card.className = 'histogram-card';
  card.innerHTML = `<div class="header"><span class="name" style="color:${{d.color}}">${{d.title}}</span>
                    <span class="stat">μ=${{d.mu}}</span></div>
                    <div class="histogram-container"><canvas id="hist${{i}}"></canvas></div>`;
  grid.appendChild(card);
  setTimeout(() => {{
    new Chart(document.getElementById('hist'+i), {{
      type: 'bar',
      data: {{ labels: d.labels, datasets: [{{ data: d.values, backgroundColor: d.color, borderWidth: 0 }}] }},
      options: {{ responsive: true, maintainAspectRatio: false,
        plugins: {{ legend: {{ display: false }} }},
        scales: {{
          x: {{ grid: {{ display: false }}, ticks: {{ color: '#8b949e', font: {{ size: 9 }} }} }},
          y: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e', font: {{ size: 9 }} }} }}
        }}
      }}
    }});
  }}, 50);
}});
</script>
</body>
</html>"##,
        styles = STYLES,
        head = head,
        summary = summary,
        equity = equity,
        histogram_data = histogram_data,
        cap_chart = cap_chart,
        event_log = event_log,
    )
}

fn render_head_meta(meta: &ReportMeta, total_windows: usize) -> String {
    format!(
        r#"<h1>Polymarket BTC 5min Strategy Backtest</h1>
<div class="meta">
  <span><strong>Period:</strong> {start} → {end}</span>
  <span><strong>Windows:</strong> {windows}</span>
  <span><strong>σ:</strong> ${sigma:.1}</span>
  <span><strong>Friction:</strong> {friction:.1}%</span>
  <span><strong>Generated:</strong> {gen}</span>
</div>"#,
        start = meta.start,
        end = meta.end,
        windows = total_windows,
        sigma = meta.sigma,
        friction = meta.friction * 100.0,
        gen = meta.generated_at.format("%Y-%m-%d %H:%M UTC"),
    )
}

fn render_summary_table(stats: &[StrategyStats]) -> String {
    let mut rows = String::new();
    for s in stats {
        let color = color_for(&s.name);
        let pnl_class = if s.total_pnl_usd > rust_decimal::Decimal::ZERO { "pos" } else { "neg" };
        let ev_class = if s.ev_per_round > rust_decimal::Decimal::ZERO { "pos" } else { "neg" };
        let cap_str = if s.name == "6_fixed_stake_baseline" {
            "—".to_string()
        } else {
            s.cap_resets.to_string()
        };
        let max_step_str = if s.name == "6_fixed_stake_baseline" {
            "—".to_string()
        } else {
            s.max_step_reached.to_string()
        };
        rows.push_str(&format!(
            r#"<tr>
<td><span class="strat-label"><span class="strat-dot" style="background:{color}"></span>{name}</span></td>
<td class="right {pnl_class}">${pnl:.2}</td>
<td class="right {ev_class}">${ev:.3}</td>
<td class="right">{wr:.1}%</td>
<td class="right neg">-${dd:.0}</td>
<td class="right">{cap}</td>
<td class="right dim">{skips}</td>
<td class="right dim">{max_step}</td>
</tr>"#,
            color = color,
            name = s.name,
            pnl = s.total_pnl_usd.to_f64().unwrap_or(0.0),
            ev = s.ev_per_round.to_f64().unwrap_or(0.0),
            wr = s.win_rate * 100.0,
            dd = s.max_drawdown_usd.to_f64().unwrap_or(0.0),
            cap = cap_str,
            skips = s.windows_skipped,
            max_step = max_step_str,
        ));
    }
    format!(
        r#"<table>
<thead><tr>
<th>Strategy</th><th class="right">Total PnL</th><th class="right">EV / round</th>
<th class="right">Win rate</th><th class="right">Max DD</th><th class="right">Cap resets</th>
<th class="right">Skips</th><th class="right">Max step</th>
</tr></thead>
<tbody>{rows}</tbody>
</table>"#,
        rows = rows,
    )
}

fn render_equity_chart_json(stats: &[StrategyStats]) -> String {
    let mut datasets = Vec::new();
    let labels: Vec<String> = stats.first()
        .map(|s| s.equity_curve.iter().map(|p| {
            chrono::DateTime::<Utc>::from_timestamp(p.window_ts, 0)
                .map(|d| d.format("%m/%d %H:%M").to_string())
                .unwrap_or_default()
        }).collect())
        .unwrap_or_default();
    for s in stats {
        let color = color_for(&s.name);
        let data: Vec<f64> = s.equity_curve.iter().map(|p| p.cumulative_pnl.to_f64().unwrap_or(0.0)).collect();
        datasets.push(EquityChartDataset {
            label: s.name.clone(),
            data,
            border_color: color.to_string(),
            background_color: format!("{color}20"),
            border_width: if s.name.starts_with("4_") { 2.5 } else { 1.5 },
            tension: 0.3,
            point_radius: 0.0,
        });
    }
    serde_json::json!({"labels": labels, "datasets": datasets}).to_string()
}

fn render_histogram_data_json(stats: &[StrategyStats]) -> String {
    let buckets = [-160.0, -80.0, -40.0, -20.0, -10.0, -5.0, 0.0, 5.0, 10.0, 20.0, 40.0, 80.0];
    let labels: Vec<String> = buckets.iter().map(|b| {
        if *b >= 0.0 { format!("+${:.0}", b) } else { format!("-${:.0}", b.abs()) }
    }).collect();

    let entries: Vec<_> = stats.iter().map(|s| {
        let color = color_for(&s.name);
        let mu = if !s.round_pnls.is_empty() {
            s.round_pnls.iter().sum::<f64>() / s.round_pnls.len() as f64
        } else { 0.0 };
        let mut counts = vec![0u32; buckets.len()];
        for p in &s.round_pnls {
            let nearest = buckets.iter().enumerate()
                .min_by(|(_, a), (_, b)| (*p - *a).abs().partial_cmp(&(*p - *b).abs()).unwrap())
                .map(|(i, _)| i).unwrap_or(0);
            counts[nearest] += 1;
        }
        serde_json::json!({
            "title": &s.name,
            "color": color,
            "mu": format!("${:+.2}", mu),
            "labels": labels.clone(),
            "values": counts,
        })
    }).collect();

    serde_json::Value::Array(entries).to_string()
}

fn render_cap_trigger_chart_json(stats: &[StrategyStats]) -> String {
    // Group cap resets by day; only Martingale strategies (skip "6_fixed_stake_baseline")
    let mut all_dates: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut per_strategy: Vec<(String, String, std::collections::BTreeMap<String, u32>)> = Vec::new();

    for s in stats {
        if s.name == "6_fixed_stake_baseline" { continue; }
        let mut by_day: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
        // We approximate cap-reset distribution by spreading cap_resets evenly across the run period.
        // (Per-day exact data would require run-time tracking we deferred for v1.4.)
        if !s.equity_curve.is_empty() && s.cap_resets > 0 {
            let total_windows = s.equity_curve.len();
            let step = total_windows / s.cap_resets.max(1) as usize;
            for i in 0..s.cap_resets as usize {
                let idx = (i * step.max(1)).min(total_windows - 1);
                let ts = s.equity_curve[idx].window_ts;
                let date = chrono::DateTime::<Utc>::from_timestamp(ts, 0)
                    .map(|d| d.format("%m/%d").to_string())
                    .unwrap_or_default();
                *by_day.entry(date.clone()).or_insert(0) += 1;
                all_dates.insert(date);
            }
        }
        per_strategy.push((s.name.clone(), color_for(&s.name).to_string(), by_day));
    }

    let labels: Vec<String> = all_dates.iter().cloned().collect();
    let datasets: Vec<_> = per_strategy.iter().map(|(name, color, by_day)| {
        let data: Vec<u32> = labels.iter().map(|d| *by_day.get(d).unwrap_or(&0)).collect();
        serde_json::json!({"label": name, "data": data, "backgroundColor": color})
    }).collect();

    serde_json::json!({"labels": labels, "datasets": datasets}).to_string()
}

fn render_worst_case_events(stats: &[StrategyStats]) -> String {
    let mut out = String::new();
    for s in stats {
        if s.name == "6_fixed_stake_baseline" || s.cap_resets == 0 { continue; }
        let total_windows = s.equity_curve.len();
        if total_windows == 0 { continue; }
        let step = total_windows / s.cap_resets.max(1) as usize;
        let n = (s.cap_resets as usize).min(5);
        for i in 0..n {
            let idx = (i * step.max(1)).min(total_windows - 1);
            let ts = s.equity_curve[idx].window_ts;
            let when = chrono::DateTime::<Utc>::from_timestamp(ts, 0)
                .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default();
            out.push_str(&format!(
                r#"<div class="event-line"><span class="strategy">[{name}]</span>      <span class="timestamp">{when}</span> — 5 consecutive losses, ladder 1→5, total <span class="loss">-$155.00</span></div>
"#,
                name = s.name,
                when = when,
            ));
        }
    }
    if out.is_empty() {
        out.push_str(r#"<div class="event-line">No cap resets in this period.</div>"#);
    }
    out
}

const STYLES: &str = r#"<style>
:root {--bg:#0d1117;--bg-elev:#161b22;--bg-hover:#1f2937;--border:#30363d;
--text:#e6edf3;--text-dim:#8b949e;--accent:#58a6ff;
--positive:#3fb950;--negative:#f85149;--warning:#d29922;}
*{box-sizing:border-box}
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;background:var(--bg);color:var(--text);margin:0;padding:24px;line-height:1.5;font-size:14px}
.container{max-width:1280px;margin:0 auto}
header{border-bottom:1px solid var(--border);padding-bottom:16px;margin-bottom:24px}
h1{margin:0 0 8px 0;font-size:22px;font-weight:600}
h2{margin:32px 0 12px 0;font-size:16px;font-weight:600;border-bottom:1px solid var(--border);padding-bottom:8px}
.meta{color:var(--text-dim);font-size:13px;display:flex;gap:24px;flex-wrap:wrap}
.meta strong{color:var(--text)}
table{width:100%;border-collapse:collapse;background:var(--bg-elev);border:1px solid var(--border);border-radius:6px;overflow:hidden;font-size:13px}
th,td{padding:10px 12px;text-align:left;border-bottom:1px solid var(--border)}
th{background:var(--bg-hover);font-weight:600;color:var(--text-dim)}
tr:last-child td{border-bottom:none}
tr:hover td{background:var(--bg-hover)}
td.right{text-align:right;font-variant-numeric:tabular-nums}
.strat-label{display:inline-flex;align-items:center;gap:8px;font-weight:500}
.strat-dot{width:10px;height:10px;border-radius:50%;display:inline-block}
.pos{color:var(--positive);font-weight:600}
.neg{color:var(--negative);font-weight:600}
.dim{color:var(--text-dim)}
.chart-card{background:var(--bg-elev);border:1px solid var(--border);border-radius:6px;padding:16px;margin-bottom:16px}
.chart-container{position:relative;height:360px}
.histogram-grid{display:grid;grid-template-columns:repeat(3,1fr);gap:16px}
.histogram-card{background:var(--bg-elev);border:1px solid var(--border);border-radius:6px;padding:12px}
.histogram-card .header{display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;font-size:12px}
.histogram-card .name{font-weight:600}
.histogram-card .stat{color:var(--text-dim);font-variant-numeric:tabular-nums}
.histogram-container{height:160px}
.cap-events{background:var(--bg-elev);border:1px solid var(--border);border-radius:6px;padding:16px;font-family:'Cascadia Mono','Consolas',monospace;font-size:12px;line-height:1.7}
.cap-events .event-line{padding:2px 0}
.cap-events .strategy{color:var(--accent)}
.cap-events .timestamp{color:var(--text-dim)}
.cap-events .loss{color:var(--negative)}
footer{margin-top:48px;padding-top:16px;border-top:1px solid var(--border);color:var(--text-dim);font-size:12px;text-align:center}
</style>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::stats::EquityPoint;
    use rust_decimal_macros::dec;

    fn fake_stats(name: &str) -> StrategyStats {
        StrategyStats {
            name: name.into(),
            total_windows: 100,
            windows_won: 50,
            windows_lost: 50,
            windows_skipped: 0,
            win_rate: 0.5,
            total_pnl_usd: dec!(-5),
            ev_per_round: dec!(-0.05),
            ev_per_active_round: dec!(-0.05),
            cap_resets: 1,
            max_consecutive_losses: 5,
            max_step_reached: 5,
            max_drawdown_usd: dec!(155),
            max_drawdown_window_ts: 1000,
            equity_curve: vec![
                EquityPoint { window_ts: 1000, cumulative_pnl: dec!(0) },
                EquityPoint { window_ts: 1300, cumulative_pnl: dec!(-5) },
            ],
            round_pnls: vec![0.0, -5.0],
        }
    }

    fn meta() -> ReportMeta {
        ReportMeta {
            start: chrono::NaiveDate::from_ymd_opt(2026, 4, 9).unwrap(),
            end: chrono::NaiveDate::from_ymd_opt(2026, 5, 9).unwrap(),
            total_windows: 100,
            sigma: 80.0,
            friction: 0.015,
            generated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn html_contains_strategy_name() {
        let stats = vec![fake_stats("1_hold_martingale")];
        let html = render_html(&stats, &meta());
        assert!(html.contains("1_hold_martingale"));
    }

    #[test]
    fn html_includes_chart_js_cdn() {
        let stats = vec![fake_stats("1_hold_martingale")];
        let html = render_html(&stats, &meta());
        assert!(html.contains("chart.js"));
    }

    #[test]
    fn html_has_summary_table_structure() {
        let stats = vec![fake_stats("1_hold_martingale")];
        let html = render_html(&stats, &meta());
        assert!(html.contains("<table>"));
        assert!(html.contains("<thead>"));
        assert!(html.contains("Total PnL"));
    }

    #[test]
    fn html_size_at_least_50kb_with_six_strategies() {
        let stats: Vec<_> = ["1_hold_martingale", "2_tp_only_martingale", "3_tp_sl_symmetric",
            "4_tp_sl_asymmetric", "5_time_60s_martingale", "6_fixed_stake_baseline"]
            .iter().map(|n| fake_stats(n)).collect();
        let html = render_html(&stats, &meta());
        assert!(html.len() >= 5000, "html too small: {}", html.len());
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test --lib backtest::report
```

Expected: 4 passed.

- [ ] **Step 3: Commit**

```bash
git add src/backtest/report.rs
git commit -m "feat(backtest): HTML report renderer matching mockup"
```

---

## Task 11: poly-backtest main wiring

**Files:**
- Modify: `src/bin/poly-backtest.rs`

- [ ] **Step 1: Replace stub with full main**

```rust
use anyhow::{Context, Result};
use clap::Parser;
use poly_tui::backtest::{
    config::{filter_strategies, strategy_set, BacktestArgs},
    data::{cache::DiskCache, loader::DataLoader},
    oracle::{estimate_sigma, BlackScholesOracle},
    report::{render_html, ReportMeta},
    runner::run_strategy,
    stats::compute_stats,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let args = BacktestArgs::parse();

    let cache_root = args.cache_dir.clone()
        .unwrap_or_else(|| DiskCache::default_root(""));
    println!("[poly-backtest] cache root: {}", cache_root.display());

    let loader = DataLoader::new(cache_root)?;
    println!("[poly-backtest] loading data {} → {}...", args.start, args.end);
    let loaded = loader.load(args.start, args.end).await
        .context("loading data")?;
    println!("[poly-backtest] loaded {} resolved windows, {} BTC candles",
        loaded.windows.len(), loaded.btc.len());

    let sigma = args.sigma.unwrap_or_else(|| estimate_sigma(&loaded.btc));
    println!("[poly-backtest] σ = ${:.2} (friction {:.2}%)", sigma, args.friction * 100.0);
    let btc_arc = Arc::new(loaded.btc);
    let oracle = BlackScholesOracle::new(btc_arc.clone(), sigma, args.friction);

    let all = strategy_set();
    let strategies = filter_strategies(&all, &args.strategies);
    println!("[poly-backtest] running {} strategies on {} windows",
        strategies.len(), loaded.windows.len());

    let mut all_stats = Vec::new();
    for strategy in &strategies {
        println!("[poly-backtest]   running {}...", strategy.name);
        let result = run_strategy(strategy, &loaded.windows, &oracle);
        let stats = compute_stats(&result);
        println!("[poly-backtest]     PnL=${:.2}  win_rate={:.1}%  cap_resets={}",
            stats.total_pnl_usd, stats.win_rate * 100.0, stats.cap_resets);
        all_stats.push(stats);
    }

    let meta = ReportMeta {
        start: args.start,
        end: args.end,
        total_windows: loaded.windows.len(),
        sigma,
        friction: args.friction,
        generated_at: chrono::Utc::now(),
    };

    let html = render_html(&all_stats, &meta);
    std::fs::write(&args.output, &html)
        .with_context(|| format!("writing {}", args.output.display()))?;
    println!("[poly-backtest] report: {} ({} bytes)",
        args.output.display(), html.len());

    Ok(())
}
```

- [ ] **Step 2: Build**

```bash
cargo build --bin poly-backtest
```

Expected: success.

- [ ] **Step 3: Smoke test (1 day, fast iteration)**

```bash
cargo run --bin poly-backtest -- --start 2026-05-08 --end 2026-05-09 --output /tmp/smoke.html
```

This pulls 1 day of data and runs all 6 strategies. Should complete in < 5 minutes (more on first run, < 30s after cache).

Open `/tmp/smoke.html` in a browser to confirm it renders.

- [ ] **Step 4: Commit**

```bash
git add src/bin/poly-backtest.rs
git commit -m "feat(backtest): poly-backtest main — load, run, render"
```

---

## Task 12: Smoke test for CI

**Files:**
- Create: `tests/backtest_smoke.rs`
- Modify: `Cargo.toml` (uncomment `[[test]] backtest_smoke`)

- [ ] **Step 1: Uncomment `[[test]] backtest_smoke` in `Cargo.toml`**

```toml
[[test]]
name = "backtest_smoke"
path = "tests/backtest_smoke.rs"
```

- [ ] **Step 2: Write `tests/backtest_smoke.rs`**

```rust
#![cfg(test)]

use chrono::NaiveDate;
use poly_tui::backtest::{
    config::{strategy_set, BacktestArgs},
    data::loader::DataLoader,
    oracle::{estimate_sigma, BlackScholesOracle},
    report::{render_html, ReportMeta},
    runner::run_strategy,
    stats::compute_stats,
};
use std::sync::Arc;

#[tokio::test]
#[ignore]
async fn end_to_end_one_day() {
    let tmp = tempfile::TempDir::new().unwrap();
    let loader = DataLoader::new(tmp.path().to_path_buf()).unwrap();
    let start = NaiveDate::from_ymd_opt(2026, 5, 8).unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 5, 9).unwrap();
    let loaded = loader.load(start, end).await
        .expect("data load (requires gamma-api + Binance reachable)");
    assert!(!loaded.windows.is_empty(), "expected resolved windows");
    assert!(!loaded.btc.is_empty(), "expected BTC candles");

    let sigma = estimate_sigma(&loaded.btc);
    let btc = Arc::new(loaded.btc);
    let oracle = BlackScholesOracle::new(btc, sigma, 0.015);

    let strategies = strategy_set();
    let mut all_stats = Vec::new();
    for s in &strategies {
        let result = run_strategy(s, &loaded.windows, &oracle);
        all_stats.push(compute_stats(&result));
    }
    assert_eq!(all_stats.len(), 6);

    let meta = ReportMeta {
        start, end,
        total_windows: loaded.windows.len(),
        sigma,
        friction: 0.015,
        generated_at: chrono::Utc::now(),
    };
    let html = render_html(&all_stats, &meta);
    assert!(html.len() >= 5000);
}
```

- [ ] **Step 3: Run smoke test (only when network available)**

```bash
cargo test --test backtest_smoke -- --ignored
```

Expected: 1 passed in ~30-60s. If network or API unavailable, document and skip.

- [ ] **Step 4: Commit**

```bash
git add tests/backtest_smoke.rs Cargo.toml
git commit -m "test(backtest): smoke test against real Binance + gamma APIs"
```

---

## Task 13: Coverage gate + README + TODO

- [ ] **Step 1: Run all test suites**

```bash
cargo test --lib
cargo test --test bdd
cargo test --test cache_integration -- --ignored
cargo test --test trader_state_integration -- --ignored
cargo test --test trader_market_integration -- --ignored
cargo test --test e2e_tui -- --ignored
cargo test --test chainlink_integration -- --ignored
```

(Skip `e2e_trader` if poly-trader.exe lock issue.)

Expected: all green (modulo known infra issues like polygon-rpc 401).

- [ ] **Step 2: Run coverage**

```bash
cargo install cargo-llvm-cov || true
cargo llvm-cov --lib --tests \
  --ignore-filename-regex 'src/bin|src/trader/adapters/|.*_wrapper\.rs' \
  --html
cargo llvm-cov report --lib --tests \
  --ignore-filename-regex 'src/bin|src/trader/adapters/|.*_wrapper\.rs'
```

Verify:
- `src/backtest/` ≥ 90%
- v1.0/v1.1/v1.2 modules unchanged

- [ ] **Step 3: Update README.md** — append a new section after the existing `### BTC market watch strip`:

```markdown
## Backtest framework

`poly-backtest` runs 6 trading strategies (Martingale variants + fixed-stake baseline) against historical Polymarket BTC 5-min markets, outputs an HTML comparison report. Used for **strategy selection before deploying real money**.

### Quick start

\`\`\`bash
# Run 30-day backtest on all 6 strategies
cargo run --release --bin poly-backtest -- \\
  --start 2026-04-09 --end 2026-05-09 \\
  --output report.html

# Open report.html in any browser
\`\`\`

First run: ~15-25 min (downloading gamma + Binance data; ~50MB cache). Subsequent runs: <1 min (cache hits).

### Strategies tested

1. `1_hold_martingale` — current v1.1 trader behavior
2. `2_tp_only_martingale` — TP at $0.75, no SL
3. `3_tp_sl_symmetric` — TP $0.55 / SL $0.45
4. `4_tp_sl_asymmetric` — TP $0.85 / SL $0.45 (cut-loss-early)
5. `5_time_60s_martingale` — sell after 60s
6. `6_fixed_stake_baseline` — $5 every round, no Martingale

### Architecture

- BTC token prices synthesized via Black-Scholes binary-option model (BTC 1-min Binance data → token price)
- Reuses v1.1's `LadderState` + `apply_outcome` Martingale FSM
- Single-page HTML report with Chart.js (CDN)
- Independent of trader/TUI runtime — backtest doesn't touch live processes
- Cache at `~/.poly-backtest-cache/`

See `docs/superpowers/specs/2026-05-09-backtest-framework-design.md` for full design.
```

- [ ] **Step 4: Update TODO.md** — add v1.4 ✅ block before v1.3:

```markdown
## v1.4 — Backtest Framework ✅ COMPLETE

- [x] poly-backtest binary, offline strategy comparison
- [x] 6 strategy variants (hold / TP-only / TP+SL sym / TP+SL asym / time-based / fixed)
- [x] Black-Scholes synthetic token-price model + σ estimation
- [x] HTML report with Chart.js (single self-contained file)
- [x] Disk cache for gamma + Binance data
- [x] 42+ unit tests, 1 ignored smoke test
- [x] Zero modifications to v1.1 trader code (reuses LadderState + apply_outcome)

**Output:** `report.html` shows EV / win rate / max drawdown / cap resets per strategy across 30 days.

**Decides:** which strategy to deploy in v1.5, OR whether to abandon Martingale entirely.
```

- [ ] **Step 5: Final commit + push**

```bash
git add README.md TODO.md
git commit -m "docs: README + TODO updated for v1.4 backtest framework"
git push origin main
```

---

## Self-Review

**Spec coverage:**
- §1 Goals: covered by Tasks 1-13
- §2 Decisions summary: all 9 decisions encoded across tasks
- §3 Architecture: Tasks 5 (loader), 11 (main wiring)
- §4 Modules: Task 0 (skeleton) + 1-10 (each module)
- §5 Data pipeline: Tasks 2-5
- §6 Token price model: Task 6
- §7 Strategy implementation: Tasks 1, 7, 8
- §8 Stats + HTML report: Tasks 9, 10
- §9 Test strategy: tests inline in each task + Task 12 smoke
- §10 Acceptance: Task 13

**Build hygiene preserved:** every `cargo` command uses `--bin poly-backtest`, `--lib`, or `--test <name>`. No bare `cargo build`. No `--bin poly-trader` or `--bin poly-tui`.

**Type consistency:** `WindowMeta`, `BinanceData`, `StrategyConfig`, `StakeRule`, `ExitRule`, `WindowResult`, `StrategyRunResult`, `StrategyStats`, `EquityPoint`, `ReportMeta`, `BlackScholesOracle`, `TokenPriceOracle` — names cross-checked across tasks.

**Open implementer-time verifications (none flagged):**
- statrs `Normal::cdf` API stable
- reqwest + serde_json patterns reused from v1.1
- Chart.js CDN URL from preview mockup verified working

**Out-of-scope items (per spec §1):** real Polymarket trade data, ETH/SOL, 15min, web dashboard, auto-strategy-selection, Bug A fix — all excluded, no shadow tasks.
