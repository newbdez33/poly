# Polymarket BTC 5min 策略回测框架 — v1.4 设计文档

- **日期：** 2026-05-09
- **范围：** v1.4（独立 `poly-backtest` 二进制 + 离线策略对比工具）
- **目标读者：** 项目作者 / 协作者 / 未来的自己
- **状态：** 待实现
- **前置：** v1.0 / v1.1 / v1.2 已交付。current commit `2436436`（v1.2.1 polish）
- **运行约束：** 不影响正在运行的 trader（PID 53896 dry-run）

---

## 1. 目标与范围

### 做什么

一个**离线 CLI 工具**，跑 6 种策略变体在过去 30 天 Polymarket BTC 5min 历史数据上的回测，输出单页 HTML 报告对比 EV、胜率、最大回撤、cap 触发频率，**用数据驱动决策**：

- 找出 EV 显著正的策略（如果有）
- 验证或推翻 user 关于"TP-only Martingale"的直觉
- 决定 Bug A（scheduler 跳窗口）是否值得修——如果换策略后不需要并发窗口，Bug A 自动消失

回测器**不修改**任何 v1.0/v1.1/v1.2 代码，**复用** v1.1 的 6 个 trait（`MarketDiscovery`, `OrderExecutor`, `WindowResolver`, `TraderStateStore`, `TraderEventEmitter`, `TraderEventStream`）和 `LadderState` / `apply_outcome` FSM。

### 不做什么（v1.4 显式排除）

- 实时回测（与 trader 并行）
- 用户可调参的 web 控制台
- 真实 Polymarket trade 数据（用合成 token 价格模型）
- ETH / SOL 等其他 5min 市场
- 15-min 窗口
- 策略组合（同时跑多种）
- 风险管理工具（Kelly、固定比例）
- 自动选最优（用户看报告自己决定）
- **不修复 Bug A**（这是回测器的下游决策）

### 成功判定

见 §10 接受标准。

---

## 2. 关键决策摘要

| 决策点 | 选择 | 理由 |
|---|---|---|
| 数据保真度 | 合成（synthetic）| 半天搞定 vs. 真实数据要重新解决 Polymarket trade history 拉取（用户选 A）|
| 时段 | 30 天默认 | ~8500 窗口，统计意义足；CLI 参数可改 |
| 策略集合 | 6 种全跑 | 用户选 all 6（hold/TP-only/TP+SL sym/asym/time/fixed-stake）|
| BTC 数据源 | Binance 1-min klines | 免费、无 key、稳定；30 天 ~44 次请求 |
| Token 价格模型 | Black-Scholes 二元期权 + 估计 σ | 数学合理、参数少、~30 行代码 |
| 摩擦 | 固定 1.5%（spread + fees）| 与 v1.1 SimulatedExecutor 的 $0.99 卖价一致 |
| HTML 报告 | 单页静态 + Chart.js CDN | 自包含、易分享、~500KB |
| 缓存 | 落盘 JSON | 重跑无需重新拉数据 |
| CapReached 行为 | 重置 ladder 继续跑 | 模拟"用户手动重启"，回测整个时段 |

---

## 3. 架构总览

回测器是**独立离线工具**，与 trader / TUI 完全分开。

