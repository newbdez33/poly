use crate::app::TraderHealth;
use crate::domain::{Balance, HealthLed, RefreshStatus};
use crate::positions::Positions;
use crate::trader::event::TraderEvent;
use crate::tui::market_watch::MarketState;
use chrono::{DateTime, Utc};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct UiState {
    pub balance: Option<Balance>,
    pub last_refresh: Option<RefreshStatus>,
    pub clob_health: HealthLed,
    pub redis_health: HealthLed,
    pub refresh_interval: Duration,
    pub now: DateTime<Utc>, // injected for deterministic snapshots
    pub trader_log: Vec<TraderEvent>,
    pub trader_latest: Option<TraderEvent>,
    pub trader_health: TraderHealth,
    pub market: Option<MarketState>,
    pub positions: Option<Positions>,
}

pub fn render(frame: &mut Frame, state: &UiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // balance
            Constraint::Length(1), // market strip (NEW)
            Constraint::Length(1), // trader sub-title
            Constraint::Min(0),    // trader log
            Constraint::Length(1), // status bar
        ])
        .split(area);

    render_balance(frame, chunks[0], state);
    render_market_strip(frame, chunks[1], state); // NEW
    render_trader_subtitle(frame, chunks[2], state);
    render_trader_log(frame, chunks[3], state);
    render_status_bar(frame, chunks[4], state);
}

fn render_balance(frame: &mut Frame, area: Rect, state: &UiState) {
    let usdc_line = match &state.balance {
        Some(b) => Line::from(format!("USDC: ${}", format_decimal(b.usdc)))
            .alignment(Alignment::Center),
        None => Line::from("USDC: --").alignment(Alignment::Center),
    };

    let positions_line = positions_line(state);

    let balance = Paragraph::new(vec![usdc_line, positions_line])
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("poly-tui"));
    frame.render_widget(balance, area);
}

/// Build the second line of the balance box.
///
/// States:
/// - positions = None  -> "Loading positions..." (dim)
/// - positions = Some(empty) -> "No open positions" (dim)
/// - positions = Some(items) -> one line per item: "Holding: 10 UP @ $0.500  now $4.85 (-3%)"
///
/// When multiple positions, only the first is shown on this line; further
/// positions overflow into additional lines (handled by the multi-line
/// Paragraph in render_balance — extend balance area Constraint::Length if
/// strategy 4 ever produces >1 simultaneous position).
fn positions_line(state: &UiState) -> Line<'static> {
    use rust_decimal::prelude::ToPrimitive;
    let p = match &state.positions {
        None => return Line::from(Span::styled(
            "Loading positions\u{2026}",
            Style::default().fg(Color::DarkGray),
        )).alignment(Alignment::Center),
        Some(p) if p.items.is_empty() => return Line::from(Span::styled(
            "No open positions",
            Style::default().fg(Color::DarkGray),
        )).alignment(Alignment::Center),
        Some(p) => p,
    };
    // Render the first position. (Multi-position rendering deferred — strategy
    // 4 never holds more than one. Spec calls out this case but defers full
    // multi-position layout to v1.7+.)
    let first = &p.items[0];
    let side = match first.side {
        crate::positions::Side::Up => "UP",
        crate::positions::Side::Down => "DOWN",
    };
    let pct = first.pnl_pct();
    let pct_int: i64 = pct.round().to_i64().unwrap_or(0);
    let (sign, color) = if pct_int > 0 {
        ("+", Color::Green)
    } else if pct_int < 0 {
        ("", Color::Red)
    } else {
        ("\u{00b1}", Color::White)
    };
    let pct_str = format!("{sign}{pct_int}%");
    let cost_str = format!("${:.3}", first.avg_price.to_f64().unwrap_or(0.0));
    let value_str = format!("${:.2}", first.value_usd().to_f64().unwrap_or(0.0));

    let spans = vec![
        Span::raw(format!(
            "Holding: {} {} @ {}  now {} ",
            first.shares, side, cost_str, value_str,
        )),
        Span::styled(format!("({pct_str})"), Style::default().fg(color)),
    ];
    Line::from(spans).alignment(Alignment::Center)
}

