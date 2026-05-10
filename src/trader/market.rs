use crate::trader::errors::MarketError;
use crate::trader::ladder::Direction;
use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// 5-min window market with both outcome token IDs and best-ask prices.
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
    pub price_to_beat: Option<Decimal>,
}

impl WindowMarket {
    pub fn ask_for(&self, side: Direction) -> Decimal {
        match side {
            Direction::Up => self.up_ask,
            Direction::Down => self.down_ask,
        }
    }
    pub fn token_id_for(&self, side: Direction) -> &str {
        match side {
            Direction::Up => &self.up_token_id,
            Direction::Down => &self.down_token_id,
        }
    }
}

#[async_trait]
pub trait MarketDiscovery: Send + Sync {
    async fn find_window(&self, window_ts: i64) -> Result<WindowMarket, MarketError>;
}

/// Total seconds in a `window_minutes`-long window.
pub fn window_seconds(window_minutes: u32) -> i64 { window_minutes as i64 * 60 }

/// Slug for the BTC up/down market at a given window boundary.
pub fn window_slug(window_ts: i64, window_minutes: u32) -> String {
    format!("btc-updown-{}m-{}", window_minutes, window_ts)
}

/// Floor `now_ts` to the start of its window of length `window_minutes`.
pub fn floor_window(now_ts: i64, window_minutes: u32) -> i64 {
    let secs = window_seconds(window_minutes);
    now_ts - now_ts.rem_euclid(secs)
}

/// Next window boundary strictly after `now_ts`.
pub fn next_window_boundary(now_ts: i64, window_minutes: u32) -> i64 {
    floor_window(now_ts, window_minutes) + window_seconds(window_minutes)
}

// Backward-compat wrappers — internal callers migrate gradually.
pub fn floor_5min(now_ts: i64) -> i64 { floor_window(now_ts, 5) }
pub fn next_5min_boundary(now_ts: i64) -> i64 { next_window_boundary(now_ts, 5) }

/// Pure decoder for a gamma-api event response. Extract the up/down outcomes by
/// matching `outcome` strings ("Up" and "Down" — case-insensitive).
pub fn decode_event_response(json: &str, window_ts: i64) -> Result<WindowMarket, MarketError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| MarketError::Decode(format!("json: {e}")))?;
    let events = v.as_array().ok_or_else(|| MarketError::Decode("expected array".into()))?;
    let event = events.first().ok_or(MarketError::NotFound { window_ts })?;
    let markets = event.get("markets").and_then(|m| m.as_array())
        .ok_or_else(|| MarketError::Decode("missing markets".into()))?;
    let market = markets.first()
        .ok_or_else(|| MarketError::Decode("empty markets".into()))?;

    let slug = market.get("slug").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let closed = market.get("closed").and_then(|c| c.as_bool()).unwrap_or(false);

    // outcomes: array of strings, e.g. ["Up", "Down"]
    let outcomes_raw = market.get("outcomes").and_then(|o| o.as_str())
        .ok_or_else(|| MarketError::Decode("missing outcomes".into()))?;
    let outcomes: Vec<String> = serde_json::from_str(outcomes_raw)
        .map_err(|e| MarketError::Decode(format!("outcomes: {e}")))?;

    let token_ids_raw = market.get("clobTokenIds").and_then(|t| t.as_str())
        .ok_or_else(|| MarketError::Decode("missing clobTokenIds".into()))?;
    let token_ids: Vec<String> = serde_json::from_str(token_ids_raw)
        .map_err(|e| MarketError::Decode(format!("clobTokenIds: {e}")))?;

    let outcome_prices_raw = market.get("outcomePrices").and_then(|p| p.as_str())
        .ok_or_else(|| MarketError::Decode("missing outcomePrices".into()))?;
    let outcome_prices: Vec<String> = serde_json::from_str(outcome_prices_raw)
        .map_err(|e| MarketError::Decode(format!("outcomePrices: {e}")))?;

    if outcomes.len() != 2 || token_ids.len() != 2 || outcome_prices.len() != 2 {
        return Err(MarketError::Decode("expected 2 outcomes".into()));
    }

    let mut up_idx = None;
    let mut down_idx = None;
    for (i, name) in outcomes.iter().enumerate() {
        match name.to_ascii_lowercase().as_str() {
            "up" => up_idx = Some(i),
            "down" => down_idx = Some(i),
            _ => {}
        }
    }
    let up = up_idx.ok_or_else(|| MarketError::Decode("no Up outcome".into()))?;
    let down = down_idx.ok_or_else(|| MarketError::Decode("no Down outcome".into()))?;

    let up_ask = parse_decimal(&outcome_prices[up])?;
    let down_ask = parse_decimal(&outcome_prices[down])?;

    let winner = if closed {
        if up_ask > down_ask { Some(Direction::Up) } else { Some(Direction::Down) }
    } else {
        None
    };

    // Gamma sometimes returns priceToBeat as a JSON number (f64) and sometimes
    // as a stringified decimal — the latter is common for live (un-resolved)
    // windows. Try both before giving up.
    let price_to_beat = event.get("eventMetadata")
        .and_then(|m| m.get("priceToBeat"))
        .and_then(|p| p.as_f64())
        .and_then(|f| rust_decimal::Decimal::from_str_exact(&f.to_string()).ok())
        .or_else(|| {
            event.get("eventMetadata")
                .and_then(|m| m.get("priceToBeat"))
                .and_then(|p| p.as_str())
                .and_then(|s| {
                    use std::str::FromStr;
                    rust_decimal::Decimal::from_str(s).ok()
                })
        });

    Ok(WindowMarket {
        window_ts,
        slug,
        up_token_id: token_ids[up].clone(),
        down_token_id: token_ids[down].clone(),
        up_ask,
        down_ask,
        closed,
        winner,
        price_to_beat,
    })
}

