//! ZNS name index in SQLite (`registry_account`, `scan_state`, `name_events`, `names`, `proof_material`).

use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use zcash_protocol::consensus::Parameters;
use zns_verify::{
    chain::prev_rcm_for, parse_memo_validated, Action, ParsedMemo, Tip,
};

use crate::orchard::{verify_binding, DecryptedNote};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const REORG_SHALLOW_MAX: u32 = 30;

// SQLite schema:
//   registry_account — singleton: registry inbox UIVK (set once at create, not scan state)
//   scan_state       — checkpoint after commit (how far we've scanned)
//   name_events      — append-only history of every verified lifecycle event
//   names            — materialized current tip per name (fast resolve)
//   proof_material   — tx + header + merkle branch for optional client audit
const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS registry_account (
    id   INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
    uivk TEXT    NOT NULL
);

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

/// Ephemeral scan `(height, hash)` — live tip from watcher or batch end before commit.
pub(crate) type Cursor = (u32, Option<[u8; 32]>);

/// Persisted `scan_state` row.
pub(crate) struct Checkpoint {
    pub(crate) scanned_height: u32,
    pub(crate) scanned_hash: Option<[u8; 32]>,
    pub(crate) chain_tip_height: Option<u32>,
    pub(crate) chain_tip_hash: Option<[u8; 32]>,
}

/// A verified ZNS name note: Transition + Binding passed for one decrypted note.
pub(crate) struct NameNote {
    pub(crate) name: String,
    pub(crate) ua: String,
    pub(crate) action: Action,
    pub(crate) prev_rcm: [u8; 32],
    pub(crate) rcm: [u8; 32],
    pub(crate) psi: [u8; 32],
    pub(crate) cmx: [u8; 32],
    pub(crate) txid: [u8; 32],
    pub(crate) height: u32,
    pub(crate) action_index: usize,
    pub(crate) raw_tx: Vec<u8>,
}

/// Current registration: a name's live tip (absent from `names` if released).
#[derive(Debug, Clone)]
pub(crate) struct Registration {
    pub(crate) name: String,
    pub(crate) ua: String,
    pub(crate) txid: [u8; 32],
    pub(crate) height: u32,
    pub(crate) last_action: Action,
}

/// One verified lifecycle step in a name's per-name chain (from `name_events`).
#[derive(Debug, Clone)]
pub(crate) struct ChainRow {
    pub(crate) action: Action,
    pub(crate) ua: String,
    pub(crate) height: u32,
    pub(crate) txid: [u8; 32],
    pub(crate) action_index: usize,
}

/// Event log entry for the `events` RPC (includes DB rowid).
#[derive(Debug, Clone)]
pub(crate) struct Event {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) action: Action,
    pub(crate) ua: String,
    pub(crate) txid: [u8; 32],
    pub(crate) height: u32,
}

/// Everything a client needs to independently verify a binding on-chain (derivability).
#[derive(Debug, Clone)]
pub(crate) struct ProofMaterial {
    pub(crate) raw_tx: Vec<u8>,
    pub(crate) header: Vec<u8>,
    /// Sibling hashes from leaf txid up to the block's tx merkle root.
    pub(crate) merkle_branch: Vec<[u8; 32]>,
    pub(crate) merkle_index: u32,
}

/// Thin SQLite wrapper — all writes go through `apply_batch` / `rewind`.
pub(crate) struct Db {
    conn: Connection,
}

// ── database ──────────────────────────────────────────────────────────────────
//
// Write path: `apply_batch` runs Visibility→Transition→Binding in one transaction,
// then updates the checkpoint. Read path: open read-only connections for RPC handlers.

impl Db {
    pub(crate) fn open_for_indexer(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch(SCHEMA_SQL)?;
        // One-time: older DBs briefly stored uivk on scan_state.
        let _ = conn.execute(
            "INSERT OR IGNORE INTO registry_account (id, uivk)
             SELECT 0, uivk FROM scan_state WHERE id = 0 AND uivk IS NOT NULL",
            [],
        );
        Ok(Self { conn })
    }

