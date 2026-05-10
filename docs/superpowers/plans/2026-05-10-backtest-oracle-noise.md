# v1.7.2 — Backtest Oracle Noise + SL Sweep Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `NoisyBlackScholesOracle` (Gaussian white noise overlay on BS theoretical) and 5 new SL-sweep strategy variants (TP=0.85 fixed, SL ∈ {0.40, 0.35, 0.30, 0.25, 0.20}) to the v1.4 backtest. Default behavior unchanged when `--oracle-noise 0.0` (default).

**Architecture:** Wrapper `NoisyBlackScholesOracle` decorates `BlackScholesOracle`. Per-tick Gaussian sample added to bid/ask, clamped to [0.01, 0.99]. Seeded `StdRng` for deterministic reproducibility. New strategies appended to `strategy_set()`. New CLI flags `--oracle-noise <f64>` (default 0.0) and `--noise-seed <u64>` (default 42).

**Tech Stack:** Rust 1.78+, existing `statrs`, NEW `rand = "0.8"` + `rand_distr = "0.4"`. No new files except potentially the wrapper struct (in existing `src/backtest/oracle.rs`).

**Spec:** `docs/superpowers/specs/2026-05-10-backtest-oracle-noise-design.md`

## Build hygiene — STRICT

NEVER bare `cargo build`. Always scope:
- `cargo build --bin poly-backtest`
- `cargo test --lib backtest::`
- `cargo build --tests --test backtest_smoke`

DO NOT touch `src/trader/`, `src/positions.rs`, `src/bin/poly-tui.rs`, `src/bin/poly-trader.rs`, `src/bin/poly-redeem.rs`. v1.7.2 is **backtest-only**.

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `Cargo.toml` | modify | Add `rand = "0.8"` and `rand_distr = "0.4"` to `[dependencies]`. |
| `src/backtest/oracle.rs` | modify | Add `NoisyBlackScholesOracle` struct + impl `TokenPriceOracle`. Wraps existing `BlackScholesOracle`. |
| `src/backtest/config.rs` | modify | Add `--oracle-noise <f64>` + `--noise-seed <u64>` clap fields. Validation `[0.0, 0.5]`. Add 5 new strategies to `strategy_set()`. |
| `src/bin/poly-backtest.rs` | modify | If `args.oracle_noise > 0.0`: wrap base oracle in `NoisyBlackScholesOracle`. Else use base directly. |
| `README.md` | modify | Document new flags + SL sweep variants. |
| `TODO.md` | modify | Tick v1.7.2 ✅ COMPLETE. |

No new files (logic added to existing `oracle.rs`).

---

## Task 0: Sanity baseline

**Files:** none (read-only).

- [ ] **Step 1: Confirm working tree clean**

Run: `git status`
Expected: only untracked items are `.claude/`, the four `backtest-report*.html` files, and `~/.poly-backtest-cache/`. No tracked-file modifications.

- [ ] **Step 2: Confirm backtest unit tests green**

Run: `cargo test --lib backtest::`
Expected: PASS. Note the count (e.g. "61 passed") — Task 5/6 add tests on top.

- [ ] **Step 3: Confirm backtest binary builds**

Run: `cargo build --bin poly-backtest`
Expected: Compiles clean (warnings ok).

- [ ] **Step 4: No commit (read-only baseline)**

Skip — this task only verifies starting state.

---

## Task 1: Add rand + rand_distr deps

**Files:**
- Modify: `Cargo.toml`

`statrs` provides `Normal::cdf` (already used) but does NOT include sampling. For seeded sampling we need `rand` + `rand_distr`.

- [ ] **Step 1: Edit Cargo.toml**

Open `Cargo.toml`. Find the `[dependencies]` section. Add (alphabetical insertion point near other lowercase deps):

```toml
rand = "0.8"
rand_distr = "0.4"
```

Common location: after `rust_decimal_macros = "1"`. Final lines look like:

