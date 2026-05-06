# Poly TUI 余额起步版 — v1.0 设计文档

- **日期：** 2026-05-06
- **范围：** v1.0 起步版本
- **目标读者：** 项目作者 / 未来的自己 / 协作者
- **状态：** 待实现

---

## 1. 目标与范围

### 做什么
单二进制 Rust TUI，启动后展示 Polymarket 账户的 USDC 余额；后台周期性从 CLOB 拉取，缓存到 Redis，TUI 从 Redis 读取并渲染。带 BDD / TDD / E2E 三层测试。

### 不做什么（v1.0 显式排除）
- 持仓、订单簿、未实现盈亏
- 下单 / 撤单 / 任何交易动作
- 守护进程拆分（推迟到 v1.1）
- Web / 移动 UI
- 多账户

### 成功判定
见 §13 接受标准。

---

## 2. 关键决策摘要

| 决策点 | 选择 | 理由 |
|---|---|---|
| 显示内容 | 仅 USDC 余额 | 起步最小路径 |
| 认证方式 | `POLYMARKET_PRIVATE_KEY` in `.env`，专用钱包 | rs-clob-client 的 `balance_allowance()` 必须 L2 鉴权，read-only by address 不可行 |
| 数据源 | rs-clob-client v2 (`/polymarket/rs-clob-client-v2`) | 用户选定；后续接入订单簿/下单天然延续 |
| Redis 用途 | 缓存余额 + 后台刷新 + TUI 读缓存 | 解耦 fetch 与 render，与未来订单簿数据流一致 |
| UI 复杂度 | 居中余额 + 状态栏 | 让缓存/刷新架构在屏幕上可观察 |
| 进程模型 | 单二进制 + 三个 tokio 任务 | 起步够用；模块按未来 crate 边界组织，方便 v1.1 拆 daemon |
| 本地 Redis | docker-compose（redis:7-alpine） | 与 E2E 用的 testcontainers 镜像一致，环境零差异 |

---

## 3. 架构总览

### 进程模型
单二进制 `poly-tui`，一个 tokio 多线程 runtime。

### 三个长生命周期任务

```
                 ┌─────────────────────────────────────────┐
                 │          poly-tui 进程                   │
                 │                                          │
   crossterm     │   ┌──────────┐    KeyEvent              │
   键盘事件 ───────▶│  Input    │────────┐                  │
                 │   │  task    │        ▼                  │
                 │   └──────────┘   ┌─────────┐  Refresh    │
                 │                  │  App    │   命令       │
                 │   ┌──────────┐   │ (TUI    │──────┐      │
                 │   │ Render   │◀──│  task)  │      │      │
                 │   │ (ratatui)│   └─────────┘      ▼      │
                 │   └──────────┘        ▲      ┌──────────┐│
                 │                       │读     │Refresher ││
                 │                       │      │  task    ││
                 │                       │      └────┬─────┘│
                 │                       │           │写    │
                 │                       │           ▼      │
                 │                  ┌────┴───────────────┐  │
                 │                  │       Redis        │  │
                 │                  │  poly:prod:bal:*   │  │
                 │                  └────────────────────┘  │
                 │                              ▲           │
                 └──────────────────────────────┼───────────┘
                                                │
                                       ┌────────┴─────────┐
                                       │  Polymarket CLOB │
                                       │ rs-clob-client-v2│
                                       └──────────────────┘
```

### 任务职责

- **Refresher task**：周期触发（默认 30s）+ 强制刷新命令；调 CLOB → 写 Redis；状态（成功/失败/延迟）通过广播 channel 上报
- **App / Render task**：每 tick（默认 250ms）从 Redis 读最新缓存；接收 Refresher 状态事件；按 ratatui 帧率重绘
- **Input task**：阻塞读 crossterm 事件 → 转成 `AppEvent` 投到 channel；`q`/`Esc`/`Ctrl+C` 触发关闭，`r` 触发强制刷新

### 关闭语义
任一触发点 → `CancellationToken::cancel()` 广播 → 各任务 `select!` 命中 `cancelled()` 分支 → drain → 主线程 `tokio::join!` 收尾 → 退出 raw mode。

---

## 4. 模块结构与 trait 边界

### 目录

