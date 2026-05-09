# BTC Market Watch Strip — TUI v1.x.1 Design

- **日期：** 2026-05-09
- **范围：** v1.x.1（一个新增 TUI 行：当前 BTC 价格 / 与开盘对比 / 5min 倒计时）
- **状态：** 待实现
- **前置：** v1.x trader 已交付；当前 commit `3519187`（cache-bust 修复）
- **运行约束：** 不影响正在跑的 dry-run 进程（PID 53896）

---

## 1. 目标与范围

### 做什么
在 `poly-tui` 余额区与 trader sub-title 之间插入一个 1 行信息条，显示：
- 当前 5-min 窗口的 `priceToBeat`（窗口开盘 BTC 价）
- 当前 BTC/USD 实时价（Chainlink Polygon 主网喂价）
- 二者差值（带正负号、颜色）
- 距离窗口结束的 MM:SS 倒计时

数据源完全独立于 trader 进程——TUI 自己拿取，trader 关掉时仍可工作。

### 不做什么（v1.x.1 显式排除）
- ETH / SOL / 其他币种
- 其他窗口时长（15-min、1-hour）
- 历史价格图 / sparkline
- BTC 价格源运行时切换
- TUI 重启后 priceToBeat 持久化
- WebSocket 推送（保持与现有架构一致用 REST 轮询）
- trader 使用此 watcher 的价格（trader 保持解耦）

### 成功判定
见 §10 接受标准。

---

## 2. 关键决策摘要

| 决策点 | 选择 | 理由 |
|---|---|---|
| 当前 BTC 价格源 | Chainlink BTC/USD on Polygon | Polymarket 用同一喂价结算；diff 为"真实" |
| 显示位置 | balance 与 trader sub-title 之间 1 行 | 视觉分组：market info ≠ trader info |
| 进程模型 | poly-tui 第 5 个 tokio task | 独立于 trader；trader 关闭时仍工作 |
| 缺数据时 | "--" 占位，不闪烁 | 单一状态机，可预测 |
| RPC 轮询频率 | 5s | 匹配 Chainlink 心跳 27s + 0.5% 偏移触发 |
| gamma 轮询频率 | 每 5min 边界（task 内每 15s tick + 边界判定） | 一窗口一拉，priceToBeat 不变 |
| 渲染频率 | 既有 250ms tick | 倒计时跳变看似流畅 |
| Polygon RPC URL | 配置项，默认 `https://polygon-rpc.com` | 公共默认免费，用户可换 |
| 测试覆盖 | 新代码纯逻辑 100%，wrapper 排除 | 与 v1.0 / v1.x 一致 |

---

## 3. 架构总览

新增 1 个 tokio 任务，独立于 trader：

```
                                    Polygon RPC                  Polymarket gamma-api
                                         ▲                              ▲
                                         │ latestRoundData              │ /events?slug=...
                                         │ every 5s                     │ at 5-min boundary
                ┌────────────────────────┼──────────────────────────────┼───────────────┐
                │   poly-tui process     │                              │               │
                │   ┌──────────────────────────────────────────────┐    │               │
                │   │     market_watch task (NEW, 5th task)        │────┘               │
                │   │  • polls Chainlink BTC/USD via alloy         │                    │
                │   │  • fetches gamma at window boundary          │────────────────────┘
                │   │  • emits AppEvent::MarketUpdate(MarketState) │
                │   └──────────────┬───────────────────────────────┘
                │                  │ via existing event_tx mpsc
                │   ┌──────────────▼─────────┐
                │   │  App task              │
                │   │  AppState.market: ...  │
                │   │  ui::render() reads it │
                │   └────────────────────────┘
                │
                │   (existing 4 tasks unchanged: refresher, app, input,
                │    trader event subscriber)
                └─────────────────────────────────────────────────────────────────────────┘
```

**Key invariants**

