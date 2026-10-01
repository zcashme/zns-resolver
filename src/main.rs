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

use orchard::keys::FullViewingKey;
use seer_sync::UnifiedFullViewingKey;
use sync::{live_tip, run_indexer};
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
pub(crate) const NETWORK: Network = Network::MainNetwork;
#[cfg(feature = "testnet")]
pub(crate) const NETWORK: Network = Network::TestNetwork;

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

/// The mint's ceremony height — the registry's first notes are the
/// ceremony anchors, so scanning starts there and never before. Keep in
/// step with `MINT_BIRTHDAY` in zns-mint `src/boot.rs`. A stored
/// registry_account row keeps its own birthday; this constant only seeds
/// a first sync.
#[cfg(feature = "mainnet")]
const MINT_BIRTHDAY: u32 = 3_400_000;
#[cfg(feature = "testnet")]
const MINT_BIRTHDAY: u32 = 4_338_933;

const RPC_ADDR: &str = "127.0.0.1:8080"; // where clients send JSON-RPC name queries

/// The registry: the resolver's local registry replica, wearing seer-sync's
/// `Account` face — the scan pipeline applies chain observations to it.
pub(crate) struct Registry {
    pub(crate) db: Db,
    pub(crate) fvk: FullViewingKey,
}

#[tokio::main]
async fn main() {
    // --- Logging ---
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .init();

    // --- The registry key: decoded before anything persists it — a bad key
    // --- parks here and never poisons the registry_account row. ---
    let fvk = match UnifiedFullViewingKey::decode(&NETWORK, UFVK) {
        Ok(decoded) => match decoded.orchard() {
            Some(fvk) => fvk.clone(),
            None => {
                tracing::error!(
                    "fatal: resolver is unconfigured — registry UFVK has no orchard component"
                );
                std::future::pending::<()>().await;
                unreachable!()
            }
        },
        Err(error) => {
            tracing::error!(error = %error, "fatal: resolver is unconfigured — registry UFVK failed to decode");
            std::future::pending::<()>().await;
            unreachable!()
        }
    };

    // --- Network path: the live chain head, observed and published ---
    let (tip_tx, tip_rx) = tokio::sync::watch::channel(None);
    tokio::spawn(live_tip(tip_tx));

    // --- Persistent layer bootstrap. Without it there is nothing to serve. ---
    let db = match Db::open(UFVK, MINT_BIRTHDAY, DB_PATH) {
        Ok(db) => db,
        Err(error) => {
            tracing::error!(error = %error, "fatal: resolver is unconfigured — registry database failed to open");
            std::future::pending::<()>().await;
            unreachable!()
        }
    };

    // --- RPC server: serves whatever the registry has ---
    let _rpc_handle = match serve_rpc(RPC_ADDR, db.clone(), tip_rx).await {
        Ok(handle) => handle,
        Err(error) => {
            tracing::error!(error = %error, "fatal: resolver is unconfigured — rpc server failed to start");
            std::future::pending::<()>().await;
            unreachable!()
        }
    };

    // --- The indexer: everything passed — run forever ---
    run_indexer(db, UFVK, fvk, MINT_BIRTHDAY).await;
}
