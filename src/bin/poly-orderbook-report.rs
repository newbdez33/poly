//! poly-orderbook-report — standalone HTML dashboard generator that analyzes
//! the orderbook snapshots recorded by `poly-orderbook-recorder` together with
//! the cached trade history used by the backtester.
//!
//! Read-only: opens the SQLite DB with SQLITE_OPEN_READ_ONLY so the recorder
//! can keep writing while this runs.
//!
//! Three analyses:
//!   1. Volatility distribution of best_ask per (window, outcome).
//!   2. best_ask price distribution split by window phase
//!      (active 0-240s, late-active 240-300s, resolution 300+s).
//!   3. Maker fill-volume estimate per window using cached trades.

use anyhow::{Context, Result};
use clap::Parser;
use poly_tui::backtest::data::trades::{Trade, TradeSide};
use rusqlite::{Connection, OpenFlags};
use rust_decimal::prelude::ToPrimitive;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "poly-orderbook-report",
    about = "Generate self-contained HTML dashboard from orderbook snapshots + trade cache"
)]
struct Args {
    /// SQLite DB path. Default: ~/.poly-orderbook/recorder.db
    #[arg(long)]
    db_path: Option<PathBuf>,

    /// Output HTML path. Default: report-orderbook.html
    #[arg(long, default_value = "report-orderbook.html")]
    output: PathBuf,

    /// Trade cache root. Default: ~/.poly-backtest-cache
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Limit the per-window fill chart to the last N windows.
    #[arg(long, default_value_t = 20)]
    fill_chart_windows: usize,
}

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Snapshot {
    ts: i64,
    window_ts: i64,
    outcome: String,
    best_bid: Option<f64>,
    best_ask: Option<f64>,
}

#[derive(Debug, Default, Clone)]
struct WindowOutcomeStats {
    /// (window_ts, outcome) volatility of best_ask across whole window.
    n: usize,
    sum: f64,
    sum_sq: f64,
}
impl WindowOutcomeStats {
    fn push(&mut self, x: f64) {
        self.n += 1;
        self.sum += x;
        self.sum_sq += x * x;
    }
    fn std_dev(&self) -> Option<f64> {
        if self.n < 2 { return None; }
        let n = self.n as f64;
        let mean = self.sum / n;
        let var = (self.sum_sq / n) - (mean * mean);
        Some(var.max(0.0).sqrt())
    }
}

#[derive(Default)]
struct PhaseBuckets {
    /// 20 buckets, $0.00-0.05 ... $0.95-1.00
    counts: [u64; 20],
    total: u64,
}
impl PhaseBuckets {
    fn push_ask(&mut self, ask: f64) {
        let idx = ((ask * 20.0).floor() as i64).clamp(0, 19) as usize;
        self.counts[idx] += 1;
        self.total += 1;
    }
    fn pct(&self) -> Vec<f64> {
        if self.total == 0 { return vec![0.0; 20]; }
        self.counts.iter().map(|&c| 100.0 * c as f64 / self.total as f64).collect()
    }
}

// ---------------------------------------------------------------------------
// DB load
// ---------------------------------------------------------------------------

fn default_db_path() -> PathBuf {
    let mut p = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push(".poly-orderbook");
    p.push("recorder.db");
    p
}

fn default_cache_dir() -> PathBuf {
    let mut p = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push(".poly-backtest-cache");
    p
}

fn load_snapshots(db_path: &PathBuf) -> Result<Vec<Snapshot>> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).with_context(|| format!("open read-only {}", db_path.display()))?;
    let mut stmt = conn.prepare(
        "SELECT ts, window_ts, outcome, best_bid, best_ask
         FROM orderbook_snapshots
         ORDER BY window_ts, ts"
    )?;
    let rows = stmt.query_map([], |r| Ok(Snapshot {
        ts: r.get(0)?,
        window_ts: r.get(1)?,
        outcome: r.get(2)?,
        best_bid: r.get(3)?,
        best_ask: r.get(4)?,
    }))?;
    let mut out = Vec::new();
    for row in rows { out.push(row?); }
    Ok(out)
}

fn load_trades(path: &PathBuf) -> Result<Vec<Trade>> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

