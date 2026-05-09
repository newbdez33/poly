# poly

A Polymarket trading bot in Rust. **v1.0 ships a TUI that displays your USDC balance**, refreshed in the background through a Redis cache. Subsequent versions will add markets, positions, and order placement; the v1.0 module layout is organized so a daemon/TUI split lands without rewrites.

```
┌─ poly-tui ─────────────────────────────────────────────┐
│                                                         │
│                    USDC: $1,234.56                      │
│                                                         │
├─────────────────────────────────────────────────────────┤
│ ● CLOB  ● Redis   refresh: 30s   last: 12s ago    q r  │
└─────────────────────────────────────────────────────────┘
```

## Quick start

**Prerequisites**
- Rust stable (1.78+)
- Docker (for local Redis + integration tests)
- A **dedicated** Polygon wallet — funded with a small amount of USDC.e on Polymarket. Do not use your main wallet.

**Run**

```bash
# Start local Redis
docker compose up -d

# Configure
cp .env.example .env
# Edit .env: paste your dedicated wallet's private key into POLYMARKET_PRIVATE_KEY

# Run
cargo run --bin poly-tui
```

**Controls**

| Key | Action |
|---|---|
| `q` / `Esc` / `Ctrl+C` | Quit |
| `r` | Force refresh |

The TUI appears within ~1s. The balance updates within 5s of startup; both LEDs go green. Auto-refresh every `REFRESH_INTERVAL_SECS` (default 30s).

## Configuration

All via `.env` (see `.env.example`):

| Variable | Default | Purpose |
|---|---|---|
| `POLYMARKET_PRIVATE_KEY` | _required_ | Polygon wallet private key (dedicated wallet) |
| `REDIS_URL` | `redis://127.0.0.1:6379` | Redis connection string |
| `REFRESH_INTERVAL_SECS` | `30` | Background fetch cadence |
| `CLOB_HOST` | `https://clob-v2.polymarket.com` | Polymarket CLOB endpoint |
| `LOG_LEVEL` | `info` | `tracing-subscriber` EnvFilter directive |

Logs go to `logs/poly.log` (daily-rotated). Stdout is never written to while the TUI is up.

## Architecture

Single binary, three tokio tasks:

```
                     ┌──────────────────────────────────┐
crossterm   keys     │      poly-tui process            │
   ─────▶ Input task ────────┐                          │
                     │       ▼                          │
                     │  ┌─────────┐   ForceRefresh      │
                     │  │  App    │ ────────┐           │
                     │  │ + UI    │         ▼           │
                     │  └─────────┘    ┌──────────┐     │
                     │       ▲         │Refresher │     │
                     │       │ read    │  task    │     │
                     │       │         └────┬─────┘     │
                     │       │              │ write     │
                     │       │              ▼           │
                     │   ┌───┴──────────────────┐       │
                     │   │       Redis          │       │
                     │   │ poly:prod:balance:*  │       │
                     │   └──────────┬───────────┘       │
                     │              │                   │
                     └──────────────┼───────────────────┘
                                    ▼
                          Polymarket CLOB
                          (rs-clob-client v2)
```

Two trait abstractions decouple the loops from concrete clients:

- `BalanceFetcher` — real impl wraps `polymarket-client-sdk-v2`; fake impl in `tests/support/fake_fetcher.rs`
- `BalanceCache` — real impl wraps `fred` (async Redis); fake impl in `tests/support/memory_cache.rs`

The Refresher writes to Redis on a schedule (or on-demand). The App reads from Redis on each render tick. They never call each other directly. This is also the seam along which v1.1 will split into a headless daemon + thin TUI.

```
src/
├── bin/poly-tui.rs    process entry — wire config, channels, three tasks, terminal
├── config.rs          .env loader (dotenvy + envy)
├── domain.rs          Balance, RefreshStatus, HealthLed, AppEvent, errors
├── clob.rs            BalanceFetcher trait + ClobBalanceFetcher
├── cache.rs           BalanceCache trait + RedisBalanceCache + key constants
├── refresher.rs       periodic fetch + ForceRefresh command + status emit
├── app.rs             AppState + handle_event + tick_once + run loop
├── ui.rs              ratatui render (pure, zero I/O)
└── input.rs           crossterm event reader (blocking thread)
```

## Tests

```bash
cargo test --lib                                    # 22 unit tests
cargo test --test bdd                               # 4 cucumber scenarios (Chinese gherkin)
cargo test --test cache_integration -- --ignored    # 4 testcontainers tests (Redis)
cargo test --test e2e_tui -- --ignored              # 3 testcontainers + FakeFetcher
cargo llvm-cov --lib --tests                        # coverage report
```

