//! # ZNS resolver
//!
//! ## What problem does this solve?
//!
//! Zcash Names (ZNS) binds human-readable names to Orchard shielded addresses.
//! The binding lives in Orchard note commitments on the Zcash chain. This resolver:
//!
//! 1. **Observes** shielded notes sent to the registry's viewing key
//! 2. **Verifies** each note actually commits to `(action, name, ua, prev_rcm)`
//! 3. **Indexes** the current tip of each name's per-name chain
//! 4. **Serves** lookups and optional audit proofs (tx + header + merkle branch)
//!
//! ## Architecture (mono-file)
//!
//! ```text
//! lightwalletd ──► observe_batch ──► apply_batch ──► SQLite
//!                                        │                  │
//!                                        ▼                  ▼
//!                              materialize_proofs      JSON-RPC server
//!                              (optional validator)    resolve / chain / events
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use group::ff::PrimeField;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::RpcResult;
use jsonrpsee::http_client::HttpClient;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::rpc_params;
use jsonrpsee::server::Server;
use jsonrpsee::types::ErrorObjectOwned;
use orchard::keys::PreparedIncomingViewingKey;
use pasta_curves::pallas;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use seer_sync::chain::{self, ChainError, LwdClient, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::proto::CompactBlock;
use seer_sync::{parse_orchard, BlockHash, BlockHeight, TxId};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BranchId, Network, Parameters};
use zcash_protocol::memo::MemoBytes;
use zns_verify::{
    chain::prev_rcm_for, memo, note_commitment_cmx, zns_psi_rcm, Action, ParsedMemo, Tip,
};

// ── hardcoded configuration ───────────────────────────────────────────────────
//
// Production would load these from env/config; kept inline for the mono-file layout.

/// Registry **incoming viewing key** (UIVK). Lets us trial-decrypt Orchard outputs
/// addressed to the registry without spending authority. Visibility invariant.
const UIVK: &str = "uivktest18a7ht78cymvm3sxdw9myrr04nrnj8nvrqdjhadj8dp3cv8pm2dqszuxnjrjyp6xyf0svtzjxnq3976l5sxzd09mmx9g6sj9xpp67ympwsrv6wen5ye25jhvq0l8zz937hcgtp90rwhjq0m02rf7qk6wmvrny26r2vt0laztqx4kgx0jqtdwu38ld0hx53m0u20rjny20gpxneavfze7aqqft5vs0jraaqed4974avkx4c3qass3prsqq2fdx08jllet4uuxzz8zmrem8xcwaya9v50l046lp2c9uuyrkp0r8jja5vlzday32pgq4cccqd2rjvtlsfnn9lne9cchrcfgn87jlx9";
const NETWORK: Network = Network::TestNetwork;
/// gRPC endpoint for compact blocks + raw tx fetch (via `seer-sync`).
const LIGHTWALLETD: &str = "https://testnet.zec.rocks:443";
const DB_PATH: &str = "zns-resolver.sqlite";
const RPC_ADDR: &str = "127.0.0.1:8080";
/// Optional `zcashd` JSON-RPC for block headers and tx merkle positions (derivability).
/// Without this, `chain` / `resolve?with_proof` return proof-unavailable errors.
const VALIDATOR_RPC: Option<&str> = None;
/// First block height to scan when no checkpoint exists (registry launch height).
const SCAN_BIRTHDAY: u32 = 1_687_104;
/// Reorgs deeper than this trigger a full rescan instead of incremental rewind.
const REORG_SHALLOW_MAX: u32 = 30;
const RETRY_DELAY: Duration = Duration::from_secs(5);
/// How often the tip watcher polls `GetLatestBlock` while idle at chain tip.
const TIP_WATCH_INTERVAL: Duration = Duration::from_secs(10);
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

// SQLite schema — four logical areas:
//   scan_state     — checkpoint after commit (how far we've scanned)
//   name_events    — append-only history of every verified lifecycle event
//   names          — materialized current tip per name (fast resolve)
//   proof_material — tx + header + merkle branch for optional client audit
const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS scan_state (
    id               INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
    height           INTEGER NOT NULL,
    hash             BLOB,
    chain_tip_height INTEGER,
    chain_tip_hash   BLOB
);

CREATE TABLE IF NOT EXISTS name_events (
    name         TEXT    NOT NULL,
    height       INTEGER NOT NULL,
    action       TEXT    NOT NULL CHECK (action IN ('claim', 'update', 'release')),
    ua           TEXT    NOT NULL,
    prev_rcm     BLOB    NOT NULL,
    rcm          BLOB    NOT NULL,
    psi          BLOB    NOT NULL,
    cmx          BLOB    NOT NULL,
    txid         BLOB    NOT NULL,
    action_index INTEGER NOT NULL,
    raw_tx       BLOB    NOT NULL,
    PRIMARY KEY (name, height)
);
CREATE INDEX IF NOT EXISTS idx_name_events_height ON name_events (height);
CREATE INDEX IF NOT EXISTS idx_name_events_txid ON name_events (txid);

CREATE TABLE IF NOT EXISTS names (
    name         TEXT    NOT NULL PRIMARY KEY,
    height       INTEGER NOT NULL,
    action       TEXT    NOT NULL CHECK (action IN ('claim', 'update', 'release')),
    ua           TEXT    NOT NULL,
    prev_rcm     BLOB    NOT NULL,
    rcm          BLOB    NOT NULL,
    psi          BLOB    NOT NULL,
    cmx          BLOB    NOT NULL,
    txid         BLOB    NOT NULL,
    action_index INTEGER NOT NULL,
    raw_tx       BLOB    NOT NULL
);