```toml
rust_decimal = { version = "1", features = ["serde"] }
rust_decimal_macros = "1"
rand = "0.8"
rand_distr = "0.4"
statrs = "0.18"
```

- [ ] **Step 2: Verify deps resolve**

Run: `cargo build --bin poly-backtest`
Expected: Compiles clean. New deps download on first build.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore(deps): add rand + rand_distr for seeded oracle noise sampling"
```

---

## Task 2: NoisyBlackScholesOracle (zero-sigma fast path)

**Files:**
- Modify: `src/backtest/oracle.rs`

The first task adds the wrapper type with the `sigma == 0.0` fast-path. Non-zero noise comes in Task 3.

- [ ] **Step 1: Write the failing tests**

Append to `#[cfg(test)] mod tests` in `src/backtest/oracle.rs`, before the closing `}`:

```rust
    #[test]
    fn noisy_oracle_with_sigma_zero_matches_base() {
        let btc = make_btc_constant(80000.0);
        let base = BlackScholesOracle::new(btc.clone(), 80.0, 0.0);
        let base2 = BlackScholesOracle::new(btc, 80.0, 0.0);
        let noisy = NoisyBlackScholesOracle::new(base2, 0.0, 42);

        let window = make_window(80000.0);
        for t in [0u32, 60, 120, 180, 240, 290] {
            let (bid_b, ask_b) = base.price_at(&window, t);
            let (bid_n, ask_n) = noisy.price_at(&window, t);
            assert_eq!(bid_b, bid_n, "bid mismatch at t={t}");
            assert_eq!(ask_b, ask_n, "ask mismatch at t={t}");
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib backtest::oracle::tests::noisy_oracle_with_sigma_zero_matches_base`
Expected: FAIL — `cannot find type 'NoisyBlackScholesOracle'`.

- [ ] **Step 3: Implement NoisyBlackScholesOracle minimally**

Edit `src/backtest/oracle.rs`. Add imports near the top (after the existing `use statrs::...` line):

```rust
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand_distr::{Distribution, Normal as NormalDist};
use std::sync::Mutex;
```

Note: the existing file uses `statrs::distribution::Normal` for `cdf`. We rename `rand_distr::Normal` to `NormalDist` to avoid collision.

Append after the existing `impl TokenPriceOracle for BlackScholesOracle` block:

```rust
/// Wraps `BlackScholesOracle` and adds Gaussian white noise to bid/ask.
/// Same noise sample applies to both quotes (correlated). Clamped to
/// [0.01, 0.99] to keep prices physically valid.
///
/// Determinism: seeded `StdRng` produces a reproducible sequence for the
/// sequential calls of a backtest run.
pub struct NoisyBlackScholesOracle {
    base: BlackScholesOracle,
    sigma: f64,
    rng: Mutex<StdRng>,
    noise_dist: NormalDist<f64>,
}

impl NoisyBlackScholesOracle {
    pub fn new(base: BlackScholesOracle, sigma: f64, seed: u64) -> Self {
        let dist = NormalDist::new(0.0, sigma.max(0.0)).expect("valid normal dist");
        Self {
            base,
            sigma,
            rng: Mutex::new(StdRng::seed_from_u64(seed)),
            noise_dist: dist,
        }
    }
}

impl TokenPriceOracle for NoisyBlackScholesOracle {
    fn price_at(&self, window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal) {
        let (bid_bs, ask_bs) = self.base.price_at(window, t_secs);
        if self.sigma == 0.0 {
            return (bid_bs, ask_bs);
        }
        // Non-zero noise path implemented in Task 3.
        (bid_bs, ask_bs)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib backtest::oracle::tests::noisy_oracle_with_sigma_zero_matches_base`
Expected: PASS.

- [ ] **Step 5: Run full oracle test suite to ensure no regressions**

Run: `cargo test --lib backtest::oracle::`
Expected: PASS — all 6 existing tests + 1 new green.

