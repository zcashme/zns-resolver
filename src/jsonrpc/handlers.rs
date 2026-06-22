//! Implementation of the JSON-RPC API.
//!
//! This file owns both the `ZnsApi` trait (the contract) and the `JsonRpcApi`
//! concrete type that implements it. This avoids tricky visibility issues
//! with the jsonrpsee proc macro across submodules.

use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use serde_json::Value;
use zns_verify::Action;

use crate::registry::Registry;

use super::models::{to_name_event, to_name_record, NameEvent, Paginated, Status};

/// Public JSON-RPC API for the ZNS resolver.
///
/// All methods are marked `blocking` because they ultimately perform
/// synchronous SQLite reads via the reader pool.
#[rpc(server)]
pub trait ZnsApi {
    /// Resolve a name, perform a reverse lookup by address prefix, or list names.
    ///
    /// This is the current legacy surface while we evolve the API.
    /// Prefer more specific methods in the future (`resolve_name`, `list_names`, etc.).
    #[method(name = "resolve", blocking)]
    fn resolve(&self, query: String, limit: Option<u64>, offset: Option<u64>) -> RpcResult<Value>;

    /// Current sync status and basic resolver metadata.
    #[method(name = "status", blocking)]
    fn status(&self) -> RpcResult<Status>;

    /// Paginated event history (the append-only log of all claims/updates/releases).
    #[method(name = "events", blocking)]
    fn events(
        &self,
        name: Option<String>,
        action: Option<String>,
        since_height: Option<u64>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Paginated<NameEvent>>;
}

/// The actual implementation of the ZNS JSON-RPC API.
///
/// This type owns the API behavior. It wraps the lower-level `Registry`
/// (which is responsible for safe concurrent storage access) rather than
/// exposing the registry directly as the API.
pub struct JsonRpcApi {
    registry: Registry,
}

impl JsonRpcApi {
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }
}

fn rpc_err(e: impl std::fmt::Display) -> ErrorObjectOwned {
    tracing::error!(error = %e, "rpc handler failed");
    ErrorObjectOwned::owned(-32603, "Internal error", None::<()>)
}

impl ZnsApiServer for JsonRpcApi {
    fn resolve(&self, query: String, limit: Option<u64>, offset: Option<u64>) -> RpcResult<Value> {
        let limit = limit.unwrap_or(50).min(500) as u32;
        let offset = offset.unwrap_or(0) as u32;

        let value = if query.is_empty() {
            let regs = self
                .registry
                .list_registrations(limit, offset)
                .map_err(rpc_err)?;
            let items: Vec<_> = regs.into_iter().map(to_name_record).collect();
            serde_json::to_value(items).map_err(rpc_err)?
        } else if let Some(reg) = self.registry.resolve_by_name(&query).map_err(rpc_err)? {
            serde_json::to_value(to_name_record(reg)).map_err(rpc_err)?
        } else {
            let regs = self
                .registry
                .registrations_by_ua(&query, limit, offset)
                .map_err(rpc_err)?;
            let items: Vec<_> = regs.into_iter().map(to_name_record).collect();
            serde_json::to_value(items).map_err(rpc_err)?
        };
        Ok(value)
    }

    fn status(&self) -> RpcResult<Status> {
        let checkpoint = self.registry.checkpoint().map_err(rpc_err)?;
        let viewing_key = self.registry.registry_ufvk().map_err(rpc_err)?;
        let registered = self.registry.name_count().map_err(rpc_err)?;

        let (synced_height, chain_tip_height, synced, blocks_behind) = match checkpoint {
            Some(c) => {
                let sh = c.scanned_height as u64;
                match c.chain_tip_height {
                    Some(tip) => {
                        let synced = c.scanned_height >= tip;
                        (
                            sh,
                            tip as u64,
                            synced,
                            if synced { 0 } else { (tip - c.scanned_height) as u64 },
                        )
                    }
                    None => (sh, 0, false, 0),
                }
            }
            None => (0, 0, false, 0),
        };

        Ok(Status {
            synced_height,
            chain_tip_height,
            synced,
            blocks_behind,
            viewing_key: viewing_key.unwrap_or_default(),
            registered,
        })
    }

    fn events(
        &self,
        name: Option<String>,
        action: Option<String>,
        since_height: Option<u64>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Paginated<NameEvent>> {
        let action = match action.as_deref().map(|s| Action::from_bytes(s.as_bytes())) {
            Some(None) => {
                return Ok(Paginated {
                    items: vec![],
                    total: 0,
                    limit: 0,
                    offset: 0,
                });
            }
            Some(some) => some,
            None => None,
        };

        let limit = limit.unwrap_or(50).min(500) as u32;
        let offset = offset.unwrap_or(0) as u32;
        let since = since_height.map(|h| h.min(u32::MAX as u64) as u32);

        let (events, total) = self
            .registry
            .events(name.as_deref(), action, since, limit, offset)
            .map_err(rpc_err)?;

        let items = events.into_iter().map(to_name_event).collect();

        Ok(Paginated {
            items,
            total,
            limit: limit as u64,
            offset: offset as u64,
        })
    }
}