`--ignored` tests require Docker. They're skipped in the fast loop.

## Data isolation

Your dev environment is also your production environment (the dev Redis holds real-money state). Tests are kept off your real data through three layers:

1. **Separate containers** — E2E tests spin up testcontainers Redis on a random port, never your `poly-redis` container on `127.0.0.1:6379`.
2. **Hard port guard** — `assert_ne!(port, 6379)` in both `tests/cache_integration.rs` and `tests/e2e_tui.rs`. Tests refuse to start if they'd touch dev Redis.
3. **Key namespace** — production data uses `poly:prod:*` (constant in `src/cache.rs`). Even if a tool connected to the wrong instance, the prefix makes the data immediately distinguishable from test fixtures.

`docker compose down -v` deletes the dev Redis volume — only run it when you intend to clear your real cache.

## Roadmap

See `TODO.md`. Highlights:

- **v1.0** ✅ — TUI starter (USDC balance display)
- **v1.1** ✅ — Polymarket BTC 5-min Martingale trader (`poly-trader` binary)
- **v1.2** ✅ — BTC market watch strip (live Chainlink price + countdown)
- **v1.4** ✅ — backtest framework (`poly-backtest` binary, 6 strategies, HTML report)
- **v1.5** ✅ — TP/SL exits in trader (`--exit-rule tp-sl`)
- **v1.3** — daemon / TUI split. Required before any new trading logic (multi-strategy, dynamic config, etc.)
- **v1.6+** — strategy selection driven by v1.4 backtest, markets, positions, observability

## Documentation

- `docs/superpowers/specs/2026-05-06-poly-tui-balance-starter-design.md` — v1.0 design
- `docs/superpowers/plans/2026-05-06-poly-tui-balance-starter.md` — v1.0 plan (14 tasks)
- `docs/superpowers/specs/2026-05-09-poly-trader-martingale-design.md` — v1.1 trader design
- `docs/superpowers/plans/2026-05-09-poly-trader-martingale.md` — v1.1 plan (23 tasks)
- `docs/superpowers/specs/2026-05-09-market-watch-strip-design.md` — v1.2 BTC strip design
- `docs/superpowers/plans/2026-05-09-market-watch-strip.md` — v1.2 plan (12 tasks)
- `docs/superpowers/specs/2026-05-09-backtest-framework-design.md` — v1.4 backtest design
- `docs/superpowers/plans/2026-05-09-backtest-framework.md` — v1.4 plan (14 tasks)
- `docs/superpowers/specs/2026-05-10-trader-tp-sl-design.md` — v1.5 design
- `docs/superpowers/plans/2026-05-10-trader-tp-sl.md` — v1.5 plan
- `TODO.md` — roadmap and v1.3 daemon split plan

## Trader

`poly-trader` is the headless trading process. It runs Martingale on Polymarket's BTC 5-minute up/down market.

### Quick start (dry-run, no real money)

```bash
docker compose up -d
poly-trader --direction up --base 5 --dry-run --max-windows 12
poly-tui    # observe events in another terminal
```

### Real money

```bash
poly-trader --direction up --base 5
```

### Take-profit / stop-loss exits (v1.5, strategy 4)

Backtest validated strategy 4 (TP+SL asymmetric) profitable across three independent 30-day samples (+$5,088 / +$9,802 / +$7,747). To run it live (start in dry-run):

```bash
poly-trader --direction up --base 5 --dry-run \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45
```

| Flag | Default | Notes |
|---|---|---|
| `--exit-rule` | `hold` | `hold` = v1.1 behavior. `tp-sl` enables strategy 4. |
| `--tp-price` | — | Required for `tp-sl`. UP-token bid level that triggers a take-profit sell. |
| `--sl-price` | — | Required for `tp-sl`. UP-token bid level that triggers a stop-loss sell. |
| `--poll-secs` | `5` | Gamma poll cadence during the window (1..=30). |

**Expected event order** (one TP-trigger window):
`WindowOpening -> EntryDecision{Enter} -> OrderPlaced -> OrderFilled -> ExitTriggered{Tp,bid} -> SellFilled -> LadderUpdated`

**Inspect trigger rate from Redis:**

```bash
docker exec poly-redis redis-cli XREVRANGE poly:prod:trader:events + - COUNT 100 \
  | grep -c ExitTriggered
```

Backtest distribution: ~29% TP, ~58% SL, ~13% deadline fall-through. If your live trace is far off, suspect gamma `outcomePrices` lag.