fn render_trader_subtitle(frame: &mut Frame, area: Rect, state: &UiState) {
    let line = match &state.trader_latest {
        None => Line::from(Span::raw(" Trader  not started — run `poly-trader` ")),
        Some(ev) => {
            let l = &ev.ladder;
            let dir = match l.direction {
                crate::trader::ladder::Direction::Up => "UP",
                crate::trader::ladder::Direction::Down => "DOWN",
            };
            Line::from(format!(
                " Trader  {dir}  ladder={}  P&L: ${} ",
                l.current_step, l.realized_pnl_usd,
            ))
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn render_trader_log(frame: &mut Frame, area: Rect, state: &UiState) {
    let lines: Vec<Line> = state
        .trader_log
        .iter()
        .rev()
        .take(area.height as usize)
        .map(|ev| {
            let ts = ev.ts.format("%H:%M:%S").to_string();
            let kind = format_event_kind(&ev.kind);
            Line::from(format!("{ts}  {kind}"))
        })
        .collect();
    let lines: Vec<Line> = lines.into_iter().rev().collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn format_event_kind(kind: &crate::trader::event::TraderEventKind) -> String {
    use crate::trader::event::TraderEventKind::*;
    match kind {
        SessionStarted => "SessionStarted".into(),
        SessionStopped { reason } => format!("SessionStopped {reason:?}"),
        WindowOpening { slug, .. } => format!("WindowOpening {slug}"),
        EntryDecision { decision } => format!("EntryDecision {decision:?}"),
        OrderPlaced { kind, dollars, .. } => format!("OrderPlaced {kind:?} ${dollars}"),
        OrderFilled {
            fill_price,
            shares,
            dollars,
        } => format!("OrderFilled {shares}sh @ {fill_price}  ${dollars}"),
        OrderRejected { reason } => format!("OrderRejected {reason}"),
        Resolved {
            winner,
            our_side,
            our_outcome,
        } => format!("Resolved winner={winner:?} side={our_side:?} we={our_outcome:?}"),
        ResolutionTimeout => "ResolutionTimeout".into(),
        ExitTriggered { kind, bid } => {
            format!("ExitTriggered {kind:?} bid={bid}")
        }
        SellFilled { proceeds_usd } => format!("SellFilled ${proceeds_usd}"),
        SellRejected { reason } => format!("SellRejected {reason}"),
        LadderUpdated {
            from_step, to_step, ..
        } => format!("LadderUpdated {from_step}->{to_step}"),
        Alert { message } => format!("ALERT {message}"),
    }
}

fn render_status_bar(frame: &mut Frame, area: Rect, state: &UiState) {
    let status = build_status_line(state);
    frame.render_widget(Paragraph::new(status), area);
}

fn format_decimal(d: rust_decimal::Decimal) -> String {
    // 2 decimal places, comma thousands separator (rust_decimal lacks built-in
    // grouping; use a simple manual format).
    let raw = format!("{:.2}", d);
    let (whole, frac) = raw.split_once('.').unwrap_or((raw.as_str(), "00"));
    let mut grouped = String::new();
    for (i, ch) in whole.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let whole_grouped: String = grouped.chars().rev().collect();
    format!("{whole_grouped}.{frac}")
}

fn led_span<'a>(label: &'a str, led: HealthLed) -> Vec<Span<'a>> {
    let dot = match led {
        HealthLed::Green => Span::styled("●", Style::default().fg(Color::Green)),
        HealthLed::Yellow => Span::styled("●", Style::default().fg(Color::Yellow)),
        HealthLed::Red => Span::styled("●", Style::default().fg(Color::Red)),
    };
    vec![dot, Span::raw(format!(" {label} "))]
}

fn trader_health_to_led(h: TraderHealth) -> HealthLed {
    match h {
        TraderHealth::Healthy => HealthLed::Green,
        TraderHealth::Lagging => HealthLed::Yellow,
        TraderHealth::Stale | TraderHealth::Stopped | TraderHealth::NotStarted => HealthLed::Red,
    }
}

fn render_market_strip(frame: &mut Frame, area: Rect, state: &UiState) {
    use rust_decimal::prelude::ToPrimitive;

    let m = match &state.market {
        Some(m) => m,
        None => {
            frame.render_widget(Paragraph::new(" BTC: -- "), area);
            return;
        }
    };

    // Health-driven dimming: if a value is older than its threshold, render
    // it in DarkGray so the user knows the number isn't fresh.
    let to_beat_style = if m.gamma_healthy(state.now) {
        Style::default()
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let current_style = if m.rpc_healthy(state.now) {
        Style::default()
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(" BTC "));

    match (m.price_to_beat, m.current_price) {
        (Some(p), Some(c)) => {
            spans.push(Span::styled(format_usd_int(p), to_beat_style));
            spans.push(Span::raw(" \u{2192} "));
            spans.push(Span::styled(format_usd_int(c), current_style));
            // Decide diff sign + color from the rounded integer, not the raw
            // Decimal. Otherwise a -$0.30 diff rounds to "0" but is still
            // classified as negative — user sees a red "0" with no minus sign,
            // which looks like a bug.
            let raw_diff = c - p;
            let diff_int: i64 = raw_diff.round().to_i64().unwrap_or(0);
            let (sign, color) = if diff_int > 0 {
                ("+", Color::Green)
            } else if diff_int < 0 {
                ("", Color::Red)
            } else {
                ("\u{00b1}", Color::White)
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("{sign}{}", format_usd_int(raw_diff)),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        }
        (None, Some(c)) => {
            spans.push(Span::raw("--"));
            spans.push(Span::raw(" \u{2192} "));
            spans.push(Span::styled(format_usd_int(c), current_style));
            spans.push(Span::raw("  --"));
        }
        (Some(p), None) => {
            spans.push(Span::styled(format_usd_int(p), to_beat_style));
            spans.push(Span::raw(" \u{2192} "));
            spans.push(Span::styled(
                "--",
                Style::default().fg(Color::DarkGray),
            ));
            spans.push(Span::raw("  --"));
        }
        (None, None) => {
            spans.push(Span::raw("--"));
        }
    }

    spans.push(Span::raw("   "));
    let now_ts = state.now.timestamp();
    let secs = m.seconds_to_next_boundary(now_ts);
    // Clock emoji (\u{23f1}) is 2-cell wide on most terminals — pad with two
    // spaces so the timer doesn't visually hug the icon.
    if secs > 0 && secs < 300 {
        spans.push(Span::raw(format!("\u{23f1}  {}:{:02}", secs / 60, secs % 60)));
    } else {
        spans.push(Span::styled(
            "\u{23f1}  rolling\u{2026}",
            Style::default().fg(Color::DarkGray),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn format_usd_int(d: rust_decimal::Decimal) -> String {
    use rust_decimal::prelude::ToPrimitive;
    let n: i64 = d.round().to_i64().unwrap_or(0);
    if n < 0 {
        format!("-{}", group_thousands(&(-n).to_string()))
    } else {
        group_thousands(&n.to_string())
    }
}

fn group_thousands(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn build_status_line<'a>(state: &'a UiState) -> Line<'a> {
    let mut spans = Vec::new();
    spans.extend(led_span("CLOB", state.clob_health));
    spans.push(Span::raw(" "));
    spans.extend(led_span("Redis", state.redis_health));
    spans.push(Span::raw(" "));
    spans.extend(led_span("Trader", trader_health_to_led(state.trader_health)));
    spans.push(Span::raw(" "));
    spans.push(Span::raw(format!("refresh: {}s", state.refresh_interval.as_secs())));
    spans.push(Span::raw("  "));

    let last_str = match &state.last_refresh {
        Some(RefreshStatus::Ok { at }) => {
            let age = state.now.signed_duration_since(*at).num_seconds().max(0);
            format!("last: {age}s ago")
        }
        Some(RefreshStatus::Failed { error, .. }) => format!("last: failed ({error})"),
        None => "last: --".to_string(),
    };
    spans.push(Span::raw(last_str));
    spans.push(Span::raw("    q quit  r refresh"));

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ratatui::{Terminal, backend::TestBackend};
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn fixed_now() -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_120, 0).unwrap()
    }

    fn render_to_buffer(state: &UiState) -> String {
        let backend = TestBackend::new(60, 12);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(f, state)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf.cell((x, y)).unwrap().symbol());
            }
            out.push('\n');
        }
        out
    }

    fn sample_trader_event() -> crate::trader::event::TraderEvent {
        use crate::trader::event::{TraderEvent, TraderEventKind};
        use crate::trader::ladder::{Direction, LadderState};
        use uuid::Uuid;
        TraderEvent {
            ts: fixed_now(),
            session_id: Uuid::nil(),
            kind: TraderEventKind::SessionStarted,
            ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, fixed_now()),
        }
    }

    fn stopped_trader_event() -> crate::trader::event::TraderEvent {
        use crate::trader::event::{TraderEvent, TraderEventKind};
        use crate::trader::ladder::{Direction, LadderState, StopReason};
        use uuid::Uuid;
        TraderEvent {
            ts: fixed_now(),
            session_id: Uuid::nil(),
            kind: TraderEventKind::SessionStopped {
                reason: StopReason::CapReached,
            },
            ladder: LadderState::new(Direction::Up, Decimal::from(5), 5, fixed_now()),
        }
    }

    fn ui_state_with_trader(
        balance: Option<Balance>,
        latest: Option<crate::trader::event::TraderEvent>,
        health: TraderHealth,
        log: Vec<crate::trader::event::TraderEvent>,
    ) -> UiState {
        UiState {
            balance,
            last_refresh: None,
            clob_health: HealthLed::Green,
            redis_health: HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now: fixed_now(),
            trader_log: log,
            trader_latest: latest,
            trader_health: health,
            market: None,
            positions: None,
        }
    }

    use crate::tui::market_watch::MarketState;

    fn ui_state_with_market(market: Option<MarketState>) -> UiState {
        UiState {
            balance: Some(Balance {
                usdc: Decimal::from_str("100").unwrap(),
                fetched_at: fixed_now(),
            }),
            last_refresh: None,
            clob_health: HealthLed::Green,
            redis_health: HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now: fixed_now(),
            trader_log: vec![],
            trader_latest: None,
            trader_health: TraderHealth::NotStarted,
            market,
            positions: None,
        }
    }

    fn make_market(price_to_beat: Option<&str>, current: Option<&str>) -> MarketState {
        let mut m = MarketState::empty();
        m.window_ts = Some(fixed_now().timestamp() / 300 * 300);
        m.price_to_beat = price_to_beat.map(|s| Decimal::from_str(s).unwrap());
        m.current_price = current.map(|s| Decimal::from_str(s).unwrap());
        m
    }

    #[test]
    fn renders_market_no_data() {
        let state = ui_state_with_market(None);
        insta::assert_snapshot!("market_no_data", render_to_buffer(&state));
    }

    #[test]
    fn renders_market_full() {
        let state = ui_state_with_market(Some(make_market(Some("80425"), Some("80431"))));
        insta::assert_snapshot!("market_full", render_to_buffer(&state));
    }

    #[test]
    fn renders_market_negative_diff() {
        let state = ui_state_with_market(Some(make_market(Some("80425"), Some("80418"))));
        insta::assert_snapshot!("market_negative_diff", render_to_buffer(&state));
    }

    #[test]
    fn renders_market_only_current() {
        let state = ui_state_with_market(Some(make_market(None, Some("80431"))));
        insta::assert_snapshot!("market_only_current", render_to_buffer(&state));
    }

    #[test]
    fn renders_market_rolling() {
        let mut s = ui_state_with_market(Some(make_market(Some("80425"), Some("80425"))));
        // Set now to a 5-min boundary so seconds_to_next_boundary returns 300,
        // triggering the "rolling..." display path.
        let boundary = fixed_now().timestamp() / 300 * 300;
        s.now = chrono::Utc.timestamp_opt(boundary, 0).unwrap();
        insta::assert_snapshot!("market_rolling", render_to_buffer(&s));
    }

    #[test]
    fn renders_market_diff_rounds_to_zero() {
        // Raw diff = -0.30 → rounds to 0 → expect white "±0", NOT red "0".
        let mut m = MarketState::empty();
        m.window_ts = Some(fixed_now().timestamp() / 300 * 300);
        m.price_to_beat = Some(Decimal::from_str("80425.30").unwrap());
        m.current_price = Some(Decimal::from_str("80425.00").unwrap());
        let state = ui_state_with_market(Some(m));
        insta::assert_snapshot!("market_diff_rounds_to_zero", render_to_buffer(&state));
    }

    #[test]
    fn renders_market_rpc_stale() {
        // last_rpc_ok_at = 60s before fixed_now → rpc_unhealthy → current price
        // renders DarkGray. last_gamma_ok_at fresh → price-to-beat normal.
        let mut m = make_market(Some("80425"), Some("80431"));
        m.last_rpc_ok_at = Some(fixed_now() - chrono::Duration::seconds(60));
        m.last_gamma_ok_at = Some(fixed_now() - chrono::Duration::seconds(30));
        let state = ui_state_with_market(Some(m));
        insta::assert_snapshot!("market_rpc_stale", render_to_buffer(&state));
    }

    #[test]
    fn renders_market_gamma_stale() {
        // last_gamma_ok_at = 10min before fixed_now → gamma_unhealthy → price-
        // to-beat renders DarkGray. RPC fresh → current price normal.
        let mut m = make_market(Some("80425"), Some("80431"));
        m.last_rpc_ok_at = Some(fixed_now() - chrono::Duration::seconds(5));
        m.last_gamma_ok_at = Some(fixed_now() - chrono::Duration::seconds(10 * 60));
        let state = ui_state_with_market(Some(m));
        insta::assert_snapshot!("market_gamma_stale", render_to_buffer(&state));
    }

    #[test]
    fn renders_balance_when_present() {
        let state = UiState {
            balance: Some(Balance {
                usdc: Decimal::from_str("1234.56").unwrap(),
                fetched_at: fixed_now(),
            }),
            last_refresh: Some(RefreshStatus::Ok {
                at: fixed_now() - chrono::Duration::seconds(12),
            }),
            clob_health: HealthLed::Green,
            redis_health: HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now: fixed_now(),
            trader_log: vec![],
            trader_latest: None,
            trader_health: TraderHealth::NotStarted,
            market: None,
            positions: None,
        };
        let out = render_to_buffer(&state);
        insta::assert_snapshot!("ui_with_balance", out);
    }

    #[test]
    fn renders_dashes_when_no_balance() {
        let state = UiState {
            balance: None,
            last_refresh: None,
            clob_health: HealthLed::Red,
            redis_health: HealthLed::Red,
            refresh_interval: Duration::from_secs(30),
            now: fixed_now(),
            trader_log: vec![],
            trader_latest: None,
            trader_health: TraderHealth::NotStarted,
            market: None,
            positions: None,
        };
        let out = render_to_buffer(&state);
        insta::assert_snapshot!("ui_no_balance", out);
    }

    #[test]
    fn renders_failure_status() {
        let state = UiState {
            balance: Some(Balance {
                usdc: Decimal::from_str("100").unwrap(),
                fetched_at: fixed_now() - chrono::Duration::seconds(120),
            }),
            last_refresh: Some(RefreshStatus::Failed {
                at: fixed_now() - chrono::Duration::seconds(2),
                error: "Network timeout".into(),
            }),
            clob_health: HealthLed::Red,
            redis_health: HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now: fixed_now(),
            trader_log: vec![],
            trader_latest: None,
            trader_health: TraderHealth::NotStarted,
            market: None,
            positions: None,
        };
        let out = render_to_buffer(&state);
        insta::assert_snapshot!("ui_failure", out);
    }

    #[test]
    fn renders_trader_not_started() {
        let state = ui_state_with_trader(
            Some(Balance {
                usdc: Decimal::from(100),
                fetched_at: fixed_now(),
            }),
            None,
            TraderHealth::NotStarted,
            vec![],
        );
        insta::assert_snapshot!("trader_not_started", render_to_buffer(&state));
    }

    #[test]
    fn renders_trader_with_events() {
        let ev = sample_trader_event();
        let state = ui_state_with_trader(
            Some(Balance {
                usdc: Decimal::from(100),
                fetched_at: fixed_now(),
            }),
            Some(ev.clone()),
            TraderHealth::Healthy,
            vec![ev.clone(); 3],
        );
        insta::assert_snapshot!("trader_with_events", render_to_buffer(&state));
    }

    #[test]
    fn renders_trader_stopped() {
        let ev = stopped_trader_event();
        let state = ui_state_with_trader(
            Some(Balance {
                usdc: Decimal::from(100),
                fetched_at: fixed_now(),
            }),
            Some(ev.clone()),
            TraderHealth::Stopped,
            vec![ev],
        );
        insta::assert_snapshot!("trader_stopped", render_to_buffer(&state));
    }

    #[test]
    fn renders_trader_lagging() {
        let ev = sample_trader_event();
        let state = ui_state_with_trader(
            Some(Balance {
                usdc: Decimal::from(100),
                fetched_at: fixed_now(),
            }),
            Some(ev.clone()),
            TraderHealth::Lagging,
            vec![ev],
        );
        insta::assert_snapshot!("trader_lagging", render_to_buffer(&state));
    }

    #[test]
    fn renders_long_log_truncated() {
        let events: Vec<_> = (0..30).map(|_| sample_trader_event()).collect();
        let state = ui_state_with_trader(
            Some(Balance {
                usdc: Decimal::from(100),
                fetched_at: fixed_now(),
            }),
            events.last().cloned(),
            TraderHealth::Healthy,
            events,
        );
        insta::assert_snapshot!("trader_long_log", render_to_buffer(&state));
    }

    fn ui_state_with_positions(positions: Option<crate::positions::Positions>) -> UiState {
        let now = fixed_now();
        UiState {
            balance: Some(Balance {
                usdc: Decimal::from_str("173.69").unwrap(),
                fetched_at: now,
            }),
            last_refresh: None,
            clob_health: HealthLed::Green,
            redis_health: HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now,
            trader_log: vec![],
            trader_latest: None,
            trader_health: TraderHealth::NotStarted,
            market: None,
            positions,
        }
    }

    fn pos_fixture(slug: &str, shares: &str, avg: &str, cur: &str) -> crate::positions::Position {
        use rust_decimal::Decimal;
        crate::positions::Position {
            token_id: "tok-1".into(),
            side: crate::positions::Side::Up,
            market_slug: slug.into(),
            shares: Decimal::from_str(shares).unwrap(),
            avg_price: Decimal::from_str(avg).unwrap(),
            current_price: Decimal::from_str(cur).unwrap(),
        }
    }

    #[test]
    fn renders_balance_no_positions() {
        let s = ui_state_with_positions(None);
        insta::assert_snapshot!("balance_no_positions", render_to_buffer(&s));
    }

    #[test]
    fn renders_balance_loading_positions() {
        // Cold-start: positions = None means "loading"
        // Distinguished from "no open positions" (positions = Some(empty))
        let s = ui_state_with_positions(None);
        let buf = render_to_buffer(&s);
        assert!(buf.contains("Loading"), "buf:\n{buf}");
    }

    #[test]
    fn renders_balance_no_open_positions() {
        let s = ui_state_with_positions(Some(crate::positions::Positions {
            items: vec![],
            fetched_at: fixed_now(),
        }));
        let buf = render_to_buffer(&s);
        assert!(buf.contains("No open"), "buf:\n{buf}");
    }

    #[test]
    fn renders_balance_with_one_losing_position() {
        let s = ui_state_with_positions(Some(crate::positions::Positions {
            items: vec![pos_fixture("btc-updown-5m-1", "10", "0.50", "0.485")],
            fetched_at: fixed_now(),
        }));
        insta::assert_snapshot!("balance_one_losing", render_to_buffer(&s));
    }

    #[test]
    fn renders_balance_with_one_winning_position() {
        let s = ui_state_with_positions(Some(crate::positions::Positions {
            items: vec![pos_fixture("btc-updown-5m-1", "10", "0.50", "0.85")],
            fetched_at: fixed_now(),
        }));
        insta::assert_snapshot!("balance_one_winning", render_to_buffer(&s));
    }

    #[test]
    fn renders_balance_with_two_positions() {
        let s = ui_state_with_positions(Some(crate::positions::Positions {
            items: vec![
                pos_fixture("btc-updown-5m-1", "10", "0.50", "0.485"),
                pos_fixture("btc-updown-5m-2", "20", "0.48", "0.52"),
            ],
            fetched_at: fixed_now(),
        }));
        insta::assert_snapshot!("balance_two_positions", render_to_buffer(&s));
    }
}
