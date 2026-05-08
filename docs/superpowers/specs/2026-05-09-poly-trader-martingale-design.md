# Poly Trader — Martingale 5min BTC v1.x 设计文档

- **日期：** 2026-05-09
- **范围：** v1.x（新增独立 `poly-trader` 二进制 + TUI log panel）
- **目标读者：** 项目作者 / 协作者 / 未来的自己
- **状态：** 待实现
- **前置：** v1.0 已交付（`poly-tui` 余额显示），current commit `9929c1a`

---

## 1. 目标与范围

### 做什么
新增独立 `poly-trader` 二进制：在 Polymarket 的 BTC 5-minute up/down 二元市场上，按预设方向（UP 或 DOWN）执行 Martingale 策略——每个 5 分钟窗口下注；输了下一注翻倍，赢了重置回起注。N=5 连输触顶后会话结束。状态持久化到 Redis；TUI 进程同步显示 trader 事件流。

### 不做什么（v1.x 显式排除）
- 多策略 / 多方向 / 多市场（只锁一个方向 + 只 BTC 5min）
- 启动后切换方向（要改只能停掉重启）
- TUI 触发 trader 启停（TUI 永远只读）
- in-flight 仓位的自动恢复（崩溃中段在 buy/sell 之间）
- CTF `redeemPositions` 链上赎回（用 CLOB 卖回，吃 ~1% 滑点）
- Polygon RPC / Chainlink 直连（结算信号全走 Polymarket）
- 自动化 testnet 集成（Polymarket 没官方 testnet）
- 多用户 / 多账户 / 自动充值

### 成功判定
见 §13 接受标准。

---

## 2. 关键决策摘要

| 决策点 | 选择 | 理由 |
|---|---|---|
| 市场产品 | Polymarket "Bitcoin Up or Down (5m)" | Chainlink BTC/USD 喂价；slug `btc-updown-5m-{ts}` 可推算 |
| 策略 | 经典 Martingale，doubling | 用户选择 A |
| 起注 | $5 | Polymarket 最小订单 5 股（最低 $2.50–$4.75 实际值），$5 是合规起点 |
| 连输上限 | N=5，触顶整局停 | 最大单局损失 $155，账户余额 ~$173 兜得住 |
| 方向 | 启动 CLI 锁定，会话期间不可改 | 简化；改向的成本就是停掉重启 5 秒 |
| 进程模型 | 独立 `poly-trader` 二进制 + Redis 总线 | 真钱不能依赖"终端不关"；同时是 v1.1 daemon 拆分的最小子集 |
| 入场时机 | T+0 立即，但 ask 必须落在 [0.45, 0.55] | 偏离 50/50 跳过该窗口（ladder 不动）|
| 下单类型 | FoK 市价 buy，失败跳过 | 状态干净不留半仓；流动性问题直接放弃这一局 |
| 现金化 | 赢局 CLOB market sell | 复用 SDK，避开 CTF 链上 tx 的代码路径与攻击面 |
| 结算检测 | Polymarket gamma-api 轮询，60s 超时 | 与市场结算最终权威一致 |
| 测试覆盖 | `src/trader/` ≥ 99%（adapter wrapper 排除） | 用户硬要求 |

---

## 3. 架构总览

### 进程模型

