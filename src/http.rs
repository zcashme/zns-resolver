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

use crate::index::{ChainRow, Event, Registration, SqliteIndex};
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

/// One event-log entry (the indexer API's `Event` shape).
#[derive(Debug, Clone, Serialize)]
struct EventEntry {
    id: i64,
    name: String,
    action: String,
    txid: String,
    height: u64,
    /// The UA involved; `null` for RELEASE (no user-visible address).
    ua: Option<String>,
    // ZcashName has no marketplace or admin signatures — these stay null/zero,
    // kept for zcashname-sdk shape compat.
    price: Option<u64>,
    nonce: u64,
    signature: Option<String>,
    pubkey: Option<String>,
}

/// Event-log result.
#[derive(Debug, Clone, Serialize)]
pub struct EventsResult {
    /// The matching page of events, newest first.
    events: Vec<EventEntry>,
    /// Total matches before pagination.
    total: u64,
}

/// One link of a name's proof chain (`PROOFS.md §2`). `action` and `ua` are
/// the resolver's claims; everything else is chain artifact the wallet's
/// `zns_verify::proof::verify_chain` recomputes. Action names here are the
/// canonical lowercase protocol strings (what the verifier hashes), not the
/// uppercase indexer-API display form.
#[derive(Debug, Clone, Serialize)]
struct ProofLinkEntry {
    action: String,
    ua: String,
    height: u64,
    txid: String,
    action_index: u64,
    /// Full raw transaction, hex.
    tx: String,
    /// Raw block header, hex.
    header: String,
    /// Merkle branch siblings (leaf → root), hex each.
    merkle_branch: Vec<String>,
    merkle_index: u64,
}

/// The `chain` method's result: a name's full applied history with proof
/// context — `DESIGN.md §11.4`'s `/chain/:name`.
#[derive(Debug, Clone, Serialize)]
pub struct ChainResult {
    /// The queried name.
    name: String,
    /// Every applied action in chain order, each verifiable standalone.
    links: Vec<ProofLinkEntry>,
}

// ── RPC trait ────────────────────────────────────────────────────────────────

/// The resolver's JSON-RPC surface (a resolution-only subset of the ZNS
/// indexer API).
#[rpc(server)]
pub trait ZnsApi {
    /// Resolve a query: empty → all registrations; an exact name → that record;
    /// otherwise an address → its names. With `with_proof` on an exact-name
    /// query, the record carries its proof chain (`PROOFS.md §1`): the links
    /// from the name's most recent CLAIM genesis to its tip, each one
    /// self-verifying via `zns_verify::proof::verify_chain`.
    #[method(name = "resolve", blocking)]
    fn resolve(
        &self,
        query: String,
        limit: Option<u64>,
        offset: Option<u64>,
        with_proof: Option<bool>,
    ) -> RpcResult<Value>;

    /// A name's full applied history (all segments, RELEASEs included) with
    /// proof context — for auditing and longest-chain comparison across
    /// resolvers (`DESIGN.md §11.4`, `§19.4`).
    #[method(name = "chain", blocking)]
    fn chain(&self, name: String) -> RpcResult<ChainResult>;

    /// Resolver status: synced height, registered count, and the scanning UIVK.
    #[method(name = "status", blocking)]
    fn status(&self) -> RpcResult<StatusResult>;

    /// Marketplace listings — always empty (ZcashName has no marketplace).
    #[method(name = "listings", blocking)]
    fn listings(&self, limit: Option<u64>, offset: Option<u64>) -> RpcResult<ListingsResult>;