```
src/
├── main.rs          ← 进程入口；加载配置、连 Redis、构建 client、启动三任务
├── config.rs        ← .env 加载（dotenvy + envy）
├── domain.rs        ← Balance、AppEvent、RefreshStatus、HealthLed、Error
├── clob.rs          ← BalanceFetcher trait + ClobBalanceFetcher 实现
├── cache.rs         ← BalanceCache trait + RedisBalanceCache 实现
├── refresher.rs     ← run_refresher() + do_fetch() 后台任务
├── app.rs           ← App 状态机 + tick_once()（暴露给测试单步驱动）
├── ui.rs            ← ratatui 渲染（纯函数，零 I/O）
└── input.rs         ← crossterm 事件循环

tests/
├── features/
│   └── balance.feature
├── bdd.rs           ← cucumber-rs 入口
├── e2e_tui.rs       ← E2E（#[ignore]）
└── support/
    ├── mod.rs
    ├── fake_fetcher.rs
    └── memory_cache.rs
```

### 核心类型与 trait

```rust
// domain.rs
pub struct Balance {
    pub usdc: rust_decimal::Decimal,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

pub enum RefreshStatus {
    Ok { at: DateTime<Utc> },
    Failed { at: DateTime<Utc>, error: String },
}

pub enum AppEvent {
    Tick,
    Key(crossterm::event::KeyEvent),
    Refresh(RefreshStatus),
    Shutdown,
}

pub enum HealthLed {
    Green,   // 最近一次成功 < 1.5 × interval
    Yellow,  // 1.5–3 × interval 之间
    Red,     // > 3 × interval 或显式失败
}

// clob.rs
#[async_trait::async_trait]
pub trait BalanceFetcher: Send + Sync {
    async fn fetch(&self) -> Result<Balance, FetchError>;
}

// cache.rs
#[async_trait::async_trait]
pub trait BalanceCache: Send + Sync {
    async fn get(&self) -> Result<Option<Balance>, CacheError>;
    async fn set(&self, balance: &Balance) -> Result<(), CacheError>;
    async fn ping(&self) -> Result<(), CacheError>;
}
```

### 模块依赖方向（禁止反向）

```
main ──▶ config, clob, cache, refresher, app, ui, input, domain
refresher ──▶ domain, clob (trait), cache (trait)
app       ──▶ domain, cache (trait)
ui        ──▶ domain                  (纯渲染，零 I/O)
input     ──▶ domain
clob, cache ──▶ domain
domain    ──▶ (无依赖)
```

### 关键纪律
1. `refresher` 和 `app` 只依赖 trait，不知道具体实现
2. `ui.rs` 是纯函数 `render(frame: &mut Frame, state: &AppState)`
3. 这些约束就是 v1.1 拆 crate 的依据：每个文件未来都能独立成 lib，零代码改动

---

## 5. 数据流与状态机

### App 状态

```rust
pub struct AppState {
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub clob_health: HealthLed,
    pub redis_health: HealthLed,
    pub refresh_interval: Duration,
    pub should_quit: bool,
}
```

### Channel 拓扑

```
input_task   ──tx_input──▶   ┐
refresher    ──tx_status─▶   ├─▶ App.recv()  (mpsc, buffer=64)
ticker (内置) ─tx_tick───▶   ┘

App ──tx_cmd──▶ refresher    (mpsc, buffer=8)
   命令: ForceRefresh

shutdown_token: tokio_util::sync::CancellationToken (广播)
```

### Refresher 主循环（伪码）

```rust
loop {
  tokio::select! {
    _ = shutdown.cancelled() => break,
    Some(Cmd::ForceRefresh) = cmd_rx.recv() => do_fetch().await,
    _ = tokio::time::sleep(interval) => do_fetch().await,
  }
}

async fn do_fetch() {
  match fetcher.fetch().await {
    Ok(b)  => { let _ = cache.set(&b).await; status_tx.send(Ok{at: now}).await.ok(); }
    Err(e) => { status_tx.send(Failed{at: now, error: e.to_string()}).await.ok(); }
  }
}
```

- 失败不重试（下个 tick 自然重试）；不 panic，不退出
- `cache.set` 失败也只走 Failed 上报，绝不让 refresher 死掉

### App 主循环（伪码）