1. 与 trader 解耦——poly-trader 进程是否运行不影响这个 task
2. 没有新 Redis key——task 的 state 只在 TUI 进程内
3. 复用现有 `AppEvent` channel——和 refresher / trader event subscriber 同模式
4. 复用现有 `MarketDiscovery` trait（trader 用它做市场发现，TUI 用它拉 priceToBeat）；`WindowMarket` 加一个 `Option<Decimal>` 字段
5. 新增 `BtcPriceFeed` trait，real impl `ChainlinkBtcPriceFeed`，fake impl 用于单测

**与正在运行的 dry-run trader 进程的关系**

PID 53896 用的是 `target/release/poly-trader.exe`——已加载到内存的二进制。源代码改动不影响其运行。`WindowMarket` 加字段是纯加法，且 `WindowMarket` 不出现在任何跨进程序列化（Redis key 里只有 `LadderState` 和 `TraderEvent`），所以即便 trader 二进制日后重新构建，状态向后兼容。

**实施期间的构建纪律**：

- 只跑 `cargo build --bin poly-tui` 和 `cargo test --lib`
- 避免 `cargo build`（无参）和 `cargo build --bin poly-trader`——会因 .exe 文件锁失败
- 需要验证 trader 仍能编译时用 `cargo check --bin poly-trader`（不写 .exe）
- 绝不强杀 poly-trader.exe 来释放锁

---

## 4. 模块结构与 trait

```
src/tui/
├── mod.rs                  ← +pub mod market_watch
├── events.rs               (unchanged)
└── market_watch.rs         ← NEW: BtcPriceFeed trait, MarketState, run()

src/trader/
├── market.rs               ← extend WindowMarket: +price_to_beat: Option<Decimal>
│                              extend decode_event_response (additive)
├── adapters/
│   └── chainlink_btc_wrapper.rs   ← NEW (excluded from coverage)

src/domain.rs               ← +AppEvent::MarketUpdate(MarketState)
src/app.rs                  ← AppState.market: Option<MarketState> field
                              handle_event branch for MarketUpdate
src/ui.rs                   ← +render_market_strip; new Layout chunk

src/bin/poly-tui.rs         ← spawn 5th task; build ChainlinkBtcPriceFeed
src/config.rs               ← +polygon_rpc_url field
.env.example                ← +POLYGON_RPC_URL=https://polygon-rpc.com
```

### New traits

```rust
// src/tui/market_watch.rs
#[async_trait]
pub trait BtcPriceFeed: Send + Sync {
    async fn latest_price(&self) -> Result<Decimal, MarketWatchError>;
}
```

`MarketDiscovery` 复用既有 trait，无变化。

### Module dependency direction

```
tui::market_watch    →  domain (AppEvent), trader::market (MarketDiscovery + WindowMarket)
trader::market       →  domain                                        (extends existing)
trader::adapters::chainlink_btc_wrapper   →  tui::market_watch (BtcPriceFeed trait), alloy
bin/poly-tui         →  tui::market_watch, trader::adapters::*
```

---

## 5. `WindowMarket` 扩展

```rust
// src/trader/market.rs
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowMarket {
    pub window_ts: i64,
    pub slug: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub up_ask: Decimal,
    pub down_ask: Decimal,
    pub closed: bool,
    pub winner: Option<Direction>,
    pub price_to_beat: Option<Decimal>,   // ← NEW
}
```

`Option` 因为：
- 新窗口可能还没设置 priceToBeat
- 老市场可能没有 `eventMetadata` 字段
- resolver / window orchestration 不关心这个字段，只看 winner / asks / token_ids

### `decode_event_response` 扩展

```rust
// after existing decode logic
let price_to_beat = event.get("eventMetadata")
    .and_then(|m| m.get("priceToBeat"))
    .and_then(|p| p.as_f64())
    .and_then(|f| Decimal::from_f64_retain(f));
```

任何缺失或解析失败 → `None`；不会让既有路径退化。

### 既有调用点

