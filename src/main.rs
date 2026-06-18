//! # ZNS resolver
//!
//! Zcash Names binds human-readable names to Orchard shielded addresses via note
//! commitments on chain. This binary watches the registry inbox, verifies bindings,
//! indexes name tips, and serves JSON-RPC.
//!

mod jsonrpc;
mod registry;
mod orchard;
mod sync;

use std::path::PathBuf;

use orchard::orchard_ivk;
use sync::{run_sync_loop, run_tip_watcher, LwdSession, TIP_WATCH_INTERVAL};
use tokio::sync::watch;
use tracing::level_filters::LevelFilter;
use zcash_protocol::consensus::Network;

use jsonrpc::serve_rpc;
use registry::Registry;

/// Registry **incoming viewing key** (UIVK).
const UIVK: &str = "uivktest18a7ht78cymvm3sxdw9myrr04nrnj8nvrqdjhadj8dp3cv8pm2dqszuxnjrjyp6xyf0svtzjxnq3976l5sxzd09mmx9g6sj9xpp67ympwsrv6wen5ye25jhvq0l8zz937hcgtp90rwhjq0m02rf7qk6wmvrny26r2vt0laztqx4kgx0jqtdwu38ld0hx53m0u20rjny20gpxneavfze7aqqft5vs0jraaqed4974avkx4c3qass3prsqq2fdx08jllet4uuxzz8zmrem8xcwaya9v50l046lp2c9uuyrkp0r8jja5vlzday32pgq4cccqd2rjvtlsfnn9lne9cchrcfgn87jlx9";
const NETWORK: Network = Network::TestNetwork;
const LIGHTWALLETD: &str = "https://testnet.zec.rocks:443";
const DB_PATH: &str = "zns-resolver.sqlite";
const RPC_ADDR: &str = "127.0.0.1:8080";
const SCAN_BIRTHDAY: u32 = 4_000_000;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .init();

    let ivk = orchard_ivk(&NETWORK, UIVK).expect("registry UIVK must decode for NETWORK");

    let (registry, db_join) = Registry::start(PathBuf::from(DB_PATH)).unwrap_or_else(|e| {
        tracing::error!(error = %e, "registry database failed to open");
        std::process::exit(1);
    });
    registry.install_registry_uivk(UIVK).unwrap_or_else(|e| {
        tracing::error!(error = %e, "registry_account uivk install failed");
        std::process::exit(1);
    });

    let rpc_addr = RPC_ADDR.to_string();
    let rpc_registry = registry.clone();
    tokio::spawn(async move {
        if let Err(e) = serve_rpc(&rpc_addr, rpc_registry).await {
            tracing::error!(error = %e, "rpc server exited");
        }
    });

    let (tip_tx, tip_rx) = watch::channel((0, None));
    let lwd = LwdSession::new(LIGHTWALLETD.to_string());
    tokio::spawn(run_tip_watcher(lwd.clone(), TIP_WATCH_INTERVAL, tip_tx));

    tokio::select! {
        () = run_sync_loop(
            lwd,
            registry.clone(),
            NETWORK,
            SCAN_BIRTHDAY,
            ivk,
            tip_rx,
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
