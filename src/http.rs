//! JSON-RPC API (jsonrpsee) — the resolver's public surface.
//!
//! Matches the live ZNS indexer's method names and response shapes so
//! `zcashname-sdk` clients work against this resolver unchanged. ZcashName
//! (Protocol A) has no marketplace, so `listings`/`events` return empty results;
//! `resolve` and `status` are the load-bearing methods.

use std::path::{Path, PathBuf};

use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::Server;
use jsonrpsee::types::ErrorObjectOwned;
use serde::Serialize;
use serde_json::Value;

use crate::index::{Registration, SqliteIndex};
use zns_verify::Action;

// ── Response shapes (subset of the indexer's, resolution-only) ───────────────

#[derive(Debug, Clone, Serialize)]
struct RegistrationEntry {
    name: String,
    address: String,
    txid: String,
    height: u64,
    last_action: String,
    // ZcashName binds via the on-chain note commitment, not signatures or
    // listings — these are always null, kept for zcashname-sdk shape compat.
    nonce: u64,
    signature: Option<String>,
    listing: Option<Value>,
}

/// Resolver status (subset of the indexer's `StatusResult`).
#[derive(Debug, Clone, Serialize)]
pub struct StatusResult {
    /// Highest block applied to the index.
    pub synced_height: u64,
    /// The UIVK this resolver scans with.
    pub uivk: String,
    /// Number of names currently registered.
    pub registered: u64,
    /// Always empty — ZcashName has no admin key.
    pub admin_pubkey: String,
    /// Always zero — ZcashName has no marketplace.
    pub listed: u64,
}

/// Marketplace listings result (always empty here).
#[derive(Debug, Clone, Serialize)]
pub struct ListingsResult {
    /// The listings (empty).
    pub listings: Vec<Value>,
    /// Total count (zero).
    pub total: u64,
}

/// Event-log result (always empty here).
#[derive(Debug, Clone, Serialize)]
pub struct EventsResult {
    /// The events (empty).
    pub events: Vec<Value>,
    /// Total count (zero).
    pub total: u64,
}

// ── RPC trait ────────────────────────────────────────────────────────────────

/// The resolver's JSON-RPC surface (a resolution-only subset of the ZNS
/// indexer API).
#[rpc(server)]
pub trait ZnsApi {
    /// Resolve a query: empty → all registrations; an exact name → that record;
    /// otherwise an address → its names. `with_proof` is reserved (inclusion
    /// proofs are not yet implemented).
    #[method(name = "resolve", blocking)]
    fn resolve(
        &self,
        query: String,
        limit: Option<u64>,
        offset: Option<u64>,
        with_proof: Option<bool>,
    ) -> RpcResult<Value>;

    /// Resolver status: synced height, registered count, and the scanning UIVK.
    #[method(name = "status", blocking)]
    fn status(&self) -> RpcResult<StatusResult>;

    /// Marketplace listings — always empty (ZcashName has no marketplace).
    #[method(name = "listings", blocking)]
    fn listings(&self, limit: Option<u64>, offset: Option<u64>) -> RpcResult<ListingsResult>;

    /// Action event log — always empty for this resolver.
    #[method(name = "events", blocking)]
    fn events(
        &self,
        name: Option<String>,
        action: Option<String>,
        since_height: Option<u64>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<EventsResult>;
}

/// Per-call context: enough to open the read-only index.
pub struct RpcContext {
    /// Path to the resolver's SQLite index (handlers open it per call; WAL
    /// allows concurrent readers alongside the sync task's writer).
    pub db: PathBuf,
    /// addr_reg's UIVK, surfaced in `status` for clients.
    pub uivk: String,
}

impl ZnsApiServer for RpcContext {
    fn resolve(
        &self,
        query: String,
        limit: Option<u64>,
        offset: Option<u64>,
        _with_proof: Option<bool>,
    ) -> RpcResult<Value> {
        let idx = open(&self.db)?;
        let limit = limit.unwrap_or(50).min(500) as u32;
        let offset = offset.unwrap_or(0) as u32;

        // Empty → list all; an exact name → that single record; otherwise treat
        // the query as an address and list its names. (Inclusion proofs are a
        // later task; `with_proof` is accepted but ignored for now.)
        let value = if query.is_empty() {
            entries(idx.list_registrations(limit, offset).map_err(internal)?)
        } else if let Some(reg) = idx.resolve_by_name(&query).map_err(internal)? {
            serde_json::to_value(entry(reg)).unwrap()
        } else {
            entries(idx.registrations_by_ua(&query, limit, offset).map_err(internal)?)
        };
        Ok(value)
    }

    fn status(&self) -> RpcResult<StatusResult> {
        let idx = open(&self.db)?;
        Ok(StatusResult {
            synced_height: idx.last_scanned_height().map_err(internal)?.unwrap_or(0) as u64,
            uivk: self.uivk.clone(),
            registered: idx.name_count().map_err(internal)?,
            admin_pubkey: String::new(),
            listed: 0,
        })
    }

    fn listings(&self, _limit: Option<u64>, _offset: Option<u64>) -> RpcResult<ListingsResult> {
        Ok(ListingsResult { listings: Vec::new(), total: 0 })
    }

    fn events(
        &self,
        _name: Option<String>,
        _action: Option<String>,
        _since_height: Option<u64>,
        _limit: Option<u64>,
        _offset: Option<u64>,
    ) -> RpcResult<EventsResult> {
        Ok(EventsResult { events: Vec::new(), total: 0 })
    }
}

/// Serve the JSON-RPC API on `addr` until the server stops.
pub async fn serve(addr: &str, ctx: RpcContext) -> anyhow::Result<()> {
    let server = Server::builder().build(addr).await?;
    let handle = server.start(ctx.into_rpc());
    tracing::info!("resolver JSON-RPC listening on {addr}");
    handle.stopped().await;
    Ok(())
}

// ── internals ────────────────────────────────────────────────────────────────

fn entry(r: Registration) -> RegistrationEntry {
    RegistrationEntry {
        name: r.name,
        address: r.ua,
        txid: hex::encode(r.txid),
        height: r.height as u64,
        last_action: action_name(r.last_action).to_string(),
        nonce: 0,
        signature: None,
        listing: None,
    }
}

fn entries(regs: Vec<Registration>) -> Value {
    serde_json::to_value(regs.into_iter().map(entry).collect::<Vec<_>>()).unwrap()
}

fn action_name(a: Action) -> &'static str {
    match a {
        Action::Claim => "claim",
        Action::Update => "update",
        Action::Release => "release",
    }
}

fn open(db: &Path) -> RpcResult<SqliteIndex> {
    SqliteIndex::open(db).map_err(internal)
}

fn internal(e: impl std::fmt::Display) -> ErrorObjectOwned {
    tracing::error!("rpc: {e}");
    ErrorObjectOwned::owned(-32603, "Internal error", None::<()>)
}
