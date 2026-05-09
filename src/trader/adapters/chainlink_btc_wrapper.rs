use crate::tui::market_watch::{BtcPriceFeed, MarketWatchError};
use alloy::primitives::{address, Address};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::sol;
use async_trait::async_trait;
use rust_decimal::Decimal;

/// Chainlink BTC/USD aggregator on Polygon mainnet.
const BTC_USD_AGGREGATOR_POLYGON: Address =
    address!("c907E116054Ad103354f2D350FD2514433D57F6f");

const BTC_USD_DECIMALS: u32 = 8;

sol! {
    #[sol(rpc)]
    interface AggregatorV3 {
        function latestRoundData() external view returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );
    }
}

pub struct ChainlinkBtcPriceFeed<P> {
    provider: P,
}

impl<P: Provider + Clone + Send + Sync + 'static> ChainlinkBtcPriceFeed<P> {
    fn new(provider: P) -> Self {
        Self { provider }
    }
}

/// Concrete alias for the HTTP provider returned by ProviderBuilder.
pub type HttpChainlinkFeed =
    ChainlinkBtcPriceFeed<alloy::providers::fillers::FillProvider<
        alloy::providers::fillers::JoinFill<
            alloy::providers::Identity,
            alloy::providers::fillers::JoinFill<
                alloy::providers::fillers::GasFiller,
                alloy::providers::fillers::JoinFill<
                    alloy::providers::fillers::BlobGasFiller,
                    alloy::providers::fillers::JoinFill<
                        alloy::providers::fillers::NonceFiller,
                        alloy::providers::fillers::ChainIdFiller,
                    >,
                >,
            >,
        >,
        alloy::providers::RootProvider,
    >>;

impl HttpChainlinkFeed {
    pub async fn connect(rpc_url: &str) -> Result<Self, MarketWatchError> {
        let url = rpc_url
            .parse::<reqwest::Url>()
            .map_err(|e| MarketWatchError::Connect(format!("invalid url: {e}")))?;
        let provider = ProviderBuilder::new().connect_http(url);
        Ok(Self::new(provider))
    }
}

#[async_trait]
impl<P: Provider + Clone + Send + Sync + 'static> BtcPriceFeed for ChainlinkBtcPriceFeed<P> {
    async fn latest_price(&self) -> Result<Decimal, MarketWatchError> {
        let agg = AggregatorV3::new(BTC_USD_AGGREGATOR_POLYGON, &self.provider);
        let result = agg
            .latestRoundData()
            .call()
            .await
            .map_err(|e| MarketWatchError::Rpc(e.to_string()))?;
        // result.answer is alloy::primitives::I256 (alloy_primitives::Signed<256, 4>)
        let answer_i128: i128 = result.answer.try_into().map_err(|e: _| {
            MarketWatchError::Decode(format!("answer overflow: {e:?}"))
        })?;
        decode_chainlink_answer(answer_i128, BTC_USD_DECIMALS)
    }
}

/// Pure helper, unit-tested. Converts a Chainlink raw integer answer to
/// plain dollars (Decimal), dividing by 10^decimals.
pub fn decode_chainlink_answer(raw: i128, decimals: u32) -> Result<Decimal, MarketWatchError> {
    let raw_dec = Decimal::from_i128_with_scale(raw, 0);
    let divisor = Decimal::from(10_u64.pow(decimals));
    Ok(raw_dec / divisor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn decode_typical_btc_price() {
        // $80,424.78 with 8 decimals = 8042478000000
        let r = decode_chainlink_answer(8_042_478_000_000_i128, 8).unwrap();
        assert_eq!(r, Decimal::from_str("80424.78").unwrap());
    }

    #[test]
    fn decode_zero() {
        let r = decode_chainlink_answer(0, 8).unwrap();
        assert_eq!(r, Decimal::ZERO);
    }

    #[test]
    fn decode_small_value() {
        let r = decode_chainlink_answer(1, 8).unwrap();
        assert_eq!(r, Decimal::from_str("0.00000001").unwrap());
    }

    #[test]
    fn decode_large_value() {
        let r = decode_chainlink_answer(100_000_000_000_000_i128, 8).unwrap();
        assert_eq!(r, Decimal::from(1_000_000));
    }
}