```
                                                  Polymarket CLOB / Gamma
                                                              ▲
                                                              │
   ┌─────────────────────┐          ┌────────────────────────┴─────────────┐
   │   poly-tui (现有)    │          │    poly-trader (新)                  │
   │                      │          │                                       │
   │  ┌──────────────┐    │          │  ┌──────────┐   ┌────────────────┐  │
   │  │ Refresher    │    │          │  │Scheduler │──▶│ Window FSM     │  │
   │  │ (USDC bal)   │    │          │  │(5min     │   │  per window    │  │
   │  └──────────────┘    │          │  │ tick)    │   │ ─ discover     │  │
   │  ┌──────────────┐    │          │  └──────────┘   │ ─ check price  │  │
   │  │ App / UI     │◀───┼──read────┼──────────────── │ ─ buy FoK      │  │
   │  │ + log panel  │    │          │                 │ ─ poll resolve │  │
   │  └──────────────┘    │          │                 │ ─ sell winners │  │
   │  ┌──────────────┐    │          │                 │ ─ update FSM   │  │
   │  │ Input        │    │          │                 └───────┬────────┘  │
   │  └──────────────┘    │          │                         │           │
   │  ┌──────────────┐    │          │  ┌──────────────────────▼────────┐  │
   │  │ Trader event │◀───┼──XREAD───┤  │ Martingale ladder state        │  │
   │  │ subscriber   │    │          │  │ (in-memory + Redis-persisted)  │  │
   │  └──────────────┘    │          │  └──────────────┬─────────────────┘  │
   └──────────┬───────────┘          └─────────────────┼───────────────────┘
              │read                                    │write
              ▼                                        ▼
        ┌─────────────────────────────────────────────────┐
        │                   Redis                          │
        │  poly:prod:balance:latest  (TUI ↔ refresher)     │
        │  poly:prod:trader:ladder   (trader self)         │
        │  poly:prod:trader:events   (stream, TUI ← trader)│
        │  poly:prod:trader:lock     (mutex)               │
        └─────────────────────────────────────────────────┘
```

### 关键不变量

1. **trader 是常驻无头进程**——TUI / 终端关闭与否无关
2. **TUI 永远只读**——绝不通过 TUI 触发交易动作
3. **Redis 是唯一跨进程契约**——只有 4 个 `poly:prod:*` key
4. **崩溃恢复语义**：trader 启动时从 `poly:prod:trader:ladder` 读取，无则用 CLI 参数新建
5. **多实例互斥**：通过 Redis 分布式锁防止两个 trader 同时下单
6. **数据隔离**：测试沿用 v1.0 的 `assert_ne!(port, 6379)` + key namespace 三层防御

---

## 4. 模块结构

### 目录布局

```
src/
├── lib.rs                        ← pub mod 新增 trader 子树 + tui::events
├── bin/
│   ├── poly-tui.rs               ← 已有，新增 log panel 接线（小改）
│   └── poly-trader.rs            ← 新进程入口
│
├── (现有模块不变)
│   ├── config.rs                 ← 通用配置（私钥、redis、log）+ 新增 GAMMA_HOST
│   ├── domain.rs                 ← Balance, RefreshStatus, ...
│   ├── clob.rs                   ← BalanceFetcher trait + ClobBalanceFetcher
│   ├── cache.rs                  ← BalanceCache trait + RedisBalanceCache
│   ├── refresher.rs              ← 余额刷新（TUI 用）
│   ├── app.rs / ui.rs / input.rs ← TUI（小改：加 log panel + trader_health LED）
│
├── tui/
│   └── events.rs                 ← TraderEventStream trait + Redis XREAD 订阅
│
└── trader/                       ← 新模块树
    ├── mod.rs
    ├── config.rs                 ← TraderArgs (clap) + TraderConfig
    ├── scheduler.rs              ← 5min tick + drift + shutdown 协调
    ├── market.rs                 ← MarketDiscovery trait + GammaMarketDiscovery
    ├── executor.rs               ← OrderExecutor trait + ClobOrderExecutor / SimulatedExecutor
    ├── resolver.rs               ← WindowResolver trait + PolymarketResolver
    ├── ladder.rs                 ← Martingale FSM (pure)
    ├── state.rs                  ← TraderStateStore trait + RedisTraderState + TraderLock
    ├── event.rs                  ← TraderEvent + TraderEventEmitter trait + RedisTraderStream
    └── window.rs                 ← run_window 单局执行（编排上述六个 trait）
```

### 核心 trait

