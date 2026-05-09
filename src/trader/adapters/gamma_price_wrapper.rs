use crate::trader::errors::PriceError;
use crate::trader::price::MidwindowPriceFetcher;
use async_trait::async_trait;
use reqwest::Client;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Fetches the current bid for a token from gamma-api's /markets endpoint.
///
/// Polymarket's gamma-api accepts `?clob_token_ids=<id>` and returns a market
/// payload whose `outcomePrices` reflects the latest mid (used here as bid
/// proxy). A cache-busting nonce is appended to defeat upstream caching, same
/// pattern as `GammaMarketDiscovery`.
pub struct GammaPriceFetcher {
    client: Client,
    base_url: String,
}

impl GammaPriceFetcher {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap(),
            base_url,
        }
    }
}

#[async_trait]
impl MidwindowPriceFetcher for GammaPriceFetcher {
    async fn current_bid(&self, token_id: &str) -> Result<Decimal, PriceError> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let url = format!(
            "{}/markets?clob_token_ids={token_id}&_t={nonce}",
            self.base_url
        );
        let resp = self
            .client
            .get(&url)
            .header("Cache-Control", "no-cache")
            .send()
            .await
            .map_err(|e| PriceError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(PriceError::Network(format!("HTTP {}", resp.status())));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| PriceError::Network(e.to_string()))?;
        decode_price_for_token(&body, token_id)
    }
}

/// Pure decoder. Pulls `outcomePrices` (JSON-encoded string array of two
/// stringified decimals) and `clobTokenIds` (likewise) from the first
/// market in the response array. Returns the price corresponding to
/// `token_id`'s position in `clobTokenIds`.
pub fn decode_price_for_token(body: &str, token_id: &str) -> Result<Decimal, PriceError> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| PriceError::Decode(format!("json: {e}")))?;
    let arr = v
        .as_array()
        .ok_or_else(|| PriceError::Decode("expected array".into()))?;
    let market = arr
        .first()
        .ok_or_else(|| PriceError::Decode("empty markets array".into()))?;

    let token_ids_raw = market
        .get("clobTokenIds")
        .and_then(|t| t.as_str())
        .ok_or_else(|| PriceError::Decode("missing clobTokenIds".into()))?;
    let token_ids: Vec<String> = serde_json::from_str(token_ids_raw)
        .map_err(|e| PriceError::Decode(format!("clobTokenIds: {e}")))?;

    let prices_raw = market
        .get("outcomePrices")
        .and_then(|p| p.as_str())
        .ok_or_else(|| PriceError::Decode("missing outcomePrices".into()))?;
    let prices: Vec<String> = serde_json::from_str(prices_raw)
        .map_err(|e| PriceError::Decode(format!("outcomePrices: {e}")))?;

    if token_ids.len() != prices.len() || token_ids.is_empty() {
        return Err(PriceError::Decode("clobTokenIds/outcomePrices size mismatch".into()));
    }
    let idx = token_ids
        .iter()
        .position(|t| t == token_id)
        .ok_or_else(|| PriceError::Decode(format!("token {token_id} not in market")))?;
    Decimal::from_str(&prices[idx])
        .map_err(|e| PriceError::Decode(format!("decimal parse: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn decodes_first_token_price() {
        let body = r#"[{
            "clobTokenIds": "[\"tok-up\",\"tok-down\"]",
            "outcomePrices": "[\"0.86\",\"0.14\"]"
        }]"#;
        let p = decode_price_for_token(body, "tok-up").unwrap();
        assert_eq!(p, Decimal::from_str("0.86").unwrap());
    }

    #[test]
    fn decodes_second_token_price() {
        let body = r#"[{
            "clobTokenIds": "[\"tok-up\",\"tok-down\"]",
            "outcomePrices": "[\"0.86\",\"0.14\"]"
        }]"#;
        let p = decode_price_for_token(body, "tok-down").unwrap();
        assert_eq!(p, Decimal::from_str("0.14").unwrap());
    }

    #[test]
    fn err_when_token_id_missing() {
        let body = r#"[{
            "clobTokenIds": "[\"tok-up\",\"tok-down\"]",
            "outcomePrices": "[\"0.86\",\"0.14\"]"
        }]"#;
        assert!(matches!(
            decode_price_for_token(body, "tok-other"),
            Err(PriceError::Decode(_))
        ));
    }

    #[test]
    fn err_when_outcome_prices_missing() {
        let body = r#"[{"clobTokenIds":"[\"tok-up\"]"}]"#;
        assert!(matches!(
            decode_price_for_token(body, "tok-up"),
            Err(PriceError::Decode(_))
        ));
    }
}
