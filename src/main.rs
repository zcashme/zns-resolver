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

use std::path::PathBuf;

use sync::run_sync_loop;
use sync::SyncError;
use tracing::level_filters::LevelFilter;

use jsonrpc::serve_rpc;
use registry::Registry;

// Re-exported network constants (chosen by Cargo feature at build time).
use crate::network::{DB_PATH, NETWORK, SCAN_BIRTHDAY, UFVK};

const RPC_ADDR: &str = "127.0.0.1:8080"; // where clients send JSON-RPC name queries

// NOTE: The network-specific values (UFVK, NETWORK, DB_PATH, SCAN_BIRTHDAY,
// lightwalletd endpoints, etc.) live in `network.rs`. They are true `const`s
// selected by the `mainnet` or `testnet` Cargo feature at build time.
// There is no CLI, env var, or config file. To target the other network,
// rebuild with the appropriate feature.

#[tokio::main]
async fn main() -> Result<(), SyncError> {
    // --- Logging ---
    // Single global subscriber; INFO level keeps disk/UX manageable but hides
    // per-block chatter (which lives at DEBUG/TRACE inside sync::run_sync_loop).
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .init();

    // --- Viewing key materialization ---
    // Decode the bech32-encoded UFVK into a strongly-typed structure.
    // Must be a full viewing key (not UIVK) because observe_batch performs
    // an OVK self-send proof using try_decrypt_orchard_sent.
    let fvk = zcash_keys::keys::UnifiedFullViewingKey::decode(&NETWORK, UFVK)
        .expect("registry name-note UFVK must decode for NETWORK");
    // ZNS encodes every binding as an Orchard note. If this UFVK has no Orchard
    // component we cannot observe name-notes, so abort.
    let orchard_fvk = fvk
        .orchard()
        .expect("name-note UFVK must have an Orchard component");

    // --- Persistent layer bootstrap ---
    // `Registry::start` creates the name index (the boundary on top of the
    // SQLite store). It owns the tokio-rusqlite writer + reader pool internally.
    // The handle is cheap to `.clone()` (Arc) and shared by sync + RPC.
    let registry = Registry::start(PathBuf::from(DB_PATH)).await.unwrap_or_else(|e| {
        tracing::error!(error = %e, "registry database failed to open");
        std::process::exit(1);
    });
    // Stamp the immutable registry configuration into the DB. Idempotent: on
    // restart it will verify the stored config matches these consts and refuse
    // to start if they disagree (a safety net against pointing the same DB at
    // a different network or key).
    //
    // The string stored in registry_account.ufvk must be a full viewing key
    // (ufvk...), not a UIVK. The OVK self-send proof performed in observe_batch
    // requires the outgoing viewing key material that only a UFVK provides.
    registry
        .install_registry_config(UFVK, NETWORK, SCAN_BIRTHDAY)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "registry_account config install failed");
            std::process::exit(1);
        });

    // --- RPC server ---
    // Started *before* the sync loop so clients can connect immediately and
    // query whatever the DB already knows. Empty results are valid until sync
    // catches up. The handle is kept alive for the scope of this function
    // (renamed `_rpc_handle`) — dropping a `ServerHandle` stops the server, and
    // we intentionally want it to live as long as the process does.
    let _rpc_handle = serve_rpc(RPC_ADDR, registry.clone())
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "rpc server failed to start");
            std::process::exit(1);
        });

    run_sync_loop(
        registry.clone(),
        NETWORK,
        SCAN_BIRTHDAY,
        orchard_fvk,
    )
    .await?;

    Ok(())
}