```rust
// trader/market.rs
#[async_trait]
pub trait MarketDiscovery: Send + Sync {
    async fn find_window(&self, window_ts: i64) -> Result<WindowMarket, MarketError>;
}

// trader/executor.rs
#[async_trait]
pub trait OrderExecutor: Send + Sync {
    async fn buy_fok(&self, token_id: &TokenId, dollars: Decimal) -> Result<FillResult, ExecError>;
    async fn sell_market(&self, token_id: &TokenId, shares: Decimal) -> Result<FillResult, ExecError>;
}

// trader/resolver.rs
#[async_trait]
pub trait WindowResolver: Send + Sync {
    async fn await_resolution(&self, market: &WindowMarket) -> Result<Resolution, ResolveError>;
}

// trader/state.rs
#[async_trait]
pub trait TraderStateStore: Send + Sync {
    async fn load(&self) -> Result<Option<LadderState>, StateError>;
    async fn save(&self, state: &LadderState) -> Result<(), StateError>;
    async fn clear(&self) -> Result<(), StateError>;

    async fn try_lock(&self, owner: &str, ttl: Duration) -> Result<bool, StateError>;
    async fn refresh_lock(&self, owner: &str, ttl: Duration) -> Result<(), StateError>;
    async fn release_lock(&self, owner: &str) -> Result<(), StateError>;
}

// trader/event.rs
#[async_trait]
pub trait TraderEventEmitter: Send + Sync {
    async fn emit(&self, event: &TraderEvent) -> Result<(), EmitError>;
}

// tui/events.rs
#[async_trait]
pub trait TraderEventStream: Send + Sync {
    async fn tail(&self, n: usize) -> Result<TraderEventTail, StreamError>;
}
pub struct TraderEventTail {
    pub history: Vec<TraderEvent>,
    pub live: Pin<Box<dyn Stream<Item = TraderEvent> + Send>>,
}
```

### 模块依赖方向（强制单向）

```
trader::ladder    ──▶ domain                    (纯函数，零 I/O)
trader::config    ──▶ domain
trader::market    ──▶ domain                    (依赖 reqwest)
trader::executor  ──▶ domain, clob (复用 SDK)
trader::resolver  ──▶ domain, market
trader::state     ──▶ domain, cache
trader::event     ──▶ domain
trader::window    ──▶ domain + 上述 trait
trader::scheduler ──▶ domain + window
bin/poly-trader   ──▶ trader::*, config, cache, domain, clob

tui::events       ──▶ cache (XREAD)
app.rs            ──▶ + tui::events
```

### 纪律

1. `trader::ladder` 是**纯函数**——所有 Martingale 正确性靠它的单元测试覆盖
2. 每个 adapter 严格分两层：薄壳 `pub async fn do_thing()` 调网络/SDK，再调 `decode_thing(raw)` 纯函数
3. `decode_*` / `encode_*` / `compute_*` 函数 100% 可单测
4. v1.1 拆 daemon 时，把 `trader/` 整个挪到独立 daemon crate 是剪切粘贴

---

## 5. Martingale FSM (`trader/ladder.rs`)

### 状态定义

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction { Up, Down }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LadderState {
    pub session_id: Uuid,
    pub direction: Direction,
    pub base_usd: Decimal,                    // $5
    pub max_step: u8,                         // 5
    pub current_step: u8,                     // 1..=5
    pub session_started_at: DateTime<Utc>,
    pub realized_pnl_usd: Decimal,
    pub windows_won: u32,
    pub windows_lost: u32,
    pub windows_skipped: u32,
    pub stopped: Option<StopReason>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    CapReached,
    ManualStop,
    FatalError(String),
}

#[derive(Clone, Debug)]
pub enum WindowOutcome {
    Won { proceeds_usd: Decimal },
    Lost { spent_usd: Decimal },
    Skipped { reason: SkipReason },
}

