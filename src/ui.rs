use crate::app::TraderHealth;
use crate::domain::{Balance, HealthLed, RefreshStatus};
use crate::trader::event::TraderEvent;
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
}

pub fn render(frame: &mut Frame, state: &UiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // balance
            Constraint::Length(1), // trader sub-title
            Constraint::Min(0),    // trader log
            Constraint::Length(1), // status bar
        ])
        .split(area);

    render_balance(frame, chunks[0], state);
    render_trader_subtitle(frame, chunks[1], state);
    render_trader_log(frame, chunks[2], state);
    render_status_bar(frame, chunks[3], state);
}

fn render_balance(frame: &mut Frame, area: Rect, state: &UiState) {
    let balance_text = match &state.balance {
        Some(b) => format!("USDC: ${}", format_decimal(b.usdc)),
        None => "USDC: --".to_string(),
    };
    let balance = Paragraph::new(balance_text)
        .alignment(Alignment::Center)
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("poly-tui"));
    frame.render_widget(balance, area);
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
        }
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
}
