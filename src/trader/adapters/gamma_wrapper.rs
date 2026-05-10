use crate::trader::errors::MarketError;
use crate::trader::market::{decode_event_response, window_slug, MarketDiscovery, WindowMarket};
use async_trait::async_trait;
use reqwest::Client;

pub struct GammaMarketDiscovery {
    client: Client,
    base_url: String,
}

impl GammaMarketDiscovery {
    pub fn new(base_url: String) -> Self {
        Self {
            client: Client::builder().timeout(std::time::Duration::from_secs(10)).build().unwrap(),
            base_url,
        }
    }
}

#[async_trait]
impl MarketDiscovery for GammaMarketDiscovery {
    async fn find_window(&self, window_ts: i64) -> Result<WindowMarket, MarketError> {
        let slug = window_slug(window_ts, 5);
        // Cache-busting: gamma-api serves cached responses with ~60s TTL,
        // which keeps `closed:false` visible past actual market closure and
        // causes resolver timeouts. Appending a nonce defeats upstream caching
        // without changing response shape.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let url = format!("{}/events?slug={slug}&_t={nonce}", self.base_url);
        let resp = self.client.get(&url)
            .header("Cache-Control", "no-cache")
            .send().await
            .map_err(|e| MarketError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            if resp.status().as_u16() == 404 {
                return Err(MarketError::NotFound { window_ts });
            }
            return Err(MarketError::Network(format!("HTTP {}", resp.status())));
        }
        let body = resp.text().await
            .map_err(|e| MarketError::Network(e.to_string()))?;
        decode_event_response(&body, window_ts)
    }
}