需要把 `WindowMarket { ... }` 字面量加上 `price_to_beat: None`：
- `src/trader/window.rs` 测试 fakes（~5 处）
- `src/trader/resolver.rs` 测试 fakes
- `tests/support/*`
- `tests/e2e_trader.rs`、`tests/trader_market_integration.rs` fixtures

机械性变更，不改语义。

---

## 6. `MarketState` + `market_watch::run`

### Shared state

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketState {
    pub window_ts: Option<i64>,
    pub price_to_beat: Option<Decimal>,
    pub current_price: Option<Decimal>,
    pub last_rpc_ok_at: Option<DateTime<Utc>>,
    pub last_gamma_ok_at: Option<DateTime<Utc>>,
}

impl MarketState {
    pub fn empty() -> Self { /* all None */ }
    pub fn diff(&self) -> Option<Decimal> { /* current - to_beat */ }
    pub fn rpc_healthy(&self, now: DateTime<Utc>) -> bool { /* < 30s old */ }
    pub fn gamma_healthy(&self, now: DateTime<Utc>) -> bool { /* < 6 min old */ }
    pub fn seconds_to_next_boundary(&self, now_ts: i64) -> i64 { /* end - now */ }
}
```

纯函数；100% 单测可达。

### Run loop

```rust
pub async fn run(
    price_feed: Arc<dyn BtcPriceFeed>,
    market: Arc<dyn MarketDiscovery>,
    event_tx: mpsc::Sender<AppEvent>,
    shutdown: CancellationToken,
) {
    let mut state = MarketState::empty();
    let mut rpc_ticker = tokio::time::interval(Duration::from_secs(5));
    let mut gamma_ticker = tokio::time::interval(Duration::from_secs(15));

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,

            _ = rpc_ticker.tick() => {
                if let Ok(p) = price_feed.latest_price().await {
                    state.current_price = Some(p);
                    state.last_rpc_ok_at = Some(Utc::now());
                }
                emit(&event_tx, &state).await;
            }

            _ = gamma_ticker.tick() => {
                let now_ts = Utc::now().timestamp();
                let current_window = floor_5min(now_ts);
                if state.window_ts != Some(current_window) {
                    if let Ok(m) = market.find_window(current_window).await {
                        state.window_ts = Some(current_window);
                        state.price_to_beat = m.price_to_beat;
                        state.last_gamma_ok_at = Some(Utc::now());
                        emit(&event_tx, &state).await;
                    }
                }
            }
        }
    }
}
```

### `ChainlinkBtcPriceFeed` real impl（adapter wrapper）

```rust
// src/trader/adapters/chainlink_btc_wrapper.rs

const BTC_USD_AGGREGATOR_POLYGON: &str = "0xc907E116054Ad103354f2D350FD2514433D57F6f";
const BTC_USD_DECIMALS: u32 = 8;

pub struct ChainlinkBtcPriceFeed { /* alloy provider + cached aggregator */ }

impl ChainlinkBtcPriceFeed {
    pub async fn connect(rpc_url: &str) -> Result<Self, MarketWatchError> { ... }
}

#[async_trait]
impl BtcPriceFeed for ChainlinkBtcPriceFeed {
    async fn latest_price(&self) -> Result<Decimal, MarketWatchError> {
        // call latestRoundData() via alloy
        // returns (roundId, answer, startedAt, updatedAt, answeredInRound)
        // answer is i256; decode to plain USDC by dividing by 10^8
        decode_chainlink_answer(raw_answer, BTC_USD_DECIMALS)
    }
}

/// Pure helper, unit-tested.
pub fn decode_chainlink_answer(raw: i128, decimals: u32) -> Decimal { ... }
```

---

## 7. TUI 渲染

### Layout

```rust
// src/ui.rs render()
let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Length(5),    // balance
        Constraint::Length(1),    // market strip (NEW)
        Constraint::Length(1),    // trader sub-title
        Constraint::Min(0),       // trader log
        Constraint::Length(1),    // status bar
    ])
    .split(area);
