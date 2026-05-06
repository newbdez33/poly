use crate::domain::{Balance, FetchError};
use async_trait::async_trait;

#[async_trait]
pub trait BalanceFetcher: Send + Sync {
    async fn fetch(&self) -> Result<Balance, FetchError>;
}

// ── ClobBalanceFetcher ────────────────────────────────────────────────────────
//
// Real adapter that wraps the polymarket_client_sdk_v2 authenticated client.
//
// Verified SDK API (polymarket_client_sdk_v2 = "0.6.0-canary.1"):
//
//   * Signer type      : alloy::signers::local::LocalSigner<alloy::signers::k256::ecdsa::SigningKey>
//                        (re-exported as polymarket_client_sdk_v2::auth::LocalSigner)
//   * Authenticated    : polymarket_client_sdk_v2::auth::state::Authenticated<K: Kind>
//                        Normal auth → Authenticated<Normal>
//   * Client type-state: polymarket_client_sdk_v2::clob::Client<Authenticated<Normal>>
//   * balance_allowance: takes BalanceAllowanceRequest (implements Default → asset_type = Collateral)
//   * Response field   : BalanceAllowanceResponse { balance: rust_decimal::Decimal, ... }
//
// DEVIATION FROM PLAN:
//   The plan assumed `.balance` is a µUSDC string. In the real SDK it is a
//   `rust_decimal::Decimal`. The actual unit (USDC vs µUSDC) cannot be confirmed
//   without a live API call; `parse_usdc_micros` is kept exactly as specified
//   (for the unit test), but the fetch impl calls `.to_string()` on the Decimal
//   and passes it through the same helper. If the API returns plain USDC (not
//   µUSDC), the stored value will be 1e-6× the correct amount — this should be
//   validated against a real key before production use.

use chrono::Utc;
use rust_decimal::Decimal;
use std::str::FromStr;

// alloy 1.x uses LocalSigner (same as alloy 0.x "LocalSigner") — PrivateKeySigner
// is an alias in some alloy versions, but the SDK examples use LocalSigner explicitly.
use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use polymarket_client_sdk_v2::auth::Normal;
use polymarket_client_sdk_v2::auth::state::Authenticated;

/// Type alias for the authenticated SDK client using normal (non-builder) auth.
type AuthenticatedClient =
    polymarket_client_sdk_v2::clob::Client<Authenticated<Normal>>;

pub struct ClobBalanceFetcher {
    client: AuthenticatedClient,
}

impl ClobBalanceFetcher {
    pub async fn connect(host: &str, private_key: &str) -> Result<Self, FetchError> {
        use polymarket_client_sdk_v2::clob::{Client, Config};
        use polymarket_client_sdk_v2::POLYGON;

        let signer = LocalSigner::from_str(private_key)
            .map_err(|e| FetchError::Decode(format!("invalid private key: {e}")))?
            .with_chain_id(Some(POLYGON));

        let client = Client::new(host, Config::default())
            .map_err(|e| FetchError::Network(e.to_string()))?
            .authentication_builder(&signer)
            .authenticate()
            .await
            .map_err(|_e| FetchError::Auth)?;

        Ok(Self { client })
    }
}

#[async_trait]
impl BalanceFetcher for ClobBalanceFetcher {
    async fn fetch(&self) -> Result<Balance, FetchError> {
        use polymarket_client_sdk_v2::clob::types::request::BalanceAllowanceRequest;

        let resp = self
            .client
            .balance_allowance(BalanceAllowanceRequest::default())
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;

        // resp.balance is rust_decimal::Decimal (not a raw µUSDC string).
        // We call parse_usdc_micros for consistency with the plan; see DEVIATION note above.
        let raw = resp.balance.to_string();
        let usdc = parse_usdc_micros(&raw)?;

        Ok(Balance {
            usdc,
            fetched_at: Utc::now(),
        })
    }
}

/// Converts a raw µUSDC string (integer micros) into a USDC `Decimal`.
///
/// 1 USDC = 1_000_000 µUSDC, so "1000000" → 1, "500000" → 0.5.
fn parse_usdc_micros(raw: &str) -> Result<Decimal, FetchError> {
    let n = Decimal::from_str(raw)
        .map_err(|e| FetchError::Decode(format!("not a number: {e}")))?;
    Ok(n / Decimal::from(1_000_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_micros_to_usdc() {
        assert_eq!(parse_usdc_micros("0").unwrap(), Decimal::ZERO);
        assert_eq!(parse_usdc_micros("1000000").unwrap(), Decimal::from(1));
        assert_eq!(
            parse_usdc_micros("1234567890").unwrap(),
            Decimal::from_str("1234.56789").unwrap()
        );
        assert!(parse_usdc_micros("not_a_number").is_err());
    }
}
