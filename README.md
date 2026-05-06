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

- **v1.0** — TUI starter (this release)
- **v1.1** — daemon / TUI split. Required before adding any real trading logic so the bot can run without a UI.
- **v1.2+** — markets, positions, order placement, strategy framework.

## Documentation

- `docs/superpowers/specs/2026-05-06-poly-tui-balance-starter-design.md` — v1.0 design
- `docs/superpowers/plans/2026-05-06-poly-tui-balance-starter.md` — implementation plan (14 tasks)
- `TODO.md` — roadmap and v1.1 refactor plan

## License

Private. Not for redistribution.