```rust
loop {
  if state.should_quit { break; }

  tokio::select! {
    _ = shutdown.cancelled() => break,
    Some(ev) = events.recv() => handle(&mut state, ev, &cmd_tx).await,
    _ = render_ticker.tick() => {
        state.balance = cache.get().await.unwrap_or(state.balance);
        state.redis_health = derive_redis_health(...);
        state.clob_health  = derive_clob_health(&state.last_refresh, interval);
        terminal.draw(|f| ui::render(f, &state))?;
    }
  }
}

fn handle(state, ev, cmd_tx) {
  match ev {
    AppEvent::Key(k) if k.code == 'q' => state.should_quit = true,
    AppEvent::Key(k) if k.code == 'r' => { let _ = cmd_tx.try_send(Cmd::ForceRefresh); }
    AppEvent::Refresh(s)              => state.last_refresh = Some(s),
    _ => {}
  }
}
```

### 首次启动顺序（避免空白屏）

1. main 加载配置、连 Redis、连 CLOB
2. main 同步先调一次 `refresher::do_fetch()`（5 秒超时，失败也继续）
3. `tokio::spawn` 三个 task
4. App 进入主循环

### Redis schema（v1.1 重构合约）

| Key | 用途 | Value | TTL |
|---|---|---|---|
| `poly:prod:balance:latest` | 最新余额 | `{"usdc": "123.45", "fetched_at": "2026-05-06T..."}` | 无（覆盖式更新） |

> **过期判定看 `fetched_at`，不依赖 Redis TTL**——TTL 会让缓存"消失"，UI 就会变空白；显式时间戳才能驱动健康灯黄/红。

---

## 6. 错误处理

### 分层

```rust
// 适配层：thiserror（精确错误码）
#[derive(thiserror::Error, Debug)]
pub enum FetchError {
  #[error("CLOB request failed: {0}")] Network(String),
  #[error("CLOB returned invalid data: {0}")] Decode(String),
  #[error("authentication failed")] Auth,
}

#[derive(thiserror::Error, Debug)]
pub enum CacheError {
  #[error("redis connection lost")] Disconnected,
  #[error("redis op failed: {0}")] Op(String),
  #[error("cache value malformed: {0}")] Decode(String),
}

// main / 启动路径：anyhow
fn main() -> anyhow::Result<()> { ... }
```

### 致命 vs 非致命

| 场景 | 处理 |
|---|---|
| 配置缺失 / 私钥无效 | 启动 panic（可读错误） |
| 启动时连不上 Redis | 退出（无缓存等于没架构） |
| 启动时连不上 CLOB | 不退出，TUI 起，CLOB 灯红 |
| 运行时 fetch 失败 | 上报 `RefreshStatus::Failed`，灯黄/红，继续跑 |
| 运行时 Redis 抖动 | App 拿到上次 state 渲染（保留旧 Balance），灯黄 |
| ratatui draw 失败 | 记录 + 继续 |

### 日志

`tracing` + `tracing-subscriber` + `tracing-appender`，按日轮转写到 `logs/poly.log`。**TUI 模式下不写 stdout**（会破坏画面）。错误同时塞进 App 的 ring buffer，下版本可在 TUI 弹日志窗。

---

## 7. 配置

```rust
#[derive(serde::Deserialize)]
pub struct Config {
  pub polymarket_private_key: String,           // 必填
  pub redis_url: String,                        // 默认 redis://127.0.0.1:6379
  pub refresh_interval_secs: u64,               // 默认 30
  pub clob_host: String,                        // 默认 https://clob-v2.polymarket.com
  pub log_level: String,                        // 默认 info
}
```

### `.env.example`（提交到仓库）

```
POLYMARKET_PRIVATE_KEY=0x...           # 专用钱包，切勿用主钱包
REDIS_URL=redis://127.0.0.1:6379
REFRESH_INTERVAL_SECS=30
CLOB_HOST=https://clob-v2.polymarket.com
LOG_LEVEL=info
```

### `.gitignore`

第一行 `.env`；同时排除 `logs/`、`target/`。

---

## 8. UI 布局

```
┌─ poly-tui ─────────────────────────────────────────────┐
│                                                         │
│                                                         │
│                    USDC: $1,234.56                      │  ← 居中粗体
│                                                         │
│                                                         │
├─────────────────────────────────────────────────────────┤
│ ● CLOB  ● Redis   refresh: 30s   last: 12s ago    q r  │  ← 状态栏
└─────────────────────────────────────────────────────────┘
```

### 布局拆分

