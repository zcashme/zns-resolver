//! Implementation of the JSON-RPC API.
//!

use jsonrpsee::core::async_trait;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use zns_verify::Action;

use crate::registry::core;
use crate::registry::Db;

use super::records::{to_name_event, to_name_record, NameEvent, NameRecord, Paginated, Status};

/// Public JSON-RPC API for the ZNS resolver.
#[rpc(server)]
pub trait ZnsApi {
    /// Resolve a name to its current binding. Returns `null` if the name is
    /// not registered (or has been released).
    #[method(name = "resolve")]
    async fn resolve(&self, name: String) -> RpcResult<Option<NameRecord>>;

    /// List all currently registered names, paginated.
    #[method(name = "list_names")]
    async fn list_names(
        &self,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Paginated<NameRecord>>;

    /// Reverse lookup: find all names currently bound to a unified address.
    #[method(name = "reverse_lookup")]
    async fn reverse_lookup(
        &self,
        address: String,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Paginated<NameRecord>>;

    /// Current sync status and basic resolver metadata.
    #[method(name = "status")]
    async fn status(&self) -> RpcResult<Status>;

    /// Paginated event history (the append-only log of all claims/updates/releases).
    #[method(name = "events")]
    async fn events(
        &self,
        name: Option<String>,
        action: Option<String>,
        since_height: Option<u64>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Paginated<NameEvent>>;
}

pub struct JsonRpcApi {
    db: Db,
}

impl JsonRpcApi {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

/// Categorizes handler failures for the wire.
///
/// `InvalidParams` → JSON-RPC `-32602`; `Internal` → `-32603` + a log line.
/// The underlying `RegistryError` is never serialized to the client.
#[derive(thiserror::Error, Debug)]
enum RpcError {
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("internal error")]
    Internal(#[from] rusqlite::Error),
}

impl From<RpcError> for ErrorObjectOwned {
    fn from(e: RpcError) -> Self {
        match e {
            RpcError::InvalidParams(msg) => {
                ErrorObjectOwned::owned(-32602, "Invalid params", Some(msg))
            }
            RpcError::Internal(inner) => {
                tracing::error!(error = %inner, "rpc handler failed");
                ErrorObjectOwned::owned(-32603, "Internal error", None::<()>)
            }
        }
    }
}

/// Clamp client-supplied pagination to safe bounds.
fn clamp_pagination(limit: Option<u64>, offset: Option<u64>) -> (u32, u32) {
    let limit = limit.unwrap_or(50).clamp(1, 500) as u32;
    let offset = offset.unwrap_or(0) as u32;
    (limit, offset)
}

#[async_trait]
impl ZnsApiServer for JsonRpcApi {
    async fn resolve(&self, name: String) -> RpcResult<Option<NameRecord>> {
        let conn = self.db.lock();
        let reg = core::resolve_by_name(&conn, &name)
            .map_err(RpcError::from)?;
        Ok(reg.map(to_name_record))
    }

    async fn list_names(
        &self,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Paginated<NameRecord>> {
        let (limit_u32, offset_u32) = clamp_pagination(limit, offset);
        let conn = self.db.lock();
        let (regs, total) = core::list_registrations(&conn, limit_u32, offset_u32)
            .map_err(RpcError::from)?;
        let items = regs.into_iter().map(to_name_record).collect();
        Ok(Paginated {
            items,
            total,
            limit: limit_u32 as u64,
            offset: offset_u32 as u64,
        })
    }

    async fn reverse_lookup(
        &self,
        address: String,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Paginated<NameRecord>> {
        let (limit_u32, offset_u32) = clamp_pagination(limit, offset);
        let conn = self.db.lock();
        let (regs, total) = core::registrations_by_ua(&conn, &address, limit_u32, offset_u32)
            .map_err(RpcError::from)?;
        let items = regs.into_iter().map(to_name_record).collect();
        Ok(Paginated {
            items,
            total,
            limit: limit_u32 as u64,
            offset: offset_u32 as u64,
        })
    }

    async fn status(&self) -> RpcResult<Status> {
        let conn = self.db.lock();
        let checkpoint = core::checkpoint(&conn).map_err(RpcError::from)?;
        let viewing_key = core::registry_ufvk(&conn).map_err(RpcError::from)?;
        let registered = core::name_count(&conn).map_err(RpcError::from)?;

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
                            if synced {
                                0
                            } else {
                                (tip - c.scanned_height) as u64
                            },
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

    async fn events(
        &self,
        name: Option<String>,
        action: Option<String>,
        since_height: Option<u64>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Paginated<NameEvent>> {
        let action = match action {
            Some(s) => Some(Action::from_bytes(s.as_bytes()).ok_or_else(|| {
                RpcError::InvalidParams(format!(
                    "invalid action '{s}': expected claim, update, or release"
                ))
            })?),
            None => None,
        };

        let (limit_u32, offset_u32) = clamp_pagination(limit, offset);
        let since = since_height.map(|h| h.min(u32::MAX as u64) as u32);

        let conn = self.db.lock();
        let (events, total) = core::events(
            &conn,
            name.as_deref(),
            action,
            since,
            limit_u32,
            offset_u32,
        )
        .map_err(RpcError::from)?;

        let items = events.into_iter().map(to_name_event).collect();

        Ok(Paginated {
            items,
            total,
            limit: limit_u32 as u64,
            offset: offset_u32 as u64,
        })
    }
}