#[derive(Clone, Debug)]
pub enum SkipReason {
    PriceOutsideBand { ask: Decimal },
    FillOrKillFailed,
    ResolutionTimeout,
    GammaApiUnavailable,
    MarketNotFound,
}
```

### 核心转换（纯函数）

```rust
pub fn apply_outcome(
    state: &LadderState,
    outcome: &WindowOutcome,
    now: DateTime<Utc>,
) -> LadderState {
    let mut next = state.clone();
    match outcome {
        WindowOutcome::Won { proceeds_usd } => {
            let bet = state.current_bet_usd();
            next.realized_pnl_usd += proceeds_usd - bet;
            next.windows_won += 1;
            next.current_step = 1;
        }
        WindowOutcome::Lost { spent_usd } => {
            next.realized_pnl_usd -= spent_usd;
            next.windows_lost += 1;
            if state.current_step >= state.max_step {
                next.stopped = Some(StopReason::CapReached);
            } else {
                next.current_step += 1;
            }
        }
        WindowOutcome::Skipped { .. } => {
            next.windows_skipped += 1;
        }
    }
    next
}

impl LadderState {
    pub fn current_bet_usd(&self) -> Decimal {
        self.base_usd * Decimal::from(2_u64.pow((self.current_step - 1) as u32))
    }
    pub fn is_stopped(&self) -> bool { self.stopped.is_some() }
}
```

### 不变量（写成单元测试）

1. `current_step ∈ [1, max_step]` 永远成立
2. step=k 时 `current_bet_usd() == base * 2^(k-1)`
3. 触顶后 `Lost` 必设置 `stopped = CapReached`
4. `Skipped` 不改 step、pnl、win/lose 计数
5. `apply_outcome` 在固定输入下产出唯一输出（无随机、无时钟依赖）
6. serde 往返保留所有字段

---

## 6. 数据流与 Redis schema

### Key 总览

| Key | 类型 | Writer | Reader | 用途 |
|---|---|---|---|---|
| `poly:prod:balance:latest` | string | refresher (TUI 内) | TUI app | v1.0 既有 |
| `poly:prod:trader:ladder` | string | trader | trader | FSM 状态——崩溃恢复唯一来源 |
| `poly:prod:trader:events` | stream | trader | TUI / 排查 | append-only，MAXLEN ~ 1000 |
| `poly:prod:trader:lock` | string | trader | trader | 多实例互斥，TTL 60s，30s 续期 |

### 事件语义

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TraderEvent {
    pub ts: DateTime<Utc>,
    pub session_id: Uuid,
    pub kind: TraderEventKind,
    pub ladder: LadderState,                  // 每条都带快照
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TraderEventKind {
    SessionStarted,
    SessionStopped { reason: StopReason },

    WindowOpening { window_ts: i64, slug: String },
    EntryDecision { decision: EntryDecision },

    OrderPlaced { kind: OrderKind, dollars: Decimal, token_id: String },
    OrderFilled { fill_price: Decimal, shares: Decimal, dollars: Decimal },
    OrderRejected { reason: String },

    Resolved { winner: Direction, our_side: Direction, our_outcome: WinLose },
    ResolutionTimeout,

    SellFilled { proceeds_usd: Decimal },
    SellRejected { reason: String },

    LadderUpdated { from_step: u8, to_step: u8, outcome: WindowOutcome },

    Alert { message: String },
}

pub enum EntryDecision {
    Enter { ask: Decimal },
    SkipBand { ask: Decimal },
    SkipNotFound,
}
```

### 单窗口数据流

```
T+0: Scheduler 触发
     │
     ├─ emit WindowOpening                 → stream
     ├─ market.find_window(ts)             → gamma-api
     │   └─ if 404 → SkipNotFound          → emit + outcome=Skipped
     ├─ ask price → if outside [0.45, 0.55]
     │             → SkipBand              → emit + outcome=Skipped
     ├─ executor.buy_fok(token, dollars)   → CLOB
     │   ├─ filled  → emit OrderFilled
     │   └─ rejected → emit OrderRejected → outcome=Skipped
     │
T+5: resolver.await_resolution(market)     → poll Polymarket gamma 2s/loop, 60s timeout
     │   ├─ resolved Won  → executor.sell_market → emit SellFilled, outcome=Won
     │   ├─ resolved Lost → emit Resolved,                         outcome=Lost
     │   └─ timeout       → emit ResolutionTimeout,                outcome=Skipped
     │
T+5+: ladder.apply_outcome(outcome)
      ├─ state_store.save(new_ladder)      → poly:prod:trader:ladder
      ├─ emit LadderUpdated                → stream
      └─ if new_ladder.stopped() → SessionStopped → break loop
```

