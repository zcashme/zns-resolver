//! # ZNS resolver
//!
//! Zcash Names binds human-readable names to Orchard shielded addresses via note
//! commitments on chain.
//!
//! The ZNS Resolver watches the registry inbox, verifies bindings,
//! indexes name tips, and serves names using a JSON-RPC HTTP API.
//!

mod jsonrpc;
mod orchard;
mod registry;
mod sync;

use std::path::PathBuf;

use seer_sync::ViewKey;
use sync::run_sync_loop;
use tracing::level_filters::LevelFilter;
use zcash_protocol::consensus::Network;

use jsonrpc::serve_rpc;
use registry::Registry;

/// Registry **incoming viewing key** (UIVK).
const UIVK: &str = "uivktest18a7ht78cymvm3sxdw9myrr04nrnj8nvrqdjhadj8dp3cv8pm2dqszuxnjrjyp6xyf0svtzjxnq3976l5sxzd09mmx9g6sj9xpp67ympwsrv6wen5ye25jhvq0l8zz937hcgtp90rwhjq0m02rf7qk6wmvrny26r2vt0laztqx4kgx0jqtdwu38ld0hx53m0u20rjny20gpxneavfze7aqqft5vs0jraaqed4974avkx4c3qass3prsqq2fdx08jllet4uuxzz8zmrem8xcwaya9v50l046lp2c9uuyrkp0r8jja5vlzday32pgq4cccqd2rjvtlsfnn9lne9cchrcfgn87jlx9";
const NETWORK: Network = Network::TestNetwork;
const LIGHTWALLETD: &str = "https://testnet.zec.rocks:443";
const DB_PATH: &str = "zns.sqlite";
const RPC_ADDR: &str = "127.0.0.1:8080";
const SCAN_BIRTHDAY: u32 = 4_000_000;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .init();

    let view_key = ViewKey::decode(&NETWORK, UIVK)
        .expect("registry viewing key (UFVK or UIVK) must decode for NETWORK");

    let (registry, db_join) = Registry::start(PathBuf::from(DB_PATH)).unwrap_or_else(|e| {
        tracing::error!(error = %e, "registry database failed to open");
        std::process::exit(1);
    });
    registry
        .install_registry_config(UIVK, NETWORK, SCAN_BIRTHDAY)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "registry_account config install failed");
            std::process::exit(1);
        });

    let rpc_addr = RPC_ADDR.to_string();
    let rpc_registry = registry.clone();
    tokio::spawn(async move {
        if let Err(e) = serve_rpc(&rpc_addr, rpc_registry).await {
            tracing::error!(error = %e, "rpc server exited");
        }
    });

    tokio::select! {
        () = run_sync_loop(
            LIGHTWALLETD,
            registry.clone(),
            NETWORK,
            SCAN_BIRTHDAY,
            &view_key,
        ) => {}
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutdown requested");
        }
    }

    registry.shutdown();
    if let Err(e) = db_join.join() {
        tracing::error!(error = ?e, "registry db thread panicked");
    }
}
