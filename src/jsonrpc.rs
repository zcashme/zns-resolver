//! ZNS JSON-RPC API module.
//!
//! `jsonrpc.rs` + submodules (no mod.rs).
//!
//! - records.rs: public DTOs
//! - service.rs: trait + impl

mod records;
mod service;

pub use service::JsonRpcApi;

use jsonrpsee::server::{ServerConfig, ServerHandle};
use service::ZnsApiServer;

use crate::registry::Db;
use crate::sync::ChainTip;

/// The largest JSON-RPC response. The full name list (`resolve` without a
/// query) must fit: a record is at most ~590 bytes (a 512-byte memo bounds
/// the name and address), so this always holds ~7,100 names and ~15,000
/// typical ones. A larger response fails with `-32008`.
const MAX_RESPONSE_BYTES: u32 = 4 * 1024 * 1024;

pub(crate) async fn serve_rpc(
    addr: &str,
    db: Db,
    tip_rx: ChainTip,
) -> std::io::Result<ServerHandle> {
    let api = JsonRpcApi::new(db, tip_rx);
    let config = ServerConfig::builder()
        .max_response_body_size(MAX_RESPONSE_BYTES)
        .build();
    let server = jsonrpsee::server::Server::builder()
        .set_config(config)
        .build(addr)
        .await?;
    let handle = server.start(api.into_rpc());
    tracing::info!("JSON-RPC listening on {addr}");
    Ok(handle)
}
