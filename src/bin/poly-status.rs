//! Read current trader state from Redis and render a static HTML status page.
//! Designed for cron-driven generation (e.g. every 5 min) served by nginx on
//! a Tailscale-only listener. Reads:
//!   - poly:prod:trader:ladder    (LadderState snapshot)
//!   - poly:prod:trader:events    (last 30 events via XREVRANGE)
//!   - poly:prod:balance:latest   ({usdc, fetched_at}, optional)

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, Utc};
use fred::interfaces::{ClientLike, KeysInterface, StreamsInterface};
use fred::prelude::{RedisClient, RedisConfig};
use poly_tui::trader::event::{EntryDecision, TraderEvent, TraderEventKind};
use poly_tui::trader::ladder::{LadderState, SkipReason, WindowOutcome};
use rust_decimal::Decimal;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;

const LADDER_KEY: &str = "poly:prod:trader:ladder";
const EVENTS_KEY: &str = "poly:prod:trader:events";
const BALANCE_KEY: &str = "poly:prod:balance:latest";
const EVENT_COUNT: usize = 30;

#[derive(Deserialize)]
struct CachedBalance {
    usdc: Decimal,
    fetched_at: DateTime<Utc>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let output_path: PathBuf = env::args()
        .nth(1)
        .map(PathBuf::from)
        .context("usage: poly-status <output-html-path>")?;
    let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());

    let config = RedisConfig::from_url(&redis_url).context("parse REDIS_URL")?;
    let client = RedisClient::new(config, None, None, None);
    client.init().await.context("redis connect")?;

    let ladder_json: Option<String> = client.get(LADDER_KEY).await.context("get ladder")?;
    let ladder: Option<LadderState> = ladder_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());

    let entries: Vec<fred::types::XReadValue<String, String, String>> = client
        .xrevrange_values(EVENTS_KEY, "+", "-", Some(EVENT_COUNT as u64))
        .await
        .context("xrevrange events")?;
    let events: Vec<TraderEvent> = entries
        .iter()
        .filter_map(|(_id, fields)| fields.get("payload"))
        .filter_map(|p| serde_json::from_str::<TraderEvent>(p).ok())
        .collect();

    let balance_json: Option<String> = client.get(BALANCE_KEY).await.ok().flatten();
    let balance: Option<CachedBalance> = balance_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());

    let _ = client.quit().await;

    let html = render(ladder.as_ref(), &events, balance.as_ref(), Utc::now());
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(&output_path, html).with_context(|| format!("write {}", output_path.display()))?;
    Ok(())
}

