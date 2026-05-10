# v1.7.2 — Backtest oracle noise + SL parameter sweep

**Goal:** Make the v1.4 backtest realistic enough to trust before betting more real money on strategy 4. Two changes:
1. Add stochastic noise to the Black-Scholes oracle to capture orderbook jitter that the current 1-min Binance interpolation misses.
2. Add 5 new strategy variants sweeping `SL ∈ {0.40, 0.35, 0.30, 0.25, 0.20}` (TP fixed at 0.85) so we can see how SL threshold affects EV under both pure and noisy oracle conditions.

**Non-goal:** Replace Binance 1-min data with 1-second ticks, build a jump-diffusion model, fit noise from real Polymarket bid history (data unavailable), or change strategy 4's TP threshold.

## Context

A real-money run today (2026-05-10, 5min maker mode) entered at ask=0.515 and triggered SL when the gamma bid hit **0.34** — well below the configured SL=0.45. The backtest's BS oracle, fed Binance 1-min candles with linear interpolation between, never produces sub-minute spikes of that magnitude. Predicted SL trigger rate from the v1.4 backtest is ~58% on strategy 4; observed real-money SL is firing at lower bids than predicted, eating proceeds.

The user also asked whether `SL=0.20` would avoid these noise-induced bails by giving more room before exiting. This is a parameter question separate from the model question. v1.7.2 covers both: noise-augmented oracle (model fix) AND SL sweep (parameter exploration).

## Architecture

A new wrapper type `NoisyBlackScholesOracle` decorates the existing `BlackScholesOracle`. After computing the BS theoretical bid/ask at each per-second tick, it adds Gaussian white noise:

```
bid_real = clamp(bid_bs + ε, 0.01, 0.99)
ask_real = clamp(ask_bs + ε, 0.01, 0.99)
ε ~ N(0, σ_noise)
```

Bid and ask receive the **same** `ε` sample (correlated — orderbook moves shift both quotes together). Clamped to [0.01, 0.99] to keep prices physically valid (no negative or above-$1 binary token).

Determinism: a single `Mutex<StdRng>` seeded at construction. Sequential `price_at` calls in iteration order produce a deterministic noise sequence. Same `--oracle-noise σ --noise-seed s` produces byte-identical output across runs. Critical for comparing strategy variants.

`σ_noise = 0.0` (default) bypasses the wrapper's noise sampling and returns pure BS — backward-compatible with v1.4 / v1.7 numbers.

```
                ┌─────────────────────────────┐
                │   BlackScholesOracle (v1.4) │
                │                             │
                │   price_at(window, t_secs)  │
                │     = BS(spot, strike, t,σ) │
                │                             │
                │   No noise. Deterministic.  │
                └─────────────┬───────────────┘
                              │
                              ▼  (impl TokenPriceOracle)
              ┌──────────────────────────────────────┐
              │   NoisyBlackScholesOracle (v1.7.2)   │
              │                                      │
              │   wraps BlackScholesOracle           │
              │   adds N(0, σ_noise) per tick        │
              │   seeded StdRng for reproducibility  │
              │                                      │
              │   σ_noise = 0  → fast-path = base    │
              │   σ_noise > 0  → bid+ε, ask+ε        │
              └──────────────────────────────────────┘
```

## CLI surface

```bash
# v1.4 / v1.7 default — pure BS oracle, 6 strategies
poly-backtest --start 2026-04-09 --end 2026-05-09

# v1.7.2 — with noise + 11 strategies (5 new SL sweep)
poly-backtest --start 2026-04-09 --end 2026-05-09 \
  --oracle-noise 0.05 --noise-seed 42

# Compare noise levels (separate runs to separate HTML files)
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle-noise 0.0  --output report-noise-0.html
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle-noise 0.03 --output report-noise-3.html
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle-noise 0.05 --output report-noise-5.html
```

| Flag | Default | Notes |
|---|---|---|
| `--oracle-noise` | `0.0` | Stddev of Gaussian noise on UP token bid/ask. Range `[0.0, 0.5]`. `0.0` = identical to v1.4. |
| `--noise-seed` | `42` | RNG seed for noise. Same seed + same σ ⇒ identical run. |

Validation: `oracle-noise` rejects `< 0` or `> 0.5` (above 0.5 the clamp dominates and noise becomes unphysical).

## Strategy sweep

`strategy_set()` returns 11 strategies (existing 6 + 5 new). All sweep variants are identical to `4_tp_sl_asymmetric` except for `sl_price`:

| # | Name | TP | SL | Status |
|---|---|---|---|---|
| 1 | 1_hold_martingale | — | — | existing |
| 2 | 2_tp_only_martingale | 0.75 | — | existing |
| 3 | 3_tp_sl_symmetric | 0.55 | 0.45 | existing |
| 4 | 4_tp_sl_asymmetric | 0.85 | 0.45 | existing |
| 5 | 5_time_60s_martingale | — | — | existing |
| 6 | 6_fixed_stake_baseline | — | — | existing |
| **7** | **7_tp85_sl40** | 0.85 | 0.40 | NEW |
| **8** | **8_tp85_sl35** | 0.85 | 0.35 | NEW |
| **9** | **9_tp85_sl30** | 0.85 | 0.30 | NEW |
| **10** | **10_tp85_sl25** | 0.85 | 0.25 | NEW |
| **11** | **11_tp85_sl20** | 0.85 | 0.20 | NEW |