    /// The action event log (newest first), with optional `name`, `action`
    /// (e.g. `"CLAIM"`), and strictly-greater-than `since_height` filters.
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
    /// Path to the resolver's SQLite index. Handlers open it read-only per
    /// call (no DDL, busy-timeout set); WAL allows these readers to run
    /// alongside the sync task's writer.
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
        with_proof: Option<bool>,
    ) -> RpcResult<Value> {
        let idx = open(&self.db)?;
        let limit = limit.unwrap_or(50).min(500) as u32;
        let offset = offset.unwrap_or(0) as u32;

        // Empty → list all; an exact name → that single record; otherwise treat
        // the query as an address and list its names. Proofs attach only to
        // the exact-name form (the only shape a wallet verifies).
        let value = if query.is_empty() {
            entries(idx.list_registrations(limit, offset).map_err(internal)?)
        } else if let Some(reg) = idx.resolve_by_name(&query).map_err(internal)? {
            let mut value = serde_json::to_value(entry(reg)).unwrap();
            if with_proof == Some(true) {
                // The current segment: links since the most recent
                // CLAIM-from-zero genesis (PROOFS.md §1).
                let rows = idx.chain_rows(&query).map_err(internal)?;
                let links = proof_links(&idx, current_segment(&rows))?;
                value["proof"] = serde_json::json!({ "links": links });
            }
            value
        } else {
            entries(idx.registrations_by_ua(&query, limit, offset).map_err(internal)?)
        };
        Ok(value)
    }

    fn chain(&self, name: String) -> RpcResult<ChainResult> {
        let idx = open(&self.db)?;
        let rows = idx.chain_rows(&name).map_err(internal)?;
        let links = proof_links(&idx, &rows)?;
        Ok(ChainResult { name, links })
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
        name: Option<String>,
        action: Option<String>,
        since_height: Option<u64>,
        limit: Option<u64>,
        offset: Option<u64>,
    ) -> RpcResult<EventsResult> {
        // An action filter this resolver can never log (LIST, BUY, SETPRICE —
        // ZcashName has no marketplace) legitimately matches zero events.
        let action = match action.as_deref().map(parse_action_filter) {
            Some(None) => return Ok(EventsResult { events: Vec::new(), total: 0 }),
            Some(some) => some,
            None => None,
        };
        let idx = open(&self.db)?;
        let limit = limit.unwrap_or(50).min(500) as u32;
        let offset = offset.unwrap_or(0) as u32;
        let since = since_height.map(|h| h.min(u32::MAX as u64) as u32);

        let (events, total) = idx
            .events(name.as_deref(), action, since, limit, offset)
            .map_err(internal)?;
        Ok(EventsResult { events: events.into_iter().map(event_entry).collect(), total })
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

/// The current segment of a name's history: the rows from its most recent
/// CLAIM genesis onward. Earlier claim/release cycles do not bear on the
/// current binding (each CLAIM restarts the rcm chain from zero).
fn current_segment(rows: &[ChainRow]) -> &[ChainRow] {
    let start = rows.iter().rposition(|r| r.action == Action::Claim).unwrap_or(0);
    &rows[start..]
}

/// Join proof material onto chain rows, building serveable links. A missing
/// row means this resolver synced without `--validator-rpc`.
fn proof_links(idx: &SqliteIndex, rows: &[ChainRow]) -> RpcResult<Vec<ProofLinkEntry>> {
    rows.iter()
        .map(|r| {
            let m = idx.proof_material(&r.txid).map_err(internal)?.ok_or_else(|| {
                ErrorObjectOwned::owned(
                    -32011,
                    "proof material unavailable (resolver synced without --validator-rpc)",
                    None::<()>,
                )
            })?;
            Ok(ProofLinkEntry {
                // Canonical lowercase protocol strings — what the verifier hashes.
                action: String::from_utf8(r.action.as_bytes().to_vec()).expect("ascii"),
                ua: r.ua.clone(),
                height: r.height as u64,
                txid: hex::encode(r.txid),
                action_index: r.action_index as u64,
                tx: hex::encode(&m.raw_tx),
                header: hex::encode(&m.header),
                merkle_branch: m.merkle_branch.iter().map(hex::encode).collect(),
                merkle_index: m.merkle_index as u64,
            })
        })
        .collect()
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
        ua: (!e.ua.is_empty()).then_some(e.ua),
        price: None,
        nonce: 0,
        signature: None,
        pubkey: None,
    }
}

/// Action names on the wire are uppercase (`"CLAIM"`), per the indexer API.
fn action_name(a: Action) -> &'static str {
    match a {
        Action::Claim => "CLAIM",
        Action::Update => "UPDATE",
        Action::Release => "RELEASE",
    }
}

/// Parse a client's `action` filter (case-insensitive). `None` means the verb
/// can never appear in this resolver's log, so the filter matches nothing.
fn parse_action_filter(s: &str) -> Option<Action> {
    match s.to_ascii_uppercase().as_str() {
        "CLAIM" => Some(Action::Claim),
        "UPDATE" => Some(Action::Update),
        "RELEASE" => Some(Action::Release),
        _ => None,
    }
}

fn open(db: &Path) -> RpcResult<SqliteIndex> {
    SqliteIndex::open_read_only(db).map_err(internal)
}

fn internal(e: impl std::fmt::Display) -> ErrorObjectOwned {
    tracing::error!("rpc: {e}");
    ErrorObjectOwned::owned(-32603, "Internal error", None::<()>)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(action: Action, height: u32) -> ChainRow {
        ChainRow { action, ua: String::new(), height, txid: [0; 32], action_index: 0 }
    }

    #[test]
    fn current_segment_starts_at_last_claim() {
        // claim → update → release → claim → update: the current binding
        // begins at the second CLAIM.
        let rows = vec![
            row(Action::Claim, 1),
            row(Action::Update, 2),
            row(Action::Release, 3),
            row(Action::Claim, 4),
            row(Action::Update, 5),
        ];
        let seg = current_segment(&rows);
        assert_eq!(seg.len(), 2);
        assert_eq!(seg[0].height, 4);

        // A single live segment is served whole.
        let rows = vec![row(Action::Claim, 1), row(Action::Update, 2)];
        assert_eq!(current_segment(&rows).len(), 2);
    }
}
