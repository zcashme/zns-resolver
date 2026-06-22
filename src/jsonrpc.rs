//! ZNS JSON-RPC API — the public contract for the resolver.
//!
//! This module owns the API surface. It is **not** a thin passthrough to the
//! database tables. The goal is:
//!
//! 1. Own the contract — method signatures and types ARE the API.
//! 2. Express domain intent — methods describe what clients want (resolve a name,
//!    get history, check status), not "query this table".
//! 3. Hide storage details — clients do not see `action_index`, `rowid`, the
//!    difference between the `names` and `name_events` tables, or other
//!    implementation artifacts.
//! 4. Real error semantics — different problems produce distinguishable errors.
//! 5. Evolvability — we can change the storage model without breaking clients.
//! 6. Documented by design — the trait, types, and docs tell the story.
//!
//! ## Module Organization (folder modules + jsonrpc.rs)
//!
//! - `jsonrpc.rs` (this file): Entry point + server bootstrap.
//! - `jsonrpc/types.rs`: Public DTO types (`NameRecord`, `NameEvent`, `Status`, `Paginated`).
//! - `jsonrpc/service.rs`: The `ZnsApi` trait + `JsonRpcApi` implementation.

mod service;
mod types;

pub use service::JsonRpcApi;
#[allow(unused_imports)]
pub use types::{NameEvent, NameRecord, Paginated, Status};

use anyhow::Result;
use jsonrpsee::server::ServerHandle;
use service::ZnsApiServer;

use crate::registry::Registry;

pub(crate) async fn serve_rpc(addr: &str, registry: Registry) -> Result<ServerHandle> {
    let api = JsonRpcApi::new(registry);
    let server = jsonrpsee::server::Server::builder().build(addr).await?;
    let handle = server.start(api.into_rpc());
    tracing::info!("JSON-RPC listening on {addr}");
    Ok(handle)
}
