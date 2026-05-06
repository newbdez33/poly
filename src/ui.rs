use crate::domain::{Balance, HealthLed, RefreshStatus};
use chrono::{DateTime, Utc};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
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
    pub now: DateTime<Utc>,   // injected for deterministic snapshots
}

pub fn render(frame: &mut Frame, state: &UiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    // Main: centered balance
    let balance_text = match &state.balance {
        Some(b) => format!("USDC: ${}", format_decimal(b.usdc)),
        None    => "USDC: --".to_string(),
    };
    let balance = Paragraph::new(balance_text)
        .alignment(Alignment::Center)
        .style(Style::default().add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).title("poly-tui"));
    frame.render_widget(balance, chunks[0]);

    // Status bar
    let status = build_status_line(state);
    frame.render_widget(Paragraph::new(status), chunks[1]);
}

fn format_decimal(d: rust_decimal::Decimal) -> String {
    // 2 decimal places, comma thousands separator (rust_decimal lacks built-in
    // grouping; use a simple manual format).
    let raw = format!("{:.2}", d);
    let (whole, frac) = raw.split_once('.').unwrap_or((raw.as_str(), "00"));
    let mut grouped = String::new();
    for (i, ch) in whole.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 { grouped.push(','); }
        grouped.push(ch);
    }
    let whole_grouped: String = grouped.chars().rev().collect();
    format!("{whole_grouped}.{frac}")
}

fn led_span<'a>(label: &'a str, led: HealthLed) -> Vec<Span<'a>> {
    let dot = match led {
        HealthLed::Green  => Span::styled("●", Style::default().fg(Color::Green)),
        HealthLed::Yellow => Span::styled("●", Style::default().fg(Color::Yellow)),
        HealthLed::Red    => Span::styled("●", Style::default().fg(Color::Red)),
    };
    vec![dot, Span::raw(format!(" {label} "))]
}

fn build_status_line<'a>(state: &'a UiState) -> Line<'a> {
    let mut spans = Vec::new();
    spans.extend(led_span("CLOB", state.clob_health));
    spans.push(Span::raw(" "));
    spans.extend(led_span("Redis", state.redis_health));
    spans.push(Span::raw("  "));
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
    use rust_decimal::Decimal;
    use std::str::FromStr;
    use ratatui::{Terminal, backend::TestBackend};

    fn fixed_now() -> DateTime<Utc> { Utc.timestamp_opt(1_700_000_120, 0).unwrap() }

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

    #[test]
    fn renders_balance_when_present() {
        let state = UiState {
            balance: Some(Balance {
                usdc: Decimal::from_str("1234.56").unwrap(),
                fetched_at: fixed_now(),
            }),
            last_refresh: Some(RefreshStatus::Ok { at: fixed_now() - chrono::Duration::seconds(12) }),
            clob_health: HealthLed::Green,
            redis_health: HealthLed::Green,
            refresh_interval: Duration::from_secs(30),
            now: fixed_now(),
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
        };
        let out = render_to_buffer(&state);
        insta::assert_snapshot!("ui_failure", out);
    }
}