```
                                                          Binance API           gamma-api
                                                              │ klines              │ /events
                                                              ▼                    ▼
         ┌─────────────────────────────────────────────────────────────────────┐
         │                 poly-backtest 二进制                                 │
         │                                                                      │
         │  ┌──────────────────┐   ┌──────────────────┐                         │
         │  │ Data fetcher     │   │ Gamma fetcher    │                         │
         │  │ (Binance 1min)   │   │ (per-window meta)│                         │
         │  └────────┬─────────┘   └────────┬─────────┘                         │
         │           │ cache                │ cache                             │
         │           ▼                      ▼                                   │
         │  ┌───────────────────────────────────────────┐                      │
         │  │ Data store (JSON on disk)                  │                      │
         │  └────────────────┬──────────────────────────┘                      │
         │                   │                                                  │
         │                   ▼                                                  │
         │  ┌──────────────────────────────────┐                               │
         │  │ Token price oracle               │ ← Black-Scholes 模型           │
         │  │ p(t) = Φ((BTC(t)-pTb)/σ√(T-t))   │                               │
         │  └──────────┬───────────────────────┘                               │
         │             │                                                        │
         │             ▼                                                        │
         │  ┌──────────────────────────────────────────────────────┐           │
         │  │   Backtest runner（注入历史适配器）                    │           │
         │  │   ────────────────────────────────────────────         │           │
         │  │   复用 v1.1 既有：                                     │           │
         │  │   • trader::ladder（Martingale FSM）                  │           │
         │  │   • LadderState + apply_outcome                       │           │
         │  │   • Direction, WindowOutcome, SkipReason              │           │
         │  │                                                        │           │
         │  │   新增 6 种策略变体（StakeRule × ExitRule）           │           │
         │  └──────────────────┬─────────────────────────────────────┘          │
         │                     │ 每轮 outcome                                    │
         │                     ▼                                                 │
         │  ┌──────────────────────────────────────┐                           │
         │  │ Stats engine                         │                           │
         │  │ EV / 胜率 / 回撤 / cap-trigger 等     │                           │
         │  └──────────────────┬───────────────────┘                           │
         │                     ▼                                                │
         │  ┌──────────────────────────────────────┐                           │
         │  │ HTML report (single static file)     │                           │
         │  └──────────────────────────────────────┘                           │
         │                                                                      │
         └──────────────────────────────────────────────────────────────────────┘
```

**关键不变量：**

1. **零侵入 v1.1**——`trader::ladder` / `scheduler` / `window` / `run_window` 一行不改
2. **复用 LadderState + apply_outcome**——Martingale FSM 数学保证一致
3. **离线运行**——不连 Polymarket CLOB、不连 Polygon RPC、不动 Redis
4. **数据缓存**——拉过的 BTC 数据 + gamma 数据落盘 JSON，重跑无需重新拉
5. **6 种策略 = 6 次 run_strategy**——同一个历史时段，统一 stats，同一个 HTML 报告

**关键设计选择：策略变体如何映射到 trait 集？**

策略 #1（hold-to-resolution）就是当前 v1.1 行为。
策略 #2-#5 需要"早卖" → 需要扩展 `simulate_window` 支持 TP/SL/timer 提前 break。

回测器**新写一个 `simulate_window`**，原 v1.1 的 `run_window` 不动。新版本接受 `ExitRule` 参数。

---

## 4. 模块结构

```
src/
├── bin/
│   └── poly-backtest.rs              ← NEW: CLI 入口
├── backtest/                          ← NEW: 整个回测模块
│   ├── mod.rs
│   ├── config.rs                      ← BacktestArgs (clap), StrategyConfig 集合
│   ├── data/
│   │   ├── mod.rs
│   │   ├── binance.rs                 ← Binance klines fetcher + cache
│   │   ├── gamma_history.rs           ← gamma-api per-window fetcher + cache
│   │   └── cache.rs                   ← 通用 JSON 缓存
│   ├── oracle.rs                      ← BlackScholesOracle + σ 估计
│   ├── exit_rule.rs                   ← ExitRule enum + simulate_window
│   ├── runner.rs                      ← run_strategy（多窗口循环）
│   ├── stats.rs                       ← StrategyStats 计算
│   └── report.rs                      ← HTML 渲染
└── (现有 v1.0/v1.1/v1.2 模块全部不动)
```

**纪律：**
- 不动 `src/trader/` 任何文件
- 不动 `src/tui/`
- 不动 `src/bin/poly-tui.rs` / `src/bin/poly-trader.rs`
- 复用 `LadderState`, `apply_outcome`, `WindowOutcome`, `Direction`, `SkipReason`（来自 `trader::ladder`）

---

## 5. 数据管道

### 数据源

**Polymarket 历史窗口元数据（gamma-api）**

每个 5min 窗口的：
- `priceToBeat`（开盘 BTC 价）
- `finalPrice`（收盘 BTC 价）
- `outcomePrices`（结算 winner = Up/Down）
- `umaResolutionStatus`（确认是否已结算，跳过未结算窗口）

接口：`GET https://gamma-api.polymarket.com/events?slug=btc-updown-5m-{ts}`
速率：~10 req/s 不被拒；30 天 ~8500 窗口；串行 ~15 分钟，带 cache 后只跑一次。