- 主区：`Constraint::Min(0)` + 居中 `Paragraph`，金额 `Modifier::BOLD`
- 状态栏：`Constraint::Length(1)`，`Span` 数组拼接，灯色按 `HealthLed`
- 余额未拿到时显示 `USDC: --`（不是 0，避免误导）

### 键位

| 键 | 动作 |
|---|---|
| `q` / `Esc` / `Ctrl+C` | 退出 |
| `r` | 强制刷新 |

---

## 9. 测试策略

### TDD — 单元测试

每模块就近 `#[cfg(test)] mod tests`。

| 模块 | 测什么 | 怎么测 |
|---|---|---|
| `domain` | `Balance` 序列化往返、`HealthLed` 阈值 | 纯函数 |
| `clob` | rs-clob-client 响应 → `Balance` 转换；金额单位（µUSDC ↔ Decimal） | 喂固定 JSON 给转换函数 |
| `cache` | Redis JSON 编码/解码；错误映射 | `mockall` mock 连接 |
| `refresher` | 成功 → 写缓存 + 上报 Ok；失败 → 不写缓存 + 上报 Failed；`ForceRefresh` 命令打断 sleep | `FakeFetcher` + `InMemoryCache`，`tokio::time::pause()` 加速时间 |
| `app` | `Refresh(Failed)` → CLOB 灯转 Red；`Key('q')` → `should_quit = true`；`Tick` → 从 cache 读到新值 | 调 `tick_once()`；`InMemoryCache` |
| `ui` | 渲染快照 | `TestBackend` + `insta::assert_snapshot!` |

**覆盖率：** `cargo llvm-cov` ≥ 80%。

### BDD — Cucumber

`tests/features/balance.feature`：

```gherkin
Feature: 余额展示
  作为机器人主人，我希望 TUI 启动后能立刻看到当前 USDC 余额

  Scenario: 缓存里已有余额，启动即显示
    Given Redis 缓存里有余额 "100.00" USDC
    When  我启动 TUI 主循环
    And   驱动 1 个 tick
    Then  屏幕上能看到 "USDC: $100.00"
    And   Redis 状态灯是绿色

  Scenario: 缓存为空，CLOB 返回 50.00
    Given Redis 缓存为空
    And   CLOB 返回余额 "50.00" USDC
    When  我启动 TUI 主循环
    And   触发一次强制刷新
    And   驱动 1 个 tick
    Then  屏幕上能看到 "USDC: $50.00"
    And   CLOB 状态灯是绿色

  Scenario: CLOB 失败，灯变红，但仍显示旧缓存
    Given Redis 缓存里有余额 "200.00" USDC
    And   CLOB 调用会失败
    When  我启动 TUI 主循环
    And   触发一次强制刷新
    And   驱动 1 个 tick
    Then  屏幕上仍显示 "USDC: $200.00"
    And   CLOB 状态灯是红色
```

**Step world：** 持有 `FakeFetcher`、`InMemoryCache`、`TestBackend` 的 ratatui Terminal、App 句柄。Step "驱动 N 个 tick" 调 `app::tick_once().await` N 次。

### E2E — 跨边界冒烟（`tests/e2e_tui.rs`）

把"真组件"接尽量多：

- **Redis**：`testcontainers` 拉一个**全新** redis:7 容器（与开发用的 `poly-redis` 完全隔离）
- **CLOB**：`wiremock` 起本地 HTTP server，喂 `balance_allowance` JSON
- **TUI**：ratatui `TestBackend`（真渲染管线，不开终端）
- **驱动**：`tokio::spawn` Refresher，发命令 + 推 tick，断言渲染缓冲

**核心 E2E 用例：**

1. `e2e_full_path_renders_balance` — wiremock 返 100 USDC → 启动全栈 → 等一次刷新 → 断言 buffer 含 `"USDC: $100.00"`
2. `e2e_clob_down_keeps_cached_value` — 先正常刷一次（100），切 wiremock 500 → 触发刷新 → 断言仍显示 100，CLOB 灯红
3. `e2e_quit_key_terminates_cleanly` — 推 `q` → 等 join → 断言 1 秒内全任务退出且无 panic

**E2E 跑法：** `cargo test --test e2e_tui -- --ignored`（带 `#[ignore]` 标，避免开发循环里启动 Docker）。

---

## 10. 数据隔离与安全（你专属）

> **背景：** 你的开发环境就是真实交易环境，开发 Redis 里跑的是真钱状态。所以测试与生产数据**必须**强隔离。

### 三层防御