**严格次序**：先 save 后 emit。崩溃恢复时事件流 ≤ 持久化状态，不会出现"emit 出去但 ladder 没更新"。

---

## 7. 错误处理

### 致命（trader 启动即退出）

| 场景 | 处理 |
|---|---|
| `.env` / CLI 缺必填 | panic + readable error |
| Redis 连不上 | exit |
| CLOB L2 auth 失败 | exit |
| 多实例（锁抢失败） | exit + 报告占用者 ID |

### 非致命（emit Alert，单局影响，不影响会话）

| 场景 | 单局结果 | ladder |
|---|---|---|
| gamma-api 错误 | `Skipped::GammaApiUnavailable` | 不动 |
| 市场未找到 | `Skipped::MarketNotFound` | 不动 |
| 价格越界 | `Skipped::PriceOutsideBand` | 不动 |
| FoK 拒单 | `Skipped::FillOrKillFailed` | 不动 |
| Polymarket 60s 未结算 | `Skipped::ResolutionTimeout` | 不动 |
| Redis 写入瞬时失败 | 重试 1 次；仍失败 in-memory，下个 tick 再持久化 | — |

### 累积升级到致命

| 场景 | 触发 | 处理 |
|---|---|---|
| 卖单连失（赢局现金化失败） | 同窗口重试 3 次都失败 | `SessionStopped::FatalError("sell failed")` + Alert |
| USDC 不足 | 连续 3 次 | `SessionStopped::FatalError("insufficient_funds")` |
| Redis 持续失联 | 5 分钟内 ≥ 3 次写失败 | trader exit；下次启动 resume |
| 系统时钟漂移 | 连续 ≥ 5 次 NotFound | Alert，不自动停 |

### 关键时序

1. **save 在 emit 之前**——保证 stream ≤ 持久化状态
2. **emit 失败重试 3 次**——3 次都失败 trader exit（不能有 ladder 已 save 但事件丢了的沉默状态）
3. **shutdown signal**：本局完成才退出，不强中断 buy/sell/poll

### 多实例锁

```
key:    poly:prod:trader:lock
value:  {hostname}:{pid}:{session_id}
TTL:    60s
refresh: 每 30s 续期；丢失 → emit Alert + SessionStopped::ManualStop
```

启动时 `SET ... NX EX 60`，失败 → exit。

### 已知限制

- **In-flight crash**：trader 在 buy 之后 / sell 之前挂掉，重启后 ladder 进入下一窗口，旧仓位变孤儿 → emit Alert，需人工清理。v1.x 不解决，v1.1 加 `in_flight: Option<WindowMarket>` 字段。

---

## 8. TUI 集成

### 新布局（80×24 终端）

```
┌─ poly-tui ─────────────────────────────────────────────┐
│                                                         │
│                  USDC: $173.70                          │  3 行
│                                                         │
├─ Trader  UP  ladder=2  P&L: -$5.00 ────────────────────┤  1 行 sub-title
│ 15:00:00 WindowOpen btc-updown-5m-1747789200            │
│ 15:00:01 OrderFilled 10sh @ 0.50  $5.00                 │
│ 15:05:02 Resolved  winner=UP we=UP  WON                 │  Min(0)
│ 15:05:03 SellFilled  $9.95                              │
│ 15:05:03 LadderUpdated 1→1  won  +$4.95                 │
│ 15:10:00 WindowOpen btc-updown-5m-1747789500            │
│ 15:10:01 SkipBand  ask=0.58                             │
│ 15:10:02 (idle until 15:15:00)                          │
├─────────────────────────────────────────────────────────┤
│ ● CLOB ● Redis ● Trader  refresh:30s  last:12s ago  q r │  1 行 status
└─────────────────────────────────────────────────────────┘
```

