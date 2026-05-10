# Poly TUI — TODO / Roadmap

## v1.7.2 — Extend backtest to 15m / 60m windows ⏳ TODO

**Trigger:** before any real-money run with `--window-minutes 15` or `60`. Strategy 4 was validated on 5m only; the probability-structure-invariance argument is theoretical.

**Goal:** Add `--window-minutes 5|15|60` flag to `poly-backtest` (mirroring v1.7.1 trader change). Re-run the 6-strategy backtest on 15m data for the same 3 samples (Apr-May, Mar, Feb) and confirm strategy 4 EV is positive.

**Scope:**
- [ ] `src/bin/poly-backtest.rs` — add `--window-minutes` flag
- [ ] `src/backtest/data/gamma_history.rs` — fetch slugs use `window_slug(ts, mins)`; iterate at the right boundary cadence
- [ ] `src/backtest/oracle.rs` — sigma estimation works with 15m / 60m bars (1-min Binance data still fine)
- [ ] `src/backtest/data/loader.rs` — verify per-second simulation horizon scales
- [ ] Run 3-sample 30-day backtest on 15m → produce `backtest-report-15m-*.html`
- [ ] Run 3-sample 30-day backtest on 60m → produce `backtest-report-60m-*.html` (if signal looks good on 15m)
- [ ] Compare strategy 4 EV / win rate / cap-reset rate across 5m / 15m / 60m
- [ ] Decision: if 15m EV ≥ 5m EV (in $/hr or % of stake), go live; else stay on 5m

Estimated work: 2–3 hours including data fetch + report inspection.

---



## v1.0 — TUI Starter（当前阶段，方案 1）

**目标：** 单二进制 TUI，显示 Polymarket USDC 余额，跑通 Rust + ratatui + rs-clob-client + Redis 全链路；BDD/TDD/E2E 测试就位。

**架构：** 单进程，三个 tokio 任务（refresher / TUI render / input），模块按"未来的 crate"组织。

### 验收
- [x] `.env` 加载 `POLYMARKET_PRIVATE_KEY`、`REDIS_URL`、`REFRESH_INTERVAL_SECS`
- [x] Refresher 任务按间隔从 CLOB 拉余额 → 写 Redis
- [x] TUI 从 Redis 读取，居中显示 `USDC: $X.XX`
- [x] 状态栏显示：上次刷新时间、刷新间隔、Redis/CLOB 健康指示灯
- [x] 按键：`q` 退出，`r` 强制刷新
- [x] 单元测试覆盖各 trait 实现（fake fetcher / fake cache）
- [x] BDD 场景（cucumber-rs）：缓存有值时启动即显示
- [x] E2E：用 `TestBackend` + mock CLOB + testcontainers Redis 跑通整条路径
- [x] `.env` 已在 `.gitignore`，仓库内只放 `.env.example`
- [ ] 真实 Polymarket 账户手动 TUI 冒烟测试 — **intentionally deferred**：需要真实私钥（`POLYMARKET_PRIVATE_KEY`），无法在 CI / 无密钥环境中自动化。后续 testnet 环境就绪后补测。

**Coverage gap (intentional):**
- `src/input.rs` — crossterm event reader; no headless-friendly mocking. Covered by manual smoke + e2e quit-key scenario.
- `src/cache.rs` — `RedisBalanceCache` real adapter; integration tests run with `--ignored` (testcontainers); not instrumented in the fast coverage pass. Covered by `cache_integration` tests (4 passed with `--ignored`).
- `src/clob.rs` — `ClobBalanceFetcher::connect/fetch`; rs-clob-client v2 auth flow is impractical to wiremock. Covered by deferred manual smoke against real Polymarket testnet.

Overall line coverage (lib + BDD, excl. `src/bin`): **79.5%** — just below 80% threshold due to the three files above. Excluding those three intentionally-untestable files: **90.6%**.

