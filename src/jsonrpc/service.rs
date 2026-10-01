//! Implementation of the JSON-RPC API.
//!

use jsonrpsee::core::async_trait;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use tokio::sync::watch;
use zns_verify::Action;

use crate::registry::core;
use crate::registry::Db;

use zcash_address::ZcashAddress;

use super::records::{EventsPage, NameEvent, NameRecord, Paginated, ResolveResult, Status};

/// The network head as published live by the tip publisher: `None` until the
/// first poll lands.
type ChainTip = watch::Receiver<Option<u32>>;

/// Public JSON-RPC API for the ZNS resolver.
///
/// One query verb: `resolve` dispatches on the query — an exact name
/// resolves to its record (or `null`), the empty query lists all
/// registrations, an address lists the names bound to it. `events`/// exposes the append-only log; `status` the sync state. Params are
/// accepted as named fields (jsonrpsee also accepts positional arrays).
#[rpc(server)]
pub trait ZnsApi {
    /// Resolve a query to its current binding state.
    ///
    /// - exact name → the record, or `null` if unregistered/released;
    /// - empty query → all currently registered names, paginated;
    /// - an address → the names currently bound to it, paginated.
    #[method(name = "resolve")]
    async fn resolve(
        &self,
        query: String,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<ResolveResult>;

    /// List all currently registered names, paginated. Typed alternative to
    /// `resolve` with the empty query.
    #[method(name = "list_names")]
    async fn list_names(
        &self,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Paginated<NameRecord>>;

    /// Reverse lookup: all names currently bound to a unified address.
    /// Typed alternative to `resolve` with an address query.
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
    ) -> RpcResult<EventsPage>;
}

pub struct JsonRpcApi {
    db: Db,
    /// The live chain head for `status` — never persisted.
    tip: ChainTip,
}

impl JsonRpcApi {
    pub fn new(db: Db, tip: ChainTip) -> Self {
        Self { db, tip }
    }
}

/// Categorizes handler failures for the wire.
///
/// `InvalidParams` → JSON-RPC `-32602`; `Internal` → `-32603` + a log line.
/// The underlying SQLite error is never serialized to the client.
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
    async fn resolve(
        &self,
        query: String,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<ResolveResult> {
        let (limit_u32, offset_u32) = clamp_pagination(limit, offset);
        let conn = self.db.lock();

        // The empty query lists all registrations — the page-through form
        // explorers and sitemaps build on.
        if query.is_empty() {
            let (regs, _) =
                core::list_registrations(&conn, limit_u32, offset_u32).map_err(RpcError::from)?;
            return Ok(ResolveResult::Many(
                regs.into_iter().map(NameRecord::from).collect(),
            ));
        }

        // An address queries the names currently bound to it.
        if query.parse::<ZcashAddress>().is_ok() {
            let (regs, _) = core::registrations_by_ua(&conn, &query, limit_u32, offset_u32)
                .map_err(RpcError::from)?;
            return Ok(ResolveResult::Many(
                regs.into_iter().map(NameRecord::from).collect(),
            ));
        }

        // Otherwise the query is a name: exact lookup, record or `null`.
        let reg = core::resolve_by_name(&conn, &query).map_err(RpcError::from)?;
        Ok(ResolveResult::Exact(reg.map(NameRecord::from)))
    }

    /// List all currently registered names, paginated.
    async fn list_names(
        &self,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Paginated<NameRecord>> {
        let (limit_u32, offset_u32) = clamp_pagination(limit, offset);
        let conn = self.db.lock();
        let (regs, total) =
            core::list_registrations(&conn, limit_u32, offset_u32).map_err(RpcError::from)?;
        let items = regs.into_iter().map(NameRecord::from).collect();
        Ok(Paginated {
            items,
            total,
            limit: limit_u32 as u64,
            offset: offset_u32 as u64,
        })
    }

    /// Reverse lookup: all names currently bound to a unified address.
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
        let items = regs.into_iter().map(NameRecord::from).collect();
        Ok(Paginated {
            items,
            total,
            limit: limit_u32 as u64,
            offset: offset_u32 as u64,
        })
    }

    async fn status(&self) -> RpcResult<Status> {
        let conn = self.db.lock();
        let position = core::checkpoint(&conn).map_err(RpcError::from)?;
        let viewing_key = core::registry_ufvk(&conn).map_err(RpcError::from)?;
        let registered = core::name_count(&conn).map_err(RpcError::from)?;
        drop(conn);

        // The network path's live observation: None until the first poll lands.
        let tip = *self.tip.borrow();

        // The verdict needs both facts: our durable position and the network's
        // live head. Either missing — not synced.
        let synced = match (position.as_ref(), tip) {
            (Some(cursor), Some(tip)) => u32::from(cursor.height) >= tip,
            _ => false,
        };

        Ok(Status {
            synced_height: position.map_or(0, |c| u64::from(u32::from(c.height))),
            synced,
            viewing_key,
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
    ) -> RpcResult<EventsPage> {
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
        let (events, total) =
            core::events(&conn, name.as_deref(), action, since, limit_u32, offset_u32)
                .map_err(RpcError::from)?;

        let events = events.into_iter().map(NameEvent::from).collect();

        Ok(EventsPage {
            events,
            total,
            limit: limit_u32 as u64,
            offset: offset_u32 as u64,
        })
    }
}
