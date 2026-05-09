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
- **v1.3** — daemon / TUI split. Required before any new trading logic (multi-strategy, dynamic config, etc.)
- **v1.4+** — markets, positions, advanced strategies, observability

## Documentation

- `docs/superpowers/specs/2026-05-06-poly-tui-balance-starter-design.md` — v1.0 design
- `docs/superpowers/plans/2026-05-06-poly-tui-balance-starter.md` — v1.0 plan (14 tasks)
- `docs/superpowers/specs/2026-05-09-poly-trader-martingale-design.md` — v1.1 trader design
- `docs/superpowers/plans/2026-05-09-poly-trader-martingale.md` — v1.1 plan (23 tasks)
- `docs/superpowers/specs/2026-05-09-market-watch-strip-design.md` — v1.2 BTC strip design
- `docs/superpowers/plans/2026-05-09-market-watch-strip.md` — v1.2 plan (12 tasks)
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

## License

Private. Not for redistribution.