**BTC 1-min 历史价格（Binance public API）**

接口：`GET https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&startTime=X&endTime=Y&limit=1000`

- 免费、无 API key
- 一次最多 1000 根 K 线 ≈ 16.6 小时
- 30 天 = 43200 根 = 44 次请求 ≈ 1 分钟拉完

> **为什么 Binance 而不是 Chainlink？** Polymarket 用 Chainlink 结算，但回测中我们已从 gamma 拿到**最终结算的 winner**。Binance 1-min 价仅用于**模拟窗口内 token 价格走势**，目的是估计 TP/SL 触发。Binance 与 Chainlink 在分钟尺度下偏差通常 < $20，对触发判断没影响。

### 缓存策略

```
~/.poly-backtest-cache/        ← 用户家目录（也支持 target/backtest-cache/）
├── binance/
│   ├── 2026-04-09.json         ← 1 天的 BTC 1min OHLC
│   ├── 2026-04-10.json
│   └── ...                      ← 每天一个文件
└── gamma/
    ├── 1778312100.json          ← 一个窗口的 gamma 响应
    ├── 1778312400.json
    └── ...                      ← 每窗口一个文件
```

每次跑回测：
1. 计算需要的时段
2. 检查缓存里缺哪些天 / 哪些窗口
3. 仅拉缺失部分
4. 全部加载到内存

CLI:
```bash
poly-backtest --start 2026-04-09 --end 2026-05-09  \
              --strategies all                       \
              --output report.html
```

### 数据完整性

- gamma 偶尔返回空（窗口刚开还没创建）→ 跳过，记录到日志
- gamma 返回但 `outcomePrices` 为空（异常未结算）→ 跳过
- Binance 偶尔有 1-2 分钟数据缺失 → 用前后线性插值
- BTC 价格 30 天可能跨越大 spike → 不特殊处理

### 速率限制 + 失败处理

- Binance: 1200 req/min weight，单 klines weight=1，远低于上限
- gamma: 没有官方限速，控制 10 req/s 以下
- 失败：指数回退，最多 3 次
- 每 100 个请求 print 进度

---

## 6. Token 价格模型

### Black-Scholes 二元期权

UP token 在窗口内的价格 = "市场认为窗口结束时 BTC ≥ priceToBeat 的概率"。

```
p_up(t) = Φ((BTC(t) - priceToBeat) / (σ × √(T - t)))
```

- `Φ`：标准正态 CDF（来自 `statrs` crate）
- `BTC(t)`：t 时刻 BTC 价（从 Binance 1min 数据，秒级用线性插值）
- `priceToBeat`：窗口开盘 BTC（来自 gamma）
- `σ`：BTC 在 5min 窗口内的标准差（关键参数）
- `T`：300 秒
- `t`：窗口已过秒数

模型行为：
- t = 0：BTC = priceToBeat → p = 0.5
- t → 300，BTC > priceToBeat：p → 1
- t → 300，BTC < priceToBeat：p → 0

DOWN token：`p_down = 1 - p_up`

### σ 的估计

```rust
// 用 30 天数据估
let log_returns: Vec<f64> = btc_5min_closes.windows(2)
    .map(|w| (w[1] / w[0]).ln())
    .collect();
let sigma_log = std_deviation(&log_returns);
let sigma_per_window = sigma_log * avg_btc_price;
```

典型值：`σ ≈ $50-150`（2026 年 BTC ~$80k，5min 标准差大约 $80）。

模型暴露 `σ` 作为可配置项（CLI `--sigma 80`），方便敏感性分析。

### 摩擦系数

每个买/卖动作扣 1.5%（中点上下 0.75%）：
- 模型给 mid = $0.50 → 实际买价 $0.5075，卖价 $0.4925
- TP 触发判断：用 bid（卖价）≥ 阈值，更保守

可调：`--friction 0.015`。

### Oracle API

```rust
pub trait TokenPriceOracle: Send + Sync {
    /// Returns (bid, ask) for the UP token at `t` seconds into a window.
    fn price_at(&self, window: &WindowMeta, t_secs: u32) -> (Decimal, Decimal);
}

pub struct BlackScholesOracle {
    sigma: f64,
    friction: f64,
    btc_data: Arc<BinanceData>,
}
```