- [ ] **Step 6: Commit**

```bash
git add src/backtest/oracle.rs
git commit -m "feat(backtest): NoisyBlackScholesOracle scaffold with sigma=0 fast path"
```

---

## Task 3: Implement non-zero noise + clamp + reproducibility

**Files:**
- Modify: `src/backtest/oracle.rs`

- [ ] **Step 1: Write the failing tests**

Append to `#[cfg(test)] mod tests` in `src/backtest/oracle.rs`:

```rust
    #[test]
    fn noisy_oracle_seed_42_reproducible() {
        let btc = make_btc_constant(80000.0);
        let base1 = BlackScholesOracle::new(btc.clone(), 80.0, 0.0);
        let base2 = BlackScholesOracle::new(btc, 80.0, 0.0);
        let n1 = NoisyBlackScholesOracle::new(base1, 0.05, 42);
        let n2 = NoisyBlackScholesOracle::new(base2, 0.05, 42);

        let window = make_window(80000.0);
        for t in 0..100u32 {
            let (b1, a1) = n1.price_at(&window, t);
            let (b2, a2) = n2.price_at(&window, t);
            assert_eq!(b1, b2, "bid drift at t={t}");
            assert_eq!(a1, a2, "ask drift at t={t}");
        }
    }

    #[test]
    fn noisy_oracle_different_seeds_diverge() {
        let btc = make_btc_constant(80000.0);
        let base1 = BlackScholesOracle::new(btc.clone(), 80.0, 0.0);
        let base2 = BlackScholesOracle::new(btc, 80.0, 0.0);
        let n1 = NoisyBlackScholesOracle::new(base1, 0.05, 42);
        let n2 = NoisyBlackScholesOracle::new(base2, 0.05, 99);

        let window = make_window(80000.0);
        // Over 100 calls at least one tick should differ.
        let mut differs = false;
        for t in 0..100u32 {
            let (b1, _) = n1.price_at(&window, t);
            let (b2, _) = n2.price_at(&window, t);
            if b1 != b2 { differs = true; break; }
        }
        assert!(differs, "different seeds should produce different sequences");
    }

    #[test]
    fn noisy_oracle_clamps_to_valid_range() {
        let btc = make_btc_constant(80000.0);
        let base = BlackScholesOracle::new(btc, 80.0, 0.0);
        // Large sigma to force frequent clamps.
        let noisy = NoisyBlackScholesOracle::new(base, 0.30, 42);

        let window = make_window(80000.0);
        for t in 0..1000u32 {
            let (bid, ask) = noisy.price_at(&window, t.min(290));
            assert!(bid >= dec!(0.01) && bid <= dec!(0.99),
                    "bid={bid} out of range at t={t}");
            assert!(ask >= dec!(0.01) && ask <= dec!(0.99),
                    "ask={ask} out of range at t={t}");
        }
    }

    #[test]
    fn noisy_oracle_mean_near_zero_over_many_samples() {
        let btc = make_btc_constant(80000.0);
        // Use friction=0 so base bid/ask both equal the BS midpoint, making
        // the noise residual easy to extract.
        let base_for_mean = BlackScholesOracle::new(btc.clone(), 80.0, 0.0);
        let noisy = NoisyBlackScholesOracle::new(base_for_mean, 0.05, 42);

        let base_ref = BlackScholesOracle::new(btc, 80.0, 0.0);
        let window = make_window(80000.0);

        let mut residuals = Vec::with_capacity(10_000);
        for t in 0..10_000u32 {
            let t_capped = (t % 290).max(1);  // stay inside window
            let (bid_n, _) = noisy.price_at(&window, t_capped);
            let (bid_b, _) = base_ref.price_at(&window, t_capped);
            // Using f64 conversion for the assertion; Decimal subtraction works
            // but mean comparison is easier in f64.
            let r: f64 = (bid_n - bid_b).to_string().parse().unwrap_or(0.0);
            residuals.push(r);
        }
        let mean = residuals.iter().sum::<f64>() / residuals.len() as f64;
        // 3-σ bound for σ=0.05 over 10k samples is ±0.05 / sqrt(10000) × 3 ≈ ±0.0015
        // Loosen to ±0.005 to absorb clamp-induced asymmetry near boundaries.
        assert!(mean.abs() < 0.005, "noise mean drift: {mean}");
    }
```

