#![cfg(test)]

use poly_tui::trader::adapters::chainlink_btc_wrapper::HttpChainlinkFeed;
use poly_tui::tui::market_watch::BtcPriceFeed;
use rust_decimal::Decimal;
use std::str::FromStr;

#[tokio::test]
#[ignore]
async fn fetches_real_btc_price_from_polygon() {
    let feed = HttpChainlinkFeed::connect("https://polygon-rpc.com").await
        .expect("connect to public Polygon RPC");
    let price = feed.latest_price().await
        .expect("fetch latest BTC/USD round");

    // Plausible BTC range: $10k < p < $1M
    assert!(price > Decimal::from_str("10000").unwrap(),
        "price too low: {price}");
    assert!(price < Decimal::from_str("1000000").unwrap(),
        "price implausibly high: {price}");
}