---

## 7. 策略实现

### 策略抽象

```rust
pub enum StakeRule {
    Martingale { base: Decimal, max_step: u8 },
    Fixed { stake: Decimal },
}

pub enum ExitRule {
    HoldToResolution,
    TpOnlyOrHold { tp_price: Decimal },
    TpSlOrHold   { tp_price: Decimal, sl_price: Decimal },
    FixedTime    { seconds: u32 },
}

pub struct StrategyConfig {
    pub name: String,
    pub direction: Direction,
    pub band_min: Decimal, pub band_max: Decimal,
    pub stake: StakeRule,
    pub exit: ExitRule,
}
```

### 6 个变体

```rust
fn strategy_set() -> Vec<StrategyConfig> {
    let common = (Direction::Up, dec!(0.45), dec!(0.55));
    let mart = StakeRule::Martingale { base: dec!(5), max_step: 5 };

    vec![
        StrategyConfig { name: "1_hold_martingale", stake: mart.clone(),
            exit: ExitRule::HoldToResolution, /*...*/ },
        StrategyConfig { name: "2_tp_only_martingale", stake: mart.clone(),
            exit: ExitRule::TpOnlyOrHold { tp_price: dec!(0.75) }, /*...*/ },
        StrategyConfig { name: "3_tp_sl_symmetric", stake: mart.clone(),
            exit: ExitRule::TpSlOrHold { tp_price: dec!(0.55), sl_price: dec!(0.45) }, /*...*/ },
        StrategyConfig { name: "4_tp_sl_asymmetric", stake: mart.clone(),
            exit: ExitRule::TpSlOrHold { tp_price: dec!(0.85), sl_price: dec!(0.45) }, /*...*/ },
        StrategyConfig { name: "5_time_60s_martingale", stake: mart.clone(),
            exit: ExitRule::FixedTime { seconds: 60 }, /*...*/ },
        StrategyConfig { name: "6_fixed_stake_baseline",
            stake: StakeRule::Fixed { stake: dec!(5) },
            exit: ExitRule::HoldToResolution, /*...*/ },
    ]
}
```

### 单窗口仿真

逐秒走窗口，检查退出规则：

```rust
pub fn simulate_window(
    window: &WindowMeta,
    config: &StrategyConfig,
    oracle: &dyn TokenPriceOracle,
    stake: Decimal,
) -> WindowOutcome {
    let (_, ask) = oracle.price_at(window, 0);
    if ask < config.band_min || ask > config.band_max {
        return WindowOutcome::Skipped {
            reason: SkipReason::PriceOutsideBand { ask }
        };
    }

    let shares = (stake / ask).floor();
    if shares < dec!(5) {
        return WindowOutcome::Skipped { reason: SkipReason::FillOrKillFailed };
    }
    let dollars_spent = shares * ask;

    for t in 1..=300u32 {
        let (bid, _) = oracle.price_at(window, t);
        let proceeds = shares * bid;

        match &config.exit {
            ExitRule::HoldToResolution => {}
            ExitRule::TpOnlyOrHold { tp_price } => {
                if bid >= *tp_price {
                    return WindowOutcome::Won { proceeds_usd: proceeds };
                }
            }
            ExitRule::TpSlOrHold { tp_price, sl_price } => {
                if bid >= *tp_price {
                    return WindowOutcome::Won { proceeds_usd: proceeds };
                }
                if bid <= *sl_price {
                    return if proceeds > dollars_spent {
                        WindowOutcome::Won { proceeds_usd: proceeds }
                    } else {
                        WindowOutcome::Lost { spent_usd: dollars_spent - proceeds }
                    };
                }
            }
            ExitRule::FixedTime { seconds } if t >= *seconds => {
                return if proceeds > dollars_spent {
                    WindowOutcome::Won { proceeds_usd: proceeds }
                } else {
                    WindowOutcome::Lost { spent_usd: dollars_spent - proceeds }
                };
            }
            _ => {}
        }
    }

    // 持有到末：用 gamma 的 winner
    if window.winner == config.direction {
        WindowOutcome::Won { proceeds_usd: shares * dec!(0.99) }
    } else {
        WindowOutcome::Lost { spent_usd: dollars_spent }
    }
}
```

