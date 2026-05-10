#![cfg(test)]

use poly_tui::trader::adapters::gamma_wrapper::GammaMarketDiscovery;
use poly_tui::trader::errors::MarketError;
use poly_tui::trader::market::MarketDiscovery;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
#[ignore]
async fn open_market_decoded_correctly() {
    let server = MockServer::start().await;
    let body = r#"[{"markets":[{
        "slug":"btc-updown-5m-1700000300", "closed":false,
        "outcomes":"[\"Up\",\"Down\"]",
        "clobTokenIds":"[\"u\",\"d\"]",
        "outcomePrices":"[\"0.50\",\"0.50\"]"
    }]}]"#;
    Mock::given(method("GET")).and(path("/events"))
        .and(query_param("slug", "btc-updown-5m-1700000300"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server).await;

    let disc = GammaMarketDiscovery::new(server.uri());
    let m = disc.find_window(1700000300, 5).await.unwrap();
    assert_eq!(m.up_token_id, "u");
    assert_eq!(m.down_token_id, "d");
}

#[tokio::test]
#[ignore]
async fn empty_response_returns_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server).await;
    let disc = GammaMarketDiscovery::new(server.uri());
    let r = disc.find_window(1700000300, 5).await;
    assert!(matches!(r, Err(MarketError::NotFound { .. })));
}

#[tokio::test]
#[ignore]
async fn http_500_returns_network() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/events"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server).await;
    let disc = GammaMarketDiscovery::new(server.uri());
    let r = disc.find_window(1700000300, 5).await;
    assert!(matches!(r, Err(MarketError::Network(_))));
}

#[tokio::test]
#[ignore]
async fn malformed_body_returns_decode() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/events"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server).await;
    let disc = GammaMarketDiscovery::new(server.uri());
    let r = disc.find_window(1700000300, 5).await;
    assert!(matches!(r, Err(MarketError::Decode(_))));
}
