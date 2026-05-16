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

## Deployment

### Platform support

Cross-platform Rust — builds and runs on **Linux**, **macOS**, and **Windows**. Linux is the recommended host for live trading: native tmux, systemd auto-restart, stable WS stack, no antivirus interference. Windows works fine for development.

### Hardware requirements

This is an IO-bound bot (HTTP polling + a handful of WebSockets), not CPU-bound. Even the most modest VPS is enough.

| Resource | Trader only | Trader + TUI + recorder |
|---|---|---|
| CPU | 1 vCPU | 1–2 vCPU |
| RAM | 256 MB | 512 MB – 1 GB |
| Disk | 1 GB (logs) | 25 GB (orderbook DB grows ~50 MB/day) |
| Network | 1 Mbps stable | 5 Mbps stable |
| Latency to AWS us-east-1 | <100 ms | <100 ms |

A $5/mo cloud VPS (1 vCPU / 1 GB RAM / 25 GB SSD) is sufficient. Polymarket's API lives in AWS us-east-1 — picking Tokyo/Singapore/Mumbai keeps WS round-trip at 30–50 ms.

### Linux quick start

```bash
# Dependencies (Debian/Ubuntu)
sudo apt install -y build-essential pkg-config libssl-dev redis-server tmux git curl

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Clone + build
git clone <repo-url> poly && cd poly
cargo build --release --bin poly-trader --bin poly-tui

# Configure
cp .env.example .env
# Edit .env — paste your dedicated wallet's POLYMARKET_PRIVATE_KEY

# Run under tmux
tmux new -s poly
./target/release/poly-trader \
  --direction up --base 10 --max-step 5 \
  --exit-rule tp-sl --tp-price 0.83 \
  --rsi-filter --maker --fixed-stake
# Ctrl-b d to detach; `tmux attach -t poly` to return
```

### systemd unit (auto-restart on crash)

Template at `docs/systemd/poly-trader.service` (edit paths/user, then `sudo cp` into place):

```ini
[Unit]
Description=Polymarket Trader
After=network.target redis.service
Requires=redis.service

[Service]
Type=simple
User=poly
WorkingDirectory=/opt/poly
EnvironmentFile=/opt/poly/.env
ExecStart=/opt/poly/target/release/poly-trader \
  --direction up --base 10 --max-step 5 \
  --exit-rule tp-sl --tp-price 0.83 \
  --rsi-filter --maker --fixed-stake
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now poly-trader
sudo journalctl -fu poly-trader   # tail logs
```

### Restarting after a rebuild

`poly-trader` writes a Redis lock (`poly:prod:trader:lock`) on startup but does not release it on shutdown. A bare `systemctl restart` after rebuilding will fail with `Error: another poly-trader is running (lock held)` because the stale lock still names the old PID. Always:

```bash
redis-cli DEL poly:prod:trader:lock
sudo systemctl reset-failed poly-trader
sudo systemctl restart poly-trader
```

### Remote monitoring (local TUI → remote trader)

Run `poly-tui` on your laptop pointed at the VM's Redis over an SSH tunnel. No need to open Redis to the internet, no need to build the TUI on the VM.

**Manual:**

```bash
# Terminal 1 — tunnel (keep open)
ssh -N -L 16379:127.0.0.1:6379 ubuntu@<vm-ip>

# Terminal 2 — TUI
REDIS_URL=redis://127.0.0.1:16379 ./target/release/poly-tui
```

**With the bundled shell helpers:**

| Shell | File | How to load |
|---|---|---|
| PowerShell (Windows) | `docs/profile/poly-tui-remote.ps1` | append to `$PROFILE` |
| bash / zsh (Linux, macOS) | `docs/profile/poly-tui-remote.sh` | `source` from `~/.bashrc` or `~/.zshrc` |

Edit the two variables at the top of the file (VM hostname/IP and TUI binary path). Then in any new shell:

| Alias | Effect |
|---|---|
| `poly-tunnel-up` | Start the SSH tunnel in the background |
| `poly-tunnel-down` | Kill the tunnel |
| `poly-tunnel` | Show tunnel status + VM event count |
| `poly-tui-remote` | Auto-start tunnel if needed, launch the TUI against VM Redis, restore `REDIS_URL` on exit |

