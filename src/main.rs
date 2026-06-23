//! # ZNS resolver
//!
//! Zcash Names binds human-readable names to shielded (unified) addresses via note
//! commitments on chain.
//!
//! The ZNS Resolver watches the registry inbox, verifies bindings,
//! indexes name tips, and serves names using a JSON-RPC HTTP API.
//

mod jsonrpc; // JSON-RPC API implementation (owns the public contract)
mod network; // network selection + all mainnet vs testnet constants (build-time only)
mod registry; // SQLite-backed name index (tokio-rusqlite writer + reader pool)
mod sync; // long-running sync loop streaming blocks from lightwalletd

use sync::run_sync_loop;
use sync::SyncError;
use tracing::level_filters::LevelFilter;

use jsonrpc::serve_rpc;
use registry::Registry;

const RPC_ADDR: &str = "127.0.0.1:8080"; // where clients send JSON-RPC name queries

#[tokio::main]
async fn main() -> Result<(), SyncError> {
    // --- Logging ---
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .init();

    // --- Persistent layer bootstrap ---
    let registry = Registry::start().await.unwrap_or_else(|e| {
        tracing::error!(error = %e, "registry database failed to open");
        std::process::exit(1);
    });

    // --- RPC server ---
    let _rpc_handle = serve_rpc(RPC_ADDR, registry.clone())
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "rpc server failed to start");
            std::process::exit(1);
        });

    run_sync_loop(registry.clone()).await?;

    Ok(())
}
