# v1.7.5 — Real Polymarket trade-history backtest

**Goal:** Validate two surviving strategy candidates (`12_tp75_early_exit_270` and `13_hold_early_exit_270`) using **real Polymarket trade prices** instead of Black-Scholes theoretical. Both strategies exit BEFORE window resolution (at t=270s) to avoid the on-chain redemption blocker (EOA has no MATIC for gas). The output decides whether v1.8 implements `--exit-rule tp-only` (12) or some equivalent of strategy 13, or whether neither is profitable and we abandon the 5min market entirely.

**Non-goal:** Replace BS oracle entirely; rebuild backtest from scratch; orderbook L2 snapshots; 15m/60m windows; high-frequency fill modeling.

## Context

**Why now.** v1.7.2's σ-sweep showed strategy 4 (TP=0.85 + SL=0.45) collapses under any noise — it was a noise-free artifact. Per the user's 2026-05-10 decision, strategy 4 is abandoned. Two candidates survive:

- `1_hold_martingale` (already implemented as `--exit-rule hold` default) — but blocked by redemption: post-resolution sell often fails, and on-chain `redeemPositions` requires MATIC the EOA doesn't have.
- `2_tp_only_martingale` — needs new code (v1.8).

Both candidates were validated only with the BS+noise oracle. We don't trust those numbers. The user proposed using actual Polymarket trade history to backtest with ground truth.

**What's available.** Polymarket's data-api `/trades?market=<conditionId>` endpoint returns paginated trade records: `(timestamp, side, price, size, outcome)`. Confirmed working via curl: a typical 5-min BTC up/down window has 100–300 in-window trades. Across 30 days × 8500 windows, total ~2M trades, ~50 MB compressed. Manageable.

**Two new strategies** redirect away from resolution-path liquidity issues:

- **12_tp75_early_exit_270**: limit BUY → limit TP @ 0.75 → if TP doesn't fill by t=270s, market-sell residual at current bid → done.
- **13_hold_early_exit_270**: market BUY → hold → at t=270s market-sell at current bid → done.

Both exit deterministically before t=300s window close. No redemption needed.

## Architecture

`RealTradeOracle` decorates the existing `TokenPriceOracle` trait. The constructor loads cached trade history for all windows in the backtest range. `price_at(window, t_secs)` returns `(bid, ask)` derived from the most recent in-window SELL/BUY trade ≤ `t_secs`. If no qualifying trade exists, falls back to gamma's `outcomePrices` snapshot at window open.

```
                          Backtest oracle dispatch
       ┌────────────────────────────────────────────────┐
       │  --oracle bs    → BlackScholesOracle            │  v1.4 default
       │  --oracle noisy → NoisyBlackScholesOracle       │  v1.7.2
       │  --oracle real  → RealTradeOracle               │  v1.7.5 NEW
       └────────────────────────────────────────────────┘
                            │
                            ▼ impl TokenPriceOracle
                       price_at(window, t_secs)
                            │
                            ▼
              RealTradeOracle: lookup in cached trades
                bid = last SELL price ≤ t_secs (UP token)
                ask = last BUY price ≤ t_secs (UP token)
                fallback to gamma open price if none yet
```

The fetch + cache pipeline is independent — runs once per backtest range, reuses cache thereafter.

```
                      Trade fetch pipeline (one-time per range)
       ┌──────────────────────────────────────────────────────┐
       │  for each window in [start, end]:                     │
       │    if cached: skip                                    │
       │    else:                                              │
       │      paginate /trades?market=<condId>&limit=500       │
       │        until response.len() < 500                     │
       │      sleep 100ms between calls                        │
       │      write ~/.poly-backtest-cache/trades/<ts>.json    │
       │                                                       │
       │  total: ~5K calls × 200ms = ~17 min for 30 days       │
       └──────────────────────────────────────────────────────┘
```

## CLI surface

```bash
# Default — BS oracle, 13 strategies (11 existing + 2 new early-exit)
poly-backtest --start 2026-04-09 --end 2026-05-09

# v1.7.2 — BS + noise
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle noisy --oracle-noise 0.05

# v1.7.5 NEW — real trade history; auto-fetches uncached windows on first run
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle real
```

| Flag | Default | Notes |
|---|---|---|
| `--oracle` | `bs` | One of `bs`, `noisy`, `real`. |
| (existing) `--oracle-noise` | `0.0` | Only meaningful with `--oracle noisy`. Silently ignored otherwise. |

Validation: `--oracle real` triggers fetch on uncached windows automatically. Operator sees a one-time progress log on first run.

## Strategy additions

