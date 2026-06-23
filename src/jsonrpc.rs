//! ZNS JSON-RPC API module.
//!
//! `jsonrpc.rs` + submodules (no mod.rs).
//!
//! - records.rs: public DTOs
//! - service.rs: trait + impl

mod records;
mod service;

pub use service::JsonRpcApi;
#[allow(unused_imports)]
pub use records::{NameEvent, NameRecord, Paginated, Status};

use service::ZnsApiServer;
use jsonrpsee::server::ServerHandle;

use crate::registry::Registry;

pub(crate) async fn serve_rpc(addr: &str, registry: Registry) -> std::io::Result<ServerHandle> {
    let api = JsonRpcApi::new(registry);
    let server = jsonrpsee::server::Server::builder().build(addr).await?;
    let handle = server.start(api.into_rpc());
    tracing::info!("JSON-RPC listening on {addr}");
    Ok(handle)
}
