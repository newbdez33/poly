# v1.6 — Live positions in TUI balance box

**Goal:** Show open Polymarket positions inside the TUI balance box so the operator can immediately see stuck shares from a `SellRejected`/`Alert` event, leftover positions from prior sessions, or any holdings the trader didn't open.

**Non-goal:** Closed-position history, per-market drill-down, manual reconciliation actions, push notifications. The strip is read-only diagnostics.

## Context

`poly-tui` v1.0 ships USDC balance via `Refresher → Redis → App` — a periodic poll writes to Redis, the App reads on each render tick. The balance box currently shows a single line: `USDC: $173.69`.

Polymarket exposes a public read-only endpoint at `https://data-api.polymarket.com/positions?user=<address>` that returns a JSON array of open positions for the given proxy address. No auth required.

The proxy address — where USDC and CTF tokens actually live for email/Magic-derived Polymarket accounts — can be derived deterministically at startup from the EOA private key via the SDK's `derive_proxy_wallet(eoa, POLYGON)` function. No new env var needed.

## Architecture

A new `Positioner` task mirrors the existing `Refresher` pattern: poll → write Redis → emit `AppEvent` for immediate render. The App reads positions from Redis on each render tick. This keeps the App pure-renderer (zero I/O), preserves the v1.3 daemon-split seam, and reuses the established trait/cache/Redis pattern operators already understand.

```
                   ┌──────────────────────────────────┐
                   │      poly-tui process            │
crossterm  keys    │                                  │
   ─────▶ Input ─┐ │                                  │
                 ▼ │                                  │
                ┌──────────┐                          │
                │   App    │                          │
                │  + UI    │                          │
                └────┬─────┘                          │
                     │ read                           │
                     ▼                                │
                ┌─────────────────────────┐           │
                │       Redis             │           │
                │  poly:prod:balance      │ ◀ Refresher (existing)
                │  poly:prod:positions    │ ◀ Positioner (new)
                └─────────────────────────┘           │
                                ▲                     │
                                │                     │
                       Polymarket data-api            │
                       /positions?user=<proxy>        │
                                                      │
                       Proxy address derived once     │
                       at startup via SDK's           │
                       derive_proxy_wallet(eoa,POLYGON)│
                   ────────────────────────────────────
```

Two new trait abstractions decouple the loop from concrete clients:

- `PositionsFetcher` — real impl wraps reqwest GET to data-api; fake impl in `tests/support/fake_positions_fetcher.rs`
- `PositionsCache` — real impl wraps `fred` (Redis); fake impl in `tests/support/memory_positions_cache.rs`

These mirror the existing `BalanceFetcher` / `BalanceCache` pattern.

## Render

The balance box gains a second line. The line always renders to keep layout stable as positions open/close.

```
┌─ poly-tui ─────────────────────────────────────────────┐
│                    USDC: $173.69                        │
│      Holding: 10 UP @ $0.500  now $4.85 (-3%)          │
└─────────────────────────────────────────────────────────┘
```

Render states:

| Cache state | Second line |
|---|---|
| Has 1 position, profitable | `Holding: 10 UP @ $0.500  now $5.85 (+17%)` (green pct) |
| Has 1 position, losing | `Holding: 10 UP @ $0.500  now $4.85 (-3%)` (red pct) |
| Has 1 position, flat (\|pct\| < 1%) | `Holding: 10 UP @ $0.500  now $4.95 (±0%)` (white) |
| Has multiple positions | One line per position; balance box grows vertically |
| `[]` (empty result) | `No open positions` (dim) |
| Cold start (no fetch yet) | `Loading positions…` (dim) |
| Fetch stale > 90s | Dim the line and append `(stale)` (mirrors balance LED behavior) |
| Cache (Redis) unavailable | `Positions unavailable` (red) |

Each line uses the same colored health pattern as the BTC strip (DarkGray for stale data, regular for fresh). Percent diff uses the same rounding rule as the BTC diff: classify sign from the rounded integer percent, not the raw decimal.

## Components

### `src/positions.rs` *(new)*

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side { Up, Down }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub token_id: String,
    pub side: Side,
    pub market_slug: String,
    pub shares: Decimal,
    pub avg_price: Decimal,     // USDC paid per share
    pub current_price: Decimal, // current bid per data-api
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Positions {
    pub items: Vec<Position>,
    pub fetched_at: DateTime<Utc>,
}

impl Position {
    pub fn cost_usd(&self) -> Decimal { self.avg_price * self.shares }
    pub fn value_usd(&self) -> Decimal { self.current_price * self.shares }
    /// Percent gain/loss vs cost. Returns 0 if cost is zero (avoid div-by-zero).
    pub fn pnl_pct(&self) -> Decimal { ... }
}

#[async_trait]
pub trait PositionsFetcher: Send + Sync {
    async fn fetch(&self) -> Result<Positions, FetchError>;
}