fn parse_decimal(s: &str) -> Result<Decimal, MarketError> {
    use std::str::FromStr;
    Decimal::from_str(s).map_err(|e| MarketError::Decode(format!("price: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn slug_format() {
        assert_eq!(window_slug(1747789200, 5), "btc-updown-5m-1747789200");
    }

    #[test]
    fn floor_5min_aligns() {
        assert_eq!(floor_5min(1747789201), 1747789200);
        assert_eq!(floor_5min(1747789499), 1747789200);
        assert_eq!(floor_5min(1747789500), 1747789500);
    }

    #[test]
    fn next_5min_advances() {
        assert_eq!(next_5min_boundary(1747789200), 1747789500);
        assert_eq!(next_5min_boundary(1747789499), 1747789500);
        assert_eq!(next_5min_boundary(1747789500), 1747789800);
    }

    #[test]
    fn window_seconds_5m() { assert_eq!(window_seconds(5), 300); }
    #[test]
    fn window_seconds_15m() { assert_eq!(window_seconds(15), 900); }
    #[test]
    fn window_seconds_60m() { assert_eq!(window_seconds(60), 3600); }

    #[test]
    fn window_slug_includes_minutes() {
        assert_eq!(window_slug(1747789200, 5), "btc-updown-5m-1747789200");
        assert_eq!(window_slug(1747789200, 15), "btc-updown-15m-1747789200");
        assert_eq!(window_slug(1747789200, 60), "btc-updown-60m-1747789200");
    }

    #[test]
    fn floor_window_5m_matches_legacy() {
        assert_eq!(floor_window(1700000100, 5), 1700000100);
        assert_eq!(floor_window(1700000100, 5), floor_5min(1700000100));
        assert_eq!(floor_window(1700000200, 5), floor_5min(1700000200));
    }

    #[test]
    fn floor_window_15m() {
        // 1700000900 % 900 = 800 → floor = 1700000100
        assert_eq!(floor_window(1700000900, 15), 1700000100);
        // 1700001000 % 900 = 0 → already on boundary
        assert_eq!(floor_window(1700001000, 15), 1700001000);
        // 1700001100 % 900 = 100 → floor = 1700001000
        assert_eq!(floor_window(1700001100, 15), 1700001000);
    }

    #[test]
    fn floor_window_60m() {
        // 1700001500 % 3600 = 2300 → floor = 1699999200
        assert_eq!(floor_window(1700001500, 60), 1699999200);
        // 1700002800 is on a 3600 boundary (1700002800 % 3600 = 0)
        assert_eq!(floor_window(1700002800, 60), 1700002800);
    }

    #[test]
    fn next_window_boundary_5m_matches_legacy() {
        assert_eq!(next_window_boundary(1700000100, 5), next_5min_boundary(1700000100));
        assert_eq!(next_window_boundary(1700000200, 5), 1700000400);
    }

    #[test]
    fn next_window_boundary_15m() {
        assert_eq!(next_window_boundary(1700000200, 15), 1700001000);
    }

    #[test]
    fn decode_open_market() {
        let json = r#"[{
            "id": "evt1",
            "markets": [{
                "slug": "btc-updown-5m-1700000300",
                "closed": false,
                "outcomes": "[\"Up\",\"Down\"]",
                "clobTokenIds": "[\"tok-up-1\",\"tok-down-1\"]",
                "outcomePrices": "[\"0.50\",\"0.50\"]"
            }]
        }]"#;
        let m = decode_event_response(json, 1700000300).unwrap();
        assert_eq!(m.up_token_id, "tok-up-1");
        assert_eq!(m.down_token_id, "tok-down-1");
        assert_eq!(m.up_ask, Decimal::from_str("0.50").unwrap());
        assert!(!m.closed);
        assert!(m.winner.is_none());
    }

    #[test]
    fn decode_closed_market_winner_up() {
        let json = r#"[{"markets":[{
            "slug":"x", "closed":true,
            "outcomes":"[\"Up\",\"Down\"]",
            "clobTokenIds":"[\"a\",\"b\"]",
            "outcomePrices":"[\"1.00\",\"0.00\"]"
        }]}]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.winner, Some(Direction::Up));
    }

    #[test]
    fn decode_closed_market_winner_down() {
        let json = r#"[{"markets":[{
            "slug":"x", "closed":true,
            "outcomes":"[\"Up\",\"Down\"]",
            "clobTokenIds":"[\"a\",\"b\"]",
            "outcomePrices":"[\"0.00\",\"1.00\"]"
        }]}]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.winner, Some(Direction::Down));
    }

    #[test]
    fn decode_outcomes_reversed_order() {
        let json = r#"[{"markets":[{
            "slug":"x", "closed":false,
            "outcomes":"[\"Down\",\"Up\"]",
            "clobTokenIds":"[\"down-id\",\"up-id\"]",
            "outcomePrices":"[\"0.50\",\"0.50\"]"
        }]}]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.up_token_id, "up-id");
        assert_eq!(m.down_token_id, "down-id");
    }

    #[test]
    fn decode_empty_returns_not_found() {
        let json = "[]";
        let err = decode_event_response(json, 42).unwrap_err();
        assert!(matches!(err, MarketError::NotFound { window_ts: 42 }));
    }

    #[test]
    fn decode_malformed_returns_decode_err() {
        let json = "not json at all";
        let err = decode_event_response(json, 0).unwrap_err();
        assert!(matches!(err, MarketError::Decode(_)));
    }

    #[test]
    fn decode_missing_outcomes_field() {
        let json = r#"[{"markets":[{"slug":"x","closed":false}]}]"#;
        let err = decode_event_response(json, 0).unwrap_err();
        assert!(matches!(err, MarketError::Decode(_)));
    }

    #[test]
    fn ask_for_returns_correct_side() {
        let m = WindowMarket {
            window_ts: 0, slug: "x".into(),
            up_token_id: "u".into(), down_token_id: "d".into(),
            up_ask: Decimal::from_str("0.51").unwrap(),
            down_ask: Decimal::from_str("0.49").unwrap(),
            closed: false, winner: None,
            price_to_beat: None,
        };
        assert_eq!(m.ask_for(Direction::Up), Decimal::from_str("0.51").unwrap());
        assert_eq!(m.ask_for(Direction::Down), Decimal::from_str("0.49").unwrap());
        assert_eq!(m.token_id_for(Direction::Up), "u");
        assert_eq!(m.token_id_for(Direction::Down), "d");
    }

    #[test]
    fn decode_extracts_price_to_beat() {
        let json = r#"[{"markets":[{
            "slug":"x", "closed":false,
            "outcomes":"[\"Up\",\"Down\"]",
            "clobTokenIds":"[\"u\",\"d\"]",
            "outcomePrices":"[\"0.50\",\"0.50\"]"
        }],
        "eventMetadata": {"priceToBeat": 80424.78}
        }]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.price_to_beat, Some(Decimal::from_str("80424.78").unwrap()));
    }

    #[test]
    fn decode_extracts_price_to_beat_from_string() {
        // Live/un-resolved windows often stringify priceToBeat. Trader was
        // dropping these → TUI strip showed `--` instead of the live BTC price.
        let json = r#"[{"markets":[{
            "slug":"x", "closed":false,
            "outcomes":"[\"Up\",\"Down\"]",
            "clobTokenIds":"[\"u\",\"d\"]",
            "outcomePrices":"[\"0.50\",\"0.50\"]"
        }],
        "eventMetadata": {"priceToBeat": "80615.42"}
        }]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.price_to_beat, Some(Decimal::from_str("80615.42").unwrap()));
    }

    #[test]
    fn decode_missing_event_metadata_yields_none_price_to_beat() {
        let json = r#"[{"markets":[{
            "slug":"x", "closed":false,
            "outcomes":"[\"Up\",\"Down\"]",
            "clobTokenIds":"[\"u\",\"d\"]",
            "outcomePrices":"[\"0.50\",\"0.50\"]"
        }]}]"#;
        let m = decode_event_response(json, 0).unwrap();
        assert_eq!(m.price_to_beat, None);
    }
}