    /// Record registry inbox UIVK when the DB is first created. No-op if already set;
    /// warns if the binary's UIVK disagrees (index identity is fixed).
    pub(crate) fn install_registry_uivk(&self, uivk: &str) -> rusqlite::Result<()> {
        if let Some(existing) = self.registry_uivk()? {
            if existing != uivk {
                tracing::warn!(
                    stored = %existing,
                    "registry_account uivk already set; not changing"
                );
            }
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO registry_account (id, uivk) VALUES (0, ?1)",
            params![uivk],
        )?;
        Ok(())
    }

    pub(crate) fn registry_uivk(&self) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT uivk FROM registry_account WHERE id = 0",
                [],
                |row| row.get(0),
            )
            .optional()
    }

    pub(crate) fn open_for_rpc(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        Ok(Self { conn })
    }

    pub(crate) fn checkpoint(&self) -> rusqlite::Result<Option<Checkpoint>> {
        self.conn
            .query_row(
                "SELECT height, hash, chain_tip_height, chain_tip_hash FROM scan_state WHERE id = 0",
                [],
                row_to_checkpoint,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Process one scanned batch: for each decrypted note, run Transition + Binding,
    /// append to `name_events`, update `names` tip, then checkpoint.
    ///
    /// Returns only name notes that were actually indexed (binding + transition passed).
    pub(crate) fn apply_batch(
        &self,
        network: &impl Parameters,
        scanned: Cursor,
        live: Cursor,
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
            }) = parse_memo_validated(n.memo.as_slice(), network)
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

        self.set_checkpoint_in_tx(
            &tx,
            Checkpoint {
                scanned_height: scanned.0,
                scanned_hash: scanned.1,
                chain_tip_height: Some(live.0),
                chain_tip_hash: live.1,
            },
        )?;
        tx.commit()?;
        Ok(indexed)
    }

    /// Handle a chain reorg: drop events above `fork_height`, rebuild `names` tips.
    ///
    /// Shallow reorgs rewind incrementally; deep ones wipe and rescan from birthday.
    pub(crate) fn rewind(&self, fork_height: u32, scanned_height: u32) -> rusqlite::Result<()> {
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
                Checkpoint {
                    scanned_height: fork_height,
                    scanned_hash: None,
                    chain_tip_height: None,
                    chain_tip_hash: None,
                },
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub(crate) fn insert_proof_material(
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

    pub(crate) fn resolve_by_name(&self, name: &str) -> rusqlite::Result<Option<Registration>> {
        self.conn
            .query_row(
                "SELECT name, ua, txid, height, action FROM names WHERE name = ?1",
                params![name],
                row_to_registration,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn registrations_by_ua(
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

    pub(crate) fn list_registrations(&self, limit: u32, offset: u32) -> rusqlite::Result<Vec<Registration>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, ua, txid, height, action FROM names ORDER BY name LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(params![limit, offset], row_to_registration)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub(crate) fn name_count(&self) -> rusqlite::Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    pub(crate) fn events(
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

    pub(crate) fn chain_rows(&self, name: &str) -> rusqlite::Result<Vec<ChainRow>> {
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

    pub(crate) fn proof_material(&self, txid: &[u8; 32]) -> rusqlite::Result<Option<ProofMaterial>> {
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
        state: Checkpoint,
    ) -> rusqlite::Result<()> {
        tx.execute(
            "INSERT INTO scan_state (id, height, hash, chain_tip_height, chain_tip_hash)
             VALUES (0, ?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET
               height = ?1, hash = ?2, chain_tip_height = ?3, chain_tip_hash = ?4",
            params![
                state.scanned_height,
                state.scanned_hash,
                state.chain_tip_height.map(|h| h as i64),
                state.chain_tip_hash,
            ],
        )?;
        Ok(())
    }
}

fn row_to_checkpoint(row: &Row<'_>) -> rusqlite::Result<Checkpoint> {
    let scanned_height: u32 = row.get(0)?;
    let scanned_hash: Option<[u8; 32]> = row
        .get::<_, Option<Vec<u8>>>(1)?
        .and_then(|v| v.try_into().ok());
    let chain_tip_height: Option<u32> = row
        .get::<_, Option<i64>>(2)?
        .and_then(|h| u32::try_from(h).ok());
    let chain_tip_hash: Option<[u8; 32]> = row
        .get::<_, Option<Vec<u8>>>(3)?
        .and_then(|v| v.try_into().ok());
    Ok(Checkpoint {
        scanned_height,
        scanned_hash,
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