The TUI auto-detects window length, refreshes every 5s, and the four bottom LEDs (CLOB · Redis · Trader · Chainlink) report dataflow health.

### Windows / macOS notes

Builds and runs identically with `cargo build --release`. The Windows dev environment uses a few PowerShell helper scripts (`/tmp/*.ps1`) for Redis inspection and process control — convenience only, not required. On macOS you can swap systemd for `launchd` or just keep `tmux` open.

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
- **v1.6** ✅ — TUI positions (live diagnostic of stuck shares)
- **v1.7** ✅ — Maker mode (`--maker` limit-order entry + TP)
- **v1.7.1** ✅ — `--window-minutes 5|15|60` flag (TUI auto-detects)
- **v1.7.2** ✅ — Backtest oracle noise + SL parameter sweep
- **v1.7.5** ✅ — Real Polymarket trade-history backtest (`--oracle real`, strategies 12/13)
- **v1.8** ✅ — `--exit-rule hold-early-exit` trader (BUY → wait → market-sell at t=exit-at-secs); pairs with Polymarket Auto-Redeem for stuck-winner recovery
- **v1.9** ✅ — `--exit-rule hold` rewritten: chainlink pre-close outcome at t=window_close−4s + Auto-Redeem (no SELL); share-based Martingale ladder (`base_shares: u32`)
- **v1.3** — daemon / TUI split. Required before any new trading logic (multi-strategy, dynamic config, etc.)
- **v1.10+** — strategies 8/9 (TP+SL, top backtest performers); unblocked now that Auto-Redeem is on

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
- `docs/superpowers/specs/2026-05-10-tui-positions-design.md` — v1.6 design
- `docs/superpowers/plans/2026-05-10-tui-positions.md` — v1.6 plan
- `docs/superpowers/specs/2026-05-10-window-minutes-design.md` — v1.7.1 design
- `docs/superpowers/plans/2026-05-10-window-minutes.md` — v1.7.1 plan
- `docs/superpowers/specs/2026-05-10-backtest-oracle-noise-design.md` — v1.7.2 design
- `docs/superpowers/plans/2026-05-10-backtest-oracle-noise.md` — v1.7.2 plan
- `docs/superpowers/specs/2026-05-10-real-trade-backtest-design.md` — v1.7.5 design
- `docs/superpowers/plans/2026-05-10-real-trade-backtest.md` — v1.7.5 plan
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

### Maker mode (v1.7)

The `--maker` flag switches BUY entry + TP exit from market orders to limit orders. Saves ~1% taker fees per round-trip. SL stays as market sell — a limit-priced SL would not protect against fast price drops.

```bash
poly-trader --direction up --base 5 \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45 \
  --maker
```

| Time | Action |
|---|---|
| t=0 | LIMIT BUY @ ask−$0.01 (e.g. 0.49) |
| t=30 | Cancel + re-post @ ask (0.50) |
| t=60 | Cancel + re-post @ ask+$0.01 (0.51, becomes taker) |
| t=90 | Cancel + skip window (no entry) |
| after buy fill | LIMIT TP @ tp_price (e.g. 0.85) |
| TP fully fills | Won, exit |
| TP partial fill | Keep resting, accumulate proceeds |
| SL bid <= sl_price | Cancel TP, market sell residual |
| t=270 | Cancel TP, market sell residual at current bid |

`--maker` requires `--exit-rule tp-sl` (will reject otherwise). Default is off — v1.5 market-order behavior preserved bit-for-bit.