fn render(
    ladder: Option<&LadderState>,
    events: &[TraderEvent],
    balance: Option<&CachedBalance>,
    now: DateTime<Utc>,
) -> String {
    let mut out = String::with_capacity(8192);
    out.push_str(HEADER);

    out.push_str("<header><h1>poly-trader status</h1>");
    out.push_str(&format!(
        "<div class='meta'>generated {} · auto-refresh 60s</div>",
        jst(&now).format("%Y-%m-%d %H:%M:%S JST")
    ));
    out.push_str("</header>");

    if let Some(l) = ladder {
        let mode_class = if l.dry_run { "dry" } else { "live" };
        let mode_label = if l.dry_run { "DRY-RUN" } else { "LIVE" };
        let stake_mode = if l.fixed_stake { "fixed-stake" } else { "martingale" };
        out.push_str(&format!(
            "<section class='ladder'>\
             <div class='mode {mode_class}'>{mode_label}</div>\
             <div class='headline'>\
               <div class='pnl {pnl_cls}'>{pnl}</div>\
               <div class='label'>realized PnL</div>\
             </div>\
             <div class='grid'>\
               <div><span class='k'>direction</span><span class='v'>{dir:?}</span></div>\
               <div><span class='k'>step</span><span class='v'>{step}/{max}</span></div>\
               <div><span class='k'>stake</span><span class='v'>{stake_mode} · base {base}</span></div>\
               <div><span class='k'>session start</span><span class='v'>{started}</span></div>\
               <div><span class='k'>won</span><span class='v win'>{won}</span></div>\
               <div><span class='k'>lost</span><span class='v lose'>{lost}</span></div>\
               <div><span class='k'>skipped</span><span class='v'>{skipped}</span></div>\
               <div><span class='k'>win rate</span><span class='v'>{win_rate}</span></div>\
             </div>",
            mode_class = mode_class,
            mode_label = mode_label,
            pnl_cls = pnl_class(l.realized_pnl_usd),
            pnl = format_money(l.realized_pnl_usd),
            dir = l.direction,
            step = l.current_step,
            max = l.max_step,
            stake_mode = stake_mode,
            base = l.base_shares,
            started = jst(&l.session_started_at).format("%Y-%m-%d %H:%M JST"),
            won = l.windows_won,
            lost = l.windows_lost,
            skipped = l.windows_skipped,
            win_rate = win_rate(l.windows_won, l.windows_lost),
        ));

        if let Some(b) = balance {
            out.push_str(&format!(
                "<div class='balance'>wallet ${} <span class='dim'>(fetched {})</span></div>",
                fmt_decimal(b.usdc, 2),
                jst(&b.fetched_at).format("%H:%M:%S JST"),
            ));
        }
        if let Some(stop) = &l.stopped {
            out.push_str(&format!(
                "<div class='stopped'>STOPPED: {:?}</div>",
                stop
            ));
        }
        out.push_str("</section>");
    } else {
        out.push_str("<section class='ladder empty'>no ladder state in Redis</section>");
    }

    out.push_str("<section class='events'><h2>recent events</h2><table>");
    out.push_str("<thead><tr><th>time (JST)</th><th>event</th></tr></thead><tbody>");
    for ev in events {
        out.push_str(&format!(
            "<tr><td class='ts'>{}</td><td>{}</td></tr>",
            jst(&ev.ts).format("%m-%d %H:%M:%S"),
            event_label(&ev.kind),
        ));
    }
    out.push_str("</tbody></table></section>");

    out.push_str(FOOTER);
    out
}

