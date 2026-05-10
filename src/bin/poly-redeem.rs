//! poly-redeem — one-shot CLI to redeem winning conditional tokens for USDC.
//!
//! When the trader's sell path fails (e.g. a CLOB error during a TP/SL trigger
//! or winner-sweep), the buy fill stays in the wallet as ERC-1155 outcome
//! tokens. After the market resolves, the winning side's tokens are worth
//! $1 each but only become USDC after an on-chain `redeemPositions` call.
//!
//! This binary fetches all open positions via Polymarket's data-api, filters
//! to the resolved-and-redeemable ones, and submits one transaction per
//! market to convert them back to USDC.
//!
//! Usage:
//!   poly-redeem --dry-run    # show what would be redeemed
//!   poly-redeem              # actually redeem

use alloy::primitives::{address, Address};
use alloy::providers::ProviderBuilder;
use alloy::signers::Signer as _;
use alloy::signers::local::LocalSigner;
use anyhow::{Context, Result};
use clap::Parser;
use polymarket_client_sdk_v2::ctf::Client as CtfClient;
use polymarket_client_sdk_v2::ctf::types::RedeemPositionsRequest;
use polymarket_client_sdk_v2::data::Client as DataClient;
use polymarket_client_sdk_v2::data::types::request::PositionsRequest;
use polymarket_client_sdk_v2::data::types::response::Position as SdkPosition;
use polymarket_client_sdk_v2::{derive_proxy_wallet, POLYGON};
use poly_tui::config::Config;
use std::collections::HashSet;
use std::str::FromStr;

/// USDC.e on Polygon mainnet — Polymarket's collateral token.
const USDC_E_POLYGON: Address = address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174");

#[derive(Parser, Debug)]
#[command(
    name = "poly-redeem",
    about = "Redeem resolved winning conditional tokens to USDC"
)]
struct Args {
    /// List redeemable positions without sending any transactions.
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("info".parse()?))
        .init();

    let args = Args::parse();
    let cfg = Config::from_env().context("loading .env / environment")?;

    // Derive proxy address from EOA — that's what holds the conditional tokens.
    let signer = LocalSigner::from_str(&cfg.polymarket_private_key)
        .context("invalid POLYMARKET_PRIVATE_KEY")?
        .with_chain_id(Some(POLYGON));
    let eoa = signer.address();
    let proxy = derive_proxy_wallet(eoa, POLYGON)
        .context("derive_proxy_wallet returned None — chain config missing?")?;
    println!("EOA:   {eoa}");
    println!("Proxy: {proxy}");

    // Fetch open positions for the proxy address.
    let data = DataClient::default();
    let req = PositionsRequest::builder().user(proxy).build();
    let positions: Vec<SdkPosition> = data
        .positions(&req)
        .await
        .context("fetching positions from data-api")?;

    if positions.is_empty() {
        println!("\nNo open positions. Nothing to redeem.");
        return Ok(());
    }

    println!("\n{} open position(s):", positions.len());
    for p in &positions {
        let flag = if p.redeemable { "✓ redeemable" } else { "  " };
        println!(
            "  [{flag}] {} {} {}sh @ ${:.4}  cur ${:.4}  ({})",
            &p.slug, p.outcome, p.size, p.avg_price, p.cur_price,
            if p.redeemable { "RESOLVED" } else { "live" },
        );
    }

    // Filter to redeemable. Group by condition_id so we don't redeem the same
    // market twice (a binary market has both UP and DOWN tokens; redeem with
    // index_sets [1,2] handles both in one tx).
    let mut markets_to_redeem: HashSet<alloy::primitives::B256> = HashSet::new();
    for p in &positions {
        if p.redeemable {
            markets_to_redeem.insert(p.condition_id);
        }
    }

    if markets_to_redeem.is_empty() {
        println!("\nNo redeemable positions yet. Resolved markets show as 'redeemable'.");
        return Ok(());
    }

    println!("\n{} market(s) ready to redeem.", markets_to_redeem.len());

    if args.dry_run {
        println!("\n--dry-run: not sending transactions. Re-run without --dry-run to redeem.");
        return Ok(());
    }

    // Build CTF client with provider+signer for write operations.
    let provider = ProviderBuilder::new()
        .wallet(signer.clone())
        .connect(&cfg.polygon_rpc_url)
        .await
        .context("connecting Polygon RPC")?;
    let ctf = CtfClient::new(provider, POLYGON)
        .context("CTF client init")?;

    println!();
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    for condition_id in &markets_to_redeem {
        let req = RedeemPositionsRequest::for_binary_market(USDC_E_POLYGON, *condition_id);
        match ctf.redeem_positions(&req).await {
            Ok(resp) => {
                println!(
                    "✓ Redeemed {condition_id} — tx {} block {}",
                    resp.transaction_hash, resp.block_number
                );
                succeeded += 1;
            }
            Err(e) => {
                eprintln!("✗ Redeem failed for {condition_id}: {e}");
                failed += 1;
            }
        }
    }

    println!("\nDone. {succeeded} succeeded, {failed} failed.");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