CREATE TABLE IF NOT EXISTS proof_material (
    txid          BLOB    NOT NULL PRIMARY KEY,
    height        INTEGER NOT NULL,
    raw_tx        BLOB    NOT NULL,
    header        BLOB    NOT NULL,
    merkle_branch BLOB    NOT NULL,
    merkle_index  INTEGER NOT NULL
) WITHOUT ROWID;
"#;

// ── types ─────────────────────────────────────────────────────────────────────

/// Persisted `scan_state` row: how far we've scanned and the chain tip at last commit.
///
/// For a live tip from the watcher, only `height` / `hash` are set (`chain_tip_*` are `None`).
/// After `apply_batch`, all four fields are written to SQLite.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cursor {
    height: u32,
    hash: Option<[u8; 32]>,
    chain_tip_height: Option<u32>,
    chain_tip_hash: Option<[u8; 32]>,
}

impl Cursor {
    /// Scan seam or live chain tip (`GetLatestBlock`).
    fn at(height: u32, hash: Option<[u8; 32]>) -> Self {
        Self {
            height,
            hash,
            chain_tip_height: None,
            chain_tip_hash: None,
        }
    }

    /// Checkpoint to persist after a batch: scanned position + tip snapshot.
    fn checkpoint(scanned: Self, live_tip: Self) -> Self {
        Self {
            height: scanned.height,
            hash: scanned.hash,
            chain_tip_height: Some(live_tip.height),
            chain_tip_hash: live_tip.hash,
        }
    }
}

/// A registry Orchard note we decrypted — carries the note (for binding
/// verification) and memo (untrusted, parsed separately).
struct DecryptedNote {
    note: orchard::Note,
    /// On-chain note commitment (cmx) from the action; must match our recomputation.
    cmx: [u8; 32],
    memo: MemoBytes,
    txid: [u8; 32],
    height: u32,
    /// Index of this Orchard action within the transaction's action list.
    action_index: usize,
    raw_tx: Vec<u8>,
}

/// A verified ZNS name note: Transition + Binding passed for one decrypted note.
struct NameNote {
    name: String,
    ua: String,
    action: Action,
    prev_rcm: [u8; 32],
    rcm: [u8; 32],
    psi: [u8; 32],
    cmx: [u8; 32],
    txid: [u8; 32],
    height: u32,
    action_index: usize,
    raw_tx: Vec<u8>,
}

/// Current registration: a name's live tip (absent from `names` if released).
#[derive(Debug, Clone)]
struct Registration {
    name: String,
    ua: String,
    txid: [u8; 32],
    height: u32,
    last_action: Action,
}

/// One verified lifecycle step in a name's per-name chain (from `name_events`).
#[derive(Debug, Clone)]
struct ChainRow {
    action: Action,
    ua: String,
    height: u32,
    txid: [u8; 32],
    action_index: usize,
}

/// Event log entry for the `events` RPC (includes DB rowid).
#[derive(Debug, Clone)]
struct Event {
    id: i64,
    name: String,
    action: Action,
    ua: String,
    txid: [u8; 32],
    height: u32,
}

/// Everything a client needs to independently verify a binding on-chain (derivability).
#[derive(Debug, Clone)]
struct ProofMaterial {
    raw_tx: Vec<u8>,
    header: Vec<u8>,
    /// Sibling hashes from leaf txid up to the block's tx merkle root.
    merkle_branch: Vec<[u8; 32]>,
    merkle_index: u32,
}

/// Thin SQLite wrapper — all writes go through `apply_batch` / `rewind`.
struct Db {
    conn: Connection,
}

/// HTTP client to a Zcash full node (`getblock` for header + tx ordering).
struct ValidatorClient {
    client: HttpClient,
}

/// Per-block context needed to build merkle inclusion proofs.
struct BlockContext {
    header: Vec<u8>,
    /// Txids in block order (internal byte order, not display hex).
    txids: Vec<[u8; 32]>,
}

// ── database ──────────────────────────────────────────────────────────────────
//
// Write path: `apply_batch` runs Visibility→Transition→Binding in one transaction,
// then updates the checkpoint. Read path: open read-only connections for RPC handlers.