fn event_label(kind: &TraderEventKind) -> String {
    use TraderEventKind::*;
    match kind {
        SessionStarted => "<span class='tag'>session started</span>".into(),
        SessionStopped { reason } => format!("<span class='tag warn'>STOPPED {:?}</span>", reason),
        WindowOpening { window_ts, .. } => {
            format!("<span class='tag dim'>window {window_ts}</span>")
        }
        EntryDecision { decision } => match decision {
            self::EntryDecision::Enter { ask } => {
                format!("<span class='tag enter'>ENTER ask={}</span>", fmt_decimal(*ask, 3))
            }
            self::EntryDecision::SkipBand { ask } => {
                format!("<span class='tag skip'>SKIP band ask={}</span>", fmt_decimal(*ask, 3))
            }
            self::EntryDecision::SkipNotFound => "<span class='tag skip'>SKIP not-found</span>".into(),
        },
        OrderPlaced { kind, dollars, .. } => {
            format!("<span class='tag'>{:?} ${}</span>", kind, fmt_decimal(*dollars, 2))
        }
        OrderFilled { fill_price, shares, dollars } => format!(
            "<span class='tag fill'>filled {sh}sh @{fp} = ${d}</span>",
            sh = fmt_decimal(*shares, 0),
            fp = fmt_decimal(*fill_price, 3),
            d = fmt_decimal(*dollars, 2),
        ),
        OrderRejected { reason } => format!("<span class='tag warn'>order rejected: {}</span>", html_escape(reason)),
        Resolved { winner, our_side, our_outcome } => format!(
            "<span class='tag'>resolved winner={:?} ours={:?} {:?}</span>",
            winner, our_side, our_outcome
        ),
        ResolutionTimeout => "<span class='tag warn'>resolution timeout</span>".into(),
        ExitTriggered { kind, bid } => format!(
            "<span class='tag'>exit {:?} bid={}</span>", kind, fmt_decimal(*bid, 3)
        ),
        SellFilled { proceeds_usd } => format!("<span class='tag fill'>sold ${}</span>", fmt_decimal(*proceeds_usd, 2)),
        SellRejected { reason } => format!("<span class='tag warn'>sell rejected: {}</span>", html_escape(reason)),
        LadderUpdated { from_step, to_step, outcome } => match outcome {
            WindowOutcome::Won { proceeds_usd, cost_usd } => format!(
                "<span class='tag win'>WON +${}</span> <span class='dim'>{}->{}, {}/{}=&gt;{}</span>",
                fmt_decimal(*proceeds_usd - *cost_usd, 2),
                from_step, to_step,
                fmt_decimal(*cost_usd, 2), fmt_decimal(*proceeds_usd, 2),
                fmt_decimal(*proceeds_usd - *cost_usd, 2),
            ),
            WindowOutcome::Lost { spent_usd } => format!(
                "<span class='tag lose'>LOST -${}</span> <span class='dim'>{}->{}</span>",
                fmt_decimal(*spent_usd, 2), from_step, to_step
            ),
            WindowOutcome::Skipped { reason } => format!(
                "<span class='tag skip'>SKIP {}</span>",
                skip_reason_label(reason)
            ),
        },
        Alert { message } => format!("<span class='tag warn'>alert: {}</span>", html_escape(message)),
        BuyLimitPosted { price, .. } => format!("<span class='tag'>buy-limit @{}</span>", fmt_decimal(*price, 3)),
        BuyLimitSwept { from_price, to_price } => format!(
            "<span class='tag dim'>sweep {} -&gt; {}</span>",
            fmt_decimal(*from_price, 3), fmt_decimal(*to_price, 3)
        ),
        TpLimitPosted { price, .. } => format!("<span class='tag'>tp-limit @{}</span>", fmt_decimal(*price, 3)),
        TpLimitFilled { fill_price, shares, partial, .. } => format!(
            "<span class='tag fill'>tp-fill {}{}sh @{}</span>",
            if *partial { "partial " } else { "" },
            fmt_decimal(*shares, 0),
            fmt_decimal(*fill_price, 3),
        ),
    }
}

fn skip_reason_label(r: &SkipReason) -> String {
    match r {
        SkipReason::PriceOutsideBand { ask } => format!("band ask={}", fmt_decimal(*ask, 3)),
        SkipReason::FillOrKillFailed => "FoK/maker-timeout".into(),
        SkipReason::ResolutionTimeout => "resolution-timeout".into(),
        SkipReason::GammaApiUnavailable => "gamma-api".into(),
        SkipReason::MarketNotFound => "market-not-found".into(),
        SkipReason::RsiNeutralFilter { rsi } => format!("RSI={}", fmt_decimal(*rsi, 1)),
        SkipReason::RsiFetchFailed => "rsi-fetch-failed".into(),
    }
}

fn jst(t: &DateTime<Utc>) -> DateTime<FixedOffset> {
    t.with_timezone(&FixedOffset::east_opt(9 * 3600).expect("9h offset is valid"))
}

fn format_money(d: Decimal) -> String {
    let sign = if d.is_sign_negative() { "-" } else { "+" };
    format!("{}${}", sign, fmt_decimal(d.abs(), 2))
}

fn fmt_decimal(d: Decimal, places: u32) -> String {
    format!("{:.*}", places as usize, d)
}

fn pnl_class(d: Decimal) -> &'static str {
    if d.is_sign_negative() {
        "lose"
    } else if d.is_zero() {
        "neutral"
    } else {
        "win"
    }
}