// ---------------------------------------------------------------------------
// Analyses
// ---------------------------------------------------------------------------

/// Volatility bucket edges (std-dev in dollars).
const VOL_EDGES: &[f64] = &[0.0, 0.02, 0.05, 0.10, 0.20, f64::INFINITY];
const VOL_LABELS: &[&str] = &["$0.00-0.02", "$0.02-0.05", "$0.05-0.10", "$0.10-0.20", "$0.20+"];

fn vol_bucket(sd: f64) -> usize {
    for i in 0..VOL_LABELS.len() {
        if sd >= VOL_EDGES[i] && sd < VOL_EDGES[i + 1] { return i; }
    }
    VOL_LABELS.len() - 1
}

#[derive(Default)]
struct VolDist {
    up: [u64; 5],
    down: [u64; 5],
}

fn compute_volatility(snaps: &[Snapshot]) -> VolDist {
    let mut by_key: BTreeMap<(i64, String), WindowOutcomeStats> = BTreeMap::new();
    for s in snaps {
        if let Some(a) = s.best_ask {
            by_key.entry((s.window_ts, s.outcome.clone()))
                .or_default()
                .push(a);
        }
    }
    let mut dist = VolDist::default();
    for ((_, outcome), stats) in &by_key {
        if let Some(sd) = stats.std_dev() {
            let b = vol_bucket(sd);
            if outcome == "Up" { dist.up[b] += 1; } else { dist.down[b] += 1; }
        }
    }
    dist
}

#[derive(Default)]
struct PhaseDist {
    active: PhaseBuckets,
    late: PhaseBuckets,
    resolution: PhaseBuckets,
}

fn compute_phase_distribution(snaps: &[Snapshot]) -> PhaseDist {
    let mut d = PhaseDist::default();
    for s in snaps {
        let Some(ask) = s.best_ask else { continue };
        let progress = s.ts - s.window_ts;
        if progress < 0 { continue; }
        if progress < 240 {
            d.active.push_ask(ask);
        } else if progress < 300 {
            d.late.push_ask(ask);
        } else {
            d.resolution.push_ask(ask);
        }
    }
    d
}

/// Per-window summary used by the fill-volume chart + aggregate cards.
#[derive(Debug, Clone)]
struct WindowAnalysis {
    window_ts: i64,
    /// Did the active-phase ask reach $0.01 (best_ask <= 0.011)?
    ask_hit_1c: bool,
    /// Did the active-phase bid reach $0.03 (best_bid >= 0.029)?
    bid_hit_3c: bool,
    /// Sum of sell-side volume at price <= $0.011 in active phase (filled-as-maker-bid proxy).
    sell_vol_1c_active: f64,
    /// Sum of buy-side volume at $0.02..=$0.05 in active phase
    /// (filled-as-maker-ask proxy after the bounce).
    buy_vol_2to5c_active: f64,
}

fn compute_window_analyses(
    snaps: &[Snapshot],
    cache_dir: &PathBuf,
) -> Vec<WindowAnalysis> {
    // Group snapshots by window to detect ask-hit / bid-hit in active phase.
    let mut by_window: BTreeMap<i64, Vec<&Snapshot>> = BTreeMap::new();
    for s in snaps { by_window.entry(s.window_ts).or_default().push(s); }

    let trades_dir = cache_dir.join("trades");
    let mut out: Vec<WindowAnalysis> = Vec::new();

    for (window_ts, win_snaps) in by_window {
        let mut ask_hit_1c = false;
        let mut bid_hit_3c = false;
        for s in &win_snaps {
            let progress = s.ts - window_ts;
            if !(0..240).contains(&progress) { continue; }
            if let Some(a) = s.best_ask { if a <= 0.011 { ask_hit_1c = true; } }
            if let Some(b) = s.best_bid { if b >= 0.029 { bid_hit_3c = true; } }
        }

        // Cross-reference trades for fill-volume estimate.
        let mut sell_vol_1c = 0.0;
        let mut buy_vol_2to5c = 0.0;
        let trade_path = trades_dir.join(format!("{}.json", window_ts));
        if let Ok(trades) = load_trades(&trade_path) {
            for t in &trades {
                let progress = t.timestamp - window_ts;
                if !(0..240).contains(&progress) { continue; }
                let price = t.price.to_f64().unwrap_or(0.0);
                let size = t.size.to_f64().unwrap_or(0.0);
                match t.side {
                    TradeSide::Sell if price <= 0.011 => sell_vol_1c += size,
                    TradeSide::Buy if price >= 0.019 && price <= 0.051 => {
                        buy_vol_2to5c += size;
                    }
                    _ => {}
                }
                // Note: t.outcome is recorded per-trade but we aggregate both
                // sides here — the proxy is "any token in this window".
            }
        }

        out.push(WindowAnalysis {
            window_ts,
            ask_hit_1c,
            bid_hit_3c,
            sell_vol_1c_active: sell_vol_1c,
            buy_vol_2to5c_active: buy_vol_2to5c,
        });
    }
    out
}

