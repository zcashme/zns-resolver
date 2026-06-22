//! ZNS JSON-RPC API module.
//!
//! `jsonrpc.rs` + submodules (no mod.rs).
//!
//! - models.rs: public DTOs
//! - handlers.rs: trait + impl

mod handlers;
mod models;

pub use handlers::JsonRpcApi;
#[allow(unused_imports)]
pub use models::{NameEvent, NameRecord, Paginated, Status};

use jsonrpsee::server::ServerHandle;
use handlers::ZnsApiServer;

use crate::registry::Registry;

pub(crate) async fn serve_rpc(addr: &str, registry: Registry) -> std::io::Result<ServerHandle> {
    let api = JsonRpcApi::new(registry);
    let server = jsonrpsee::server::Server::builder().build(addr).await?;
    let handle = server.start(api.into_rpc());
    tracing::info!("JSON-RPC listening on {addr}");
    Ok(handle)
}
