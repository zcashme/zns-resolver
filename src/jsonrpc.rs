//! ZNS JSON-RPC API module.
//!
//! `jsonrpc.rs` + submodules (no mod.rs).
//!
//! - records.rs: public DTOs
//! - service.rs: trait + impl

mod records;
mod service;

pub use service::JsonRpcApi;

use jsonrpsee::server::ServerHandle;
use service::ZnsApiServer;

use crate::registry::Db;
use crate::sync::ChainTip;

pub(crate) async fn serve_rpc(
    addr: &str,
    db: Db,
    tip_rx: ChainTip,
) -> std::io::Result<ServerHandle> {
    let api = JsonRpcApi::new(db, tip_rx);
    let server = jsonrpsee::server::Server::builder().build(addr).await?;
    let handle = server.start(api.into_rpc());
    tracing::info!("JSON-RPC listening on {addr}");
    Ok(handle)
}