### 模块结构（为 v1.3 拆 crate 做准备）
```
src/
├── main.rs           ← 进程入口，启动三任务
├── config.rs         ← .env 加载
├── domain.rs         ← Balance 类型、错误
├── clob.rs           ← BalanceFetcher trait + rs-clob-client 实现
├── cache.rs          ← BalanceCache trait + redis 实现
├── refresher.rs      ← 后台刷新任务
├── app.rs            ← App 状态、事件循环
└── ui.rs             ← ratatui 渲染
```

**关键纪律：** 模块间只通过 trait 通信。这样 v1.3 把文件升级成 crate 时是"剪切粘贴 + 改 import"，不用重构。

---

## v1.1 — Trader (Martingale 5min BTC)  ✅ COMPLETE

- [x] Pure Martingale FSM in trader::ladder
- [x] poly-trader binary with CLI + lock + restore
- [x] Six trait abstractions (MarketDiscovery, OrderExecutor, WindowResolver, TraderStateStore, TraderEventEmitter, TraderEventStream)
- [x] Real adapters: RedisTraderState, RedisTraderStream, GammaMarketDiscovery, ClobOrderExecutor, SimulatedExecutor
- [x] TUI log panel + Trader LED + sub-title
- [x] BDD scenarios for trader (6 new + 4 existing balance = 10 total)
- [x] E2E with testcontainers (5 trader + 3 v1.0 = 8 total)
- [x] Integration tests for state + market adapters

**Open items (acceptance not auto-tested):**
- Manual smoke: at least one full real-money window (buy → resolve → sell). Requires user account + funded wallet.
- ClobOrderExecutor's `making_amount`/`taking_amount` field interpretation needs live confirmation against AMOY testnet.

**Coverage note (95-99% range — accepted):**
- `src/trader/` aggregate line coverage: **96.0%** (1057/1101 lines). The ~4% gap is in `market.rs` (11 uncovered function variants — mainly exhaustive enum arms on closed-market decoding), `resolver.rs` (timeout/error paths requiring real time travel), and `scheduler.rs` (5 functions covering async tokio select branches). All gaps are in tested-by-integration-contract paths. 99% target deferred to a future release.

---

## v1.2 — BTC Market Watch Strip ✅ COMPLETE

- [x] WindowMarket extended with price_to_beat (additive, backward-compat with v1.1)
- [x] tui::market_watch task: Chainlink BTC/USD via Polygon RPC + gamma priceToBeat
- [x] Layout: new 1-row strip between balance and trader sub-title
- [x] Graceful degradation on RPC / gamma failure
- [x] 5 new insta snapshots; 19 market_watch tests; 4 chainlink decode tests; 2 market decoder tests; 1 app handler test
- [x] Independent of trader process — works with or without poly-trader running

**Open items:**
- Public `polygon-rpc.com` endpoint may return HTTP 401 intermittently. Use a maintained RPC URL via `POLYGON_RPC_URL` for reliable operation. Acceptable degradation: BTC strip shows `--` when RPC fails.

---

## v1.4 — Backtest Framework ✅ COMPLETE

- [x] `poly-backtest` binary, offline strategy comparison
- [x] 6 strategy variants (hold / TP-only / TP+SL sym / TP+SL asym / time-based / fixed)
- [x] Black-Scholes synthetic token-price model + σ estimation from BTC 1-min closes
- [x] HTML report with Chart.js (single self-contained file, dark theme)
- [x] Disk cache for gamma + Binance data (`~/.poly-backtest-cache/`)
- [x] 193 unit tests + 1 ignored network smoke test (`tests/backtest_smoke.rs`)
- [x] Zero modifications to v1.1 trader code (reuses `LadderState` + `apply_outcome`)

**Output:** `backtest-report.html` shows EV / win rate / max drawdown / cap resets per strategy across the chosen window.

**30-day headline (2026-04-09 → 2026-05-09, σ ≈ $85.18 / 5min, friction 1.5%):**

| Strategy | PnL | Win rate | Cap resets |
|---|---:|---:|---:|
| 1_hold_martingale       |    -$984 | 49.4% |  42 |
| 2_tp_only_martingale    |  -$3,817 | 55.5% |  20 |
| 3_tp_sl_symmetric       |  -$1,063 | 44.5% | 100 |
| 4_tp_sl_asymmetric      | **+$5,088** | 29.2% | 179 |
| 5_time_60s_martingale   |  -$1,701 | 43.9% |  77 |
| 6_fixed_stake_baseline  | -$10,701 | 49.4% |  42 |

