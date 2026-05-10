// ClobOrderExecutor — real Polymarket SDK wrapper for order execution.
//
// Verified SDK API (polymarket_client_sdk_v2 = "0.6.0-canary.1"):
//
//   Signing sequence (same as ClobBalanceFetcher for auth):
//     1. LocalSigner::from_str(pk)?.with_chain_id(Some(POLYGON))
//     2. Client::new(host, Config::default())?.authentication_builder(&signer)
//           .signature_type(SignatureType::Proxy).authenticate().await?
//
//   Market buy (FOK, USDC amount):
//     let order = client.market_order()
//         .token_id(U256::from_str(token_id)?)
//         .side(Side::Buy)
//         .amount(Amount::usdc(dollars)?)
//         .order_type(OrderType::FOK)
//         .build().await?;
//     let signed = client.sign(&signer, order).await?;
//     let resp = client.post_order(signed).await?;
//
//   Market sell (FAK, shares amount):
//     let order = client.market_order()
//         .token_id(U256::from_str(token_id)?)
//         .side(Side::Sell)
//         .amount(Amount::shares(shares)?)
//         .order_type(OrderType::FAK)
//         .build().await?;
//     let signed = client.sign(&signer, order).await?;
//     let resp = client.post_order(signed).await?;
//
//   PostOrderResponse fields used:
//     - success: bool          — false → FoK rejected or error
//     - error_msg: Option<str> — server-side error text
//     - making_amount: Decimal — for Buy: USDC spent; for Sell: shares sold
//     - taking_amount: Decimal — for Buy: shares received; for Sell: USDC received
//     - status: OrderStatusType
//
// Note: `sign()` requires the original `LocalSigner`, so we store it alongside
// the authenticated client. The signer is Clone so this is safe.

use crate::trader::errors::ExecError;
use crate::trader::executor::{FillResult, OrderExecutor, OrderId, OrderSide};
use async_trait::async_trait;
use rust_decimal::Decimal;
use std::str::FromStr;

use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use polymarket_client_sdk_v2::auth::Normal;
use polymarket_client_sdk_v2::auth::state::Authenticated;
use polymarket_client_sdk_v2::clob::types::{Amount, OrderType, Side};
use polymarket_client_sdk_v2::clob::types::SignatureType;
use polymarket_client_sdk_v2::types::U256;

/// The authenticated SDK client using normal (non-builder) auth.
type AuthClient = polymarket_client_sdk_v2::clob::Client<Authenticated<Normal>>;

/// The concrete signer type used throughout: LocalSigner wrapping secp256k1.
type K256Signer = LocalSigner<alloy::signers::k256::ecdsa::SigningKey>;

pub struct ClobOrderExecutor {
    client: AuthClient,
    /// The signer is stored so we can call `client.sign(&signer, order)` later.
    signer: K256Signer,
}

impl ClobOrderExecutor {
    /// Expose the inner SDK client (Arc-wrapped) for OrderEventStream polling.
    /// The executor already owns one auth'd client; sharing it keeps a single
    /// session vs opening a parallel auth flow.
    pub fn inner_client(&self) -> std::sync::Arc<AuthClient> {
        std::sync::Arc::new(self.client.clone())
    }

    /// Authenticates with the Polymarket CLOB and returns a ready executor.
    ///
    /// `host`        — e.g. `"https://clob.polymarket.com"`
    /// `private_key` — hex-encoded EOA private key (no `0x` prefix required)
    pub async fn connect(host: &str, private_key: &str) -> Result<Self, ExecError> {
        use polymarket_client_sdk_v2::clob::{Client, Config};
        use polymarket_client_sdk_v2::POLYGON;

        let signer = LocalSigner::from_str(private_key)
            .map_err(|e| ExecError::Decode(format!("invalid private key: {e}")))?
            .with_chain_id(Some(POLYGON));

        // SignatureType::Proxy — for email/Magic-login Polymarket accounts.
        // Use GnosisSafe if the account was created via browser wallet.
        let client = Client::new(host, Config::default())
            .map_err(|e| ExecError::Network(e.to_string()))?
            .authentication_builder(&signer)
            .signature_type(SignatureType::Proxy)
            .authenticate()
            .await
            .map_err(|e| ExecError::Network(format!("auth failed: {e}")))?;

        Ok(Self { client, signer })
    }
}

#[async_trait]
impl OrderExecutor for ClobOrderExecutor {
    /// Submit a Fill-or-Kill market BUY for `dollars` USDC worth of the given token.
    ///
    /// The SDK automatically derives the worst-acceptable price by walking the ask
    /// book; if liquidity is insufficient the exchange rejects and `success = false`.
    async fn buy_fok(&self, token_id: &str, dollars: Decimal) -> Result<FillResult, ExecError> {
        let tid = U256::from_str(token_id)
            .map_err(|e| ExecError::Decode(format!("invalid token_id '{token_id}': {e}")))?;

        let amount = Amount::usdc(dollars)
            .map_err(|e| ExecError::Decode(format!("invalid USDC amount {dollars}: {e}")))?;

        let signable = self
            .client
            .market_order()
            .token_id(tid)
            .side(Side::Buy)
            .amount(amount)
            .order_type(OrderType::FOK)
            .build()
            .await
            .map_err(|e| ExecError::Network(format!("order build failed: {e}")))?;

        let signed = self
            .client
            .sign(&self.signer, signable)
            .await
            .map_err(|e| ExecError::Network(format!("order sign failed: {e}")))?;

        let resp = self
            .client
            .post_order(signed)
            .await
            .map_err(|e| ExecError::Network(format!("post_order failed: {e}")))?;

        if !resp.success {
            return Err(ExecError::FillOrKillFailed);
        }

        // For a BUY market order:
        //   making_amount = USDC committed by the maker (us)
        //   taking_amount = conditional shares received
        let shares = resp.taking_amount;
        let dollars_spent = resp.making_amount;
        let fill_price = if shares.is_zero() {
            Decimal::ZERO
        } else {
            dollars_spent / shares
        };

        Ok(FillResult {
            fill_price,
            shares,
            dollars: dollars_spent,
        })
    }