**Caveats:**
- Requires Polymarket maker-fee structure for actual savings. If maker == taker, v1.7 == v1.5 cost.
- Lower window participation (~5–10% windows skipped due to entry sweep exhausting). Backtest assumed 100% — discount expectation accordingly.
- Fill detection via 2s polling (≤2s latency vs market order's instant fill).

### v1.8 — `hold-early-exit`

```bash
poly-trader --direction up \
  --exit-rule hold-early-exit \
  --exit-at-secs 270
```

BUY taker at entry, hold the position, then market-sell at `t = exit-at-secs` (max `window_seconds - 30`). For 5-min windows, backtest-validated value is `270`.

**Backtest:** 30-day real-trade replay (`report-real-30d.html`) shows **+$1,505 PnL** over 8503 windows for `13_hold_early_exit_270`, on par with the legacy `1_hold_martingale` baseline. Trade-data freshness check: 99.5% of windows have a SELL trade within 60s before t=270s (median gap = 0s).

**Live run #1 (2026-05-12, 12 windows / 1 hour):** **+$16.83** realized PnL (8× backtest projection). FAK SELL rejection rate: 25% — both losing and winning windows can fail (winners stop quoting near $1.00). See `TODO.md` v1.8 for the operational findings.

**Prerequisite — Auto-Redeem (enabled per-wallet):** Polymarket's "Get Paid Instantly" auto-pays winning stuck shares to USDC at resolution. Enabled via the portfolio UI on first redeem (one-time signature). With Auto-Redeem on, the FAK SELL failure mode (25% of windows) is operationally invisible — stuck winners convert to cash automatically; stuck losers resolve to $0 with no action needed.

**Without Auto-Redeem:** stuck winning shares require `poly-redeem` + MATIC for gas. Auto-Redeem is strictly recommended for live use.

**Alternative — strategies 8/9:** TP=0.85 / SL=0.30-0.35 score higher in backtest (+$1,696 to +$1,824) but require a different exit-rule (v1.10, planned).

| Flag | Valid with | Notes |
|---|---|---|
| `--exit-rule hold-early-exit` | (new) | Requires `--exit-at-secs`. Rejects `--maker`. |
| `--exit-at-secs <u32>` | only with `hold-early-exit` | Range: 1..=(window_seconds - 30). No default — must specify explicitly. |

### v1.9 — `hold` rewritten (chainlink pre-close + Auto-Redeem)

```bash
poly-trader --direction up \
  --exit-rule hold \
  --base 5 --max-step 8
```

Replaces the v1.1 "hold + market-sell winner" flow with a Chainlink-driven outcome decision:

1. BUY taker at window open
2. Sleep until **t = window_close − 4s** (e.g., t=296 for 5-min windows)
3. Query Chainlink BTC/USD on Polygon, compare to `price_to_beat`
4. Emit `Won` / `Lost` to the FSM (Martingale escalates on Lost as usual)
5. **No SELL** — Polymarket Auto-Redeem credits winning shares to USDC at gamma resolution (~3-10s later)

**Requires Auto-Redeem enabled** on the proxy wallet (one-time on-chain signature via portfolio UI). Without it, winning resolutions still pay, but you'd need `poly-redeem` + MATIC to claim them.

**Why pre-close instead of post-resolution:**
- Returns 4s before window close → scheduler catches next window's t=0 boundary
- Strict Martingale: each outcome resolves before next entry (no lag from gamma's UMA delay)
- Risk: BTC moves >$price_to_beat threshold in the final 4s ⇒ misclassification (~1% borderline windows). Auto-Redeem still pays correct cash; only the ladder's instantaneous accounting may briefly disagree.

**v1.8.2 share-based Martingale** (shipped same commit):
- `--base <N>` is interpreted as **shares** (was USD); defaults align with Polymarket's 5-share CLOB minimum
- Doubling sequence: 5/10/20/40/80/160/320/640 shares
- USD cost per step depends on entry ask (e.g., 5 sh × $0.50 ask = $2.50 step-1 cost)

**Fallbacks:**
- Chainlink RPC failure → falls back to gamma resolver (may miss next window's t=0 BUY)
- `price_to_beat` missing → same fallback

Spec / commit: `209e859`.

### v1.11 — RSI direction filter + LIVE/DRY-RUN indicator + CLOB rounding fix

```bash
poly-trader --direction up \
  --base 5 --max-step 5 \
  --exit-rule tp-sl --tp-price 0.87 \
  --rsi-filter
  # (omit --sl-price for TP-only behavior; --dry-run for sim)
```

**RSI gate.** Before each window the trader fetches the last 15 BTC 1-min closes from Binance and computes Wilder RSI(14):

| RSI | Action |
|---|---|
| < `--rsi-oversold` (30) | bet **UP** (mean reversion: BTC sold off) |
| > `--rsi-overbought` (70) | bet **DOWN** (BTC ran up) |
| neutral zone | **SKIP** (no trade) |

`--direction` becomes a fallback used only on Binance fetch failure; the live direction is decided per window. Matches the `33_rsi_fixed_tp87` and `41_rsi_mart_tp87` backtest variants exactly.

**TP-only via tp-sl mode.** Omit `--sl-price` and the trader uses an effective $0.001 floor (never triggers) for clean take-profit behavior.

**Safety indicator.** TUI status bar prefixes the Trader line with **`LIVE`** (white-on-red) for real-money mode or **`DRY-RUN`** (yellow-on-black) for simulation. Persisted on `LadderState.dry_run` so resumes can't silently flip modes — mid-session mode switch errors out and demands `--reset`.

**CLOB rounding fix.** `buy_fok` rounds `maker_amount` to 2 decimals (AwayFromZero) before submission; Polymarket rejects 3+ decimals with `"invalid amounts"`. SELL path already had this via `trunc_with_scale(2)`.

### v1.12 — Fixed-stake + hybrid TP-limit-sell + retry backoff

```bash
poly-trader --direction up \
  --base 5 --max-step 5 \
  --exit-rule tp-sl --tp-price 0.87 \
  --rsi-filter \
  --fixed-stake \         # every BUY = base_shares; no Martingale; no cap
  --tp-limit-sell         # taker BUY + limit SELL @ TP (no missed touches)
```

**`--fixed-stake`.** Disables Martingale doubling. Every BUY is `base_shares` shares (e.g., 5 shares ≈ $2.50 stake at $0.50 ask). Losses do not advance the ladder, so `StopReason::CapReached` is never triggered — the trader runs indefinitely. Matches backtest `StakeRule::Fixed` (strategies 6, 22-40). `LadderState.fixed_stake` is persisted to Redis; mid-session mode-switch is refused (you'd silently halve or 6× your risk profile).

**`--tp-limit-sell`.** Hybrid mode: taker FoK BUY (guaranteed entry) + limit SELL posted at `--tp-price` immediately after the fill. The sell sits in the order book so any momentary bid touch ≥ TP gets filled — eliminates the miss-rate of the 5-second polling exit watcher. Falls back to market sell at t=window_close−30s if the limit hasn't filled. Mutually exclusive with `--maker`.

**CLOB settlement-lag handling.** Polymarket's order-match engine returns BUY fills synchronously, but the position ledger updates asynchronously (~1-2s). Posting a SELL during that gap produces `"balance: 0"` rejections. v1.12.1 sleeps 2s after every BUY before attempting SELL. v1.12.2 retries with **2s / 4s / 8s exponential backoff** (each failure emits an `Alert` event), and the market-sell fallback uses the same pattern. Combined max wait 30s; window has 300s. When both SELL paths exhaust retries, the trader counts the win at `shares × $1.00` (Auto-Redeem will credit USDC on the winning side at gamma resolution).

### Backtest validation tools (v1.12)

```bash
poly-backtest --start 2026-05-13 --end 2026-05-14 \
  --oracle real \
  --strategies 41_rsi_mart_tp87 \
  --stop-at-cap \                # exit at first cap (matches live behavior)
  --start-ts 1778638500 \        # filter windows to ≥ this unix epoch
  --output report-live-match.html
```

**Use case.** Replay a live session in backtest to validate that observed outcomes match the model. `--stop-at-cap` makes the runner exit at the first `is_stopped()` ladder transition (mirrors `StopReason::CapReached`); without it, the runner resets the ladder and continues. `--start-ts` filters out warm-up windows when the live session started mid-day.

**Finding from the 2026-05-13 validation.** With matched time range and stop-at-cap, the backtest produced **−$51.66 / 1 cap / max_consec_losses=5**, vs live **−$75.62 / 1 cap / max_consec_losses=5**. Structurally identical, but the backtest's `real` oracle is ~30% optimistic on PnL because it counts any recorded trade ≥ TP as a hit — live's 5s polling missed the brief touches. v1.12's `--tp-limit-sell` is the direct fix for that bias.

### Scalp research tooling (v1.13)

Four standalone binaries for exploring strategies outside the RSI-Martingale family. All read-only with respect to the live trader's Redis lock + state.

| Binary | Purpose |
|---|---|
| `poly-scalp-analyze` | Offline analyzer over cached trade history. Finds $0.01 → $0.02 oscillation events per window with magnitude / time / per-window-count histograms. |
| `poly-scalp` | Live observation mode (no real orders). Subscribes to Polymarket CLOB `market` channel, runs a dual-leg maker scalp state machine (BID @ $0.01 + ASK @ $0.02), prints would-have-filled events + per-minute stats. |
| `poly-orderbook-recorder` | Continuous SQLite recorder of best bid/ask per token, 1-sec sampling. Writes to `~/.poly-orderbook/recorder.db` (WAL mode). ~50 MB/day. Foundation for depth-aware backtests. |
| `poly-orderbook-report` | Read-only HTML dashboard combining the recorder DB + cached trade history. Renders volatility distribution, active-vs-resolution phase comparison, and a maker fill-rate estimate proxy. |

**Negative-result finding (1c/2c scalp).** Backtest of cached trade history showed a 90% bounce rate from $0.01 to $0.02+ within 484 sample windows. Live observation contradicted this: 5 entries / 0 bounces / 0% hit rate over 3+ hours. Root cause confirmed by the orderbook report: 92% of bounces in the trade-history analysis concentrate in the last 30% of each window (resolution-phase order-book noise). The `poly-scalp` prototype's required 240s entry cutoff (no new entries in the last 60s) by design avoids that phase, leaving zero captureable opportunities within active trading. Sell-volume at $0.01 in the active 0-240s phase = 0 shares across observed days. **Documented for future reference: any maker-scalp idea must constrain analysis to the active-trading window from the start.**

### Window length (v1.7.1, 5/15/60 min)

Default `--window-minutes 5` reproduces v1.7 behavior. Polymarket also offers 15-minute and 60-minute BTC up/down markets:

```bash
# 15min — 4x deeper liquidity than 5min, better for --maker
poly-trader --window-minutes 15 --direction up --base 5 --maker \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45

# 60min — supported but unvalidated by backtest
poly-trader --window-minutes 60 --direction up --base 5 \
  --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45
```

| Window | Backtest | Liquidity | Hourly exposure (--max-step 5) |
|---|---|---|---|
| 5min  | validated +$7.5K/30d | ~$8K | $60/hr base |
| 15min | unvalidated         | ~$32K | $20/hr base |
| 60min | unvalidated         | varies | $5/hr base |

Probability structure (band, TP, SL on UP token) is window-length invariant; the strategy's positive expectancy on 5min should carry to 15/60 in theory, but real-money A/B is the only proof.

The TUI auto-detects the trader's window length from its event stream — no separate flag needed. If the trader switches windows mid-state (without `--reset`), it refuses to start with a clear error.

### Dry-run vs real-money differences

Dry-run uses `SimulatedExecutor` with these approximations vs the real CLOB:

| Aspect | Dry-run | Real CLOB |
|---|---|---|
| Buy fill price | Hard-coded `$0.50` | Actual best-ask at request time |
| Sell on TP/SL trigger | `bid_hint × 0.99` (1% slippage) | Crosses spread; deeper books at high stakes can slip 5–10% |
| Polymarket taker fee | Lumped into the 1% slippage | Separate ~1% taker fee on each market sell |
| Buy/sell rejection | Never fails | `FoK rejected` on thin liquidity → window skipped |
| Gas (Polygon) | Zero | ~$0.001 per order |
| Execution latency | Immediate | 200–500ms REST round-trip; price can drift between trigger detection and fill |

Net effect: real PnL is typically 5–15% lower than dry-run for the same window outcomes. Plan for the gap before scaling up.

### First real-money run checklist

Before dropping `--dry-run`:

1. Wallet has at least `(base × 2^max_step)` USDC available — for `base=5 max_step=5`, that's $155 single-session cap.
2. Run `--dry-run --max-windows 12` first to verify event flow and that trigger distribution is in the ballpark of backtest (~29% TP / 58% SL / 13% resolution).
3. Start real money with `--max-windows 12` (1hr) to compare real fills vs dry-run for a small sample.
4. Watch for `Alert` events in Redis stream — they indicate stuck shares that need manual reconciliation. Stop the trader if any fire.
5. After 1hr looks clean, drop `--max-windows` to run continuously.

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

## TUI positions

The balance box shows live Polymarket positions polled from `data-api.polymarket.com/positions?user=<proxy_address>` every 30s. The proxy address is derived deterministically from your `POLYMARKET_PRIVATE_KEY` at startup.

| Cache state | Render |
|---|---|
| No fetch yet | `Loading positions…` |
| Empty array | `No open positions` |
| 1 holding | `Holding: 10 UP @ $0.500  now $4.85 (-3%)` |
| Multiple | One line per holding |

Why: catches stuck shares from `SellRejected` / `Alert` events, leftover positions from prior sessions, and any holdings the trader didn't open.

To inspect the cached positions outside the TUI:

```bash
docker exec poly-redis redis-cli GET poly:prod:positions | jq .
```

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

### Real Polymarket trade-history oracle (v1.7.5)

Replaces theoretical BS with **actual recorded SELL/BUY trades** from Polymarket's data-api. Auto-fetches uncached windows on first run; cached at `~/.poly-backtest-cache/trades/<window_ts>.json`.

```bash
# First run on a 30-day range — auto-fetches trades (~17 min, throttled).
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle real \
  --output report-real.html

# Subsequent runs reuse cache (~30s):
poly-backtest --start 2026-04-09 --end 2026-05-09 --oracle real \
  --strategies 12_tp75_early_exit_270,13_hold_early_exit_270 \
  --output report-real-candidates.html
```

| `--oracle` | Source | Friction | Determinism |
|---|---|---|---|
| `bs` (default) | Black-Scholes mid + symmetric friction | `--friction` (default 1.5%) | exact |
| `noisy` | BS + per-tick Gaussian noise | `--friction` + `--oracle-noise` | seeded reproducible |
| `real` | Last in-window SELL/BUY trade | embedded in observed prices | exact (data-driven) |

**New strategies (added v1.7.5):**

- `12_tp75_early_exit_270`: BUY in band → limit TP @ 0.75 → at t=270s, market-sell residual at bid. No resolution path.
- `13_hold_early_exit_270`: BUY in band → hold → at t=270s, market-sell at bid. No resolution path.

Both candidates exit BEFORE window resolution (t=300s) to avoid post-resolution redemption (which currently requires MATIC the EOA doesn't have). Pre-trade fallback is a flat 0.5 (no forward-look bias).

`strategy_set()` now returns 13 strategies (1-11 unchanged + 12 + 13). `--oracle bs` (default) reproduces v1.7.2 numbers byte-identically.

### RSI strategies (v1.11)

Strategies 16-41 add an RSI(14) direction signal: `RSI<30 → UP`, `RSI>70 → DOWN`, neutral zone → SKIP. Combined with TP/SL exit rules and stake (Fixed or Martingale), they form the v1.11 strategy family. `strategy_set()` now returns 41 strategies.

**Backtest headline (real oracle, 2026-02-10 → 2026-05-10, 22,561 windows):**

| Strategy | PnL | Win rate | Cap resets | Notes |
|---|---:|---:|---:|---|
| 17 RSI Mart, hold to resolution | $3,546 | 52.3% | 62 | original RSI Mart |
| **33 RSI Fixed, TP=$0.87** | **$4,337** | **60.6%** | 44 | safest +EV (single trade max −$5) |
| **41 RSI Mart, TP=$0.87** | **$7,050** | **60.6%** | 44 | highest PnL; max sequence loss −$155 at step 5 |
| 19 always_down baseline | $718 | 49.9% | 162 | direction baseline (no RSI) |
| 20 random_direction baseline | −$820 | 50.1% | 173 | no-alpha control (SplitMix64-seeded) |

**Sweep findings:**
- TP grid 0.55→0.95: PnL plateau at $0.85-$0.91 (~$4.3k), peak $0.87 ($4,337). Tighter TP = higher win rate but smaller per-win profit.
- SL grid 0.20→0.40 (on TP=$0.87): adding any SL **hurts** PnL by ~$3.6-3.9k. RSI<30 oversold windows V-shape recover too often to cut early.
- Random baseline (-$820) confirms always-UP's +$2,444 in this period was bull-market luck, not direction edge — the RSI strategies' alpha (~$2k vs random) is the real signal.
- **EMA trend filter (strategies 42-44, v1.12 experiment) HURTS PnL.** RSI Mart + TP=0.87 with EMA(50) slope-based "skip counter-trend" filter at threshold = $2/$5/$10/min produced $2,064 / $2,754 / $4,959 respectively — all below the no-filter baseline of $7,050. Win rate barely moved (60.6% → 59.1-59.8%), but the filter cuts winning windows along with losing ones. Lesson: 5-min binary markets reverse often enough during strong trends that betting against trend still has positive expectancy.

## License

Private. Not for redistribution.