**Fall back to v1.1:** omit `--exit-rule` (or pass `--exit-rule hold`). No state migration needed; the ladder is mode-agnostic.

### Stop / resume

```bash
# stop
Ctrl+C  # current window completes, then exit

# resume
poly-trader --direction up --base 5    # picks up ladder from Redis

# fresh start (DANGER: discards open ladder)
poly-trader --direction up --base 5 --reset
```

### Inspect state

```bash
docker exec poly-redis redis-cli GET poly:prod:trader:ladder | jq .
docker exec poly-redis redis-cli XREVRANGE poly:prod:trader:events + - COUNT 10
tail -f logs/trader-*.log
```

### Risk caps

- `--max-step N` (default 5) — stop session after N consecutive losses
- `--band-min/--band-max` (default 0.45/0.55) — only enter when ask is in this range
- See `docs/superpowers/specs/2026-05-09-poly-trader-martingale-design.md` §7 for full failure handling

### BTC market watch strip

The TUI shows a 1-row strip with the current Polymarket BTC 5-min window's
price-to-beat, live Chainlink BTC/USD price, signed diff, and countdown to
window close. Independent of trader; works whether or not poly-trader is
running.

Configure the Polygon RPC endpoint via `POLYGON_RPC_URL` in `.env` (default:
`https://polygon-rpc.com`). If the default endpoint is rate-limited (HTTP 401
"API key disabled" is a known intermittent issue with the public RPC), use a
maintained provider URL like Alchemy or Infura.

## Backtest framework

`poly-backtest` runs 6 trading strategies (Martingale variants + a fixed-stake
baseline) against historical Polymarket BTC 5-min markets and writes a
self-contained HTML comparison report. Used for **strategy selection before
deploying real money**.

### Quick start

```bash
# Run a 30-day backtest on all 6 strategies
cargo run --release --bin poly-backtest -- \
  --start 2026-04-09 --end 2026-05-09 \
  --output backtest-report.html

# Open backtest-report.html in any browser
```

First run: ~15-25 min (downloads gamma + Binance data; ~50MB cache at
`~/.poly-backtest-cache/`). Subsequent runs: <1 min (cache hits).

### Strategies tested

1. `1_hold_martingale` — current v1.1 trader behavior (hold to resolution)
2. `2_tp_only_martingale` — take-profit at $0.75, no stop-loss
3. `3_tp_sl_symmetric` — TP $0.55 / SL $0.45
4. `4_tp_sl_asymmetric` — TP $0.85 / SL $0.45 (cut-loss-early)
5. `5_time_60s_martingale` — sell after 60 s
6. `6_fixed_stake_baseline` — $5 every round, no Martingale

### Headline results (30-day, 2026-04-09 → 2026-05-09)

σ ≈ $85.18 / 5min, friction 1.5%.

| Strategy | PnL | Win rate | Cap resets |
|---|---:|---:|---:|
| 1_hold_martingale       |    -$984 | 49.4% |  42 |
| 2_tp_only_martingale    |  -$3,817 | 55.5% |  20 |
| 3_tp_sl_symmetric       |  -$1,063 | 44.5% | 100 |
| 4_tp_sl_asymmetric      |  **+$5,088** | 29.2% | 179 |
| 5_time_60s_martingale   |  -$1,701 | 43.9% |  77 |
| 6_fixed_stake_baseline  | -$10,701 | 49.4% |  42 |

Only `4_tp_sl_asymmetric` is profitable in this window. See
`backtest-report.html` for equity curves, per-round PnL histograms, and
cap-reset event logs.

### Architecture

- BTC token prices synthesized via Black-Scholes binary-option oracle
  (BTC 1-min Binance closes → token bid/ask, parameterised by σ + friction)
- Reuses v1.1's `LadderState` + `apply_outcome` Martingale FSM unchanged
- Single-page HTML report with Chart.js (CDN)
- Independent of trader/TUI runtime — backtest doesn't touch live processes
- Disk cache at `~/.poly-backtest-cache/` (gamma windows + Binance candles)

```bash
# Run all backtest unit tests (42+ tests)
cargo test --lib backtest

# Network smoke test (1-day end-to-end, hits real gamma + Binance)
cargo test --test backtest_smoke -- --ignored
```

See `docs/superpowers/specs/2026-05-09-backtest-framework-design.md` and
`docs/superpowers/plans/2026-05-09-backtest-framework.md` for full design and
task breakdown.

## License

Private. Not for redistribution.