### `AppState` 新字段

```rust
pub struct AppState {
    // 现有
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub redis_ok: bool,
    pub refresh_interval: Duration,
    pub should_quit: bool,

    // 新
    pub trader_log: VecDeque<TraderEvent>,    // ring buffer N=64
    pub trader_latest: Option<TraderEvent>,
    pub trader_health: TraderHealth,
}

pub enum TraderHealth {
    NotStarted,
    Healthy,                                   // < 6 分钟
    Lagging,                                   // 6–12 分钟
    Stale,                                     // > 12 分钟
    Stopped,
}
```

### 接入

第 4 个 tokio task：`subscribe_trader_events` 在启动时 XREVRANGE 拿历史 64 条 + XREAD 阻塞订阅新事件，转成 `AppEvent::TraderEvent(...)` 投到 channel。`AppEvent` 枚举新增 `TraderEvent` 变体，`app::handle_event` 添加分支。

### 渲染

`ui.rs` 新增 `render_trader_panel(...)`，`UiState` 派生新增 `TraderUiState`（保持 `ui.rs` 纯函数）。

---

## 9. 测试策略（99% 覆盖率达成路径）

### 三层结构 + 覆盖率排除

```
单元（lib + tests）   pure logic + adapter decode/encode 100%
BDD（cucumber-rs）   关键策略场景全覆盖
E2E（testcontainers + fakes）  跨边界冒烟
集成（real Redis / wiremock）  #[ignore]，决定真实 I/O 行为

ignore 列表：src/bin/，*_wrapper.rs
```

### 单元覆盖矩阵

| 模块 | 目标 | 手法 |
|---|---|---|
| `trader::ladder` | 100% | 直接 assert，包括 proptest 不变量 |
| `trader::config` | 100% | env / CLI 解析，复用 v1.0 with_env helper |
| `trader::scheduler` | ≥ 95% | `tokio::time::pause()` + 注入 `WindowExecutor` |
| `trader::market` | decode 100%（wrapper 由 e2e 覆盖） | 真实 gamma-api 响应 fixtures |
| `trader::executor` | decode/compute 100%（wrapper 由 e2e 覆盖） | `compute_share_count`, `decode_fill`, `map_sdk_error` 抽纯函数 |
| `trader::resolver` | 100% | 注入 `MarketProbe` trait |
| `trader::state` | encode/decode 100%（adapter 由集成覆盖） | JSON 往返 + Redis SETNX 在集成测试 |
| `trader::event` | encode/decode 100% | 同上 |
| `tui::events` | event-handling logic 100% | XREAD wrapper 由 e2e 覆盖 |

### BDD 场景（`tests/features/trader.feature`）

```gherkin
Feature: Martingale 5-minute trader

  Background:
    Given direction "UP", base $5, max_step 5
    And trader has fresh ladder state

  Scenario: 第一局赢
    When window opens with ask UP=0.50
    And FoK buy fills 10 shares at $0.50
    And resolution returns winner=UP
    And sell market fills at $0.99 for $9.90 proceeds
    Then ladder step is 1
    And realized_pnl is +$4.90

  Scenario: 连输三局，第四局赢，回到起注
    Given ladder advanced to step 4 ($40)
    When resolution returns winner=UP and sell yields $79.20
    Then ladder step is 1
    And realized_pnl is +$4.20

  Scenario: 连输 5 局触顶停止
    Given ladder at step 5 ($80)
    When resolution returns winner=DOWN
    Then session_stopped is CapReached
    And no further windows are processed

  Scenario: 价格偏离 50/50 跳过
    When window opens with ask UP=0.62
    Then no order is placed
    And ladder step is unchanged
    And event SkipBand is emitted

  Scenario: FoK 失败跳过
    When FoK buy returns NoLiquidity
    Then ladder step is unchanged
    And event OrderRejected is emitted

  Scenario: 60s 未解析跳过
    When resolution polling exceeds 60s
    Then ladder step is unchanged
    And event ResolutionTimeout is emitted

  Scenario: dry-run 不下真单
    Given trader started with --dry-run
    When window opens normally
    Then no real CLOB order is placed
    And SimulatedExecutor records the call but returns synthetic fill
    And resolver returns winner per dry-run policy (default: 50/50 coin flip)
    And ladder advances accordingly
```