#### 防御 1：E2E 必须用 testcontainers，硬断言不连开发 Redis

`tests/e2e_tui.rs` 入口：

```rust
fn connection_url(node: &ContainerAsync<Redis>) -> String {
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let url = format!("redis://127.0.0.1:{port}");
    assert_ne!(port, 6379, "E2E 试图连开发 Redis！abort.");
    url
}
```

任何 E2E 测试必须从此函数取 URL。

#### 防御 2：单元测试不准引 redis 客户端

- `tests/` 目录下，**只有 `e2e_tui.rs` 允许引入 `redis` / `fred` crate**
- 单元/BDD 一律用 `tests/support/memory_cache.rs` 中的 `InMemoryCache`
- CI 上跑一条 `grep` 检查（或 `cargo deny`）兜底

#### 防御 3：Key namespace

| 场景 | 前缀 |
|---|---|
| 真实交易（开发 + 生产） | `poly:prod:*` |
| E2E（即使容器隔离） | `poly-test:*` |
| 单元 / BDD | 不进 Redis |

即便哪天 `redis-cli` 连错，`KEYS poly:prod:*` 一眼能区分真实数据。

### 操作纪律（写进 README）

- `docker compose down -v` 会**删除你的真实交易缓存**——只在确认要清空时执行
- 调试时 `docker exec -it poly-redis redis-cli` 谨慎，避免 `FLUSHDB`/`FLUSHALL`
- 不要在 E2E 测试里 `dotenvy::dotenv()`——只允许从 testcontainers 取连接串

---

## 11. 本地开发先决条件

### 工具

- Rust stable（建议 1.78+）
- Docker Desktop（开发 Redis + E2E 测试）
- Polygon 链上专用钱包（**非主钱包**），里面有少量 USDC.e 用作测试

### 启动

```bash
# 起开发 Redis
docker compose up -d
docker compose ps           # 应显示 (healthy)

# 配置
cp .env.example .env
# 编辑 .env，填入专用钱包私钥

# 跑
cargo run

# 测试
cargo test                              # 单元 + BDD
cargo test --test e2e_tui -- --ignored  # E2E（需要 Docker）
cargo llvm-cov                          # 覆盖率
```

---

## 12. 依赖

```toml
[package]
name = "poly-tui"
version = "0.1.0"
edition = "2021"

[dependencies]
# Async
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"
async-trait = "0.1"
futures = "0.3"

# TUI
ratatui = "0.29"
crossterm = "0.28"

# Polymarket
polymarket-client-sdk-v2 = "*"     # cargo add 时锁实测版本
alloy = { version = "0.8", features = ["signer-local"] }

# Redis
fred = { version = "9", features = ["enable-rustls"] }

# 数据 / 时间
rust_decimal = { version = "1", features = ["serde"] }
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# 配置
dotenvy = "0.15"
envy = "0.4"

# 错误 & 日志
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-appender = "0.2"

[dev-dependencies]
cucumber = { version = "0.21", features = ["macros"] }
mockall = "0.13"
insta = "1"
wiremock = "0.6"
testcontainers = "0.23"
testcontainers-modules = { version = "0.11", features = ["redis"] }
tokio-test = "0.4"
```

> **Redis 客户端选 `fred`**：原生 async + 自带重连退避 + 内置健康检测，契合健康灯逻辑；v1.1 拆 daemon 时也省事。

---

## 13. 接受标准

- [ ] `cargo run` 启动后 1 秒内出现 TUI，状态灯就位
- [ ] 用真钱包 + 真 Redis 跑，看到自己的 USDC 余额
- [ ] 拔网线 → CLOB 灯变红、显示值不归零
- [ ] 关 Redis → Redis 灯变红、值停留在最后一次
- [ ] `q` 1 秒内干净退出，终端正常
- [ ] `cargo test`（含 BDD）全绿
- [ ] `cargo test -- --ignored`（E2E）全绿
- [ ] `cargo llvm-cov` ≥ 80%
- [ ] `.env` 在 `.gitignore`，仓库内只有 `.env.example`
- [ ] 单元测试不引入 redis 客户端 crate（grep 验证）
- [ ] E2E 测试硬断言 `port != 6379`

---

## 14. 后续路线（指针）

详见仓库根目录 `TODO.md`。v1.1 是 daemon / TUI 拆分（方案 2），触发条件为开始写真正的交易循环。
