//! # ZNS resolver
//!
//! Zcash Names binds human-readable names to shielded (unified) addresses via note
//! commitments on chain.
//!
//! The ZNS Resolver watches the registry inbox, verifies bindings,
//! indexes name tips, and serves names using a JSON-RPC HTTP API.
//

mod jsonrpc; // API implementation
mod registry; // Name index Database
mod sync; // Sync Loop

use sync::run_sync_loop;
use sync::SyncError;
use tracing::level_filters::LevelFilter;
use zcash_protocol::consensus::Network;

use jsonrpc::serve_rpc;
use registry::Db;

// ── compile-time network selection ───────────────────────────────────────────

#[cfg(all(feature = "mainnet", feature = "testnet"))]
compile_error!("mainnet and testnet are mutually exclusive");
#[cfg(not(any(feature = "mainnet", feature = "testnet")))]
compile_error!("enable either mainnet (default) or testnet feature");

#[cfg(feature = "mainnet")]
const NETWORK: Network = Network::MainNetwork;
#[cfg(feature = "testnet")]
const NETWORK: Network = Network::TestNetwork;

/// Registry unified full viewing key for the active network.
#[cfg(feature = "mainnet")]
const UFVK: &str = "ufvk1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq"; // TODO: replace with real mainnet registry UFVK before building for mainnet
#[cfg(feature = "testnet")]
const UFVK: &str = "uviewtest1m6ttk6khq8gy0s5v5e5c9snavnwzyv9hl9d5g7kc9lczlv36mjj4tpkmqqd5jep4cg0ea79ahqjpz3huv28kp2frtr3vc9wgerseynuntyu92ky6nwd746w8waz7jv34ax32h4uffcj7ky8qphxesmqqzvt7ykdle5lg2vv69we9nz2q89m8pudjzngxk82mh2s3p3uqedjucnca95tzdqqsg7pn5htvulp8hcyhqa8t4qhlxnpqw7elupkeyvzwky4lta26yy4tvgqz5pjx6ew9e3hm4wmu5t4jt7ku450atn83fezs6r5mc6jkxjc4xcptzss3c3e8ldrnj0uru9tnjteelxzzx7mzrwetu965t2z8luz24h9cj37g9q5nclyczp4gnx2g5z4twlkl9mtvdxwdwxza7chztzcgw6e4eye36auh6p5ltzclppxykhmalghf0fk8087jhknjyzxfzkukj4fmt3umm0k27mh44lfxmc8m0kvh";

/// Persisted name index filename. Distinct per network so mainnet and testnet
/// builds do not share on-disk state.
#[cfg(feature = "mainnet")]
const DB_PATH: &str = "zns.sqlite";
#[cfg(feature = "testnet")]
const DB_PATH: &str = "zns-testnet.sqlite";

/// Skip all blocks before this height on first sync (performance).
#[cfg(feature = "mainnet")]
const SCAN_BIRTHDAY: u32 = 3000000;
#[cfg(feature = "testnet")]
const SCAN_BIRTHDAY: u32 = 4_000_000;

const RPC_ADDR: &str = "127.0.0.1:8080"; // where clients send JSON-RPC name queries

#[tokio::main]
async fn main() -> Result<(), SyncError> {
    // --- Logging ---
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .init();

    // --- Persistent layer bootstrap ---
    let db = Db::open(NETWORK, UFVK, SCAN_BIRTHDAY, DB_PATH)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "registry database failed to open");
            std::process::exit(1);
        });

    // --- RPC server ---
    let _rpc_handle = serve_rpc(RPC_ADDR, db.clone())
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "rpc server failed to start");
            std::process::exit(1);
        });

    // --- Sync loop ---
    run_sync_loop(db.clone(), NETWORK, UFVK, SCAN_BIRTHDAY).await?;

    Ok(())
}