### E2E（`tests/e2e_trader.rs`，#[ignore]）

1. `e2e_full_session_5_wins` — 5 连赢，ladder 永远在 1，pnl 累加
2. `e2e_cap_reached_stops_session` — 5 连输到顶，进程清理退出
3. `e2e_resume_from_crash` — 第 3 局后 trader 进程被 kill，重启接 ladder=4
4. `e2e_lock_prevents_double_run` — 起两个 trader，第二个 exit
5. `e2e_tui_subscribes_to_stream` — trader 写事件 → TUI 渲染

### 集成（`tests/trader_*_integration.rs`，#[ignore]）

- `trader_state_integration.rs`：RedisTraderState save/load/clear/lock/refresh，testcontainers
- `trader_market_integration.rs`：wiremock 模拟 gamma-api 200/404/500/超时

### 覆盖率门槛

```bash
cargo llvm-cov --lib --tests \
  --ignore-filename-regex 'src/bin|.*_wrapper\.rs'
```

`src/trader/` ≥ **99%**；v1.0 既有模块覆盖率不退化（v1.0 实测 79.5%）。

---

## 10. 依赖

### `Cargo.toml` 新增

```toml
[dependencies]
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
uuid = { version = "1", features = ["v4", "serde"] }
clap = { version = "4", features = ["derive"] }
# tokio 已有，包含 signal feature

[dev-dependencies]
proptest = "1"        # ladder 不变量属性测试
```

> **验证待办**：impl 时确认 `polymarket_client_sdk_v2` 是否暴露 gamma 客户端。有则用 SDK；无则用 `reqwest` 直连。

---

## 11. CLI 与启动

### `poly-trader --help`

```
poly-trader --direction <up|down> --base <USD>
            [--max-step N]         (default 5)
            [--band-min DECIMAL]   (default 0.45)
            [--band-max DECIMAL]   (default 0.55)
            [--dry-run]            模拟成交，不下真单
            [--reset]              抹掉之前的 ladder
            [--max-windows N]      跑 N 局后自然停（受控测试用）
```

### `restore_or_init` 决策

| Redis ladder | --reset | 行为 |
|---|---|---|
| 无 | - | 新建（用 args 中的参数） |
| 有，未 stopped | 否 | 接着跑（**args 中 direction/base 被忽略**——已 lock 在 ladder） |
| 有，stopped | 否 | exit + 提示 `--reset` |
| 有 | 是 | 抹掉 + 新建 |

### 操作手册（写进 README）

```bash
# 启动 trader
poly-trader --direction up --base 5

# dry-run 试跑（推荐第一晚）
poly-trader --direction up --base 5 --dry-run --max-windows 24

# 看 TUI
poly-tui

# 停 trader
Ctrl+C  (或 kill)

# 看日志
tail -f logs/trader-*.log

# 看状态
docker exec poly-redis redis-cli GET poly:prod:trader:ladder | jq .
docker exec poly-redis redis-cli XREVRANGE poly:prod:trader:events + - COUNT 10
```

---