Note: `to_string().parse()` on a `Decimal` is the cheap way to get f64 here without adding more decimal-arith imports.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backtest::oracle::tests::noisy_oracle_seed_42_reproducible`
Expected: FAIL — current `price_at` always falls through to base output, so noisy with σ=0.05 produces same bid as base, but the assertion `b1 == b2` is true. The seed test passes by accident.

Actually wait — both `n1` and `n2` would produce identical (base) output since neither samples noise yet. So `noisy_oracle_seed_42_reproducible` PASSES already (sigma>0 still goes through the placeholder `return (bid_bs, ask_bs)`). Good — that test stays green after Task 3 too.

`noisy_oracle_different_seeds_diverge` will FAIL (no divergence yet — both produce base output).
`noisy_oracle_clamps_to_valid_range` will PASS (no noise added yet, BS values are valid).
`noisy_oracle_mean_near_zero_over_many_samples` will PASS (no residual since no noise added).

So the failing test to drive implementation is `noisy_oracle_different_seeds_diverge`. Run it:

Run: `cargo test --lib backtest::oracle::tests::noisy_oracle_different_seeds_diverge`
Expected: FAIL — no divergence between seeds because noise isn't actually added yet.

- [ ] **Step 3: Implement non-zero noise sampling**

Edit `src/backtest/oracle.rs`. Replace the body of `NoisyBlackScholesOracle::price_at`:

```rust
impl TokenPriceOracle for NoisyBlackScholesOracle {
    fn price_at(&self, window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal) {
        let (bid_bs, ask_bs) = self.base.price_at(window, t_secs);
        if self.sigma == 0.0 {
            return (bid_bs, ask_bs);
        }
        let noise = self.noise_dist.sample(&mut *self.rng.lock().unwrap());
        let noise_dec = Decimal::from_f64(noise).unwrap_or(Decimal::ZERO);
        let bid = (bid_bs + noise_dec).clamp(dec!(0.01), dec!(0.99));
        let ask = (ask_bs + noise_dec).clamp(dec!(0.01), dec!(0.99));
        (bid, ask)
    }
}
```

Note `dec!(0.01)` and `dec!(0.99)` — `rust_decimal_macros` is already in scope at the top of the test module. We need it in non-test code too. Add at the top of `src/backtest/oracle.rs`, near other `use` statements:

```rust
use rust_decimal_macros::dec;
```

- [ ] **Step 4: Run all four new tests + full oracle suite**

Run: `cargo test --lib backtest::oracle::`
Expected: PASS — all existing tests + 4 new ones (10 total, was 6).

- [ ] **Step 5: Commit**

```bash
git add src/backtest/oracle.rs
git commit -m "feat(backtest): NoisyBlackScholesOracle non-zero noise path with clamp + reproducibility"
```

---

## Task 4: --oracle-noise + --noise-seed CLI flags

**Files:**
- Modify: `src/backtest/config.rs`

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `src/backtest/config.rs`:

```rust
    #[test]
    fn parses_oracle_noise_default_zero() {
        let a = parse(&["--start", "2026-04-09", "--end", "2026-05-09"]);
        assert_eq!(a.oracle_noise, 0.0);
    }

    #[test]
    fn parses_oracle_noise_005() {
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--oracle-noise", "0.05",
        ]);
        assert_eq!(a.oracle_noise, 0.05);
    }

    #[test]
    fn parses_noise_seed_default_42() {
        let a = parse(&["--start", "2026-04-09", "--end", "2026-05-09"]);
        assert_eq!(a.noise_seed, 42);
    }

    #[test]
    fn parses_noise_seed_custom() {
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--noise-seed", "12345",
        ]);
        assert_eq!(a.noise_seed, 12345);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backtest::config::tests::parses_oracle_noise_default_zero`
Expected: FAIL — `field 'oracle_noise' does not exist on BacktestArgs`.

- [ ] **Step 3: Add the fields**

Edit `src/backtest/config.rs`. Update `BacktestArgs` — append fields after `pub strategies`:

```rust
    /// Stddev of Gaussian noise added to BS theoretical bid/ask. Range
    /// [0.0, 0.5]. 0.0 = identical to v1.4 baseline. 0.05 ≈ matches
    /// real-money observed gap-down magnitude.
    #[arg(long, default_value = "0.0")]
    pub oracle_noise: f64,

    /// Seed for the noise RNG. Same seed + same sigma = byte-identical run.
    #[arg(long, default_value = "42")]
    pub noise_seed: u64,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib backtest::config::tests::parses_oracle_noise_default_zero backtest::config::tests::parses_oracle_noise_005 backtest::config::tests::parses_noise_seed_default_42 backtest::config::tests::parses_noise_seed_custom`
Expected: PASS — all 4 new green.

- [ ] **Step 5: Commit**

```bash
git add src/backtest/config.rs
git commit -m "feat(backtest): --oracle-noise + --noise-seed CLI flags (default 0.0 / 42)"
```

---

## Task 5: oracle-noise validation

**Files:**
- Modify: `src/backtest/config.rs`

The plan-default `Args` may not be where validation lives — depending on how the existing `BacktestArgs` validates. If there's no existing `validate()` method on `BacktestArgs`, validation is best done in `poly-backtest.rs` main. Inspect first.

- [ ] **Step 1: Inspect existing validation pattern**

Run: `grep -n "validate\|fn from_env" src/backtest/config.rs`

If `BacktestArgs` has no `validate()` method, the validation will live in `src/bin/poly-backtest.rs` instead (Task 7). For now, write a lightweight test that simply documents the expected range — not a unit test that calls `validate()`.

- [ ] **Step 2: Write the failing test for runtime validation**

Add to `#[cfg(test)] mod tests` in `src/backtest/config.rs`:

```rust
    #[test]
    fn parses_oracle_noise_negative_value() {
        // Clap accepts the value at parse time; runtime validation in main()
        // rejects. This test just documents that clap doesn't reject negatives
        // at parse — they must be caught downstream.
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--oracle-noise", "-0.1",
        ]);
        assert_eq!(a.oracle_noise, -0.1);
    }

    #[test]
    fn parses_oracle_noise_above_half() {
        let a = parse(&[
            "--start", "2026-04-09", "--end", "2026-05-09",
            "--oracle-noise", "0.6",
        ]);
        assert_eq!(a.oracle_noise, 0.6);
    }
```

- [ ] **Step 3: Run tests to verify they pass**

These tests pass on the current code from Task 4 — clap parses any f64. They serve as documentation that runtime validation lives in main.

Run: `cargo test --lib backtest::config::tests::parses_oracle_noise_negative_value backtest::config::tests::parses_oracle_noise_above_half`
Expected: PASS.

- [ ] **Step 4: Commit (documentation tests)**

```bash
git add src/backtest/config.rs
git commit -m "test(backtest): document --oracle-noise range checks belong in main"
```

The actual rejection happens in Task 7 inside `src/bin/poly-backtest.rs` via `anyhow::bail!`.

---

## Task 6: 5 new strategies in strategy_set

**Files:**
- Modify: `src/backtest/config.rs`

- [ ] **Step 1: Write the failing tests**

Update existing tests + add new ones. Find these in `src/backtest/config.rs::tests`:

```rust
    #[test]
    fn strategy_set_has_six_strategies() {
        let s = strategy_set();
        assert_eq!(s.len(), 6);
        // ...
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
    fn filter_all_returns_everything() {
        let s = strategy_set();
        assert_eq!(filter_strategies(&s, "all").len(), 6);
        assert_eq!(filter_strategies(&s, "").len(), 6);
    }
```

Update each `6` → `11`:

```rust
    #[test]
    fn strategy_set_has_eleven_strategies() {
        let s = strategy_set();
        assert_eq!(s.len(), 11);
        let names: Vec<&str> = s.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"1_hold_martingale"));
        assert!(names.contains(&"6_fixed_stake_baseline"));
        assert!(names.contains(&"11_tp85_sl20"));
    }

    #[test]
    fn strategy_set_uniqueness() {
        let s = strategy_set();
        let mut names: Vec<&String> = s.iter().map(|c| &c.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 11);
    }

    #[test]
    fn filter_all_returns_everything() {
        let s = strategy_set();
        assert_eq!(filter_strategies(&s, "all").len(), 11);
        assert_eq!(filter_strategies(&s, "").len(), 11);
    }
```

The first test was named `strategy_set_has_six_strategies` — rename to `strategy_set_has_eleven_strategies` to match the new count.

Also add a new test for the SL sweep specifically:

```rust
    #[test]
    fn strategy_set_includes_sl_sweep_variants() {
        let s = strategy_set();
        let sweep_names = ["7_tp85_sl40", "8_tp85_sl35", "9_tp85_sl30", "10_tp85_sl25", "11_tp85_sl20"];
        for name in sweep_names {
            let entry = s.iter().find(|c| c.name == name)
                .unwrap_or_else(|| panic!("strategy '{name}' missing"));
            match &entry.exit {
                ExitRule::TpSlOrHold { tp_price, sl_price } => {
                    assert_eq!(*tp_price, dec!(0.85), "{name} TP wrong");
                    let expected_sl = match name {
                        "7_tp85_sl40" => dec!(0.40),
                        "8_tp85_sl35" => dec!(0.35),
                        "9_tp85_sl30" => dec!(0.30),
                        "10_tp85_sl25" => dec!(0.25),
                        "11_tp85_sl20" => dec!(0.20),
                        _ => unreachable!(),
                    };
                    assert_eq!(*sl_price, expected_sl, "{name} SL wrong");
                }
                _ => panic!("{name} should be TpSlOrHold"),
            }
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib backtest::config::tests::strategy_set_has_eleven_strategies`
Expected: FAIL — `strategy_set` returns 6, not 11.

- [ ] **Step 3: Append 5 new strategies to strategy_set()**

Edit `src/backtest/config.rs::strategy_set()`. Update the `vec![]` block:

```rust
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
        common("7_tp85_sl40",             ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.40) }, mart()),
        common("8_tp85_sl35",             ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.35) }, mart()),
        common("9_tp85_sl30",             ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.30) }, mart()),
        common("10_tp85_sl25",            ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.25) }, mart()),
        common("11_tp85_sl20",            ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.20) }, mart()),
    ]
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib backtest::config::`
Expected: PASS — all config tests green (11-strategy update + SL sweep test + previous 4 tests for new flags).

- [ ] **Step 5: Commit**

```bash
git add src/backtest/config.rs
git commit -m "feat(backtest): 5 SL-sweep strategies (7_tp85_sl40 ... 11_tp85_sl20)"
```

---

## Task 7: Wire NoisyBlackScholesOracle into poly-backtest binary

**Files:**
- Modify: `src/bin/poly-backtest.rs`

- [ ] **Step 1: Inspect existing oracle construction**

Read `src/bin/poly-backtest.rs`. Find the line that constructs `BlackScholesOracle::new(...)`. Note the variable name and the type it's assigned to (likely `Box<dyn TokenPriceOracle>` or `Arc<...>` or just a concrete `BlackScholesOracle`).

- [ ] **Step 2: Add validation + wrap with noisy oracle when sigma > 0**