// ---------------------------------------------------------------------------
// HTML rendering
// ---------------------------------------------------------------------------

const STYLES: &str = r#"<style>
:root {--bg:#0d1117;--bg-elev:#161b22;--bg-hover:#1f2937;--border:#30363d;
--text:#e6edf3;--text-dim:#8b949e;--accent:#58a6ff;
--positive:#3fb950;--negative:#f85149;--warning:#d29922;}
*{box-sizing:border-box}
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;background:var(--bg);color:var(--text);margin:0;padding:24px;line-height:1.5;font-size:14px}
.container{max-width:1280px;margin:0 auto}
header{border-bottom:1px solid var(--border);padding-bottom:16px;margin-bottom:24px}
h1{margin:0 0 8px 0;font-size:22px;font-weight:600}
h2{margin:32px 0 12px 0;font-size:16px;font-weight:600;border-bottom:1px solid var(--border);padding-bottom:8px}
.meta{color:var(--text-dim);font-size:13px;display:flex;gap:24px;flex-wrap:wrap}
.meta strong{color:var(--text)}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px;margin:16px 0}
.card{background:var(--bg-elev);border:1px solid var(--border);border-radius:6px;padding:14px 16px}
.card .label{color:var(--text-dim);font-size:12px;text-transform:uppercase;letter-spacing:0.04em}
.card .value{font-size:22px;font-weight:600;font-variant-numeric:tabular-nums;margin-top:4px}
.card .sub{color:var(--text-dim);font-size:12px;margin-top:2px}
.chart-card{background:var(--bg-elev);border:1px solid var(--border);border-radius:6px;padding:16px;margin-bottom:16px}
.chart-container{position:relative;height:360px}
.caption{color:var(--text-dim);font-size:0.9em;margin:-0.5em 0 0.8em 0}
footer{margin-top:48px;padding-top:16px;border-top:1px solid var(--border);color:var(--text-dim);font-size:12px;text-align:center}
</style>"#;

struct AggregateSummary {
    total_windows: usize,
    windows_ask_1c: usize,
    windows_bid_3c: usize,
    windows_both: usize,
    sum_sell_vol_1c: f64,
    sum_buy_vol_2to5c: f64,
}

fn build_summary(analyses: &[WindowAnalysis]) -> AggregateSummary {
    let total_windows = analyses.len();
    let mut ask = 0usize;
    let mut bid = 0usize;
    let mut both = 0usize;
    let mut sv = 0.0f64;
    let mut bv = 0.0f64;
    for a in analyses {
        if a.ask_hit_1c { ask += 1; }
        if a.bid_hit_3c { bid += 1; }
        if a.ask_hit_1c && a.bid_hit_3c { both += 1; }
        sv += a.sell_vol_1c_active;
        bv += a.buy_vol_2to5c_active;
    }
    AggregateSummary {
        total_windows,
        windows_ask_1c: ask,
        windows_bid_3c: bid,
        windows_both: both,
        sum_sell_vol_1c: sv,
        sum_buy_vol_2to5c: bv,
    }
}

fn pct(num: usize, denom: usize) -> f64 {
    if denom == 0 { 0.0 } else { 100.0 * num as f64 / denom as f64 }
}