### 多窗口 runner

```rust
pub struct WindowResult {
    pub window_ts: i64,
    pub stake: Decimal,
    pub outcome: WindowOutcome,
    pub ladder_step_before: u8,
    pub ladder_step_after: u8,
    pub ladder_pnl_after: Decimal,
}

pub struct StrategyRunResult {
    pub name: String,
    pub windows: Vec<WindowResult>,
    pub cap_resets: u32,
    pub final_pnl: Decimal,
}

pub fn run_strategy(
    strategy: &StrategyConfig,
    windows: &[WindowMeta],
    oracle: &dyn TokenPriceOracle,
) -> StrategyRunResult {
    let mut ladder = LadderState::new(strategy.direction, dec!(5), 5, /* now */);
    let mut total_pnl = Decimal::ZERO;
    let mut cap_resets = 0;
    let mut history = Vec::with_capacity(windows.len());

    for window in windows {
        if ladder.is_stopped() {
            cap_resets += 1;
            ladder = LadderState::new(strategy.direction, dec!(5), 5, /* now */);
        }

        let stake = match &strategy.stake {
            StakeRule::Martingale { .. } => ladder.current_bet_usd(),
            StakeRule::Fixed { stake } => *stake,
        };

        let step_before = ladder.current_step;
        let outcome = simulate_window(window, strategy, oracle, stake);
        ladder = apply_outcome(&ladder, &outcome, /* now */);
        total_pnl = ladder.realized_pnl_usd;

        history.push(WindowResult {
            window_ts: window.ts,
            stake,
            outcome,
            ladder_step_before: step_before,
            ladder_step_after: ladder.current_step,
            ladder_pnl_after: total_pnl,
        });
    }

    StrategyRunResult {
        name: strategy.name.clone(),
        windows: history,
        cap_resets,
        final_pnl: total_pnl,
    }
}
```

**CapReached 后重置 ladder 继续跑**——模拟"用户手动重启"，回测整个时段。`cap_resets` 计数器是关键指标。

---

## 8. Stats + HTML 报告

### Stats 引擎

```rust
pub struct StrategyStats {
    pub name: String,

    // 总览
    pub total_windows: u32,
    pub windows_won: u32,
    pub windows_lost: u32,
    pub windows_skipped: u32,
    pub win_rate: f64,

    // 资金
    pub total_pnl_usd: Decimal,
    pub ev_per_round: Decimal,           // total_pnl / total_windows
    pub ev_per_active_round: Decimal,    // total_pnl / (won + lost)

    // Martingale
    pub cap_resets: u32,
    pub max_consecutive_losses: u32,
    pub max_step_reached: u8,

    // 风险
    pub max_drawdown_usd: Decimal,
    pub max_drawdown_window_ts: i64,
    pub equity_curve: Vec<(i64, Decimal)>,

    // 单轮分布
    pub round_pnls: Vec<Decimal>,
}
```

### HTML 报告布局

单个静态 HTML 文件 ~500KB，自包含 Chart.js（CDN 加载）：

```
┌──────────────────────────────────────────────────────────┐
│ Polymarket BTC 5min Strategy Backtest                    │
│ 2026-04-09 to 2026-05-09  |  8,640 windows  |  σ=$80     │
├──────────────────────────────────────────────────────────┤
│  Summary table（一行一个策略，可点击表头排序）             │
├──────────────────────────────────────────────────────────┤
│  Equity curves（6 条线叠在一张图）                          │
├──────────────────────────────────────────────────────────┤
│  Per-strategy PnL distribution（6 个并列直方图）           │
├──────────────────────────────────────────────────────────┤
│  Cap-trigger histogram（仅 #1-#5 显示）                    │
├──────────────────────────────────────────────────────────┤
│  Worst-case event log（前 5 个 cap reset 的明细）          │
└──────────────────────────────────────────────────────────┘
```

### 不允许的交互

- 不允许调参重跑（需要后端）
- 想换参数：CLI 重新跑

### 报告生成代码