impl Db {
    fn open_for_indexer(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    fn open_for_rpc(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        Ok(Self { conn })
    }

    fn checkpoint(&self) -> rusqlite::Result<Option<Cursor>> {
        self.conn
            .query_row(
                "SELECT height, hash, chain_tip_height, chain_tip_hash FROM scan_state WHERE id = 0",
                [],
                row_to_cursor,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Process one scanned batch: for each decrypted note, run Transition + Binding,
    /// append to `name_events`, update `names` tip, then checkpoint.
    ///
    /// Returns only name notes that were actually indexed (binding + transition passed).
    fn apply_batch(
        &self,
        scanned: Cursor,
        live_tip: Cursor,
        decrypted: &[DecryptedNote],
    ) -> rusqlite::Result<Vec<NameNote>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut indexed = Vec::new();

        for n in decrypted {
            // ── Transition (step 1): parse memo narration ──
            // Memo format is defined by ZNS; non-lifecycle memos are ignored.
            let Ok(ParsedMemo::Lifecycle {
                action,
                name,
                ua,
                prev_rcm: memo_prev,
            }) = memo::parse_memo(n.memo.as_slice())
            else {
                continue;
            };
            let name = name.to_string();
            let ua = ua.to_string();

            // Names that look like UAs are rejected to avoid namespace confusion.
            if shadows_ua_namespace(&name) {
                continue;
            }

            // ── Transition (step 2): legal move on our per-name chain ──
            // claim requires no tip; update/release require tip.rcm as prev_rcm.
            let tip = self.name_tip_in_tx(&tx, &name)?;
            let Some(prev_rcm) = prev_rcm_for(tip.as_ref(), action) else {
                continue;
            };

            // ── Binding: recompute cmx; memo is not trusted for this ──
            let Some((psi, rcm)) = verify_binding(&n.note, n.cmx, action, &name, &ua, &prev_rcm)
            else {
                // If memo claimed a different prev_rcm that *would* verify, someone
                // may be building an alternate fork — log but don't index.
                if let Some(claimed) = memo_prev.filter(|p| {
                    *p != prev_rcm
                        && verify_binding(&n.note, n.cmx, action, &name, &ua, p).is_some()
                }) {
                    tracing::warn!(
                        name,
                        height = n.height,
                        claimed = hex::encode(claimed),
                        tip = hex::encode(prev_rcm),
                        "registry fork: note extends a different predecessor than our tip"
                    );
                }
                continue;
            };

            let name_note = NameNote {
                name: name.clone(),
                ua: ua.clone(),
                action,
                prev_rcm,
                rcm,
                psi,
                cmx: n.cmx,
                txid: n.txid,
                height: n.height,
                action_index: n.action_index,
                raw_tx: n.raw_tx.clone(),
            };

            insert_event(
                &tx,
                &name_note.name,
                &name_note.ua,
                &name_note.prev_rcm,
                &name_note.rcm,
                &name_note.psi,
                &name_note.cmx,
                &name_note.txid,
                name_note.height,
                name_note.action,
                name_note.action_index,
                &name_note.raw_tx,
            )?;

            // Release removes the name from the live index; event stays in history.
            if name_note.action == Action::Release {
                tx.execute("DELETE FROM names WHERE name = ?1", params![name_note.name])?;
            } else {
                tx.execute(
                    "INSERT INTO names (name, height, action, ua, prev_rcm, rcm, psi, cmx, txid, action_index, raw_tx)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT (name) DO UPDATE SET
                       height = excluded.height, action = excluded.action, ua = excluded.ua,
                       prev_rcm = excluded.prev_rcm, rcm = excluded.rcm, psi = excluded.psi,
                       cmx = excluded.cmx, txid = excluded.txid, action_index = excluded.action_index,
                       raw_tx = excluded.raw_tx",
                    params![
                        name_note.name,
                        name_note.height as i64,
                        action_str(name_note.action),
                        name_note.ua,
                        name_note.prev_rcm.as_slice(),
                        name_note.rcm.as_slice(),
                        name_note.psi.as_slice(),
                        name_note.cmx.as_slice(),
                        name_note.txid.as_slice(),
                        name_note.action_index as i64,
                        name_note.raw_tx,
                    ],
                )?;
            }

            indexed.push(name_note);
        }

        self.set_checkpoint_in_tx(&tx, Cursor::checkpoint(scanned, live_tip))?;
        tx.commit()?;
        Ok(indexed)
    }

    /// Handle a chain reorg: drop events above `fork_height`, rebuild `names` tips.
    ///
    /// Shallow reorgs rewind incrementally; deep ones wipe and rescan from birthday.
    fn rewind(&self, fork_height: u32, scanned_height: u32) -> rusqlite::Result<()> {
        let depth = scanned_height.saturating_sub(fork_height);
        let tx = self.conn.unchecked_transaction()?;

        if depth > REORG_SHALLOW_MAX {
            tx.execute("DELETE FROM name_events", [])?;
            tx.execute("DELETE FROM names", [])?;
            tx.execute("DELETE FROM proof_material", [])?;
            tx.execute("DELETE FROM scan_state", [])?;
        } else {
            let mut stmt = tx.prepare("SELECT DISTINCT name FROM name_events WHERE height > ?1")?;
            let affected: Vec<String> = stmt
                .query_map(params![fork_height as i64], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);

            tx.execute(
                "DELETE FROM name_events WHERE height > ?1",
                params![fork_height as i64],
            )?;
            tx.execute(
                "DELETE FROM proof_material WHERE height > ?1",
                params![fork_height as i64],
            )?;

            for name in &affected {
                rebuild_name_tip(&tx, name)?;
            }

            self.set_checkpoint_in_tx(
                &tx,
                Cursor {
                    height: fork_height,
                    hash: None,
                    chain_tip_height: None,
                    chain_tip_hash: None,
                },
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    fn insert_proof_material(
        &self,
        txid: &[u8; 32],
        height: u32,
        raw_tx: &[u8],
        header: &[u8],
        merkle_branch: &[[u8; 32]],
        merkle_index: u32,
    ) -> rusqlite::Result<()> {
        let branch: Vec<u8> = merkle_branch.iter().flatten().copied().collect();
        self.conn.execute(
            "INSERT OR IGNORE INTO proof_material
                 (txid, height, raw_tx, header, merkle_branch, merkle_index)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                txid.as_slice(),
                height as i64,
                raw_tx,
                header,
                branch,
                merkle_index as i64
            ],
        )?;
        Ok(())
    }

    fn resolve_by_name(&self, name: &str) -> rusqlite::Result<Option<Registration>> {
        self.conn
            .query_row(
                "SELECT name, ua, txid, height, action FROM names WHERE name = ?1",
                params![name],
                row_to_registration,
            )
            .optional()
            .map_err(Into::into)
    }

    fn registrations_by_ua(
        &self,
        ua: &str,
        limit: u32,
        offset: u32,
    ) -> rusqlite::Result<Vec<Registration>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, ua, txid, height, action FROM names
             WHERE ua = ?1 ORDER BY name LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt
            .query_map(params![ua, limit, offset], row_to_registration)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn list_registrations(&self, limit: u32, offset: u32) -> rusqlite::Result<Vec<Registration>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, ua, txid, height, action FROM names ORDER BY name LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(params![limit, offset], row_to_registration)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn name_count(&self) -> rusqlite::Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    fn events(
        &self,
        name: Option<&str>,
        action: Option<Action>,
        since_height: Option<u32>,
        limit: u32,
        offset: u32,
    ) -> rusqlite::Result<(Vec<Event>, u64)> {
        const WHERE: &str = "WHERE (?1 IS NULL OR name = ?1)
                             AND (?2 IS NULL OR action = ?2)
                             AND (?3 IS NULL OR height > ?3)";
        let p = params![
            name,
            action.map(action_str),
            since_height.map(|h| h as i64),
            limit,
            offset
        ];

        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM name_events {WHERE}"),
            &p[..3],
            |r| r.get(0),
        )?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT rowid, name, action, ua, txid, height FROM name_events {WHERE}
             ORDER BY height DESC, rowid DESC LIMIT ?4 OFFSET ?5"
        ))?;
        let events = stmt
            .query_map(p, |r| {
                let txid: Vec<u8> = r.get(4)?;
                Ok(Event {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    action: parse_action(&r.get::<_, String>(2)?)?,
                    ua: r.get(3)?,
                    txid: txid
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, 0))?,
                    height: r.get::<_, i64>(5)? as u32,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((events, total as u64))
    }

    fn chain_rows(&self, name: &str) -> rusqlite::Result<Vec<ChainRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT action, ua, height, txid, action_index FROM name_events
             WHERE name = ?1 ORDER BY height ASC, rowid ASC",
        )?;
        let rows = stmt
            .query_map(params![name], |r| {
                let txid: Vec<u8> = r.get(3)?;
                Ok(ChainRow {
                    action: parse_action(&r.get::<_, String>(0)?)?,
                    ua: r.get(1)?,
                    height: r.get::<_, i64>(2)? as u32,
                    txid: txid
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, 0))?,
                    action_index: r.get::<_, i64>(4)? as usize,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn proof_material(&self, txid: &[u8; 32]) -> rusqlite::Result<Option<ProofMaterial>> {
        self.conn
            .query_row(
                "SELECT raw_tx, header, merkle_branch, merkle_index
                 FROM proof_material WHERE txid = ?1",
                params![txid.as_slice()],
                |r| {
                    let branch: Vec<u8> = r.get(2)?;
                    Ok(ProofMaterial {
                        raw_tx: r.get(0)?,
                        header: r.get(1)?,
                        merkle_branch: branch
                            .chunks_exact(32)
                            .map(|c| c.try_into().expect("32-byte siblings"))
                            .collect(),
                        merkle_index: r.get::<_, i64>(3)? as u32,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn name_tip_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        name: &str,
    ) -> rusqlite::Result<Option<Tip>> {
        tx.query_row(
            "SELECT action, rcm FROM names WHERE name = ?1",
            params![name],
            |row| {
                let action = parse_action(&row.get::<_, String>(0)?)?;
                let rcm: Vec<u8> = row.get(1)?;
                let rcm: [u8; 32] = rcm
                    .try_into()
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, 0))?;
                Ok(Tip { action, rcm })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    fn set_checkpoint_in_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        state: Cursor,
    ) -> rusqlite::Result<()> {
        tx.execute(
            "INSERT INTO scan_state (id, height, hash, chain_tip_height, chain_tip_hash)
             VALUES (0, ?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET
               height = ?1, hash = ?2, chain_tip_height = ?3, chain_tip_hash = ?4",
            params![
                state.height,
                state.hash,
                state.chain_tip_height.map(|h| h as i64),
                state.chain_tip_hash,
            ],
        )?;
        Ok(())
    }
}

fn row_to_cursor(row: &Row<'_>) -> rusqlite::Result<Cursor> {
    let height: u32 = row.get(0)?;
    let hash: Option<[u8; 32]> = row
        .get::<_, Option<Vec<u8>>>(1)?
        .and_then(|v| v.try_into().ok());
    let chain_tip_height: Option<u32> = row
        .get::<_, Option<i64>>(2)?
        .and_then(|h| u32::try_from(h).ok());
    let chain_tip_hash: Option<[u8; 32]> = row
        .get::<_, Option<Vec<u8>>>(3)?
        .and_then(|v| v.try_into().ok());
    Ok(Cursor {
        height,
        hash,
        chain_tip_height,
        chain_tip_hash,
    })
}

/// After deleting post-fork events, set `names` to the highest surviving event
/// for this name (or delete the row if the tip was a release).
fn rebuild_name_tip(tx: &rusqlite::Transaction<'_>, name: &str) -> rusqlite::Result<()> {
    let row: Option<(
        String,
        i64,
        String,
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        Vec<u8>,
        i64,
        Vec<u8>,
        i64,
    )> = tx
        .query_row(
            "SELECT name, height, action, ua, prev_rcm, rcm, psi, cmx, txid, raw_tx, action_index
             FROM name_events WHERE name = ?1 ORDER BY height DESC, rowid DESC LIMIT 1",
            params![name],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            },
        )
        .optional()?;

    match row {
        None => {
            tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
        }
        Some((name, height, action, ua, prev_rcm, rcm, psi, cmx, txid, raw_tx, action_index)) => {
            if action == "release" {
                tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
            } else {
                tx.execute(
                    "INSERT INTO names (name, height, action, ua, prev_rcm, rcm, psi, cmx, txid, action_index, raw_tx)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT (name) DO UPDATE SET
                       height = excluded.height, action = excluded.action, ua = excluded.ua,
                       prev_rcm = excluded.prev_rcm, rcm = excluded.rcm, psi = excluded.psi,
                       cmx = excluded.cmx, txid = excluded.txid, action_index = excluded.action_index,
                       raw_tx = excluded.raw_tx",
                    params![
                        name,
                        height,
                        action,
                        ua,
                        prev_rcm,
                        rcm,
                        psi,
                        cmx,
                        txid,
                        action_index,
                        raw_tx
                    ],
                )?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_event(
    conn: &Connection,
    name: &str,
    ua: &str,
    prev_rcm: &[u8; 32],
    rcm: &[u8; 32],
    psi: &[u8; 32],
    cmx: &[u8; 32],
    txid: &[u8; 32],
    height: u32,
    action: Action,
    action_index: usize,
    raw_tx: &[u8],
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO name_events (name, height, action, ua, prev_rcm, rcm, psi, cmx, txid, action_index, raw_tx)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            name,
            height as i64,
            action_str(action),
            ua,
            prev_rcm.as_slice(),
            rcm.as_slice(),
            psi.as_slice(),
            cmx.as_slice(),
            txid.as_slice(),
            action_index as i64,
            raw_tx
        ],
    )?;
    Ok(())
}

fn row_to_registration(r: &Row<'_>) -> rusqlite::Result<Registration> {
    let txid: Vec<u8> = r.get(2)?;
    Ok(Registration {
        name: r.get(0)?,
        ua: r.get(1)?,
        txid: txid
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 0))?,
        height: r.get::<_, i64>(3)? as u32,
        last_action: parse_action(&r.get::<_, String>(4)?)?,
    })
}

fn parse_action(s: &str) -> rusqlite::Result<Action> {
    Action::from_bytes(s.as_bytes()).ok_or(rusqlite::Error::IntegralValueOutOfRange(0, 0))
}

fn action_str(a: Action) -> &'static str {
    match a {
        Action::Claim => "claim",
        Action::Update => "update",
        Action::Release => "release",
    }
}

/// Reject names that could be mistaken for Zcash unified addresses.
fn shadows_ua_namespace(name: &str) -> bool {
    name.starts_with("u1") || name.starts_with("utest1")
}

// ── binding verification ──────────────────────────────────────────────────────
//
// Core ZNS invariant: the name binding lives in (ψ, rcm) → cmx, not the memo.
//
//   ψ, rcm = zns_psi_rcm(action, name, ua, prev_rcm)   // domain-separated hash
//   cmx    = Orchard note commitment with those extras
//
// We extract g_d, pk_d, ρ from the decrypted note and check cmx matches chain.

/// Returns `(psi, rcm)` byte reprs if the note commitment matches on-chain cmx.
fn verify_binding(
    note: &orchard::Note,
    on_chain_cmx: [u8; 32],
    action: Action,
    name: &str,
    ua: &str,
    prev_rcm: &[u8; 32],
) -> Option<([u8; 32], [u8; 32])> {
    let (g_d, pk_d) = note.recipient().zns_commitment_keys();
    let rho = pallas::Base::from_repr(note.rho().to_bytes()).into_option()?;
    let expected = pallas::Base::from_repr(on_chain_cmx).into_option()?;

    let (psi, rcm) = zns_psi_rcm(action.as_bytes(), name.as_bytes(), ua.as_bytes(), prev_rcm);
    let cmx = note_commitment_cmx(g_d, pk_d, note.value().inner(), rho, psi, rcm)?;
    (cmx == expected).then(|| (psi.to_repr(), rcm.to_repr()))
}

// ── observe (visibility invariant) ────────────────────────────────────────────
//
// Compact blocks support trial-decrypt only; memos need the full transaction.

/// Parse UIVK/UFVK encoding into a prepared Orchard IVK for trial decryption.
fn orchard_ivk(network: &impl Parameters, encoding: &str) -> Result<PreparedIncomingViewingKey> {
    use orchard::keys::Scope;
    use zcash_keys::keys::{UnifiedFullViewingKey, UnifiedIncomingViewingKey};

    if let Ok(ufvk) = UnifiedFullViewingKey::decode(network, encoding) {
        if let Some(fvk) = ufvk.orchard() {
            return Ok(PreparedIncomingViewingKey::new(
                &fvk.to_ivk(Scope::External),
            ));
        }
    }
    if let Ok(uivk) = UnifiedIncomingViewingKey::decode(network, encoding) {
        if let Some(ivk) = uivk.orchard() {
            return Ok(PreparedIncomingViewingKey::new(ivk));
        }
    }
    anyhow::bail!("no Orchard incoming viewing key in the provided encoding")
}

/// Visibility pipeline for one compact-block batch from lightwalletd.
async fn observe_batch(
    client: &mut LwdClient,
    network: &impl Parameters,
    ivk: &PreparedIncomingViewingKey,
    blocks: &[CompactBlock],
) -> Result<Vec<DecryptedNote>> {
    type Fetched = Option<(Transaction, u32, Vec<u8>)>;
    let mut fetched: HashMap<[u8; 32], Fetched> = HashMap::new();
    let mut out = Vec::new();

    for block in blocks {
        for tx in &block.vtx {
            let Ok(txid) = tx.txid[..].try_into() else {
                continue;
            };
            for (action_index, act) in tx.actions.iter().enumerate() {
                let Some(action) = parse_orchard(act) else {
                    continue;
                };
                if zns_verify::decrypt::try_compact_orchard(ivk, &action).is_none() {
                    continue;
                }

                if let std::collections::hash_map::Entry::Vacant(e) = fetched.entry(txid) {
                    let raw = chain::fetch_raw_transaction(client, &TxId::from_bytes(txid))
                        .await
                        .with_context(|| format!("fetch tx {}", hex::encode(txid)))?;
                    let height = raw.height as u32;
                    let parsed = Transaction::read(
                        &raw.data[..],
                        BranchId::for_height(network, BlockHeight::from_u32(height)),
                    )
                    .ok()
                    .map(|tx| (tx, height, raw.data));
                    e.insert(parsed);
                }

                let Some((parsed_tx, height, raw)) =
                    fetched.get(&txid).and_then(|o| o.as_ref())
                else {
                    continue;
                };
                let Some(bundle) = parsed_tx.orchard_bundle() else {
                    continue;
                };
                let Some(action) = bundle.actions().get(action_index) else {
                    continue;
                };
                let Some((note, _recipient, memo)) =
                    zns_verify::decrypt::try_decrypt_orchard(action, ivk)
                else {
                    continue;
                };

                out.push(DecryptedNote {
                    note,
                    cmx: action.cmx().to_bytes(),
                    memo,
                    txid,
                    height: *height,
                    action_index,
                    raw_tx: raw.clone(),
                });
            }
        }
    }

    Ok(out)
}

// ── proof material (derivability invariant) ─────────────────────────────────────
//
// Clients can audit bindings without trusting this resolver: given raw tx, block
// header, and merkle siblings they can re-derive inclusion and re-run binding checks.

impl ValidatorClient {
    fn new(url: &str) -> Result<Self> {
        let client = HttpClient::builder()
            .build(url)
            .context("validator RPC url")?;
        Ok(Self { client })
    }

    async fn block_context(&self, height: u32) -> Result<BlockContext> {
        let arg = height.to_string();

        let info: serde_json::Value = self
            .client
            .request("getblock", rpc_params![&arg, 1])
            .await
            .with_context(|| format!("getblock {height} (verbose)"))?;
        let txids = info
            .get("tx")
            .and_then(|t| t.as_array())
            .ok_or_else(|| anyhow!("getblock {height}: no tx list"))?
            .iter()
            .map(|v| {
                let hex_str = v.as_str().ok_or_else(|| anyhow!("non-string txid"))?;
                let mut bytes: [u8; 32] = hex::decode(hex_str)?
                    .try_into()
                    .map_err(|_| anyhow!("txid length"))?;
                bytes.reverse();
                Ok(bytes)
            })
            .collect::<Result<Vec<_>>>()?;

        let raw_hex: String = self
            .client
            .request("getblock", rpc_params![&arg, 0])
            .await
            .with_context(|| format!("getblock {height} (raw)"))?;
        let raw = hex::decode(raw_hex.trim()).context("raw block hex")?;
        let parsed = zcash_primitives::block::BlockHeader::read(&raw[..])
            .with_context(|| format!("block {height} header parse"))?;
        let mut header = Vec::new();
        parsed.write(&mut header)?;

        Ok(BlockContext { header, txids })
    }
}

/// Build a Bitcoin-style merkle inclusion path (double-SHA256 pairs).
fn merkle_branch(txids: &[[u8; 32]], index: usize) -> Vec<[u8; 32]> {
    assert!(index < txids.len(), "leaf index in range");
    let mut level: Vec<[u8; 32]> = txids.to_vec();
    let mut idx = index;
    let mut branch = Vec::new();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().expect("non-empty"));
        }
        let sibling = if idx % 2 == 1 {
            level[idx - 1]
        } else {
            level[idx + 1]
        };
        branch.push(sibling);
        level = level
            .chunks_exact(2)
            .map(|pair| sha256d(&pair[0], &pair[1]))
            .collect();
        idx /= 2;
    }
    branch
}

fn sha256d(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let first = Sha256::new()
        .chain_update(left)
        .chain_update(right)
        .finalize();
    Sha256::digest(first).into()
}

/// For each newly indexed name note, fetch block context from validator and persist proofs.
async fn materialize_proofs(
    db: &Db,
    validator: &ValidatorClient,
    indexed: &[NameNote],
) -> Result<()> {
    let mut heights: Vec<u32> = indexed.iter().map(|n| n.height).collect();
    heights.sort_unstable();
    heights.dedup();

    for height in heights {
        let ctx = validator.block_context(height).await?;
        for n in indexed.iter().filter(|n| n.height == height) {
            let Some(pos) = ctx.txids.iter().position(|t| *t == n.txid) else {
                anyhow::bail!(
                    "validator block {height} does not contain tx {}",
                    hex::encode(n.txid)
                );
            };
            let branch = merkle_branch(&ctx.txids, pos);
            db.insert_proof_material(
                &n.txid,
                height,
                &n.raw_tx,
                &ctx.header,
                &branch,
                pos as u32,
            )?;
        }
    }
    Ok(())
}

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

#[derive(Debug, Clone, Serialize)]
struct ProofLinkEntry {
    action: String,
    ua: String,
    height: u64,
    txid: String,
    action_index: u64,
    tx: String,
    header: String,
    merkle_branch: Vec<String>,
    merkle_index: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChainResult {
    name: String,
    links: Vec<ProofLinkEntry>,
}

/// Public resolver API. `blocking` methods run on jsonrpsee's thread pool since
/// SQLite is synchronous.
#[rpc(server)]
trait ZnsApi {
    /// Lookup by name (exact), UA prefix (reverse lookup), or list all if query empty.
    /// `with_proof=true` attaches merkle proof links for the current claim segment.
    #[method(name = "resolve", blocking)]
    fn resolve(
        &self,
        query: String,
        limit: Option<u64>,
        offset: Option<u64>,
        with_proof: Option<bool>,
    ) -> RpcResult<Value>;

    /// Full per-name event chain with proof material for every link.
    #[method(name = "chain", blocking)]
    fn chain(&self, name: String) -> RpcResult<ChainResult>;

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

struct RpcContext {
    db: PathBuf,
    uivk: String,
}

impl ZnsApiServer for RpcContext {
    fn resolve(
        &self,
        query: String,
        limit: Option<u64>,
        offset: Option<u64>,
        with_proof: Option<bool>,
    ) -> RpcResult<Value> {
        let db = open_for_rpc(&self.db)?;
        let limit = limit.unwrap_or(50).min(500) as u32;
        let offset = offset.unwrap_or(0) as u32;

        let value = if query.is_empty() {
            entries(db.list_registrations(limit, offset).map_err(rpc_err)?)
        } else if let Some(reg) = db.resolve_by_name(&query).map_err(rpc_err)? {
            let mut value = serde_json::to_value(entry(reg)).unwrap();
            if with_proof == Some(true) {
                let rows = db.chain_rows(&query).map_err(rpc_err)?;
                let links = proof_links(&db, current_segment(&rows))?;
                value["proof"] = serde_json::json!({ "links": links });
            }
            value
        } else {
            entries(
                db.registrations_by_ua(&query, limit, offset)
                    .map_err(rpc_err)?,
            )
        };
        Ok(value)
    }

    fn chain(&self, name: String) -> RpcResult<ChainResult> {
        let db = open_for_rpc(&self.db)?;
        let rows = db.chain_rows(&name).map_err(rpc_err)?;
        let links = proof_links(&db, &rows)?;
        Ok(ChainResult { name, links })
    }

    fn status(&self) -> RpcResult<StatusResult> {
        let db = open_for_rpc(&self.db)?;
        let cp = db.checkpoint().map_err(rpc_err)?;
        let synced_height = cp.map(|c| c.height).unwrap_or(0) as u64;
        let (chain_tip_height, synced, blocks_behind) =
            match cp.and_then(|c| c.chain_tip_height.map(|tip| (c.height, tip))) {
                Some((scanned, tip)) => {
                    let synced = scanned >= tip;
                    (
                        tip as u64,
                        synced,
                        if synced { 0 } else { (tip - scanned) as u64 },
                    )
                }
                None => (0, false, 0),
            };
        Ok(StatusResult {
            synced_height,
            chain_tip_height,
            synced,
            blocks_behind,
            uivk: self.uivk.clone(),
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
        let db = open_for_rpc(&self.db)?;
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

/// Proof links cover the current ownership segment: from the latest claim forward.
/// Earlier claim/update/release chains are historical only.
fn current_segment(rows: &[ChainRow]) -> &[ChainRow] {
    let start = rows
        .iter()
        .rposition(|r| r.action == Action::Claim)
        .unwrap_or(0);
    &rows[start..]
}

fn proof_links(db: &Db, rows: &[ChainRow]) -> RpcResult<Vec<ProofLinkEntry>> {
    rows.iter()
        .map(|r| {
            let m = db
                .proof_material(&r.txid)
                .map_err(rpc_err)?
                .ok_or_else(|| {
                    ErrorObjectOwned::owned(
                        -32011,
                        "proof material unavailable (no validator RPC configured)",
                        None::<()>,
                    )
                })?;
            Ok(ProofLinkEntry {
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

async fn serve_rpc(addr: &str, ctx: RpcContext) -> Result<()> {
    let server = Server::builder().build(addr).await?;
    let handle = server.start(ctx.into_rpc());
    tracing::info!("JSON-RPC listening on {addr}");
    handle.stopped().await;
    Ok(())
}

// ── tip watcher ───────────────────────────────────────────────────────────────

/// Polls lightwalletd for the chain tip and notifies the sync worker when it moves.
async fn run_tip_watcher(url: &'static str, interval: Duration, tx: watch::Sender<Cursor>) {
    loop {
        match chain::connect(url).await {
            Ok(mut client) => match chain::tip(&mut client).await {
                Ok((height, hash)) => {
                    let _ = tx.send(Cursor::at(height, hash));
                }
                Err(e) => eprintln!("tip watcher: {e}"),
            },
            Err(e) => eprintln!("tip watcher connect: {e}"),
        }
        tokio::time::sleep(interval).await;
    }
}

// ── main: sync loop ───────────────────────────────────────────────────────────
//
// ```text
//   tip watcher (GetLatestBlock) ──notify──► sync worker
//                                                  │
//   connect lightwalletd ◄─────────────────────────┘
//        │
//        ▼
//   stream compact blocks from checkpoint+1 ──► observe_batch
//        │                                          │
//        │                                          ▼
//        │                                    apply_batch + checkpoint
//        │                                          │
//        │                                          ▼
//        └──────────────────────────── materialize_proofs (if validator set)
//
//   On reorg: rewind with exponential backoff (rewind_by *= 2)
//   When caught up: sync worker waits on tip watcher (no poll sleep in sync loop)
// ```

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("zns_resolver=info".parse().unwrap()),
        )
        .init();

    let ivk = match orchard_ivk(&NETWORK, UIVK) {
        Ok(ivk) => ivk,
        Err(e) => {
            eprintln!("uivk: {e}");
            std::process::exit(1);
        }
    };

    let validator = VALIDATOR_RPC.map(ValidatorClient::new).transpose();
    let validator = match validator {
        Ok(v) => v,
        Err(e) => {
            eprintln!("validator: {e}");
            std::process::exit(1);
        }
    };

    // RPC server runs concurrently; sync loop owns the writable DB connection.
    let rpc_ctx = RpcContext {
        db: PathBuf::from(DB_PATH),
        uivk: UIVK.to_string(),
    };
    let rpc_addr = RPC_ADDR.to_string();
    tokio::spawn(async move {
        if let Err(e) = serve_rpc(&rpc_addr, rpc_ctx).await {
            eprintln!("rpc: {e}");
        }
    });

    let (tip_tx, mut tip_rx) = watch::channel(Cursor::at(0, None));
    tokio::spawn(run_tip_watcher(
        LIGHTWALLETD,
        TIP_WATCH_INTERVAL,
        tip_tx,
    ));

    // Reorg rewind depth doubles on each consecutive reorg (1, 2, 4, … blocks).
    let mut rewind_by = 1u32;

    loop {
        let db = match Db::open_for_indexer(DB_PATH) {
            Ok(db) => db,
            Err(e) => {
                eprintln!("database: {e}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let checkpoint = match db.checkpoint() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("checkpoint: {e}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let client = match chain::connect(LIGHTWALLETD).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("lightwalletd: {e}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let mut fetch_client = client.clone();

        let live_tip = *tip_rx.borrow_and_update();

        // Resume from checkpoint+1; `seam` hash lets seer-sync detect reorgs at the boundary.
        let (start, seam) = match checkpoint {
            Some(c) => (c.height.saturating_add(1), c.hash.map(BlockHash)),
            None => (SCAN_BIRTHDAY, None),
        };

        if live_tip.height == 0 || start > live_tip.height {
            if tip_rx.changed().await.is_err() {
                eprintln!("tip watcher stopped");
                break;
            }
            continue;
        }

        let mut stream = chain::blocks(
            client,
            start,
            live_tip.height,
            DEFAULT_CHUNK_OUTPUTS,
            seam,
        );

        // Inner loop: process block batches until stream ends or an error breaks out.
        loop {
            match stream.next().await {
                None => break,
                Some(Ok(batch)) => {
                    let Some(last) = batch.last() else {
                        continue;
                    };
                    let scanned =
                        Cursor::at(last.height as u32, last.hash[..].try_into().ok());

                    match observe_batch(&mut fetch_client, &NETWORK, &ivk, &batch).await {
                        Ok(decrypted) => match db.apply_batch(scanned, live_tip, &decrypted) {
                            Ok(indexed) => {
                                if let Some(ref validator) = validator {
                                    if let Err(e) =
                                        materialize_proofs(&db, validator, &indexed).await
                                    {
                                        eprintln!("proofs: {e}");
                                    }
                                }
                                rewind_by = 1;
                                tracing::info!(
                                    height = scanned.height,
                                    tip = live_tip.height,
                                    decrypted = decrypted.len(),
                                    indexed = indexed.len(),
                                    "batch applied"
                                );
                            }
                            Err(e) => {
                                eprintln!("apply: {e}");
                                break;
                            }
                        },
                        Err(e) => {
                            eprintln!("observe: {e}");
                            break;
                        }
                    }
                }
                // Chain hash mismatch at seam → rewind and retry outer loop.
                Some(Err(ChainError::Reorg(at))) => {
                    let rewind_to = at.saturating_sub(rewind_by);
                    let scanned = db
                        .checkpoint()
                        .ok()
                        .flatten()
                        .map(|c| c.height)
                        .unwrap_or(0);
                    eprintln!("reorg at {at}, rewind to {rewind_to}");
                    if let Err(e) = db.rewind(rewind_to, scanned) {
                        eprintln!("rewind: {e}");
                    }
                    rewind_by = rewind_by.saturating_mul(2);
                    break;
                }
                Some(Err(e)) => {
                    eprintln!("scan: {e}");
                    break;
                }
            }
        }
    }
}
