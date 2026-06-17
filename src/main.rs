//! # ZNS resolver
//!
//! Zcash Names binds human-readable names to Orchard shielded addresses via note
//! commitments on chain. This binary watches the registry inbox, verifies bindings,
//! indexes name tips, and serves JSON-RPC.
//!
//! ```text
//! lightwalletd ──► orchard::observe_batch ──► names::apply_batch ──► SQLite
//!                                        │                          │
//!                                        ▼                          ▼
//!                              sync::materialize_proofs      jsonrpc
//!                                        (zebrad getblock)
//! ```
//!
//! Modules: `sync`, `orchard`, `names`, `jsonrpc` (no `lib.rs`). Proof I/O lives in `sync`.

mod jsonrpc;
mod names;
mod orchard;
mod sync;

use std::path::PathBuf;

use orchard::orchard_ivk;
use sync::{run_sync_loop, run_tip_watcher, ZebradClient, TIP_WATCH_INTERVAL};
use tokio::sync::watch;
use tracing::level_filters::LevelFilter;
use zcash_protocol::consensus::Network;

use jsonrpc::{serve_rpc, RpcContext};
use names::Db;

/// Registry **incoming viewing key** (UIVK).
const UIVK: &str = "uivktest18a7ht78cymvm3sxdw9myrr04nrnj8nvrqdjhadj8dp3cv8pm2dqszuxnjrjyp6xyf0svtzjxnq3976l5sxzd09mmx9g6sj9xpp67ympwsrv6wen5ye25jhvq0l8zz937hcgtp90rwhjq0m02rf7qk6wmvrny26r2vt0laztqx4kgx0jqtdwu38ld0hx53m0u20rjny20gpxneavfze7aqqft5vs0jraaqed4974avkx4c3qass3prsqq2fdx08jllet4uuxzz8zmrem8xcwaya9v50l046lp2c9uuyrkp0r8jja5vlzday32pgq4cccqd2rjvtlsfnn9lne9cchrcfgn87jlx9";
const NETWORK: Network = Network::TestNetwork;
const LIGHTWALLETD: &str = "https://testnet.zec.rocks:443";
const DB_PATH: &str = "zns-resolver.sqlite";
const RPC_ADDR: &str = "127.0.0.1:8080";
/// zebrad JSON-RPC for proof material (regtest: `zebra-regtest` → `:18232`). `None` → proofs omitted.
const ZEBRAD_RPC: Option<&str> = None;
const SCAN_BIRTHDAY: u32 = 4_000_000;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .init();

    let ivk = orchard_ivk(&NETWORK, UIVK)
        .expect("registry UIVK must decode for NETWORK");

    let zebrad = ZEBRAD_RPC
        .map(ZebradClient::new)
        .transpose()
        .expect("ZEBRAD_RPC URL must be valid when set");

    let db_path = PathBuf::from(DB_PATH);
    if let Ok(db) = Db::open_for_indexer(&db_path) {
        if let Err(e) = db.install_registry_uivk(UIVK) {
            eprintln!("registry_account: {e}");
        }
    }

    let rpc_addr = RPC_ADDR.to_string();
    let rpc_ctx = RpcContext {
        db: db_path,
        uivk: UIVK.to_string(),
    };
    tokio::spawn(async move {
        if let Err(e) = serve_rpc(&rpc_addr, rpc_ctx).await {
            eprintln!("rpc: {e}");
        }
    });

    let (tip_tx, tip_rx) = watch::channel((0, None));
    tokio::spawn(run_tip_watcher(
        LIGHTWALLETD,
        TIP_WATCH_INTERVAL,
        tip_tx,
    ));

    run_sync_loop(
        LIGHTWALLETD,
        DB_PATH,
        NETWORK,
        SCAN_BIRTHDAY,
        ivk,
        zebrad,
        tip_rx,
    )
    .await;
}

#[cfg(test)]
mod sealed_registry_config {
    use super::{NETWORK, UIVK};
    use crate::orchard::orchard_ivk;

    #[test]
    fn registry_uivk_decodes_for_network() {
        orchard_ivk(&NETWORK, UIVK).expect("registry UIVK must decode for NETWORK");
    }
}