## 12. 主进程流程（`src/bin/poly-trader.rs`）

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = TraderArgs::parse();
    dotenvy::dotenv().ok();
    let cfg = Config::from_env()?;

    // 1. 日志 → 文件，不污染 stdout
    let appender = tracing_appender::rolling::daily("logs", "trader.log");
    let (nb, _guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt().with_writer(nb)
        .with_env_filter(EnvFilter::new(&cfg.log_level)).init();

    // 2. 适配器
    let state_store: Arc<dyn TraderStateStore> =
        Arc::new(RedisTraderState::connect(&cfg.redis_url).await?);
    let emitter: Arc<dyn TraderEventEmitter> =
        Arc::new(RedisTraderStream::connect(&cfg.redis_url).await?);
    let market: Arc<dyn MarketDiscovery> =
        Arc::new(GammaMarketDiscovery::new(cfg.gamma_host.clone()));
    let resolver: Arc<dyn WindowResolver> =
        Arc::new(PolymarketResolver::new(market.clone(), Duration::from_secs(60)));
    let executor: Arc<dyn OrderExecutor> = if args.dry_run {
        Arc::new(SimulatedExecutor::new())
    } else {
        Arc::new(ClobOrderExecutor::connect(&cfg.clob_host, &cfg.polymarket_private_key).await?)
    };

    // 3. 多实例锁
    let lock = TraderLock::acquire(state_store.clone()).await?;

    // 4. 恢复或新建 ladder
    let ladder = restore_or_init(&state_store, &args).await?;

    // 5. shutdown signal
    let shutdown = CancellationToken::new();
    spawn_signal_handler(shutdown.clone());

    // 6. 调度循环
    let result = trader::scheduler::run(
        ladder,
        TraderRunCtx { args, market, executor, resolver, state_store, emitter, shutdown },
    ).await;

    lock.release().await.ok();
    result.map(|_| ())
}
```

---

## 13. 接受标准

### 功能
- [ ] `poly-trader --dry-run --max-windows 12` 跑完一小时无崩溃
- [ ] 真钱跑通至少一个完整窗口（buy → resolve → sell）
- [ ] FoK 流动性不足场景观察 `OrderRejected` + ladder 不动
- [ ] 价格越界观察 `SkipBand` + ladder 不动
- [ ] N=5 触顶（dry-run 强制全输）观察 `SessionStopped::CapReached`
- [ ] kill 后重启，ladder + P&L 完整恢复
- [ ] 起两个 `poly-trader`，第二个抢锁失败退出

### TUI
- [ ] log panel 在 trader 未启动时显示 "trader not started"
- [ ] trader 启动 1 秒内 TUI 收到 `SessionStarted`
- [ ] 长行截断不破坏布局
- [ ] sub-title 显示当前 ladder step + direction + 累计 P&L
- [ ] Trader 健康灯在 6/12 分钟阈值正确变色

### 测试
- [ ] `cargo test --lib` 全绿
- [ ] `cargo test --test bdd` 全绿（v1.0 既有 4 + trader 新增 7+ 个）
- [ ] `cargo test --test cache_integration -- --ignored` 全绿
- [ ] `cargo test --test trader_state_integration -- --ignored` 全绿
- [ ] `cargo test --test trader_market_integration -- --ignored` 全绿
- [ ] `cargo test --test e2e_trader -- --ignored` 全绿
- [ ] `src/trader/` 覆盖率 ≥ 99%
- [ ] v1.0 既有模块覆盖率不退化

### 安全 / 隔离
- [ ] 所有 Redis key 在 `poly:prod:*` 命名空间
- [ ] E2E / 集成硬断言 `port != 6379`
- [ ] `.env` 不在仓库
- [ ] 私钥使用路径与 v1.0 一致（`SignatureType::Proxy`）

---

## 14. 后续路线（v1.1+ 指针）

详见仓库根 `TODO.md`。本设计完成后会更新 TODO 加上：

- **v1.1 daemon 拆分**：把 v1.0 的 refresher 也搬到 trader 进程，TUI 全只读，trader 升级为 `poly-daemon`
- **v1.2 in-flight 恢复**：`LadderState` 加 `in_flight: Option<WindowMarket>` 字段，崩溃后能精确接续未完成的局
- **v1.3 多市场 / 多策略并行**：每个策略独立 ladder + 独立 lock；统一 daemon 调度
- **v1.4 风控扩展**：每日最大损失上限、滑点告警、会话内 win-rate 监控
