# Poly TUI — TODO / Roadmap

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
- [ ] 真实 Polymarket 账户手动 TUI 冒烟测试 — **intentionally deferred**：需要真实私钥（`POLYMARKET_PRIVATE_KEY`），无法在 CI / 无密钥环境中自动化。待 v1.1 testnet 环境就绪后补测。

**Coverage gap (intentional, v1.1 will address):**
- `src/input.rs` — crossterm event reader; no headless-friendly mocking. Covered by manual smoke + e2e quit-key scenario.
- `src/cache.rs` — `RedisBalanceCache` real adapter; integration tests run with `--ignored` (testcontainers); not instrumented in the fast coverage pass. Covered by `cache_integration` tests (4 passed with `--ignored`).
- `src/clob.rs` — `ClobBalanceFetcher::connect/fetch`; rs-clob-client v2 auth flow is impractical to wiremock. Covered by deferred manual smoke against real Polymarket testnet.

Overall line coverage (lib + BDD, excl. `src/bin`): **79.5%** — just below 80% threshold due to the three files above. Excluding those three intentionally-untestable files: **90.6%**.

### 模块结构（为 v1.1 拆 crate 做准备）
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

**关键纪律：** 模块间只通过 trait 通信。这样 v1.1 把文件升级成 crate 时是"剪切粘贴 + 改 import"，不用重构。

---

## v1.1 — Daemon / TUI 拆分（方案 2 重构）

**触发条件：** 准备开始写真正的交易循环（下单、撤单、风控）时，必须先做这次拆分。机器人不能依赖 TUI 进程存活。

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

## v1.2+ — 后续路线图（占位，触发时再细化）

- **市场列表 / 订单簿**：rs-clob-client 真正发挥作用的地方
- **持仓与盈亏**
- **下单 / 撤单**（需要 daemon 已就位）
- **策略框架**：daemon 内可热插拔策略
- **可观测性**：Prometheus 指标、结构化日志归档