```
src/backtest/report.rs
├── render_html(stats, meta) -> String
├── render_summary_table(stats) -> String
├── render_equity_curves_chart(stats) -> String
├── render_pnl_histograms(stats) -> String
└── render_cap_trigger_panel(stats) -> String
```

总约 ~400 行 Rust。

---

## 9. 测试策略

### 单元测试

| 模块 | 测试目标 | 数量 |
|---|---|---|
| `backtest::oracle` | Black-Scholes 公式正确性、σ 估计、对称性、收敛性 | 6 |
| `backtest::exit_rule::simulate_window` | 6 种 ExitRule 在控制场景下的 Win/Lost/Skipped 输出 | 12 |
| `backtest::runner::run_strategy` | Martingale 阶梯演进、CapReached 重置、Fixed stake 不阶梯 | 5 |
| `backtest::stats` | EV / win_rate / drawdown / cap_resets 计算 | 6 |
| `backtest::data::cache` | 缓存读写、过期/部分命中处理 | 3 |
| `backtest::data::binance` | 数据切片、缺失插值 | 3 |
| `backtest::data::gamma_history` | 解码、缺失字段容错 | 3 |
| `backtest::report` | HTML 包含 6 个策略数据、关键 DOM 标记 | 4 |

合计 ~42 个新单元测试。

### 集成测试

`tests/backtest_smoke.rs`（`#[ignore]`，需联网）：
- 拉 1 天真实 gamma + Binance 数据
- 跑全部 6 策略
- 验证 HTML 文件生成且 ≥ 50KB

### 覆盖率门槛

`src/backtest/` ≥ 90%。`data::*` 的 wrapper 部分（HTTP 调用）排除。

---

## 10. 接受标准

**功能**

- [ ] `cargo run --bin poly-backtest -- --start 2026-04-09 --end 2026-05-09 --output /tmp/report.html` 30 分钟内跑通
- [ ] 6 个策略全部执行，stats 表格全部填出
- [ ] HTML 在 Chrome / Firefox 打开看到所有 panel
- [ ] equity curve、pnl 直方图、cap-trigger 列表全部渲染
- [ ] 数据缓存：第二次跑相同时段 < 1 分钟
- [ ] 增量时段：拉过 1 周后再跑 30 天，只补拉缺失的 23 天

**正确性**

- [ ] 策略 #1 (hold-to-resolution Martingale) EV/round ≈ -$0.05（理论值）
- [ ] 策略 #6 (Fixed stake) cap_resets = 0
- [ ] 所有策略 win_rate 在 0.45-0.55 区间（公平硬币）
- [ ] 累计 PnL 曲线中 cap_resets 与跌幅匹配

**测试 / 代码质量**

- [ ] `cargo test --lib backtest` ≥ 42 测试全绿
- [ ] `cargo test --test backtest_smoke -- --ignored` 1 测试通过
- [ ] `src/backtest/` 覆盖率 ≥ 90%
- [ ] 既有 v1.0 / v1.1 / v1.2 测试不退化

**安全 / 隔离**

- [ ] 回测器**不连**任何线上服务（除 Binance + gamma 公共）
- [ ] **不动**现有 Redis 任何 key
- [ ] **不读** `.env` 里的 `POLYMARKET_PRIVATE_KEY`
- [ ] **不依赖** Polygon RPC（不用 alloy）
- [ ] 缓存目录在用户家目录或 `target/`，不污染源码树

**不影响正在运行的 trader/TUI**

- [ ] 实施期间 PID 53896（如还在跑）不被打扰
- [ ] 只用 `cargo build --bin poly-backtest`，不触发 poly-trader.exe 文件锁

---

## 11. 后续路线

回测结果出来后：

1. **某策略 EV 显著正** → v1.5：把 v1.1 trader 改造支持那个策略（新增 ExitRule 字段、扩展 run_window）
2. **所有策略 EV 都为负** → 停掉真钱跑；v1.1 当作完成的工程练习
3. **某策略 EV 接近 0 但 cap_resets 很少** → 低风险但不赚钱，作为"风险可控的 dry-run 玩具"

**Bug A 的命运由这次回测决定**——换策略后并发窗口已不重要，Bug A 自动消失；新策略仍想要 5min 节奏，Bug A 才需要正式修。
