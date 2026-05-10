use crate::domain::FetchError;
use crate::positions::{Position as DomainPosition, Positions, PositionsFetcher, Side};
use alloy::primitives::Address;
use async_trait::async_trait;
use chrono::Utc;
use polymarket_client_sdk_v2::data::Client as DataClient;
use polymarket_client_sdk_v2::data::types::request::PositionsRequest;
use polymarket_client_sdk_v2::data::types::response::Position as SdkPosition;

pub struct PolymarketPositionsFetcher {
    client: DataClient,
    user: Address,
}

impl PolymarketPositionsFetcher {
    pub fn new(user: Address) -> Self {
        Self { client: DataClient::default(), user }
    }
    pub fn with_host(user: Address, host: &str) -> Result<Self, FetchError> {
        let client = DataClient::new(host)
            .map_err(|e| FetchError::Network(format!("data-api init: {e}")))?;
        Ok(Self { client, user })
    }
}

#[async_trait]
impl PositionsFetcher for PolymarketPositionsFetcher {
    async fn fetch(&self) -> Result<Positions, FetchError> {
        let req = PositionsRequest::builder().user(self.user).build();
        let raw = self.client.positions(&req)
            .await
            .map_err(|e| FetchError::Network(format!("data-api positions: {e}")))?;
        let items = raw.into_iter().filter_map(map_position).collect();
        Ok(Positions { items, fetched_at: Utc::now() })
    }
}

/// Converts an SDK Position into our slim domain Position. Returns None for
/// markets whose outcome name isn't "Up"/"Down" (Polymarket has many markets;
/// only BTC up/down has those outcome names).
pub fn map_position(p: SdkPosition) -> Option<DomainPosition> {
    let side = Side::parse(&p.outcome)?;
    Some(DomainPosition {
        token_id: p.asset.to_string(),
        side,
        market_slug: p.slug,
        shares: p.size,
        avg_price: p.avg_price,
        current_price: p.cur_price,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    /// Build an SdkPosition fixture. The SDK's struct is non_exhaustive but has
    /// a Builder, so we use that. If the builder API changes, only this helper
    /// needs to update.
    fn sdk_fixture(outcome: &str, slug: &str, size: &str, avg: &str, cur: &str) -> SdkPosition {
        // The SDK Position struct is non_exhaustive; constructed via deserializing
        // a JSON fixture matching its camelCase shape.
        let json = format!(r#"{{
            "proxyWallet": "0x0000000000000000000000000000000000000001",
            "asset": "12345",
            "conditionId": "0x0000000000000000000000000000000000000000000000000000000000000001",
            "size": "{size}",
            "avgPrice": "{avg}",
            "initialValue": "0",
            "currentValue": "0",
            "cashPnl": "0",
            "percentPnl": "0",
            "totalBought": "0",
            "realizedPnl": "0",
            "percentRealizedPnl": "0",
            "curPrice": "{cur}",
            "redeemable": false,
            "mergeable": false,
            "title": "test",
            "slug": "{slug}",
            "icon": "",
            "eventSlug": "",
            "eventId": "",
            "outcome": "{outcome}",
            "outcomeIndex": 0,
            "oppositeOutcome": "",
            "oppositeAsset": "0",
            "endDate": "",
            "negativeRisk": false
        }}"#);
        serde_json::from_str(&json).expect("sdk position fixture decodes")
    }

    #[test]
    fn maps_up_outcome() {
        let sdk = sdk_fixture("Up", "btc-updown-5m-1", "10", "0.50", "0.485");
        let p = map_position(sdk).expect("Up should map");
        assert_eq!(p.side, Side::Up);
        assert_eq!(p.market_slug, "btc-updown-5m-1");
        assert_eq!(p.shares, Decimal::from(10));
        assert_eq!(p.avg_price, Decimal::from_str("0.50").unwrap());
        assert_eq!(p.current_price, Decimal::from_str("0.485").unwrap());
    }

    #[test]
    fn maps_down_outcome() {
        let sdk = sdk_fixture("Down", "btc-updown-5m-1", "5", "0.50", "0.50");
        let p = map_position(sdk).expect("Down should map");
        assert_eq!(p.side, Side::Down);
    }

    #[test]
    fn filters_unknown_outcome() {
        let sdk = sdk_fixture("Yes", "presidential-2024", "100", "0.60", "0.58");
        assert!(map_position(sdk).is_none());
    }

    #[test]
    fn fetcher_constructs_with_default_host() {
        let user = Address::from([0u8; 20]);
        let _f = PolymarketPositionsFetcher::new(user);
    }

    #[test]
    fn fetcher_constructs_with_custom_host() {
        let user = Address::from([0u8; 20]);
        let f = PolymarketPositionsFetcher::with_host(user, "https://data-api.polymarket.com");
        assert!(f.is_ok());
    }

    #[test]
    fn fetcher_rejects_invalid_host_url() {
        let user = Address::from([0u8; 20]);
        let f = PolymarketPositionsFetcher::with_host(user, "not a url");
        assert!(matches!(f, Err(FetchError::Network(_))));
    }
}