#[async_trait]
pub trait PositionsCache: Send + Sync {
    async fn read(&self) -> Result<Option<Positions>, CacheError>;
    async fn write(&self, p: &Positions) -> Result<(), CacheError>;
}

pub const POSITIONS_KEY: &str = "poly:prod:positions";
```

`FetchError` and `CacheError` reuse the existing variants in `src/domain.rs` where shape matches; otherwise add minimal new variants.

### `src/adapters/polymarket_positions_wrapper.rs` *(new)*

```rust
pub struct PolymarketPositionsFetcher {
    client: reqwest::Client,
    user_address: String,
    base_url: String, // default https://data-api.polymarket.com
}

impl PolymarketPositionsFetcher {
    pub fn new(user_address: String) -> Self { ... }
    pub fn with_base_url(user_address: String, base_url: String) -> Self { ... }
}

#[async_trait]
impl PositionsFetcher for PolymarketPositionsFetcher {
    async fn fetch(&self) -> Result<Positions, FetchError>;
}

/// Pure decoder. Polymarket data-api returns an array of objects with these
/// fields (per observation):
/// - asset (token_id, string)
/// - outcome ("Up" | "Down")
/// - slug (market slug)
/// - size (shares, number)
/// - avgPrice (number)
/// - curPrice (current bid, number)
///
/// Unknown fields are ignored. Missing required fields cause a Decode error
/// for that one entry; other entries still parse. Outcome strings other than
/// "Up"/"Down" are filtered out (Polymarket uses other markets too).
pub fn decode_positions_response(body: &str) -> Result<Vec<Position>, FetchError>;
```

The decoder treats a top-level `[]` as success (zero positions). HTTP non-2xx is a `FetchError::Network`. Empty body is a `FetchError::Decode`.

### `src/adapters/redis_positions_wrapper.rs` *(new)*

```rust
pub struct RedisPositionsCache { ... }

impl RedisPositionsCache {
    pub async fn connect(redis_url: &str) -> Result<Self, CacheError>;
}

#[async_trait]
impl PositionsCache for RedisPositionsCache {
    async fn read(&self) -> Result<Option<Positions>, CacheError>;
    async fn write(&self, p: &Positions) -> Result<(), CacheError>;
}
```

JSON serde via `serde_json`. Key: `poly:prod:positions`. No TTL (positions are always read; staleness shown via `fetched_at` comparison in the UI).

### `src/positioner.rs` *(new)*

Mirrors `src/refresher.rs` line-for-line:

```rust
pub struct PositionerConfig {
    pub interval: Duration,
}

pub async fn run(
    fetcher: Arc<dyn PositionsFetcher>,
    cache: Arc<dyn PositionsCache>,
    event_tx: mpsc::Sender<AppEvent>,
    cfg: PositionerConfig,
    shutdown: CancellationToken,
);
```

Loop:
1. `tokio::select!` between `shutdown.cancelled()` and `interval.tick()`.
2. On tick: fetch. On `Ok(p)` → `cache.write(&p)` and `event_tx.send(AppEvent::PositionsUpdate(p))`. On `Err(e)` → log warn, continue.
3. First fetch happens immediately at task start (before first 30s sleep) so the operator doesn't wait 30s on launch for positions to appear.

### `src/domain.rs` *(modify)*

Add to `AppEvent`:

```rust
PositionsUpdate(Positions),
```

### `src/app.rs` *(modify)*

`UiState` gains `positions: Option<Positions>`. The `handle_event` arm for `PositionsUpdate` updates that field. The `tick_once` reads the latest `Positions` from cache on tick (same pattern as balance) and updates `UiState`.

### `src/ui.rs` *(modify)*

`render_balance` becomes a 2-line `Paragraph` instead of a single-line one. First line: existing USDC. Second line: positions per the render-states table above.

### `src/bin/poly-tui.rs` *(modify)*

Adds:
1. Derive proxy address from `cfg.polymarket_private_key` using SDK function.
2. Construct `PolymarketPositionsFetcher::new(proxy_addr_str)` (always real — public API).
3. Construct `RedisPositionsCache::connect(&cfg.redis_url)`.
4. Spawn `positioner::run` with `interval = Duration::from_secs(cfg.refresh_interval_secs)` (reuses existing `REFRESH_INTERVAL_SECS` env var).

If proxy derivation fails, hard-fail with a clear message before any UI starts.

## Data flow (typical 30s cycle)

```
t=0      tui startup: derive proxy_addr 0x123...abc
         spawn Refresher (USDC balance, every 30s)
         spawn Positioner (positions, every 30s)
         render shows: "USDC: --" "Loading positions…"

t=0+1s   Refresher: writes balance to Redis, emits BalanceUpdate
         App reads balance from Redis or event → renders "USDC: $173.69"