fn render_html(
    summary: &AggregateSummary,
    vol: &VolDist,
    phase: &PhaseDist,
    analyses: &[WindowAnalysis],
    fill_window_limit: usize,
    db_path: &PathBuf,
) -> String {
    // Volatility chart JSON
    let vol_json = serde_json::json!({
        "labels": VOL_LABELS,
        "datasets": [
            {"label": "Up",   "data": vol.up,   "backgroundColor": "#58a6ff"},
            {"label": "Down", "data": vol.down, "backgroundColor": "#f85149"},
        ],
    });

    // Phase chart JSON — 20 buckets across $0.00-$1.00.
    let phase_labels: Vec<String> = (0..20)
        .map(|i| format!("${:.2}", i as f64 * 0.05))
        .collect();
    let phase_json = serde_json::json!({
        "labels": phase_labels,
        "datasets": [
            {"label": "Active (0-240s)",     "data": phase.active.pct(),     "backgroundColor": "rgba(63,185,80,0.7)"},
            {"label": "Late-active (240-300s)", "data": phase.late.pct(),    "backgroundColor": "rgba(210,153,34,0.7)"},
            {"label": "Resolution (300s+)",  "data": phase.resolution.pct(), "backgroundColor": "rgba(248,81,73,0.7)"},
        ],
    });

    // Fill chart: last N windows
    let take_from = analyses.len().saturating_sub(fill_window_limit);
    let recent: Vec<&WindowAnalysis> = analyses[take_from..].iter().collect();
    let fill_labels: Vec<String> = recent.iter()
        .map(|a| {
            let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(a.window_ts, 0)
                .map(|d| d.format("%m-%d %H:%M").to_string())
                .unwrap_or_else(|| a.window_ts.to_string());
            dt
        })
        .collect();
    let sell_data: Vec<f64> = recent.iter().map(|a| a.sell_vol_1c_active).collect();
    let buy_data: Vec<f64> = recent.iter().map(|a| a.buy_vol_2to5c_active).collect();
    let fill_json = serde_json::json!({
        "labels": fill_labels,
        "datasets": [
            {"label": "Sell-vol at <=$0.01 (would-fill our $0.01 BUY)",
             "data": sell_data, "backgroundColor": "#3fb950"},
            {"label": "Buy-vol at $0.02-$0.05 (would-fill our $0.02 SELL)",
             "data": buy_data,  "backgroundColor": "#d29922"},
        ],
    });

    let generated = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
    let (start_str, end_str) = if let (Some(first), Some(last)) = (analyses.first(), analyses.last()) {
        let fmt = |ts: i64| chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
            .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
            .unwrap_or_default();
        (fmt(first.window_ts), fmt(last.window_ts))
    } else {
        ("-".into(), "-".into())
    };

    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Polymarket Orderbook Report</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js"></script>
{styles}
</head>
<body>
<div class="container">

<header>
  <h1>Polymarket Orderbook Report</h1>
  <div class="meta">
    <span><strong>DB:</strong> {db}</span>
    <span><strong>Range:</strong> {start} → {end}</span>
    <span><strong>Generated:</strong> {generated}</span>
  </div>
</header>

<h2>Summary</h2>
<div class="cards">
  <div class="card"><div class="label">Windows analyzed</div>
    <div class="value">{total_windows}</div></div>
  <div class="card"><div class="label">Active ask hit $0.01</div>
    <div class="value">{ask_1c}</div>
    <div class="sub">{ask_pct:.1}% of windows</div></div>
  <div class="card"><div class="label">Active bid hit $0.03</div>
    <div class="value">{bid_3c}</div>
    <div class="sub">{bid_pct:.1}% of windows</div></div>
  <div class="card"><div class="label">Both in same window</div>
    <div class="value">{both}</div>
    <div class="sub">{both_pct:.1}% — complete scalp opportunity</div></div>
  <div class="card"><div class="label">Σ sell-vol @ $0.01 (active)</div>
    <div class="value">{sell_vol:.0}</div>
    <div class="sub">shares — upper bound on filled BUY</div></div>
  <div class="card"><div class="label">Σ buy-vol @ $0.02-$0.05 (active)</div>
    <div class="value">{buy_vol:.0}</div>
    <div class="sub">shares — upper bound on filled SELL</div></div>
</div>

<h2>Best-ask volatility per window-token (std dev)</h2>
<p class="caption">Per (window, outcome): standard deviation of best_ask across all snapshots in the window. Up vs Down side compared.</p>
<div class="chart-card"><div class="chart-container"><canvas id="vol"></canvas></div></div>

<h2>best_ask distribution by window phase</h2>
<p class="caption">% of snapshots in each phase where best_ask fell into a $0.05 bucket. Shows price concentration at $0.01 / $0.99 during resolution vs active.</p>
<div class="chart-card"><div class="chart-container"><canvas id="phase"></canvas></div></div>

<h2>Maker fill-volume estimate per window (active phase only)</h2>
<p class="caption">Per window, total taker-side volume at our hypothetical maker levels during the 0-240s active phase. Sell-vol at $0.01 estimates BUY fills; Buy-vol at $0.02-$0.05 estimates SELL fills. Last {fill_n} windows shown.</p>
<div class="chart-card"><div class="chart-container"><canvas id="fill"></canvas></div></div>

<footer>poly-orderbook-report — analysis of {total_windows} windows from {db}.</footer>

</div>

<script>
const volData   = {vol_json};
const phaseData = {phase_json};
const fillData  = {fill_json};

new Chart(document.getElementById('vol'), {{
  type: 'bar', data: volData,
  options: {{ responsive: true, maintainAspectRatio: false,
    plugins: {{ legend: {{ position: 'top', labels: {{ color: '#e6edf3' }} }} }},
    scales: {{
      x: {{ grid: {{ display: false }}, ticks: {{ color: '#8b949e' }}, title: {{ display: true, text: 'std-dev of best_ask ($)', color: '#8b949e' }} }},
      y: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e' }}, title: {{ display: true, text: 'window count', color: '#8b949e' }} }}
    }}
  }}
}});

new Chart(document.getElementById('phase'), {{
  type: 'bar', data: phaseData,
  options: {{ responsive: true, maintainAspectRatio: false,
    plugins: {{ legend: {{ position: 'top', labels: {{ color: '#e6edf3' }} }} }},
    scales: {{
      x: {{ grid: {{ display: false }}, ticks: {{ color: '#8b949e', maxRotation: 60, minRotation: 60, font: {{ size: 10 }} }}, title: {{ display: true, text: 'best_ask bucket', color: '#8b949e' }} }},
      y: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e', callback: v => v+'%' }}, title: {{ display: true, text: '% of snapshots in phase', color: '#8b949e' }} }}
    }}
  }}
}});

new Chart(document.getElementById('fill'), {{
  type: 'bar', data: fillData,
  options: {{ responsive: true, maintainAspectRatio: false,
    plugins: {{ legend: {{ position: 'top', labels: {{ color: '#e6edf3' }} }} }},
    scales: {{
      x: {{ grid: {{ display: false }}, ticks: {{ color: '#8b949e', maxRotation: 60, minRotation: 60, font: {{ size: 10 }} }} }},
      y: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e' }}, title: {{ display: true, text: 'shares', color: '#8b949e' }} }}
    }}
  }}
}});
</script>
</body>
</html>"##,
        styles = STYLES,
        db = db_path.display(),
        start = start_str,
        end = end_str,
        generated = generated,
        total_windows = summary.total_windows,
        ask_1c = summary.windows_ask_1c,
        ask_pct = pct(summary.windows_ask_1c, summary.total_windows),
        bid_3c = summary.windows_bid_3c,
        bid_pct = pct(summary.windows_bid_3c, summary.total_windows),
        both = summary.windows_both,
        both_pct = pct(summary.windows_both, summary.total_windows),
        sell_vol = summary.sum_sell_vol_1c,
        buy_vol  = summary.sum_buy_vol_2to5c,
        fill_n = recent.len(),
        vol_json = vol_json,
        phase_json = phase_json,
        fill_json = fill_json,
    )
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let args = Args::parse();
    let db_path = args.db_path.unwrap_or_else(default_db_path);
    let cache_dir = args.cache_dir.unwrap_or_else(default_cache_dir);

    eprintln!("Loading snapshots from {} (read-only)...", db_path.display());
    let snaps = load_snapshots(&db_path)?;
    eprintln!("  loaded {} snapshots", snaps.len());

    let vol = compute_volatility(&snaps);
    let phase = compute_phase_distribution(&snaps);
    let analyses = compute_window_analyses(&snaps, &cache_dir);
    let summary = build_summary(&analyses);

    eprintln!("Summary:");
    eprintln!("  windows analyzed:        {}", summary.total_windows);
    eprintln!("  active ask hit $0.01:    {} ({:.1}%)",
        summary.windows_ask_1c, pct(summary.windows_ask_1c, summary.total_windows));
    eprintln!("  active bid hit $0.03:    {} ({:.1}%)",
        summary.windows_bid_3c, pct(summary.windows_bid_3c, summary.total_windows));
    eprintln!("  both in same window:     {} ({:.1}%)",
        summary.windows_both, pct(summary.windows_both, summary.total_windows));
    eprintln!("  Σ sell-vol @ $0.01:      {:.0} shares", summary.sum_sell_vol_1c);
    eprintln!("  Σ buy-vol @ $0.02-0.05:  {:.0} shares", summary.sum_buy_vol_2to5c);

    let html = render_html(&summary, &vol, &phase, &analyses, args.fill_chart_windows, &db_path);
    std::fs::write(&args.output, html)
        .with_context(|| format!("write {}", args.output.display()))?;
    eprintln!("Wrote {}", args.output.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vol_bucket_assigns_correctly() {
        assert_eq!(vol_bucket(0.00), 0);
        assert_eq!(vol_bucket(0.019), 0);
        assert_eq!(vol_bucket(0.02), 1);
        assert_eq!(vol_bucket(0.07), 2);
        assert_eq!(vol_bucket(0.15), 3);
        assert_eq!(vol_bucket(0.50), 4);
    }

    #[test]
    fn phase_buckets_normalize() {
        let mut p = PhaseBuckets::default();
        p.push_ask(0.01);
        p.push_ask(0.03);
        p.push_ask(0.99);
        let pcts = p.pct();
        // 0.01 -> bucket 0, 0.03 -> bucket 0, 0.99 -> bucket 19
        let sum: f64 = pcts.iter().sum();
        assert!((sum - 100.0).abs() < 1e-6);
        assert!(pcts[0] > 60.0);
        assert!(pcts[19] > 30.0);
    }

    #[test]
    fn window_outcome_stats_std_dev() {
        let mut s = WindowOutcomeStats::default();
        for x in [0.10, 0.12, 0.08, 0.10, 0.10] {
            s.push(x);
        }
        let sd = s.std_dev().unwrap();
        // mean = 0.10; variance = ((0+.04+.04+0+0)*0.01^2)/5  = 0.0008 / 5 -> ~0.00016
        // std-dev ≈ 0.01265
        assert!(sd > 0.01 && sd < 0.02, "sd was {sd}");
    }

    #[test]
    fn compute_volatility_groups_by_outcome() {
        let snaps = vec![
            Snapshot { ts: 100, window_ts: 100, outcome: "Up".into(),
                       best_bid: None, best_ask: Some(0.50) },
            Snapshot { ts: 101, window_ts: 100, outcome: "Up".into(),
                       best_bid: None, best_ask: Some(0.55) },
            Snapshot { ts: 102, window_ts: 100, outcome: "Down".into(),
                       best_bid: None, best_ask: Some(0.50) },
            Snapshot { ts: 103, window_ts: 100, outcome: "Down".into(),
                       best_bid: None, best_ask: Some(0.50) },
        ];
        let dist = compute_volatility(&snaps);
        let total_up: u64 = dist.up.iter().sum();
        let total_down: u64 = dist.down.iter().sum();
        // Up: 2 samples (0.50, 0.55) -> std dev > 0 -> falls in bucket 1 ($0.02-0.05)
        // Down: 2 samples (0.50, 0.50) -> std dev = 0 -> bucket 0
        assert_eq!(total_up, 1);
        assert_eq!(total_down, 1);
        assert_eq!(dist.down[0], 1);
    }
}
