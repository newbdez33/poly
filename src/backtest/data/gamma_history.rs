use crate::backtest::data::cache::DiskCache;
use crate::trader::ladder::Direction;
use anyhow::{Context, Result};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

fn decimal_from_f64(f: f64) -> Option<Decimal> {
    // Round-trip via string to avoid f64 precision artifacts in serialized form.
    Decimal::from_str(&f.to_string()).ok().or_else(|| Decimal::from_f64(f))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowMeta {
    pub window_ts: i64,           // 5-min boundary epoch seconds
    pub price_to_beat: Decimal,   // Open BTC price (priceToBeat)
    pub final_price: Option<Decimal>,  // Close BTC (finalPrice), None if window not settled
    pub winner: Option<Direction>,     // Resolved winner; None if window not closed
    /// Hex-prefixed market condition_id ("0x..."). Optional for back-compat
    /// with cached JSON written before v1.7.5 — those deserialize with None
    /// here. The `--oracle real` path skips windows where this is None.
    #[serde(default)]
    pub condition_id: Option<String>,
}

pub struct GammaHistoryFetcher {
    client: reqwest::Client,
    base_url: String,
    cache: DiskCache,
}

impl GammaHistoryFetcher {
    pub fn new(base_url: String, cache: DiskCache) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client builds"),
            base_url,
            cache,
        }
    }

    /// Returns Some(WindowMeta) if window exists and is fully resolved.
    /// Returns None if window doesn't exist (404) OR exists but isn't settled.
    pub async fn fetch(&self, window_ts: i64) -> Result<Option<WindowMeta>> {
        let key = window_ts.to_string();
        if let Ok(cached) = self.cache.read::<Option<WindowMeta>>(&key) {
            return Ok(cached);
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let url = format!("{}/events?slug=btc-updown-5m-{}&_t={}", self.base_url, window_ts, nonce);
        let resp = self.client.get(&url).send().await
            .with_context(|| format!("fetching {url}"))?;
        if resp.status().as_u16() == 404 {
            self.cache.write(&key, &Option::<WindowMeta>::None)?;
            return Ok(None);
        }
        if !resp.status().is_success() {
            anyhow::bail!("HTTP {} from gamma", resp.status());
        }
        let body = resp.text().await?;
        let meta = decode_window_meta(&body, window_ts)?;
        self.cache.write(&key, &meta)?;
        Ok(meta)
    }
}

