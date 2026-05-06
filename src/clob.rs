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
// SDK source (response.rs:461) confirms `balance` is plain USDC Decimal — no
// µUSDC scaling needed. fetch() uses it directly.

use chrono::Utc;
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

        Ok(Balance {
            usdc: resp.balance,
            fetched_at: Utc::now(),
        })
    }
}