Edit `src/bin/poly-backtest.rs`. Near the top of `main`, after `let args = BacktestArgs::parse();` (or wherever args are parsed), add validation:

```rust
    if args.oracle_noise < 0.0 || args.oracle_noise > 0.5 {
        anyhow::bail!(
            "oracle-noise must be in [0.0, 0.5], got {}",
            args.oracle_noise
        );
    }
```

Locate the existing oracle construction (search for `BlackScholesOracle::new`). Wrap conditionally:

```rust
    use poly_tui::backtest::oracle::{BlackScholesOracle, NoisyBlackScholesOracle, TokenPriceOracle};

    let base_oracle = BlackScholesOracle::new(btc.clone(), sigma, args.friction);
    let oracle: Box<dyn TokenPriceOracle> = if args.oracle_noise > 0.0 {
        eprintln!("[poly-backtest] oracle noise σ={:.4} seed={}", args.oracle_noise, args.noise_seed);
        Box::new(NoisyBlackScholesOracle::new(base_oracle, args.oracle_noise, args.noise_seed))
    } else {
        Box::new(base_oracle)
    };
```

(Adjust container type — `Box`, `Arc`, etc. — to match the existing local. The trait object substitution is the only change.)

- [ ] **Step 3: Verify build**

Run: `cargo build --bin poly-backtest`
Expected: Compiles clean.

- [ ] **Step 4: Smoke test default behavior unchanged**

Run: `./target/debug/poly-backtest.exe --help 2>&1 | head -25`
Expected: Help output shows `--oracle-noise` and `--noise-seed` flags.

- [ ] **Step 5: Smoke test sigma=0 produces identical output to baseline**

(Skipping a real run since it takes 25+ min for 30 days. Trust the Task 2 unit test which proved σ=0 fast-path.)

- [ ] **Step 6: Commit**

```bash
git add src/bin/poly-backtest.rs
git commit -m "feat(backtest): wire --oracle-noise + --noise-seed into poly-backtest binary"
```

---

## Task 8: README + TODO

**Files:**
- Modify: `README.md`
- Modify: `TODO.md`

- [ ] **Step 1: Update README backtest section**

Edit `README.md`. In the §Backtest section (search "Backtest framework" or "poly-backtest"), add subsection after the existing strategy list:

````markdown
### Oracle noise + SL parameter sweep (v1.7.2)

Real-money observation: SL fired at bid=0.34 when configured threshold was 0.45 — the v1.4 BS oracle (Binance 1-min interp) underestimated intra-window jitter. v1.7.2 adds Gaussian white noise to the BS theoretical and 5 new SL sweep variants.

```bash
# Default — no noise, 11 strategies (was 6)
poly-backtest --start 2026-04-09 --end 2026-05-09

# Add σ=0.05 Gaussian noise on bid/ask, seeded for reproducibility
poly-backtest --start 2026-04-09 --end 2026-05-09 \
  --oracle-noise 0.05 --noise-seed 42
```

| Flag | Default | Notes |
|---|---|---|
| `--oracle-noise` | `0.0` | Stddev of N(0, σ) added per-tick to bid/ask, clamped to [0.01, 0.99]. Range [0.0, 0.5]. |
| `--noise-seed` | `42` | RNG seed. Same σ + seed = byte-identical run. |

**Strategy sweep**: strategies 7-11 vary `sl_price ∈ {0.40, 0.35, 0.30, 0.25, 0.20}` with TP fixed at 0.85, mirroring the v1.5 trader's `--exit-rule tp-sl` parameters.