t=0+2s   Positioner: fetches /positions?user=<proxy>
         Decodes 1 position: 10 UP @ 0.50, current 0.485
         Cache.write → poly:prod:positions
         Emits PositionsUpdate
         App: UiState.positions = Some(...)
         render: "Holding: 10 UP @ $0.500  now $4.85 (-3%)"

t=30s    Positioner: fetches again
         Position now empty (trader sold)
         render: "No open positions"

t=60s    Positioner: fetches, network error
         Log warn, no Redis write
         render: line stays at last known state
         (after 90s without success → "(stale)" suffix appears)
```

## Errors and edge cases

| Scenario | Handling |
|---|---|
| Network error on fetch | Log warn, keep cached value, no event emit |
| Decode error on response | Log warn with payload prefix (≤200 chars), keep cached |
| Single bad entry in array | Drop just that entry, log warn, keep good entries |
| Outcome name unknown (not "Up"/"Down") | Drop entry silently — Polymarket has many markets, only BTC up/down has Up/Down outcomes |
| Redis unavailable on write | Log warn; don't crash the loop. Last in-memory event still flows to UI. |
| Redis unavailable on read | UI sees `read()` Err → renders "Positions unavailable" |
| Cold start, no positions ever | Render "Loading positions…" until first Positioner tick |
| Cold start, first fetch returns `[]` | Render "No open positions" |
| Proxy derivation returns `None` | Hard-fail at startup with clear message — TUI cannot proceed |
| Position with `cost == 0` | `pnl_pct()` returns 0, prevents div-by-zero |
| Multiple positions | Stack each on its own line in balance box |
| Refresh stale (no successful fetch in last 90s) | Append `(stale)` and dim the line |

90s staleness threshold = 3× the 30s poll interval — gives 3 chances to recover before signalling stale.

## Testing

### Unit

| File | New tests |
|---|---|
| `positions.rs` | 5 tests: cost_usd math, value_usd math, pnl_pct positive/negative/zero/zero-cost; Side parse from "Up"/"Down"/"up"/"down". |
| `polymarket_positions_wrapper.rs` | 6 tests: decode single position; decode empty array; decode multiple; decode missing required field returns partial; decode unknown outcome filtered; decode malformed JSON errors. |
| `redis_positions_wrapper.rs` | 3 tests: round-trip Positions; read returns None when key missing; write overwrites previous. (Reuses test patterns from `BalanceCache`.) |
| `positioner.rs` | 4 tests: emits PositionsUpdate after first tick; writes to cache; logs and continues on fetch error; exits on shutdown. (Reuses test patterns from `refresher.rs`.) |
| `ui.rs` | 6 insta snapshots for render_balance: no positions, 1 position profitable, 1 losing, 1 flat, multiple, loading. |
| `app.rs` | 2 tests: PositionsUpdate event updates UiState; tick reads positions from cache. |

### Integration (`tests/positions_integration.rs`)

Testcontainers Redis + fake fetcher → spawn Positioner → assert:
- After 1 fake tick, Redis key `poly:prod:positions` contains the expected JSON.
- AppEvent::PositionsUpdate received on the channel.

### E2E (`tests/positions_e2e.rs`, `#[ignore]`)

Real `data-api.polymarket.com` fetch with a known empty address (e.g. `0x0000…0001`). Asserts decoder accepts an empty array without panicking. No assertion on specific positions (depends on real account state).

### Coverage gate

`cargo llvm-cov` must keep ≥80% on the new files. Trait definitions and pure decoders should hit ≥95%.

## Configuration

No new env vars. Reuses existing:

- `POLYMARKET_PRIVATE_KEY` — to derive proxy address at startup
- `REDIS_URL` — for the new cache
- `REFRESH_INTERVAL_SECS` — for poll cadence (defaults 30s)

The data-api hostname is hard-coded as a sane default but constructible via `with_base_url` for tests.

## Migration / rollback

- Adds a new Redis key `poly:prod:positions`; doesn't touch existing keys.
- TUI gracefully renders without a Redis key (cold start path).
- Rollback: revert the binary; the dead Redis key is harmless until next `docker compose down -v` cleanup.
- No state migration required.

## Out of scope (explicit YAGNI)

- Authenticated positions endpoints (`data-api/v2/positions` requires CLOB API key — public unauthenticated endpoint is sufficient for the diagnostic use case).
- Per-position close button or manual reconcile UI.
- Position-change alerts when stuck shares detected (could be added later as a separate `Alert`-style mechanism).
- Closed/historical position tracking — only currently-held shares are shown.
- Caching positions across multiple users (single-user TUI).
- Aggregating exposure across markets (single-market trader, agg = sum).

## Related documents

- v1.0 TUI starter spec: `docs/superpowers/specs/2026-05-06-poly-tui-balance-starter-design.md`
- v1.5 trader TP/SL spec: `docs/superpowers/specs/2026-05-10-trader-tp-sl-design.md`
- Polymarket data-api docs: https://docs.polymarket.com/data-api (referenced; no offline copy)