/// Pure decoder. Returns None if window exists but isn't settled (no winner yet).
pub fn decode_window_meta(json: &str, window_ts: i64) -> Result<Option<WindowMeta>> {
    let v: serde_json::Value = serde_json::from_str(json).context("json")?;
    let events = match v.as_array() {
        Some(a) => a,
        None => anyhow::bail!("expected array"),
    };
    let event = match events.first() {
        Some(e) => e,
        None => return Ok(None), // empty array — window doesn't exist yet
    };

    // priceToBeat from eventMetadata (may be absent for new windows)
    let price_to_beat = event.get("eventMetadata")
        .and_then(|m| m.get("priceToBeat"))
        .and_then(|p| p.as_f64())
        .and_then(decimal_from_f64)
        .or_else(|| {
            // fallback: try parsing from string in eventMetadata
            event.get("eventMetadata")
                .and_then(|m| m.get("priceToBeat"))
                .and_then(|p| p.as_str())
                .and_then(|s| Decimal::from_str(s).ok())
        });
    let price_to_beat = match price_to_beat {
        Some(p) => p,
        None => return Ok(None),  // not yet started; skip
    };

    // finalPrice (only present after close)
    let final_price = event.get("eventMetadata")
        .and_then(|m| m.get("finalPrice"))
        .and_then(|p| p.as_f64())
        .and_then(decimal_from_f64);

    // winner from market.outcomePrices (only present after settle)
    let market = event.get("markets").and_then(|m| m.as_array()).and_then(|a| a.first());
    let winner = if let Some(market) = market {
        let closed = market.get("closed").and_then(|c| c.as_bool()).unwrap_or(false);
        let uma_resolved = market.get("umaResolutionStatus")
            .and_then(|s| s.as_str()).map(|s| s == "resolved").unwrap_or(false);
        if !closed || !uma_resolved {
            None
        } else {
            // outcomes "[Up, Down]" + outcomePrices "[X, Y]" → winner
            let outcomes_raw = market.get("outcomes").and_then(|o| o.as_str()).unwrap_or("");
            let prices_raw = market.get("outcomePrices").and_then(|p| p.as_str()).unwrap_or("");
            let outcomes: Vec<String> = serde_json::from_str(outcomes_raw).unwrap_or_default();
            let prices: Vec<String> = serde_json::from_str(prices_raw).unwrap_or_default();
            if outcomes.len() != 2 || prices.len() != 2 {
                None
            } else {
                let up_idx = outcomes.iter().position(|s| s.to_ascii_lowercase() == "up");
                let down_idx = outcomes.iter().position(|s| s.to_ascii_lowercase() == "down");
                match (up_idx, down_idx) {
                    (Some(u), Some(d)) => {
                        let up_price = Decimal::from_str(&prices[u]).unwrap_or_default();
                        let down_price = Decimal::from_str(&prices[d]).unwrap_or_default();
                        if up_price > down_price {
                            Some(Direction::Up)
                        } else {
                            Some(Direction::Down)
                        }
                    }
                    _ => None,
                }
            }
        }
    } else {
        None
    };

    let condition_id = market.and_then(|m| {
        m.get("conditionId")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
    });

    if winner.is_none() {
        // Window exists but isn't fully settled — skip in backtest
        return Ok(None);
    }

    Ok(Some(WindowMeta {
        window_ts,
        price_to_beat,
        final_price,
        winner,
        condition_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn decode_resolved_up_winner() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78, "finalPrice": 80450.0},
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"resolved",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"1\",\"0\"]"
            }]
        }]"#;
        let m = decode_window_meta(json, 1700000000).unwrap().unwrap();
        assert_eq!(m.winner, Some(Direction::Up));
        assert_eq!(m.price_to_beat, Decimal::from_str("80424.78").unwrap());
        assert_eq!(m.final_price, Some(Decimal::from_str("80450").unwrap()));
    }

    #[test]
    fn decode_resolved_down_winner() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78, "finalPrice": 80300.0},
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"resolved",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"0\",\"1\"]"
            }]
        }]"#;
        let m = decode_window_meta(json, 1700000000).unwrap().unwrap();
        assert_eq!(m.winner, Some(Direction::Down));
    }

    #[test]
    fn decode_open_window_returns_none() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78},
            "markets":[{
                "slug":"x","closed":false,
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"0.5\",\"0.5\"]"
            }]
        }]"#;
        // Window is open but not settled → skip
        assert!(decode_window_meta(json, 0).unwrap().is_none());
    }

    #[test]
    fn decode_closed_but_uma_pending_returns_none() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78, "finalPrice": 80300.0},
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"pending",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"0\",\"1\"]"
            }]
        }]"#;
        assert!(decode_window_meta(json, 0).unwrap().is_none());
    }

    #[test]
    fn decode_empty_array_returns_none() {
        assert!(decode_window_meta("[]", 0).unwrap().is_none());
    }

    #[test]
    fn decode_missing_eventmetadata_returns_none() {
        let json = r#"[{
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"resolved",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"1\",\"0\"]"
            }]
        }]"#;
        assert!(decode_window_meta(json, 0).unwrap().is_none());
    }

    #[test]
    fn decode_extracts_condition_id() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78, "finalPrice": 80450.0},
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"resolved",
                "conditionId":"0x16b6deeed0603035fe1fab25c868f60fc5e7ac5e761dd4a15d34eb897dbbfa49",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"1\",\"0\"]"
            }]
        }]"#;
        let m = decode_window_meta(json, 1700000000).unwrap().unwrap();
        assert_eq!(
            m.condition_id.as_deref(),
            Some("0x16b6deeed0603035fe1fab25c868f60fc5e7ac5e761dd4a15d34eb897dbbfa49")
        );
    }

    #[test]
    fn decode_missing_condition_id_returns_none() {
        let json = r#"[{
            "eventMetadata": {"priceToBeat": 80424.78, "finalPrice": 80450.0},
            "markets":[{
                "slug":"x","closed":true,"umaResolutionStatus":"resolved",
                "outcomes":"[\"Up\",\"Down\"]",
                "clobTokenIds":"[\"u\",\"d\"]",
                "outcomePrices":"[\"1\",\"0\"]"
            }]
        }]"#;
        let m = decode_window_meta(json, 1700000000).unwrap().unwrap();
        assert!(m.condition_id.is_none());
    }

    #[test]
    fn windowmeta_deserializes_legacy_json_without_condition_id() {
        // Real on-disk legacy cache files use lowercase "up"/"down" because
        // `Direction` is `#[serde(rename_all = "lowercase")]`.
        let json = r#"{
            "window_ts": 1700000000,
            "price_to_beat": "80424.78",
            "final_price": "80450",
            "winner": "up"
        }"#;
        let m: WindowMeta = serde_json::from_str(json).unwrap();
        assert!(m.condition_id.is_none());
        assert_eq!(m.window_ts, 1700000000);
    }
}