All other parameters identical: band [0.45, 0.55], Martingale base=$5 max-step=5, Direction=Up. The HTML report iterates these in order; summary table grows from 6 to 11 rows; histogram grid grows accordingly.

## Components

### `src/backtest/oracle.rs` *(modify)*

Add `NoisyBlackScholesOracle` to the existing module:

```rust
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand_distr::{Distribution, Normal};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal_macros::dec;
use std::sync::Mutex;

pub struct NoisyBlackScholesOracle {
    base: BlackScholesOracle,
    sigma: f64,
    rng: Mutex<StdRng>,
    noise_dist: Normal<f64>,
}

impl NoisyBlackScholesOracle {
    pub fn new(base: BlackScholesOracle, sigma: f64, seed: u64) -> Self {
        let dist = Normal::new(0.0, sigma.max(0.0)).expect("valid normal dist");
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
        let noise = self.noise_dist.sample(&mut *self.rng.lock().unwrap());
        let noise_dec = Decimal::from_f64(noise).unwrap_or(Decimal::ZERO);
        let bid = (bid_bs + noise_dec).clamp(dec!(0.01), dec!(0.99));
        let ask = (ask_bs + noise_dec).clamp(dec!(0.01), dec!(0.99));
        (bid, ask)
    }
}
```

### `src/backtest/config.rs` *(modify)*

Add CLI fields:

```rust
#[derive(Parser, Debug, Clone)]
pub struct BacktestArgs {
    // ... existing fields
    #[arg(long, default_value = "0.0")]
    pub oracle_noise: f64,
    #[arg(long, default_value = "42")]
    pub noise_seed: u64,
}
```