```

固定行总数 8；最小终端高度 10（标准 24 行没问题）。

### `render_market_strip`

```rust
pub fn render_market_strip(frame: &mut Frame, area: Rect, state: &UiState) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    let m = match &state.market {
        Some(m) => m,
        None => {
            frame.render_widget(Paragraph::new(" BTC: -- "), area);
            return;
        }
    };

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(" BTC "));

    match (m.price_to_beat, m.current_price) {
        (Some(p), Some(c)) => {
            spans.push(Span::raw(format_usd_int(p)));
            spans.push(Span::raw(" → "));
            spans.push(Span::raw(format_usd_int(c)));

            let diff = c - p;
            let (sign, color) = if diff > Decimal::ZERO {
                ("+", Color::Green)
            } else if diff < Decimal::ZERO {
                ("", Color::Red)
            } else {
                ("±", Color::White)
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("{sign}{}", format_usd_int(diff)),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
        (None, Some(c)) => {
            spans.push(Span::raw("--"));
            spans.push(Span::raw(" → "));
            spans.push(Span::raw(format_usd_int(c)));
            spans.push(Span::raw("  --"));
        }
        (Some(p), None) => {
            spans.push(Span::raw(format_usd_int(p)));
            spans.push(Span::raw(" → "));
            spans.push(Span::styled("--",
                Style::default().fg(Color::DarkGray)));
            spans.push(Span::raw("  --"));
        }
        (None, None) => {
            spans.push(Span::raw("--"));
        }
    }

    spans.push(Span::raw("   "));
    let now_ts = state.now.timestamp();
    let secs = m.seconds_to_next_boundary(now_ts);
    if secs > 0 {
        spans.push(Span::raw(format!("⏱ {}:{:02}", secs / 60, secs % 60)));
    } else {
        spans.push(Span::styled("⏱ rolling…",
            Style::default().fg(Color::DarkGray)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
```

### Visual examples

无 trader 在跑：
```
┌─ poly-tui ─────────────────────────────────────────────┐
│         USDC: $173.70                                   │
├─────────────────────────────────────────────────────────┤
│ BTC 80,425 → 80,431  +6  ⏱ 3:42                         │
├─────────────────────────────────────────────────────────┤
│ Trader  not started — run `poly-trader`                 │
├─────────────────────────────────────────────────────────┤
│                                                         │
├─────────────────────────────────────────────────────────┤
│ ● CLOB ● Redis ● Trader  refresh: 30s  last: --    q r  │
└─────────────────────────────────────────────────────────┘
```

下跌：`BTC 80,425 → 80,418  -7  ⏱ 1:08`（红色粗体）
窗口边界：`BTC 80,418 → 80,418  ±0  ⏱ rolling…`
RPC 故障：`BTC 80,425 → -- (stale)  --  ⏱ 0:43`

---

## 8. Config / 错误处理

### Config 扩展

```rust
// src/config.rs
pub struct Config {
    // existing fields...
    #[serde(default = "default_polygon_rpc_url")]
    pub polygon_rpc_url: String,
}

fn default_polygon_rpc_url() -> String { "https://polygon-rpc.com".to_string() }
```

`.env.example` 加：
```
POLYGON_RPC_URL=https://polygon-rpc.com
```

Aggregator 地址硬编码为常量（Polygon mainnet 固定不变）。

### 错误处理

| 场景 | 行为 |
|---|---|
| Polygon RPC 启动连接失败 | warn，task 继续轮询；UI 显示 `--` |
| RPC 中途抖动 | `last_rpc_ok_at` 老化 → UI 显示最后已知价格 + 暗灰 + "(stale)" |
| gamma-api 5xx / 超时 | priceToBeat 维持上一窗口值（不刷新）；warn |
| eventMetadata.priceToBeat 缺失 | `state.price_to_beat = None`；UI 显示 `--` |
| `Decimal::from_f64_retain` 失败 | `None`；当作缺失 |
| Task crash | `tokio::spawn` join handle 忽略；MarketUpdate 不再到达 → state 老化 → UI degrades |
| Shutdown signal | task 观察 `shutdown.cancelled()` → 干净退出 |

不引入新致命错误路径——市场行情条只是显示，不触发动作。

---

## 9. 测试策略

### 单元测试（目标 100% 新纯逻辑覆盖）

`tui::market_watch::tests`：
- `MarketState::diff` 全分支
- `MarketState::seconds_to_next_boundary` 边界 / 中段 / 刚过
- `MarketState::rpc_healthy` / `gamma_healthy` 老化逻辑
- `run()` 用 `tokio::time::pause()` + 注入的 `FakeBtcPriceFeed` + `FakeMarketDiscovery`：
  - rpc 正常更新
  - gamma 边界更新
  - 两侧故障老化
  - shutdown 干净退出

`trader::market::tests`：
- `decode_extracts_price_to_beat`（新 fixture 含 eventMetadata）
- `decode_missing_event_metadata_is_none`（既有 fixtures 仍然 Ok，price_to_beat = None）

`trader::adapters::chainlink_btc_wrapper::tests`（纯 helper）：
- `decode_chainlink_answer(raw, decimals) -> Decimal`：典型值、零、极小值

`ui::tests` 新增 5 个 insta snapshots：
- `renders_market_no_data`
- `renders_market_full`（diff 正，绿）
- `renders_market_negative_diff`（diff 负，红）
- `renders_market_only_current`（priceToBeat 缺失）
- `renders_market_rolling`（countdown == 0）

### 集成测试 (#[ignore]，需真 RPC)

`tests/chainlink_integration.rs`：
- 连公共 Polygon RPC，调 `latestRoundData()`
- 断言 price 在合理区间（10k < p < 1M）
- 断言 decimals == 8

### 不新增 BDD / 不新增 trader E2E

这是一个显示功能，不改变交易行为。既有 trader BDD 场景全部不动。

### 覆盖率门槛

- `src/tui/market_watch.rs` ≥ 95%（其余 wrapper 和 alloy 调用排除）
- `src/trader/market.rs` 不退化（既有 92.86%）
- 既有覆盖率不退化（v1.x trader 96%）

---

## 10. 接受标准

- [ ] BTC strip 在 poly-tui 启动 ~5s 内首次渲染（首次 RPC 拉到价）
- [ ] `priceToBeat` 在 ~15s 内填入（首次 gamma 拉成功）
- [ ] diff 符号 + 颜色：正绿 / 负红 / 零白
- [ ] 倒计时每秒可见跳动（既有 250ms render tick 驱动）
- [ ] 跨过 5-min 边界时，`priceToBeat` 切到新窗口的值
- [ ] Polygon RPC 关掉 → strip 优雅退化（先 stale 暗灰，再 `--`）
- [ ] gamma 关掉 → strip 维持上一窗口的 priceToBeat（不显示空）
- [ ] 终端窄于一行宽 → 截断，不 panic
- [ ] 5 个新 insta snapshot 全绿
- [ ] 既有所有 test suites 全绿
- [ ] `src/trader/` 覆盖率不退化
- [ ] 实施期间 PID 53896 dry-run trader 不被打扰

---

## 11. 后续路线（指针）

- v1.x.2 候选：在 strip 加 ETH/SOL 切换（`--watch <symbol>`）
- v1.x.3 候选：sparkline 显示窗口内 BTC 价格走势
- v1.1 daemon 拆分时，market_watch 一起搬到 daemon 侧；TUI 改成纯订阅
- 当前 strip 的 priceToBeat 也可以作为 trader 的入场参考（"如果 priceToBeat 距 current 超过某阈值则跳过"）；现版本 trader 不读它
