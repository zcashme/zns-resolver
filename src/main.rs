//! # ZNS resolver
//!
//! Zcash Names binds human-readable names to shielded (unified) addresses via note
//! commitments on chain.
//!
//! The ZNS Resolver watches the registry inbox, verifies bindings,
//! indexes name tips, and serves names using a JSON-RPC HTTP API.
//

mod jsonrpc;   // JSON-RPC HTTP server (read-only queries against the DB)
mod orchard;   // Orchard note parsing / verification helpers
mod registry;  // SQLite-backed store + background writer pool
mod sync;      // long-running sync loop streaming blocks from lightwalletd

use std::path::PathBuf;

use sync::run_sync_loop;
use tracing::level_filters::LevelFilter;
use zcash_protocol::consensus::Network;

use jsonrpc::serve_rpc;
use registry::Registry;

/// Registry name-note account **full viewing key** (UFVK).
///
/// This viewing key is the lens through which the resolver observes notes that
/// land in the ZNS registry's "name-note" account. With it we can decrypt every
/// note destined for that account *without* spending authority — enough to read
/// name→address bindings encoded in note memos.
const UFVK: &str = "uivktest18a7ht78cymvm3sxdw9myrr04nrnj8nvrqdjhadj8dp3cv8pm2dqszuxnjrjyp6xyf0svtzjxnq3976l5sxzd09mmx9g6sj9xpp67ympwsrv6wen5ye25jhvq0l8zz937hcgtp90rwhjq0m02rf7qk6wmvrny26r2vt0laztqx4kgx0jqtdwu38ld0hx53m0u20rjny20gpxneavfze7aqqft5vs0jraaqed4974avkx4c3qass3prsqq2fdx08jllet4uuxzz8zmrem8xcwaya9v50l046lp2c9uuyrkp0r8zz937hcgtp90rwhjq0m02rf7qk6wmvrny26r2vt0laztqx4kgx0jqtdwu38ld0hx53m0u20rjny20gpxneavfze7aqqft5vs0jraaqed4974avkx4c3qass3prsqq2fdx08jllet4uuxzz8zmrem8xcwaya9v50l046lp2c9uuyrkp0r8jja5vlzday32pgq4cccqd2rjvtlsfnn9lne9cchrcfgn87jlx9";
const NETWORK: Network = Network::TestNetwork;     // which chain rules + address prefixes to use
const LIGHTWALLETD: &str = "https://testnet.zec.rocks:443"; // upstream gRPC stream of compact blocks
const DB_PATH: &str = "zns.sqlite";                  // persisted index of verified name tips
const RPC_ADDR: &str = "127.0.0.1:8080";             // where clients send JSON-RPC name queries
const SCAN_BIRTHDAY: u32 = 4_000_000;                // skip all blocks before this height on first sync

// NOTE: everything above is constant. This binary is statically configured at
// compile time — there is no CLI, env, or config file. To change behavior, edit
// the consts and rebuild.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Logging ---
    // Single global subscriber; INFO level keeps disk/UX manageable but hides
    // per-block chatter (which lives at DEBUG/TRACE inside sync::run_sync_loop).
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .init();

    // --- Viewing key materialization ---
    // Decode the bech32-encoded UFVK into a strongly-typed structure. The expect
    // here is a "panic at boot" — if the key is malformed we have nothing to do.
    let fvk = zcash_keys::keys::UnifiedFullViewingKey::decode(&NETWORK, UFVK)
        .expect("registry name-note UFVK must decode for NETWORK");
    // ZNS encodes every binding as an Orchard note. If this UFVK has no Orchard
    // component we cannot observe name-notes, so abort.
    let orchard_fvk = fvk
        .orchard()
        .expect("name-note UFVK must have an Orchard component");

    // --- Persistent layer bootstrap ---
    // `Registry::start` opens the SQLite file (creating if absent) and spins up
    // the background writer pool. Returns a handle that is cheap to `.clone()`
    // (Arc) and shared by the sync loop and the RPC server.
    let registry = Registry::start(PathBuf::from(DB_PATH)).unwrap_or_else(|e| {
        tracing::error!(error = %e, "registry database failed to open");
        std::process::exit(1);
    });
    // Stamp the immutable registry configuration into the DB. Idempotent: on
    // restart it will verify the stored config matches these consts and refuse
    // to start if they disagree (a safety net against pointing the same DB at
    // a different network or key).
    registry
        .install_registry_config(UFVK, NETWORK, SCAN_BIRTHDAY)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "registry_account config install failed");
            std::process::exit(1);
        });

    // --- RPC server ---
    // Started *before* the sync loop begins: clients can connect immediately and
    // query whatever the DB already knows. Empty results are valid until sync
    // catches up. The handle lets us shut the listener down gracefully later.
    let rpc_handle = serve_rpc(RPC_ADDR, registry.clone())
        .await
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "rpc server failed to start");
            std::process::exit(1);
        });

    // --- Steady-state ---
    // The sync loop is the core long-running service. It only returns on
    // unrecoverable fatal error (or very rarely on clean voluntary exit).
    // Transient issues (LWD disconnects, reorgs) are retried internally.
    let loop_result = run_sync_loop(
        LIGHTWALLETD,
        registry.clone(),
        NETWORK,
        SCAN_BIRTHDAY,
        orchard_fvk,
    )
    .await;

    // --- Ordered shutdown (best-effort on both clean and fatal exit paths) ---
    // Reverse order of startup.
    let _ = rpc_handle.stop();
    rpc_handle.stopped().await;
    registry.shutdown().await;

    loop_result?;
    Ok(())
}