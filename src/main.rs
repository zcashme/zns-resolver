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
const UFVK: &str = "uviewtest1akm8qc7xya8227z4crzpqx5jj23esvf9l2kc5ddt7zupzg2s785gu70rlv42ge228wwu323el8h8qm4kucus4fl6py0eq7etx8dkhe8wedjz390gvxthrylxrhp22zvzq65vcs2mg3s8krpssevaqzwed4murqtghp4x9jj84kkp5txtcrvcauvx36kae9pmrwjuvs7k29f8glqkf53yzhspralwrej7g0t0nrn86ryqs9rcc3k797vpk7jj33suxefjl4sk2va2furxkh3sude0v7ve2htrqgf03lw0yyrh5lnt4vx97787rj9l0gnprg5r2fqlthnx3f6kcv6p96taz3wce8krzl8uw3aeukhleuvzwvhp7v3y6mvyw23g9ych5qhda6gupc8r06c5c8hf4mplrt40m5mvywmdy0xvs49katqhj7amr26pdzfrj0up69t5ksjuxv6wf2tgw6lxevpprtuqj9en4w3sx2w3jv5j2vrjsuf9";

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
