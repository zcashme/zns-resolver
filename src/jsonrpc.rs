//! JSON-RPC read API

use std::path::{Path, PathBuf};

use anyhow::Result;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::Server;
use jsonrpsee::types::ErrorObjectOwned;
use serde::Serialize;
use serde_json::Value;
use zns_verify::Action;

use crate::names::{Db, Event, Registration};

// ── JSON-RPC (thin consumer API) ──────────────────────────────────────────────
//
// Handlers open a read-only DB handle per request. No crypto here — just serving
// what the indexer already verified.

#[derive(Debug, Clone, Serialize)]
struct RegistrationEntry {
    name: String,
    address: String,
    txid: String,
    height: u64,
    last_action: String,
    nonce: u64,
    signature: Option<String>,
    listing: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusResult {
    synced_height: u64,
    chain_tip_height: u64,
    synced: bool,
    blocks_behind: u64,
    uivk: String,
    registered: u64,
    admin_pubkey: String,
    listed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ListingsResult {
    listings: Vec<Value>,
    total: u64,
}

#[derive(Debug, Clone, Serialize)]
struct EventEntry {
    id: i64,
    name: String,
    action: String,
    txid: String,
    height: u64,
    action_index: u64,
    ua: Option<String>,
    price: Option<u64>,
    nonce: u64,
    signature: Option<String>,
    pubkey: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventsResult {
    events: Vec<EventEntry>,
    total: u64,
}

/// Public resolver API. `blocking` methods run on jsonrpsee's thread pool since
/// SQLite is synchronous.
#[rpc(server)]
trait ZnsApi {
    /// Lookup by name (exact), UA prefix (reverse lookup), or list all if query empty.
    #[method(name = "resolve", blocking)]
    fn resolve(
        &self,
        query: String,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Value>;

    /// Sync progress: scanned height vs chain tip, registration count.
    #[method(name = "status", blocking)]
    fn status(&self) -> RpcResult<StatusResult>;

    /// Placeholder for marketplace listings (not implemented in resolver).
    #[method(name = "listings", blocking)]
    fn listings(&self, limit: Option<u64>, offset: Option<u64>) -> RpcResult<ListingsResult>;

    /// Paginated lifecycle event log with optional filters.
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

impl ZnsApiServer for PathBuf {
    fn resolve(
        &self,
        query: String,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<Value> {
        let db = open_for_rpc(self)?;
        let limit = limit.unwrap_or(50).min(500) as u32;
        let offset = offset.unwrap_or(0) as u32;

        let value = if query.is_empty() {
            entries(db.list_registrations(limit, offset).map_err(rpc_err)?)
        } else if let Some(reg) = db.resolve_by_name(&query).map_err(rpc_err)? {
            serde_json::to_value(entry(reg)).unwrap()
        } else {
            entries(
                db.registrations_by_ua(&query, limit, offset)
                    .map_err(rpc_err)?,
            )
        };
        Ok(value)
    }

    fn status(&self) -> RpcResult<StatusResult> {
        let db = open_for_rpc(self)?;
        let cp = db.checkpoint().map_err(rpc_err)?;
        let (synced_height, chain_tip_height, synced, blocks_behind) = match cp {
            Some(c) => {
                let synced_height = c.scanned_height as u64;
                match c.chain_tip_height {
                    Some(tip) => {
                        let synced = c.scanned_height >= tip;
                        (
                            synced_height,
                            tip as u64,
                            synced,
                            if synced {
                                0
                            } else {
                                (tip - c.scanned_height) as u64
                            },
                        )
                    }
                    None => (synced_height, 0, false, 0),
                }
            }
            None => (0, 0, false, 0),
        };
        let uivk = db.registry_uivk().map_err(rpc_err)?.unwrap_or_default();
        Ok(StatusResult {
            synced_height,
            chain_tip_height,
            synced,
            blocks_behind,
            uivk,
            registered: db.name_count().map_err(rpc_err)?,
            admin_pubkey: String::new(),
            listed: 0,
        })
    }

    fn listings(&self, _limit: Option<u64>, _offset: Option<u64>) -> RpcResult<ListingsResult> {
        Ok(ListingsResult {
            listings: Vec::new(),
            total: 0,
        })
    }

    fn events(
        &self,
        name: Option<String>,
        action: Option<String>,
        since_height: Option<u64>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<EventsResult> {
        let action = match action.as_deref().map(parse_action_filter) {
            Some(None) => {
                return Ok(EventsResult {
                    events: Vec::new(),
                    total: 0,
                })
            }
            Some(some) => some,
            None => None,
        };
        let db = open_for_rpc(self)?;
        let limit = limit.unwrap_or(50).min(500) as u32;
        let offset = offset.unwrap_or(0) as u32;
        let since = since_height.map(|h| h.min(u32::MAX as u64) as u32);

        let (events, total) = db
            .events(name.as_deref(), action, since, limit, offset)
            .map_err(rpc_err)?;
        Ok(EventsResult {
            events: events.into_iter().map(event_entry).collect(),
            total,
        })
    }
}

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

fn event_entry(e: Event) -> EventEntry {
    EventEntry {
        id: e.id,
        name: e.name,
        action: action_name(e.action).to_string(),
        txid: hex::encode(e.txid),
        height: e.height as u64,
        action_index: e.action_index as u64,
        ua: (!e.ua.is_empty()).then_some(e.ua),
        price: None,
        nonce: 0,
        signature: None,
        pubkey: None,
    }
}

fn action_name(a: Action) -> &'static str {
    match a {
        Action::Claim => "CLAIM",
        Action::Update => "UPDATE",
        Action::Release => "RELEASE",
    }
}

fn parse_action_filter(s: &str) -> Option<Action> {
    match s.to_ascii_uppercase().as_str() {
        "CLAIM" => Some(Action::Claim),
        "UPDATE" => Some(Action::Update),
        "RELEASE" => Some(Action::Release),
        _ => None,
    }
}

fn open_for_rpc(db: &Path) -> RpcResult<Db> {
    Db::open_for_rpc(db).map_err(rpc_err)
}

fn rpc_err(e: impl std::fmt::Display) -> ErrorObjectOwned {
    tracing::error!("rpc: {e}");
    ErrorObjectOwned::owned(-32603, "Internal error", None::<()>)
}

pub(crate) async fn serve_rpc(addr: &str, db: PathBuf) -> Result<()> {
    let server = Server::builder().build(addr).await?;
    let handle = server.start(db.into_rpc());
    tracing::info!("JSON-RPC listening on {addr}");
    handle.stopped().await;
    Ok(())
}
