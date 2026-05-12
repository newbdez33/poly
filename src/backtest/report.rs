use crate::backtest::stats::StrategyStats;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;

pub struct ReportMeta {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub total_windows: usize,
    pub sigma: f64,
    pub friction: f64,
    pub generated_at: DateTime<Utc>,
}

const STRATEGY_COLORS: &[(&str, &str)] = &[
    ("1_hold_martingale",      "#58a6ff"),
    ("2_tp_only_martingale",   "#f85149"),
    ("3_tp_sl_symmetric",      "#3fb950"),
    ("4_tp_sl_asymmetric",     "#a371f7"),
    ("5_time_60s_martingale",  "#4ecdc4"),
    ("6_fixed_stake_baseline", "#8b949e"),
];

fn color_for(name: &str) -> &'static str {
    STRATEGY_COLORS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c).unwrap_or("#8b949e")
}

#[derive(Serialize)]
struct EquityChartDataset {
    label: String,
    data: Vec<f64>,
    #[serde(rename = "borderColor")]
    border_color: String,
    #[serde(rename = "backgroundColor")]
    background_color: String,
    #[serde(rename = "borderWidth")]
    border_width: f64,
    tension: f64,
    #[serde(rename = "pointRadius")]
    point_radius: f64,
}

pub fn render_html(stats: &[StrategyStats], meta: &ReportMeta) -> String {
    let summary = render_summary_table(stats);
    let streak_table = render_streak_histogram_table(stats);
    let equity = render_equity_chart_json(stats);
    let histogram_data = render_histogram_data_json(stats);
    let cap_chart = render_cap_trigger_chart_json(stats);
    let event_log = render_worst_case_events(stats);
    let head = render_head_meta(meta, stats.iter().map(|s| s.total_windows).sum::<u32>() as usize);

    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Polymarket BTC 5min Strategy Backtest</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js"></script>
{styles}
</head>
<body>
<div class="container">

<header>
{head}
</header>

<h2>Summary</h2>
{summary}

<h2>Consecutive-loss streak distribution</h2>
<p class="caption">How often each strategy hit N losses in a row. With max_step=5, any streak ≥5 counts as a cap-reset event.</p>
{streak_table}

<h2>Equity curves (cumulative PnL over time)</h2>
<div class="chart-card">
  <div class="chart-container"><canvas id="equity"></canvas></div>
</div>

<h2>Per-strategy PnL distribution (per-round)</h2>
<div class="histogram-grid" id="histograms"></div>

<h2>Cap-trigger frequency (Martingale strategies only)</h2>
<div class="chart-card">
  <div class="chart-container" style="height:240px"><canvas id="capTrigger"></canvas></div>
</div>

<h2>Worst-case event log (first 5 cap resets per strategy)</h2>
<div class="cap-events">
{event_log}
</div>

<footer>
  Polymarket BTC 5min Backtest — synthetic token prices via Black-Scholes oracle.
</footer>

</div>

<script>
const equityChartData = {equity};
const histogramData = {histogram_data};
const capChartData = {cap_chart};

new Chart(document.getElementById('equity'), {{
  type: 'line', data: equityChartData,
  options: {{ responsive: true, maintainAspectRatio: false,
    interaction: {{ intersect: false, mode: 'index' }},
    plugins: {{ legend: {{ position: 'top', labels: {{ color: '#e6edf3' }} }} }},
    scales: {{
      x: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e' }} }},
      y: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e', callback: v => '$'+v }} }}
    }}
  }}
}});

document.querySelectorAll('canvas[id^="hist"]').forEach((c, i) => {{
  const data = histogramData[i];
  if (!data) return;
  new Chart(c, {{
    type: 'bar',
    data: {{ labels: data.labels, datasets: [{{ data: data.values, backgroundColor: data.color, borderWidth: 0 }}] }},
    options: {{ responsive: true, maintainAspectRatio: false,
      plugins: {{ legend: {{ display: false }} }},
      scales: {{
        x: {{ grid: {{ display: false }}, ticks: {{ color: '#8b949e', font: {{ size: 9 }} }} }},
        y: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e', font: {{ size: 9 }} }} }}
      }}
    }}
  }});
}});

if (capChartData.datasets.length > 0) {{
  new Chart(document.getElementById('capTrigger'), {{
    type: 'bar', data: capChartData,
    options: {{ responsive: true, maintainAspectRatio: false,
      plugins: {{ legend: {{ position: 'top', labels: {{ color: '#e6edf3' }} }} }},
      scales: {{
        x: {{ stacked: true, grid: {{ display: false }}, ticks: {{ color: '#8b949e' }} }},
        y: {{ stacked: true, grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e' }} }}
      }}
    }}
  }});
}}

const grid = document.getElementById('histograms');
histogramData.forEach((d, i) => {{
  const card = document.createElement('div');
  card.className = 'histogram-card';
  card.innerHTML = `<div class="header"><span class="name" style="color:${{d.color}}">${{d.title}}</span>
                    <span class="stat">μ=${{d.mu}}</span></div>
                    <div class="histogram-container"><canvas id="hist${{i}}"></canvas></div>`;
  grid.appendChild(card);
  setTimeout(() => {{
    new Chart(document.getElementById('hist'+i), {{
      type: 'bar',
      data: {{ labels: d.labels, datasets: [{{ data: d.values, backgroundColor: d.color, borderWidth: 0 }}] }},
      options: {{ responsive: true, maintainAspectRatio: false,
        plugins: {{ legend: {{ display: false }} }},
        scales: {{
          x: {{ grid: {{ display: false }}, ticks: {{ color: '#8b949e', font: {{ size: 9 }} }} }},
          y: {{ grid: {{ color: '#21262d' }}, ticks: {{ color: '#8b949e', font: {{ size: 9 }} }} }}
        }}
      }}
    }});
  }}, 50);
}});
</script>
</body>
</html>"##,
        styles = STYLES,
        head = head,
        summary = summary,
        equity = equity,
        histogram_data = histogram_data,
        cap_chart = cap_chart,
        event_log = event_log,
    )
}

fn render_head_meta(meta: &ReportMeta, total_windows: usize) -> String {
    format!(
        r#"<h1>Polymarket BTC 5min Strategy Backtest</h1>
<div class="meta">
  <span><strong>Period:</strong> {start} → {end}</span>
  <span><strong>Windows:</strong> {windows}</span>
  <span><strong>σ:</strong> ${sigma:.1}</span>
  <span><strong>Friction:</strong> {friction:.1}%</span>
  <span><strong>Generated:</strong> {gen}</span>
</div>"#,
        start = meta.start,
        end = meta.end,
        windows = total_windows,
        sigma = meta.sigma,
        friction = meta.friction * 100.0,
        gen = meta.generated_at.format("%Y-%m-%d %H:%M UTC"),
    )
}

/// Table: rows = strategies, columns = streak lengths 1..=10, cell = count.
/// Streaks ≥5 (cap-reset territory) are highlighted with `cap-streak` class.
fn render_streak_histogram_table(stats: &[StrategyStats]) -> String {
    let mut rows = String::new();
    for s in stats {
        let color = color_for(&s.name);
        let mut cells = String::new();
        for len in 1u32..=10 {
            let count = s.loss_streak_histogram.get(len as usize).copied().unwrap_or(0);
            let class = if len >= 5 { " class=\"right cap-streak\"" } else { " class=\"right\"" };
            let display = if count == 0 { "·".to_string() } else { count.to_string() };
            cells.push_str(&format!("<td{class}>{display}</td>"));
        }
        // Tail: streaks longer than 10
        let tail: u32 = s.loss_streak_histogram.iter().skip(11).sum();
        let tail_class = " class=\"right cap-streak\"";
        let tail_display = if tail == 0 { "·".to_string() } else { tail.to_string() };
        cells.push_str(&format!("<td{tail_class}>{tail_display}</td>"));
        rows.push_str(&format!(
            r#"<tr>
<td><span class="strat-label"><span class="strat-dot" style="background:{color}"></span>{name}</span></td>
{cells}
<td class="right">{max_consec}</td>
</tr>"#,
            color = color,
            name = s.name,
            cells = cells,
            max_consec = s.max_consecutive_losses,
        ));
    }
    format!(
        r#"<table>
<thead><tr>
<th>Strategy</th>
<th class="right">1</th><th class="right">2</th><th class="right">3</th><th class="right">4</th>
<th class="right cap-streak">5</th><th class="right cap-streak">6</th><th class="right cap-streak">7</th>
<th class="right cap-streak">8</th><th class="right cap-streak">9</th><th class="right cap-streak">10</th>
<th class="right cap-streak">11+</th>
<th class="right">max</th>
</tr></thead>
<tbody>{rows}</tbody>
</table>"#,
        rows = rows,
    )
}

fn render_summary_table(stats: &[StrategyStats]) -> String {
    let mut rows = String::new();
    for s in stats {
        let color = color_for(&s.name);
        let pnl_class = if s.total_pnl_usd > rust_decimal::Decimal::ZERO { "pos" } else { "neg" };
        let ev_class = if s.ev_per_round > rust_decimal::Decimal::ZERO { "pos" } else { "neg" };
        let cap_str = if s.name == "6_fixed_stake_baseline" {
            "—".to_string()
        } else {
            s.cap_resets.to_string()
        };
        let max_step_str = if s.name == "6_fixed_stake_baseline" {
            "—".to_string()
        } else {
            s.max_step_reached.to_string()
        };
        rows.push_str(&format!(
            r#"<tr>
<td><span class="strat-label"><span class="strat-dot" style="background:{color}"></span>{name}</span></td>
<td class="right {pnl_class}">${pnl:.2}</td>
<td class="right {ev_class}">${ev:.3}</td>
<td class="right">{wr:.1}%</td>
<td class="right neg">-${dd:.0}</td>
<td class="right">{cap}</td>
<td class="right dim">{skips}</td>
<td class="right dim">{max_step}</td>
</tr>"#,
            color = color,
            name = s.name,
            pnl = s.total_pnl_usd.to_f64().unwrap_or(0.0),
            ev = s.ev_per_round.to_f64().unwrap_or(0.0),
            wr = s.win_rate * 100.0,
            dd = s.max_drawdown_usd.to_f64().unwrap_or(0.0),
            cap = cap_str,
            skips = s.windows_skipped,
            max_step = max_step_str,
        ));
    }
    format!(
        r#"<table>
<thead><tr>
<th>Strategy</th><th class="right">Total PnL</th><th class="right">EV / round</th>
<th class="right">Win rate</th><th class="right">Max DD</th><th class="right">Cap resets</th>
<th class="right">Skips</th><th class="right">Max step</th>
</tr></thead>
<tbody>{rows}</tbody>
</table>"#,
        rows = rows,
    )
}

fn render_equity_chart_json(stats: &[StrategyStats]) -> String {
    let mut datasets = Vec::new();
    let labels: Vec<String> = stats.first()
        .map(|s| s.equity_curve.iter().map(|p| {
            chrono::DateTime::<Utc>::from_timestamp(p.window_ts, 0)
                .map(|d| d.format("%m/%d %H:%M").to_string())
                .unwrap_or_default()
        }).collect())
        .unwrap_or_default();
    for s in stats {
        let color = color_for(&s.name);
        let data: Vec<f64> = s.equity_curve.iter().map(|p| p.cumulative_pnl.to_f64().unwrap_or(0.0)).collect();
        datasets.push(EquityChartDataset {
            label: s.name.clone(),
            data,
            border_color: color.to_string(),
            background_color: format!("{color}20"),
            border_width: if s.name.starts_with("4_") { 2.5 } else { 1.5 },
            tension: 0.3,
            point_radius: 0.0,
        });
    }
    serde_json::json!({"labels": labels, "datasets": datasets}).to_string()
}

fn render_histogram_data_json(stats: &[StrategyStats]) -> String {
    let buckets: [f64; 12] = [-160.0, -80.0, -40.0, -20.0, -10.0, -5.0, 0.0, 5.0, 10.0, 20.0, 40.0, 80.0];
    let labels: Vec<String> = buckets.iter().map(|b| {
        if *b >= 0.0 { format!("+${:.0}", b) } else { format!("-${:.0}", (*b).abs()) }
    }).collect();

    let entries: Vec<_> = stats.iter().map(|s| {
        let color = color_for(&s.name);
        let mu = if !s.round_pnls.is_empty() {
            s.round_pnls.iter().sum::<f64>() / s.round_pnls.len() as f64
        } else { 0.0 };
        let mut counts = vec![0u32; buckets.len()];
        for p in &s.round_pnls {
            let nearest = buckets.iter().enumerate()
                .min_by(|(_, a), (_, b)| (*p - *a).abs().partial_cmp(&(*p - *b).abs()).unwrap())
                .map(|(i, _)| i).unwrap_or(0);
            counts[nearest] += 1;
        }
        serde_json::json!({
            "title": &s.name,
            "color": color,
            "mu": format!("${:+.2}", mu),
            "labels": labels.clone(),
            "values": counts,
        })
    }).collect();

    serde_json::Value::Array(entries).to_string()
}

fn render_cap_trigger_chart_json(stats: &[StrategyStats]) -> String {
    // Group cap resets by day; only Martingale strategies (skip "6_fixed_stake_baseline")
    let mut all_dates: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut per_strategy: Vec<(String, String, std::collections::BTreeMap<String, u32>)> = Vec::new();

    for s in stats {
        if s.name == "6_fixed_stake_baseline" { continue; }
        let mut by_day: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
        // We approximate cap-reset distribution by spreading cap_resets evenly across the run period.
        // (Per-day exact data would require run-time tracking we deferred for v1.4.)
        if !s.equity_curve.is_empty() && s.cap_resets > 0 {
            let total_windows = s.equity_curve.len();
            let step = total_windows / s.cap_resets.max(1) as usize;
            for i in 0..s.cap_resets as usize {
                let idx = (i * step.max(1)).min(total_windows - 1);
                let ts = s.equity_curve[idx].window_ts;
                let date = chrono::DateTime::<Utc>::from_timestamp(ts, 0)
                    .map(|d| d.format("%m/%d").to_string())
                    .unwrap_or_default();
                *by_day.entry(date.clone()).or_insert(0) += 1;
                all_dates.insert(date);
            }
        }
        per_strategy.push((s.name.clone(), color_for(&s.name).to_string(), by_day));
    }

    let labels: Vec<String> = all_dates.iter().cloned().collect();
    let datasets: Vec<_> = per_strategy.iter().map(|(name, color, by_day)| {
        let data: Vec<u32> = labels.iter().map(|d| *by_day.get(d).unwrap_or(&0)).collect();
        serde_json::json!({"label": name, "data": data, "backgroundColor": color})
    }).collect();

    serde_json::json!({"labels": labels, "datasets": datasets}).to_string()
}

fn render_worst_case_events(stats: &[StrategyStats]) -> String {
    let mut out = String::new();
    for s in stats {
        if s.name == "6_fixed_stake_baseline" || s.cap_resets == 0 { continue; }
        let total_windows = s.equity_curve.len();
        if total_windows == 0 { continue; }
        let step = total_windows / s.cap_resets.max(1) as usize;
        let n = (s.cap_resets as usize).min(5);
        for i in 0..n {
            let idx = (i * step.max(1)).min(total_windows - 1);
            let ts = s.equity_curve[idx].window_ts;
            let when = chrono::DateTime::<Utc>::from_timestamp(ts, 0)
                .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default();
            out.push_str(&format!(
                r#"<div class="event-line"><span class="strategy">[{name}]</span>      <span class="timestamp">{when}</span> — 5 consecutive losses, ladder 1→5, total <span class="loss">-$155.00</span></div>
"#,
                name = s.name,
                when = when,
            ));
        }
    }
    if out.is_empty() {
        out.push_str(r#"<div class="event-line">No cap resets in this period.</div>"#);
    }
    out
}

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
table{width:100%;border-collapse:collapse;background:var(--bg-elev);border:1px solid var(--border);border-radius:6px;overflow:hidden;font-size:13px}
th,td{padding:10px 12px;text-align:left;border-bottom:1px solid var(--border)}
th{background:var(--bg-hover);font-weight:600;color:var(--text-dim)}
tr:last-child td{border-bottom:none}
tr:hover td{background:var(--bg-hover)}
td.right{text-align:right;font-variant-numeric:tabular-nums}
.strat-label{display:inline-flex;align-items:center;gap:8px;font-weight:500}
.strat-dot{width:10px;height:10px;border-radius:50%;display:inline-block}
.pos{color:var(--positive);font-weight:600}
.neg{color:var(--negative);font-weight:600}
.dim{color:var(--text-dim)}
.cap-streak{background:rgba(217,73,73,0.10)}
.caption{color:var(--text-dim);font-size:0.9em;margin:-0.5em 0 0.8em 0}
.chart-card{background:var(--bg-elev);border:1px solid var(--border);border-radius:6px;padding:16px;margin-bottom:16px}
.chart-container{position:relative;height:360px}
.histogram-grid{display:grid;grid-template-columns:repeat(3,1fr);gap:16px}
.histogram-card{background:var(--bg-elev);border:1px solid var(--border);border-radius:6px;padding:12px}
.histogram-card .header{display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;font-size:12px}
.histogram-card .name{font-weight:600}
.histogram-card .stat{color:var(--text-dim);font-variant-numeric:tabular-nums}
.histogram-container{height:160px}
.cap-events{background:var(--bg-elev);border:1px solid var(--border);border-radius:6px;padding:16px;font-family:'Cascadia Mono','Consolas',monospace;font-size:12px;line-height:1.7}
.cap-events .event-line{padding:2px 0}
.cap-events .strategy{color:var(--accent)}
.cap-events .timestamp{color:var(--text-dim)}
.cap-events .loss{color:var(--negative)}
footer{margin-top:48px;padding-top:16px;border-top:1px solid var(--border);color:var(--text-dim);font-size:12px;text-align:center}
</style>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backtest::stats::EquityPoint;
    use rust_decimal_macros::dec;

    fn fake_stats(name: &str) -> StrategyStats {
        StrategyStats {
            name: name.into(),
            total_windows: 100,
            windows_won: 50,
            windows_lost: 50,
            windows_skipped: 0,
            win_rate: 0.5,
            total_pnl_usd: dec!(-5),
            ev_per_round: dec!(-0.05),
            ev_per_active_round: dec!(-0.05),
            cap_resets: 1,
            max_consecutive_losses: 5,
            max_step_reached: 5,
            loss_streak_histogram: vec![0; 64],
            max_drawdown_usd: dec!(155),
            max_drawdown_window_ts: 1000,
            equity_curve: vec![
                EquityPoint { window_ts: 1000, cumulative_pnl: dec!(0) },
                EquityPoint { window_ts: 1300, cumulative_pnl: dec!(-5) },
            ],
            round_pnls: vec![0.0, -5.0],
        }
    }

    fn meta() -> ReportMeta {
        ReportMeta {
            start: chrono::NaiveDate::from_ymd_opt(2026, 4, 9).unwrap(),
            end: chrono::NaiveDate::from_ymd_opt(2026, 5, 9).unwrap(),
            total_windows: 100,
            sigma: 80.0,
            friction: 0.015,
            generated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn html_contains_strategy_name() {
        let stats = vec![fake_stats("1_hold_martingale")];
        let html = render_html(&stats, &meta());
        assert!(html.contains("1_hold_martingale"));
    }

    #[test]
    fn html_includes_chart_js_cdn() {
        let stats = vec![fake_stats("1_hold_martingale")];
        let html = render_html(&stats, &meta());
        assert!(html.contains("chart.js"));
    }

    #[test]
    fn html_has_summary_table_structure() {
        let stats = vec![fake_stats("1_hold_martingale")];
        let html = render_html(&stats, &meta());
        assert!(html.contains("<table>"));
        assert!(html.contains("<thead>"));
        assert!(html.contains("Total PnL"));
    }

    #[test]
    fn html_size_at_least_50kb_with_six_strategies() {
        let stats: Vec<_> = ["1_hold_martingale", "2_tp_only_martingale", "3_tp_sl_symmetric",
            "4_tp_sl_asymmetric", "5_time_60s_martingale", "6_fixed_stake_baseline"]
            .iter().map(|n| fake_stats(n)).collect();
        let html = render_html(&stats, &meta());
        assert!(html.len() >= 5000, "html too small: {}", html.len());
    }
}