    /// Submit a market SELL of `shares` worth of the given token.
    ///
    /// Sell orders must specify their amount in shares (the SDK enforces this;
    /// USDC-denominated sells are rejected). Uses FAK (Fill-and-Kill) so partial
    /// fills are accepted — a full FOK sell often fails due to thin bids.
    async fn sell_market(
        &self,
        token_id: &str,
        shares: Decimal,
    ) -> Result<FillResult, ExecError> {
        let tid = U256::from_str(token_id)
            .map_err(|e| ExecError::Decode(format!("invalid token_id '{token_id}': {e}")))?;

        // Polymarket CLOB requires share amounts with at most 2 decimals
        // (SDK's `LOT_SIZE_SCALE = 2`). Real BUY fills come back with up to 6
        // decimals (e.g. 9.259258), so we must truncate before submitting the
        // SELL. Truncation (not rounding) ensures we never try to sell more
        // shares than we hold; the rounded-off fraction (~$0.001 typical) is
        // intentionally stuck and reclaimable on resolution.
        let sellable = shares.trunc_with_scale(2);
        if sellable.is_zero() {
            return Err(ExecError::Decode(format!(
                "share amount {shares} truncates to 0 — too small to sell"
            )));
        }
        let amount = Amount::shares(sellable)
            .map_err(|e| ExecError::Decode(format!("invalid share amount {sellable} (orig {shares}): {e}")))?;

        let signable = self
            .client
            .market_order()
            .token_id(tid)
            .side(Side::Sell)
            .amount(amount)
            .order_type(OrderType::FAK)
            .build()
            .await
            .map_err(|e| ExecError::Network(format!("order build failed: {e}")))?;

        let signed = self
            .client
            .sign(&self.signer, signable)
            .await
            .map_err(|e| ExecError::Network(format!("order sign failed: {e}")))?;

        let resp = self
            .client
            .post_order(signed)
            .await
            .map_err(|e| ExecError::Network(format!("post_order failed: {e}")))?;

        if !resp.success {
            return Err(ExecError::Network(
                resp.error_msg
                    .unwrap_or_else(|| "sell order rejected".to_owned()),
            ));
        }

        // For a SELL market order:
        //   making_amount = shares committed by the maker (us)
        //   taking_amount = USDC received
        let shares_sold = resp.making_amount;
        let dollars_received = resp.taking_amount;
        let fill_price = if shares_sold.is_zero() {
            Decimal::ZERO
        } else {
            dollars_received / shares_sold
        };

        Ok(FillResult {
            fill_price,
            shares: shares_sold,
            dollars: dollars_received,
        })
    }

    async fn place_limit(
        &self,
        token_id: &str,
        side: OrderSide,
        price: Decimal,
        shares: Decimal,
    ) -> Result<OrderId, ExecError> {
        let tid = U256::from_str(token_id)
            .map_err(|e| ExecError::Decode(format!("invalid token_id '{token_id}': {e}")))?;

        // Same precision rule as sell_market — CLOB requires <=2 decimal places.
        let sellable = shares.trunc_with_scale(2);
        if sellable.is_zero() {
            return Err(ExecError::Decode(format!(
                "share amount {shares} truncates to 0 — too small to place"
            )));
        }

        let sdk_side = match side {
            OrderSide::Buy => Side::Buy,
            OrderSide::Sell => Side::Sell,
        };

        let signable = self
            .client
            .limit_order()
            .token_id(tid)
            .side(sdk_side)
            .price(price)
            .size(sellable)
            .order_type(OrderType::GTC)
            .build()
            .await
            .map_err(|e| ExecError::Network(format!("limit build failed: {e}")))?;

        let signed = self
            .client
            .sign(&self.signer, signable)
            .await
            .map_err(|e| ExecError::Network(format!("limit sign failed: {e}")))?;

        let resp = self
            .client
            .post_order(signed)
            .await
            .map_err(|e| ExecError::Network(format!("limit post failed: {e}")))?;

        if !resp.success {
            return Err(ExecError::FillOrKillFailed);
        }

        Ok(OrderId(resp.order_id))
    }

    async fn cancel(&self, order_id: &OrderId) -> Result<(), ExecError> {
        self.client
            .cancel_order(&order_id.0)
            .await
            .map(|_| ())
            .map_err(|e| ExecError::Network(format!("cancel failed: {e}")))
    }
}