Add validation (in BacktestArgs::validate or wherever the binary's pre-flight runs):

```rust
if !(0.0..=0.5).contains(&self.oracle_noise) {
    return Err(format!("oracle-noise must be in [0.0, 0.5], got {}", self.oracle_noise));
}
```

Append 5 new strategies to `strategy_set()`. Re-use the existing `mart()` builder for stake rule:

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

### `src/bin/poly-backtest.rs` *(modify)*

Wire the new flags into oracle construction:

```rust
let base_oracle = BlackScholesOracle::new(sigma, friction);
let oracle: Arc<dyn TokenPriceOracle> = if args.oracle_noise > 0.0 {
    Arc::new(NoisyBlackScholesOracle::new(base_oracle, args.oracle_noise, args.noise_seed))
} else {
    Arc::new(base_oracle)
};
```

(If the existing flow uses `&dyn` rather than `Arc`, follow the same pattern.)

### `src/backtest/report.rs` *(check only)*

Already iterates over `Vec<StrategyStats>` — no change needed for 11 strategies. Verify summary table doesn't overflow at common terminal/browser widths (1280px target). If layout breaks, defer to a follow-up commit.

### `Cargo.toml` *(check)*

`statrs` already in deps; it transitively brings `rand_distr`. We need `rand` directly for `StdRng + SeedableRng`. Verify; add `rand = "0.8"` if missing.

## Data flow (one window with σ=0.05)

```
t=0   strategy 4 enters at ask=0.50 (BS theoretical)
      shares = floor($5 / 0.50) = 10

t=1   BS theoretical: spot=80,000 → bid=0.49, ask=0.50
      noise sample: ε = -0.018
      observed: bid=0.472, ask=0.482

t=2   BS theoretical: spot=79,950 → bid=0.45, ask=0.46
      noise sample: ε = -0.020
      observed: bid=0.430, ask=0.440  ← below SL=0.45 → trigger!
                                          (would NOT have triggered without noise)
      simulate_window returns Lost{spent_usd: $5 - 0.430 × 10 = $0.70}

Same window with σ=0:
t=2   bid=0.45 → exactly at threshold, may or may not trigger
       windows often hold to resolution → loses $5 if Down wins
```

The illustration shows how noise can cause earlier SL triggers (smaller dollar loss per hit) but more frequent ones — the net EV impact is what the backtest will quantify across thousands of windows.

## Calibration guidance (operator notes)

The user calibrates `--oracle-noise` against real-money observations:

| Approximate σ | Expected SL trigger rate inflation | Use case |
|---|---|---|
| 0.00 | 0% (baseline) | v1.4 reproduction |
| 0.02 | ~5–15% more SL | mild jitter, conservative |
| 0.05 | ~30–60% more SL | matches today's observed gap-down |
| 0.10 | dominant noise, BS signal lost | stress test, not realistic |

One real data point so far: bid was 0.34 when BS theoretical was probably ~0.40 (BTC had moved against UP, time elapsed) → noise residual ≈ −0.06. A single observation; calibrate properly with N≥30 real-money trigger samples once available.

The recommended path:
1. Run backtest at σ=0.0, 0.02, 0.05, 0.08 — produce 4 HTML reports.
2. After 24 hours of real-money data, count actual SL trigger rate.
3. Pick σ that makes backtest's SL rate match real ±10%.
4. Use that σ for ongoing strategy decisions.

## Errors and edge cases

| Scenario | Handling |
|---|---|
| `--oracle-noise -0.1` | Validation rejects: must be ≥ 0. |
| `--oracle-noise 0.6` | Validation rejects: above 0.5 noise dominates. |
| Noise pushes bid below 0.01 | Clamped to 0.01. (UP token can't go negative.) |
| Noise pushes bid above 0.99 | Clamped to 0.99. |
| `σ = 0.0` exactly | Fast-path returns base oracle output unchanged. |
| Concurrent `price_at` calls (current code is single-threaded) | `Mutex<StdRng>` makes it safe; no behavior change since calls are sequential. |
| Unique seed per strategy | NOT done. All 11 strategies share the same RNG state because the runner iterates them sequentially. As long as iteration order is deterministic, results are reproducible. |

## Testing

### Unit (`oracle.rs`)

| Test | Assertion |
|---|---|
| `noisy_with_sigma_zero_matches_base` | σ=0.0 → noisy.price_at == base.price_at for 100 random (window, t_secs) points |
| `noisy_with_seed_42_reproducible` | Two `NoisyBlackScholesOracle` instances with same base + σ=0.05 + seed=42 produce identical bid sequences over 10000 calls |
| `noisy_mean_near_zero_over_10000_samples` | Mean of `bid_noisy - bid_bs` over 10k samples is within ±0.005 of 0 (3σ for σ_noise=0.05 / sqrt(10k)) |
| `noisy_clamps_to_valid_range` | With base bid=0.05 and σ=0.20 (large), all returned bids ∈ [0.01, 0.99] over 1000 samples |

### Unit (`config.rs`)

| Test | Assertion |
|---|---|
| `parses_oracle_noise_default_zero` | omitted → `oracle_noise == 0.0` |
| `parses_oracle_noise_005` | `--oracle-noise 0.05` → `oracle_noise == 0.05` |
| `parses_noise_seed_default_42` | omitted → `noise_seed == 42` |
| `validate_rejects_negative_oracle_noise` | `-0.01` → ValidationError |
| `validate_rejects_oracle_noise_above_half` | `0.6` → ValidationError |
| `strategy_set_has_eleven_strategies` | `strategy_set().len() == 11` |
| `strategy_set_includes_sl_sweep` | strategies 7–11 are present with names `7_tp85_sl40` ... `11_tp85_sl20` and matching SL prices |

### Integration / coverage

- Existing tests for `BlackScholesOracle` remain green (we don't modify it).
- Existing strategy 4 backtest produces identical output when run with `--oracle-noise 0.0`. Add an integration smoke that asserts this.
- Coverage: ≥80% on new code in `oracle.rs` (achievable via the 4 unit tests above).

## Backward compatibility

- Default `--oracle-noise 0.0` → byte-identical output to v1.4 / v1.7.
- Strategy 4 (`4_tp_sl_asymmetric`) unchanged. New variants are additions, not modifications.
- HTML report renders 11 columns instead of 6; visual layout may need polish but is non-breaking.
- No new Redis keys, no new env vars.

## Risk

- **Sweep results are noisy under noisy oracle** — small sample (8.5K windows × 30 days) divided across 11 strategies × multiple σ values. Run all 3 historical samples (Apr-May, Mar, Feb) and average.
- **σ calibration is empirical** — initial guesses are educated estimates, not derived. The user iterates against observed real-money trigger rates over weeks.
- **The "right" σ depends on Polymarket liquidity, time of day, BTC vol regime** — a single static σ is an approximation. Future work might segment by market state.

## Out of scope (explicit YAGNI)

- **Autocorrelated noise (AR(1) / GARCH)** — white noise is the simplest model. v1.7.3 if calibration shows persistent residuals.
- **Jump-diffusion (Merton)** — captures fat tails better but bigger refactor + parameter fit. Defer.
- **Per-window or per-strategy seeds** — global RNG suffices given deterministic iteration.
- **HTML report layout overhaul for 11 strategies** — accept current layout; iterate later if visually crowded.
- **Calibration from real Polymarket bid data** — data unavailable; operator calibrates manually.
- **Sweep on TP threshold** — only SL is swept; TP=0.85 stays fixed (the user's specific question was about SL).

## Migration / rollback

- New CLI flags default to no-op behavior. Existing scripts continue to work.
- Adds 5 new strategy entries; old reports comparing 6 strategies still semantically meaningful.
- Rollback: revert the commits; no schema or data changes to reverse.

## Related documents

- v1.4 backtest spec: `docs/superpowers/specs/2026-05-09-backtest-framework-design.md`
- v1.7.1 trader window-minutes spec: `docs/superpowers/specs/2026-05-10-window-minutes-design.md`
- TODO: `TODO.md` — v1.7.2 backtest improvements section