fn win_rate(won: u32, lost: u32) -> String {
    let total = won + lost;
    if total == 0 {
        "—".into()
    } else {
        format!("{:.1}% ({}/{})", (won as f64 / total as f64) * 100.0, won, total)
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

const HEADER: &str = r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta http-equiv="refresh" content="60">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>poly-trader status</title>
<style>
:root { color-scheme: dark; --bg:#0d1117; --fg:#c9d1d9; --dim:#7d8590; --accent:#58a6ff; --win:#3fb950; --lose:#f85149; --neutral:#d29922; }
* { box-sizing: border-box; }
body { margin:0; padding:24px; background:var(--bg); color:var(--fg); font: 14px/1.5 ui-monospace, 'SF Mono', Consolas, monospace; max-width: 920px; margin:0 auto; }
header { padding-bottom:18px; border-bottom:1px solid #21262d; margin-bottom:24px; }
h1 { margin:0; font-size:22px; font-weight:600; }
h2 { font-size:14px; font-weight:600; color:var(--dim); text-transform:uppercase; letter-spacing:0.5px; margin: 32px 0 12px; }
.meta { color:var(--dim); font-size:12px; margin-top:4px; }
section.ladder { background:#161b22; padding:20px; border-radius:8px; border:1px solid #21262d; }
.mode { display:inline-block; padding:2px 8px; border-radius:4px; font-size:11px; font-weight:600; letter-spacing:0.5px; }
.mode.live { background:var(--lose); color:white; }
.mode.dry { background:var(--neutral); color:black; }
.headline { margin: 14px 0 18px; }
.pnl { font-size:36px; font-weight:700; font-variant-numeric: tabular-nums; }
.pnl.win { color:var(--win); }
.pnl.lose { color:var(--lose); }
.pnl.neutral { color:var(--dim); }
.headline .label { color:var(--dim); font-size:11px; text-transform:uppercase; letter-spacing:0.5px; }
.grid { display:grid; grid-template-columns: repeat(2, 1fr); gap:8px 24px; }
.grid > div { display:flex; justify-content:space-between; padding:4px 0; border-bottom:1px dotted #21262d; }
.k { color:var(--dim); }
.v { font-variant-numeric: tabular-nums; }
.v.win { color:var(--win); }
.v.lose { color:var(--lose); }
.balance { margin-top:14px; padding-top:14px; border-top:1px solid #21262d; }
.stopped { margin-top:14px; padding:8px; background:var(--lose); color:white; border-radius:4px; }
.dim { color:var(--dim); }
.empty { color:var(--dim); }
section.events table { width:100%; border-collapse: collapse; font-size:12px; }
section.events th { text-align:left; color:var(--dim); font-weight:500; padding:6px 8px; border-bottom:1px solid #21262d; text-transform:uppercase; letter-spacing:0.5px; font-size:10px; }
section.events td { padding:5px 8px; border-bottom:1px dotted #21262d; font-variant-numeric: tabular-nums; }
section.events td.ts { color:var(--dim); white-space:nowrap; width:120px; }
section.events { overflow-x:auto; }
.tag { display:inline-block; }
.tag.win { color:var(--win); font-weight:600; }
.tag.lose { color:var(--lose); font-weight:600; }
.tag.warn { color:var(--neutral); }
.tag.enter { color:var(--accent); }
.tag.fill { color:var(--accent); }
.tag.skip { color:var(--dim); }
.tag.dim { color:var(--dim); }
footer { color:var(--dim); font-size:11px; margin-top:32px; text-align:center; }

@media (max-width: 640px) {
    body { padding:12px; font-size:13px; }
    h1 { font-size:18px; }
    section.ladder { padding:14px; }
    .pnl { font-size:28px; }
    .grid { grid-template-columns: 1fr; gap:4px; }
    .grid > div { padding:6px 0; }
    h2 { margin: 24px 0 8px; }
    section.events th, section.events td { padding:6px 6px; font-size:11px; }
    section.events td.ts { width:auto; font-size:10px; }
    .balance { font-size:13px; }
}
</style>
</head><body>"#;

const FOOTER: &str = r#"<footer>poly-status · Tailscale-only · refresh every 60s</footer></body></html>"#;