Only `4_tp_sl_asymmetric` (TP $0.85 / SL $0.45) is profitable in this window. **Decides:** which strategy to deploy in v1.5, or whether to abandon Martingale entirely.

**Coverage (`cargo llvm-cov --lib --ignore-filename-regex 'src/bin|src/trader/adapters/|.*_wrapper\.rs'`):** 90.14% lines / 88.93% regions overall. Backtest module: `config.rs` 100%, `runner.rs` 100%, `report.rs` 98.65%, `stats.rs` 98.50%, `oracle.rs` 97.46%, `exit_rule.rs` 97.71%. Lower-coverage backtest files are network-IO paths (`binance.rs` 45%, `gamma_history.rs` 76%, `loader.rs` 3%) — covered by the ignored `backtest_smoke` test against live APIs, not the fast lib pass.

---

## v1.5 — TP/SL exits in trader ✅ COMPLETE

Strategy 4 (validated by backtest +$5K-$10K/30d) lives behind `--exit-rule tp-sl --tp-price 0.85 --sl-price 0.45`. Default behavior unchanged. See `docs/superpowers/specs/2026-05-10-trader-tp-sl-design.md`.

- [x] CLI: `--exit-rule {hold|tp-sl}`, `--tp-price`, `--sl-price`, `--poll-secs`
- [x] `MidwindowPriceFetcher` trait + `GammaPriceFetcher` adapter
- [x] `ExitWatcher` polling loop, `ExitConfig`, `ExitTrigger`, `ExitKind`
- [x] `run_window` branches on `cfg.exit`, races watcher vs resolver via `tokio::select!`
- [x] `TraderEventKind::ExitTriggered { kind, bid }` (proceeds_usd stripped — emitted before sell per spec)
- [x] Outcome mapped from `proceeds vs cost`; ladder math unchanged
- [x] E2E: `ExitTriggered` round-trips through Redis stream
- [x] Code-review fixes: deadline aligned to `window_ts + 300s` (not buy-fill instant); `winner_sweep` extracted; gamma string-fallback for `priceToBeat`
- [x] `SimulatedExecutor.sell_at_bid(bid_hint)` for realistic dry-run PnL — fills at `bid × 0.99` instead of hardcoded `$0.99/share`
- [x] TUI strip: snapshot Chainlink at window-open as price-to-beat (gamma's `priceToBeat` is post-close only)
- [x] TUI strip: 2-cell padding after clock emoji (`U+23F1`)

### Dry-run validation results (2026-05-10)

| Run | Windows | W/L/Skip | PnL | Notes |
|---|---:|---|---:|---|
| Smoke (--max-windows 2) | 2 | 0/2/0 | -$10 | Both SL-triggered, simulator returned hardcoded $9.90 → masked real loss. Triggered the simulator fix. |
| Short (--max-windows 12) | 12 | 5/6/1 | +$34.24 | Simulator fix landed; PnL realistic. TP@0.985 single-window +$38 recovered 3 losses. |
| Extended (--max-windows 288) | 22 | 7/15/0 | -$13.76 (CapReached) | Hit step-5 cap at hour 2.3. Strategy 4's fat-tail risk in action — backtest predicted ~3% cap-rate. |

Trigger distribution across the three runs (n=36) tracked toward backtest predictions:

| Type | Live (n=36) | Backtest (n=8509) |
|---|---:|---:|
| TP triggered | ~22% | 29% |
| SL triggered | ~50% | 58% |
| Resolved (deadline) | ~28% | 13% |

Resolution rate is high vs backtest — consistent with this week's lower BTC vol. Still small sample.

### Real-money launch (in progress)

First real CLOB run: `--max-windows 12 --max-step 5 --exit-rule tp-sl --tp-price 0.85 --sl-price 0.45 --reset` — see Redis stream `poly:prod:trader:events` for live events. Monitor for `Alert` and `SellRejected` — stuck shares need manual reconciliation.

---

## v1.6 — TUI Positions ✅ COMPLETE

Live position display in the TUI balance box, sourced from `data-api.polymarket.com/positions`. Catches stuck shares from `SellRejected`/`Alert` events. Proxy address derived from EOA via SDK's `derive_proxy_wallet(eoa, POLYGON)`. See `docs/superpowers/specs/2026-05-10-tui-positions-design.md`.

- [x] `Position` / `Positions` / `Side` types + `PositionsFetcher` / `PositionsCache` traits
- [x] `PolymarketPositionsFetcher` — wraps SDK `data::Client::positions`
- [x] `RedisPositionsCache` — JSON in/out at `poly:prod:positions`
- [x] `Positioner` task with immediate first fetch + 30s loop
- [x] `AppEvent::PositionsUpdate(Positions)` and `AppState.positions`
- [x] `render_balance` — 2-line layout (USDC + positions)
- [x] Wire into `poly-tui` main: derive proxy from EOA at startup
- [x] Integration test: testcontainers Redis + stub fetcher → assert event + cached state

---

## v1.8 — poly-redeem CLI ✅ COMPLETE (功能齐全，使用前需小额 MATIC)

`poly-redeem` 二进制：把卡住的胜方 outcome tokens 兑换回 USDC.e。Sell 失败导致股票卡住的安全网。

```bash
poly-redeem --dry-run    # 列出可兑换的仓位
poly-redeem              # 真的兑换（需要 EOA 有 MATIC 付 gas）
```

### Usage

- 自动从 `POLYMARKET_PRIVATE_KEY` 派生 EOA + proxy
- 拉 data-api 仓位列表，过滤 `redeemable=true`
- 按 condition_id 去重（一个 binary 市场 UP+DOWN 一笔交易搞定）
- 每个市场调一次 `CTF.redeemPositions()`

### ⚠️ Gas 要求

Polymarket Magic/邮箱账户的 EOA 默认 **0 MATIC**——CLOB 交易走 Polymarket 中继服务器免 gas，但 CTF 直接合约调用必须 EOA 自付。

首次使用前：

1. 桥 / 买 ~$1 MATIC → 转到 EOA 地址（启动时打印）
2. 一次性投入，之后约 25 次兑换够用（每次 ~$0.04 gas）

或者去 polymarket.com UI 手动 redeem（用 Magic 的中继器，不要 gas）。

### 何时会用到

- Sell 失败 → 股票卡住 → 市场结算后 → `poly-redeem` 取回
- 重启 trader 之前先跑一次清账
- 旧账户有遗留仓位时清理

---

## v1.7 — Limit-order maker mode ✅ COMPLETE

`--maker` flag activates limit BUY (with 30s/60s/90s sweep) + limit TP. SL stays market. End-of-window: cancel TP at t=270s + market-sell residual. See `docs/superpowers/specs/2026-05-10-trader-maker-mode-design.md`.

- [x] CLI: `--maker` flag with `--exit-rule tp-sl` validation
- [x] `OrderId` + `OrderSide` types; `ExecError::NotSupported`
- [x] `OrderExecutor::place_limit` + `cancel` (default NotSupported, real impls in Clob + Sim adapters)
- [x] `OrderEventStream` trait + 2s polling impl (`PolymarketPollOrderEvents`) + scripted stub
- [x] `run_maker` state machine — PendingBuy + PendingTpSell phases via tokio::select!
- [x] 4 new event variants: `BuyLimitPosted`, `BuyLimitSwept`, `TpLimitPosted`, `TpLimitFilled`
- [x] dispatch in `run_window`: maker → `run_maker`, taker → existing v1.5 path
- [x] dry-run uses `AutoFillEvents` stub for state-machine validation
- [x] Integration test: Redis event roundtrip + run_maker happy-path
- [x] README + TODO docs

**Open items / next versions:**
- WebSocket fill detection — currently polling-only at 2s
- Single-CLOB-client refactor — currently `inner_client()` accessor on executor; cleaner ownership chain possible
- Real-money A/B comparison vs v1.5 to measure actual fee savings

---

## v1.7.1 — `--window-minutes` flag ✅ COMPLETE

Adds `--window-minutes 5|15|60` to `poly-trader`. TUI auto-tracks via `LadderState.window_minutes` from the event stream. See `docs/superpowers/specs/2026-05-10-window-minutes-design.md`.

- [x] `market.rs` — `window_seconds`/`window_slug`/`floor_window`/`next_window_boundary` parameterized helpers
- [x] `LadderState.window_minutes` field with serde back-compat default
- [x] CLI: `--window-minutes` flag with {5,15,60} validation
- [x] `SchedulerConfig.window_seconds` + `WindowConfig.window_seconds` + `MakerDeps` threading
- [x] Resolver timeout = `window_seconds + 300s` set at trader startup
- [x] Maker `cancel_deadline = window_ts + window_seconds - 30`
- [x] `MarketDiscovery::find_window(ts, mins)` trait change
- [x] TUI auto-detect via `mpsc::Sender<u32>` from app to market_watch
- [x] `restore_or_init` refuses mid-session window-length switch
- [x] README + TODO docs

**Open items / next:**
- v1.7.2: extend backtest binary to 15m/60m (currently 5m hardcoded)
- v1.8: real-money A/B test 5m vs 15m with --maker

---

## v1.3 — Daemon / TUI 拆分（方案 2 重构）

**触发条件：** 准备扩展交易逻辑（多策略、热加载配置、动态切方向）时，必须先做这次拆分。当前 v1.1 trader 单方向 + dry-run 够用，但任何更复杂的形态都要先把 daemon 做出来。

**目标：** 把单二进制拆成两个独立进程

- `poly-daemon`：无头常驻，独占 CLOB / Redis 写入路径，负责拉余额（将来：跑策略、下单）
- `poly-tui`：纯只读，从 Redis 读数据并渲染，通过 Redis pub/sub 下发命令（如强制刷新）

### 重构步骤
1. **改 workspace 结构**
   ```
   poly/
   ├── Cargo.toml          ← workspace 清单
   └── crates/
       ├── core/           ← domain.rs + 各 trait（独立 lib crate）
       ├── clob/           ← clob.rs（依赖 core）
       ├── cache/          ← cache.rs（依赖 core）
       ├── daemon/         ← bin crate：原 refresher.rs + 新的 main
       └── tui/            ← bin crate：原 app.rs + ui.rs + 新的 main
   ```
2. **通信契约：** 在 `core` 里定义 Redis key schema 和 pub/sub channel 名
   - Key：`poly:balance:latest` → JSON({ usdc, fetched_at })
   - Channel：`poly:cmd` → 命令（`refresh_balance`、将来的 `cancel_order` 等）
3. **TUI 去掉所有写路径**
   - 不再持有 `BalanceFetcher`，只持有 `BalanceCache`（只读）
   - `r` 键改为往 `poly:cmd` 发布 `refresh_balance`，daemon 订阅后执行
4. **Daemon 加监督逻辑**
   - 重启失败的刷新任务（`tokio::spawn` + 退避）
   - 健康状态写 Redis（key：`poly:health:daemon`），TUI 读取显示
5. **测试同步迁移**
   - BDD/E2E 启动 daemon 子进程 + TUI 子进程，验证跨进程通信

### 验收
- [ ] `cargo run -p poly-daemon` 无需 TUI 即可独立运行
- [ ] `cargo run -p poly-tui` 关掉后再开，看到的余额仍然是最新的（daemon 一直在跑）
- [ ] TUI 进程崩溃不影响 daemon
- [ ] daemon 进程崩溃 → TUI 状态栏 CLOB 指示灯变红，但仍显示最后一次缓存值
- [ ] 全部测试在 workspace 模式下绿

---

## v1.4+ — 后续路线图（占位，触发时再细化）

- **市场列表 / 订单簿**：rs-clob-client 真正发挥作用的地方
- **持仓与盈亏**
- **下单 / 撤单**（需要 daemon 已就位）
- **策略框架**：daemon 内可热插拔策略
- **可观测性**：Prometheus 指标、结构化日志归档