| # | Name | Exit rule | New code? |
|---|---|---|---|
| 12 | **12_tp75_early_exit_270** | `TpOnlyOrEarlyExit { tp_price: 0.75, exit_at_secs: 270 }` | YES — new ExitRule variant + arm in `simulate_window` |
| 13 | **13_hold_early_exit_270** | `FixedTime { seconds: 270 }` | NO — reuses existing variant |

Strategy 13 reuses the existing `FixedTime` variant. Strategy 5 (`5_time_60s_martingale`) already uses this with `seconds: 60`. We just add a new strategy entry with `seconds: 270`.

Strategy 12 introduces a new variant `TpOnlyOrEarlyExit { tp_price, exit_at_secs }` semantically equivalent to: "sell at `tp_price` if reached during the window; otherwise sell at the current bid at second `exit_at_secs`."

All other strategy fields (band 0.45–0.55, Martingale base $5 max-step 5, Direction Up) match the existing variants.

## Components

### `src/backtest/data/trades.rs` *(new)*

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trade {
    pub timestamp: i64,           // unix seconds
    pub side: TradeSide,           // Buy or Sell
    pub price: Decimal,
    pub size: Decimal,
    pub outcome: Outcome,          // Up or Down
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide { Buy, Sell }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome { Up, Down }

#[async_trait]
pub trait TradeFetcher: Send + Sync {
    /// Fetch all trades for one market (5-min window) by condition_id.
    /// Paginates internally. Returns sorted by timestamp ascending.
    async fn fetch_window(
        &self,
        condition_id: &str,
        window_ts: i64,
    ) -> Result<Vec<Trade>, FetchError>;
}

pub struct PolymarketTradeFetcher {
    client: polymarket_client_sdk_v2::data::Client,
    throttle_ms: u64,
}

impl TradeFetcher for PolymarketTradeFetcher { /* paginates +sleep_ms */ }

pub struct CachedTradeStore {
    cache_dir: PathBuf,
}

impl CachedTradeStore {
    pub fn load(&self, window_ts: i64) -> Option<Vec<Trade>>;
    pub fn save(&self, window_ts: i64, trades: &[Trade]) -> io::Result<()>;
}
```

JSON format: `~/.poly-backtest-cache/trades/<window_ts>.json` containing array of Trade. Compatibility with v1.4's `DiskCache` pattern.

### `src/backtest/oracle.rs` *(modify)*

Add `RealTradeOracle` next to existing `BlackScholesOracle` and `NoisyBlackScholesOracle`:

```rust
pub struct RealTradeOracle {
    /// HashMap<window_ts, Vec<Trade>>, pre-loaded for the backtest range.
    /// Trades sorted by timestamp ascending. Filtered to outcome=Up only.
    up_trades_by_window: HashMap<i64, Vec<Trade>>,
}

const PRE_TRADE_FALLBACK: Decimal = dec!(0.5);

impl RealTradeOracle {
    pub fn new(
        all_trades: HashMap<i64, Vec<Trade>>,
    ) -> Self {
        let up_trades = all_trades.into_iter()
            .map(|(ts, trades)| {
                let mut up: Vec<Trade> = trades.into_iter()
                    .filter(|t| t.outcome == Outcome::Up
                             && t.timestamp >= ts
                             && t.timestamp < ts + 300)
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

### `src/backtest/data/gamma_history.rs` *(unchanged)*

`WindowMeta` does NOT gain new fields. The pre-trade fallback is a flat constant 0.50 (band-mid), set inside `RealTradeOracle`. Rationale:

- For closed windows, gamma's `outcomePrices` shows RESOLVED prices ($0 or $1) — wrong as a pre-trade bid.
- Using the first-trade-of-window as fallback introduces forward-look bias (uses t=15s data at t=0s).
- BS theoretical at t=0 defeats the "no model" goal.
- **0.50 (band-mid) is the only no-info-leak option.**

Impact on strategies 12 and 13:
- Pre-trade fallback only affects the first ~10–30s of each window before the first SELL trade arrives.
- Strategy 12's TP=0.75 won't trigger off a fallback bid of 0.50.
- Strategy 13 exits at t=270s — by then dozens of real trades have happened.
- Net EV impact: negligible.

### `src/backtest/exit_rule.rs` *(modify)*

Add new variant + handling:

```rust
pub enum ExitRule {
    HoldToResolution,
    TpOnlyOrHold { tp_price: Decimal },
    TpSlOrHold { tp_price: Decimal, sl_price: Decimal },
    FixedTime { seconds: u32 },
    /// NEW v1.7.5: Try TP first; if not filled by `exit_at_secs`, market-sell
    /// at current bid. Avoids resolution path entirely.
    TpOnlyOrEarlyExit { tp_price: Decimal, exit_at_secs: u32 },
}
```

Add arm in `simulate_window`:

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

### `src/backtest/config.rs` *(modify)*

Add `--oracle` flag + 2 strategies:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum OracleKind { Bs, Noisy, Real }

#[derive(Parser, Debug, Clone)]
pub struct BacktestArgs {
    // ... existing fields
    /// Oracle to use for token price simulation.
    #[arg(long, value_enum, default_value = "bs")]
    pub oracle: OracleKind,
}
```

Append 2 strategies to `strategy_set()`:

```rust
common("12_tp75_early_exit_270",
    ExitRule::TpOnlyOrEarlyExit { tp_price: dec!(0.75), exit_at_secs: 270 },
    mart()),
common("13_hold_early_exit_270",
    ExitRule::FixedTime { seconds: 270 },
    mart()),
```

`strategy_set()` now returns 13 strategies.

### `src/bin/poly-backtest.rs` *(modify)*

Dispatch on `args.oracle`:

```rust
let oracle: Box<dyn TokenPriceOracle> = match args.oracle {
    OracleKind::Bs => Box::new(BlackScholesOracle::new(btc.clone(), sigma, args.friction)),
    OracleKind::Noisy => {
        let base = BlackScholesOracle::new(btc.clone(), sigma, args.friction);
        Box::new(NoisyBlackScholesOracle::new(base, args.oracle_noise, args.noise_seed))
    }
    OracleKind::Real => {
        eprintln!("[poly-backtest] loading real trade history (auto-fetching uncached)...");
        let store = CachedTradeStore::new(&cache_dir);
        let fetcher = PolymarketTradeFetcher::new(100); // 100ms throttle
        let mut all_trades = HashMap::new();
        for window in &windows {
            let trades = match store.load(window.window_ts) {
                Some(t) => t,
                None => {
                    let condition_id = window.condition_id.clone();
                    let fetched = fetcher.fetch_window(&condition_id, window.window_ts).await?;
                    store.save(window.window_ts, &fetched)?;
                    fetched
                }
            };
            all_trades.insert(window.window_ts, trades);
        }
        Box::new(RealTradeOracle::new(all_trades))
    }
};
```

### HTML report

No changes needed. The existing renderer iterates over `Vec<StrategyStats>` — 13 strategies render correctly. Operator can scroll the table.

### `Cargo.toml`

No new deps. SDK's `data::Client::trades()` already available.

## Edge cases

| Scenario | Handling |
|---|---|
| Window with zero trades | bid=ask=fallback (0.5). Strategy 12 won't trigger TP, falls through to t=270s exit at 0.5 → break-even less friction. Strategy 13 same. |
| Window with only BUY, no SELL | Bid=fallback. Strategy can still trigger TP if a BUY price ≥ tp_price exists — but TP looks at bid, not ask. So no TP. |
| Trade exactly at t_secs | `≤ abs_t` is inclusive — uses it. Realistic. |
| Cache miss mid-backtest | Auto-fetch one window inline. Adds ~2s latency per uncached window but doesn't restart. |
| Trade before window_ts | Filtered out in `RealTradeOracle::new` (constructor pre-filter). Defensive against bad data. |
| Trade ≥ window_ts + 300 (post-close) | Filtered out — only intra-window trades affect the in-window simulation. |
| Tick precision: trade at price 0.5012345 | Rust Decimal preserves precision. Comparisons (`bid >= tp_price`) use exact decimal arithmetic. |
| Gamma condition_id missing for a window | Skip the window from backtest with a warn log. (Existing `WindowMeta` already filters unresolved windows.) |
| Network error during fetch | Retry once after 1s. If still failing, abort with clear error message — caller decides whether to retry. |

## Testing

### Unit (`oracle.rs`)

| Test | Assertion |
|---|---|
| `real_oracle_returns_last_sell_as_bid` | Stub trades: SELL at t=10s price=0.50; SELL at t=20s price=0.45. Query at t=15s → bid=0.50; at t=25s → bid=0.45. |
| `real_oracle_returns_last_buy_as_ask` | Mirror for ask. |
| `real_oracle_falls_back_when_no_qualifying_trade` | No SELL trades; query bid → fallback price. |
| `real_oracle_handles_empty_window` | Window with zero trades → both bid and ask = fallback. |
| `real_oracle_filters_post_close_trades` | Trade at t=window_ts+400s (post-close) is excluded. |
| `real_oracle_filters_down_outcome_trades` | DOWN-outcome trades don't affect UP token bid/ask. |

### Unit (`trades.rs`)

| Test | Assertion |
|---|---|
| `cached_trade_store_save_then_load` | Round-trip Vec<Trade> through JSON file. |
| `cached_trade_store_returns_none_when_missing` | `load(unknown_ts)` returns None. |
| `polymarket_trade_fetcher_paginates_correctly` | Mock SDK client returns 500 then <500 → fetcher concatenates and returns sorted. |

### Unit (`exit_rule.rs`)

| Test | Assertion |
|---|---|
| `tp_only_or_early_exit_fills_at_tp` | Stub oracle: bid reaches 0.80 at t=100s → TP=0.75 triggers, returns Won. |
| `tp_only_or_early_exit_falls_through_to_exit` | Stub oracle: bid stays at 0.50 forever → at t=270s, market-sell at 0.50, returns Won/Lost based on proceeds vs cost. |

### Integration

`tests/real_trade_backtest.rs` (`#[ignore]`):

- Pre-populate cache with one window's trades (manually saved JSON fixture).
- Run backtest with `--oracle real --start <window_date> --end <window_date+1>`.
- Assert: report HTML generated, contains expected 13 strategies, run_strategy returned non-empty StrategyStats for the new strategies.

### Coverage

`cargo llvm-cov` ≥80% on new files. Trade fetcher's network path can stay uncovered (network-only); pagination logic via mocks ≥90%.

## Backward compatibility

- `--oracle bs` (default) → byte-identical to v1.7.2 backtest output.
- Existing strategies 1–11 unchanged.
- v1.7.2's `--oracle-noise` flag still works under `--oracle noisy`. (For `--oracle bs` it's silently ignored — already 0.0 by default.)

## Operator workflow

```
# First-time fetch (one-time, ~17 min)
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle real \
  --output report-real.html

# Subsequent runs (cached, ~30s)
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle real \
  --strategies 12_tp75_early_exit_270,13_hold_early_exit_270 \
  --output report-real-candidates.html
```

Inspect `report-real.html`. Decision criteria:

- If `12_tp75_early_exit_270` PnL > 0 → implement v1.8 with `--exit-rule tp-only` using `--tp-price 0.75 --exit-at-secs 270`.
- If `13_hold_early_exit_270` PnL > 0 → implement v1.8b with new `--exit-rule hold-early-exit` flag using `FixedTime { seconds: 270 }` semantics.
- If both ≤ 0 → abandon Polymarket 5min market entirely.

## Risk / known limitations

- **Selection bias**: replay against trades that already happened. Adding our own orders would have moved prices slightly — negligible at our $5–80 stake size given $4–32K window liquidity.
- **Fill assumption**: if the orderbook had a SELL at price X at time T, we assume our limit at X would have filled at T. In reality, queue position matters. Approximation acceptable for backtest.
- **First-trade fallback**: bid before the first SELL trade defaults to either gamma's open or first-SELL-of-window. Both are imperfect — the actual mid-price at t=0 isn't recorded. Best available approximation.
- **Friction**: existing backtest applies `--friction 0.015` to `BlackScholesOracle`'s mid → bid/ask. `RealTradeOracle` returns trade prices directly (already includes spread). Zero additional friction needed when using `--oracle real`. Document this in the operator notes.

## Out of scope (explicit YAGNI)

- **Orderbook L2 snapshots** — Polymarket doesn't publish historical L2.
- **Multi-market parallel fetch** — sequential is fine for one-time 17-min hit.
- **Mid-price interpolation between trades** — last-price is good enough.
- **15min / 60min variants** — v1.7.3 (deprioritized).
- **Realistic fill modeling** (partial fill, queue position) — backtest assumes full fill at observed price.
- **Trade size weighting** — the oracle uses last price regardless of size. A 10000-share trade at 0.50 and a 5-share trade at 0.50 weigh the same in our lookup. Acceptable.
- **Auto-incremental fetch** — if user expands the date range, refetch is automatic per-window; no smart "fetch only new days" pre-flight.

## Migration / rollback

- `--oracle` defaults to `bs` → v1.7.2 behavior preserved.
- New strategies added to `strategy_set()` are appended; previous comparisons still meaningful.
- Trade cache directory is independent from existing `gamma/` and `binance/` caches. Can be deleted at any time without breaking the BS path.
- Rollback: revert commits; `--oracle` flag rejected by clap → users explicitly specify `--oracle bs` if they want the old behavior. No data loss.

## Related documents

- v1.4 backtest spec: `docs/superpowers/specs/2026-05-09-backtest-framework-design.md`
- v1.7.2 oracle noise spec: `docs/superpowers/specs/2026-05-10-backtest-oracle-noise-design.md`
- TODO: `TODO.md` — v1.8 trader candidate, deprioritized v1.7.3/4/5