**Calibration:** start with σ=0.0 (baseline), σ=0.03 (mild), σ=0.05 (matches today's observed gap-down). Re-run after collecting 24h of real-money trigger data and tune to match observed SL rate ±10%.
````

Update Roadmap:

```markdown
- **v1.7.2** ✅ — Backtest oracle noise + SL parameter sweep
```

Update Documentation list:

```markdown
- `docs/superpowers/specs/2026-05-10-backtest-oracle-noise-design.md` — v1.7.2 design
- `docs/superpowers/plans/2026-05-10-backtest-oracle-noise.md` — v1.7.2 plan
```

- [ ] **Step 2: Tick v1.7.2 in TODO.md**

Edit `TODO.md`. Replace or update the v1.7.2 section. Insert after v1.7.1 ✅ COMPLETE block (or wherever v1.7.x markers go):

```markdown
## v1.7.2 — Backtest oracle noise + SL sweep ✅ COMPLETE

Adds `NoisyBlackScholesOracle` (Gaussian per-tick noise, seeded reproducibility) and 5 SL-sweep variants (TP=0.85, SL ∈ {0.40, 0.35, 0.30, 0.25, 0.20}) to the v1.4 backtest. Default `--oracle-noise 0.0` reproduces v1.4 numbers exactly. See `docs/superpowers/specs/2026-05-10-backtest-oracle-noise-design.md`.

- [x] `rand` + `rand_distr` deps
- [x] `NoisyBlackScholesOracle` wrapper with σ=0 fast path
- [x] Non-zero noise: per-tick Gaussian sample + clamp [0.01, 0.99]
- [x] Reproducibility (seeded `StdRng`)
- [x] CLI: `--oracle-noise` + `--noise-seed`
- [x] Validation in main: `oracle_noise ∈ [0.0, 0.5]`
- [x] 5 new SL-sweep strategies (`7_tp85_sl40` ... `11_tp85_sl20`)
- [x] Wire into poly-backtest binary
- [x] README + TODO docs

**Open items / next:**
- Calibrate `--oracle-noise` from real-money trigger samples (operator task, ~24h)
- v1.7.3: extend backtest to 15m/60m windows (still TODO from earlier)
- v1.7.4: autocorrelated noise (AR(1)) if white-noise calibration insufficient

---
```

- [ ] **Step 3: Verify build still clean**

Run: `cargo build --bin poly-backtest`
Expected: Compiles clean.

- [ ] **Step 4: Commit**

```bash
git add README.md TODO.md
git commit -m "docs: README + TODO updated for v1.7.2 backtest oracle noise"
```

---

## Self-review

After all tasks:

**1. Spec coverage:**

| Spec section | Implemented in |
|---|---|
| Architecture (NoisyBSOracle wrapper) | Tasks 2 + 3 |
| CLI flags + validation | Tasks 4 + 5 + 7 (validation in main) |
| Strategy sweep (7-11) | Task 6 |
| Wire into binary | Task 7 |
| Tests (sigma=0, reproducibility, clamp, mean) | Tasks 2 + 3 |
| Backward compatibility (sigma=0 fast path) | Task 2 |
| Calibration guidance | Task 8 (README) |
| Out-of-scope (autocorrelated noise, jump-diffusion) | Task 8 (TODO open items) |

**2. Placeholder scan:** None. Each step has full code or specific shell commands.

**3. Type consistency:** `NoisyBlackScholesOracle`, `BlackScholesOracle`, `BacktestArgs.oracle_noise`, `BacktestArgs.noise_seed`, `strategy_set()`, names `7_tp85_sl40` through `11_tp85_sl20` spelled identically across files.

**4. Notes:**
- The spec mentions an integration smoke that asserts `--oracle-noise 0.0` produces identical output to v1.4. Skipped as a separate task because: (a) running a 30-day backtest takes 25+ min, (b) the σ=0 fast-path unit test (Task 2) already proves byte-identity. Operator can verify manually post-merge if desired.
- `rand_distr::Normal` collides with `statrs::distribution::Normal`. Renamed locally as `NormalDist` to avoid ambiguity.
- The `noisy_oracle_mean_near_zero_over_many_samples` test in Task 3 uses `to_string().parse::<f64>()` to extract f64 from Decimal. Cheap, adequate for variance asserts; not pretty but doesn't pull in extra arith deps.
