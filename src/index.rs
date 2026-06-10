//! SQLite name index.
//!
//! The blockchain is the state machine; this index is a materialized view of it:
//!  - `actions`    — append-only log, one row per applied Name Note. The
//!    canonical per-name history. The `current_names` view folds it to the
//!    latest non-released action per name (the resolution answer).
//!  - `sync_state` — last applied `(height, hash)`; the resume cursor.
//!
//! Reorgs are detected by the [`observe`](crate::observe) loop, which calls
//! [`SqliteIndex::rewind`] to delete actions above the fork. Because the log
//! retains history, the prior action for each affected name is still present,
//! so the view re-folds correctly at any rewind depth — no separate reorg
//! buffer is needed.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use seer_sync::BlockHeight;
use zcash_protocol::memo::MemoBytes;

use crate::verify::{self, NameChainEntry};
use zns_verify::{memo, prev_rcm_for, Action, ParsedMemo};

/// A synced position: the last applied block's height and hash.
#[derive(Clone, Copy)]
pub struct Cursor {
    /// Height of the last applied block.
    pub height: BlockHeight,
    /// Its block hash, if known.
    pub hash: Option<[u8; 32]>,
}

/// A relaxed-decrypted Orchard Name Note candidate, ready for binding
/// verification. The [`observe`](crate::observe) loop produces these and hands
/// them to [`SqliteIndex::apply_notes`].
pub struct NameNote {
    /// The decrypted Orchard note.
    pub note: orchard::Note,
    /// Its on-chain extracted note commitment.
    pub cmx: [u8; 32],
    /// The recovered memo, carrying the `ZNS:…` action grammar.
    pub memo: MemoBytes,
    /// The transaction that mined it.
    pub txid: [u8; 32],
    /// The block height it was mined at.
    pub height: u32,
    /// Which Orchard action in the transaction this note is — part of the
    /// proof bundle (`PROOFS.md §2`).
    pub action_index: usize,
    /// The full raw transaction bytes, kept from the recovery fetch so proof
    /// material needs no refetch.
    pub tx_bytes: Vec<u8>,
}

/// An action `apply_notes` recorded — what the observer must materialize
/// proof context for (header + Merkle branch via the validator RPC).
#[derive(Debug, Clone, Copy)]
pub struct Recorded {
    /// The transaction the action was mined in.
    pub txid: [u8; 32],
    /// Its block height.
    pub height: u32,
}

/// The SQLite-backed ZNS name index.
pub struct SqliteIndex {
    conn: Connection,
}

/// A public registration record — the RPC's view of a name.
#[derive(Debug, Clone)]
pub struct Registration {
    /// The name.
    pub name: String,
    /// The UA bound to it.
    pub ua: String,
    /// The txid of the Name Note that set the current state.
    pub txid: [u8; 32],
    /// The block height of that Name Note.
    pub height: u32,
    /// The kind of the latest action.
    pub last_action: Action,
}

/// The stored proof context for one Name Note transaction (`PROOFS.md §2`).
#[derive(Debug, Clone)]
pub struct ProofMaterial {
    /// The full raw transaction.
    pub raw_tx: Vec<u8>,
    /// The raw block header.
    pub header: Vec<u8>,
    /// Merkle branch siblings, leaf → root.
    pub merkle_branch: Vec<[u8; 32]>,
    /// The tx's leaf position in the block's Merkle tree.
    pub merkle_index: u32,
}

/// One applied action of a name's chain, with its proof pointers — the link
/// skeleton before proof material is joined on.
#[derive(Debug, Clone)]
pub struct ChainRow {
    /// The action kind.
    pub action: Action,
    /// The UA involved (empty for RELEASE).
    pub ua: String,
    /// The block height it was mined at.
    pub height: u32,
    /// The transaction that mined it.
    pub txid: [u8; 32],
    /// Which Orchard action in that transaction is the Name Note.
    pub action_index: usize,
}

/// One row of the public event log — an applied action, as served by the
/// `events` RPC. The append-only `actions` table *is* the event log; this is
/// just its row shape.
#[derive(Debug, Clone)]
pub struct Event {
    /// Stable log id (the row's insertion order — chain order).
    pub id: i64,
    /// The name acted on.
    pub name: String,
    /// The action kind.
    pub action: Action,
    /// The UA involved (empty for RELEASE).
    pub ua: String,
    /// The transaction that mined the action.
    pub txid: [u8; 32],
    /// The block height it was mined at.
    pub height: u32,
}

/// How long a connection waits out a competing writer before failing. The RPC
/// path opens read-only against the sync task's WAL writer; 5s comfortably
/// covers a batch commit.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS actions (
    name         TEXT NOT NULL,
    height       INTEGER NOT NULL,
    action       TEXT NOT NULL CHECK (action IN ('claim','update','release')),
    ua           TEXT NOT NULL,
    rcm          BLOB NOT NULL,
    cmx          BLOB NOT NULL,
    txid         BLOB NOT NULL,
    action_index INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS actions_name_height_idx ON actions(name, height);

-- Proof context per Name Note transaction (PROOFS.md §2/§6): the raw tx, its
-- block header, and the Merkle branch joining them. Materialized by the
-- observer at apply time (when a validator RPC is configured); immutable
-- under finality; purged with the actions it proves on rewind.
CREATE TABLE IF NOT EXISTS proof_material (
    txid          BLOB NOT NULL PRIMARY KEY,
    height        INTEGER NOT NULL,
    raw_tx        BLOB NOT NULL,
    header        BLOB NOT NULL,
    merkle_branch BLOB NOT NULL,  -- concatenated 32-byte siblings, leaf → root
    merkle_index  INTEGER NOT NULL
) WITHOUT ROWID;

-- The fold: the latest action per name (rowid breaks same-height ties by
-- insertion order), with released names excluded — a released name is free, so
-- it is not a current registration. `rewind` deletes from `actions`, so this
-- view always reflects the canonical chain at the synced height.
CREATE VIEW IF NOT EXISTS current_names AS
SELECT name, ua, rcm, cmx, txid, height, last_action FROM (
    SELECT name, ua, rcm, cmx, txid, height, action AS last_action,
           ROW_NUMBER() OVER (PARTITION BY name ORDER BY height DESC, rowid DESC) AS rn
    FROM actions
) WHERE rn = 1 AND last_action != 'release';

CREATE TABLE IF NOT EXISTS sync_state (
    id     INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
    height INTEGER NOT NULL,
    hash   BLOB
);
"#;

impl SqliteIndex {
    /// Open or create the index at `path` (WAL mode, schema applied).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).context("opening sqlite db")?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA_SQL).context("applying schema")?;
        // Migrate pre-`action_index` databases in place; the default is only
        // a placeholder until their next rescan.
        match conn.execute(
            "ALTER TABLE actions ADD COLUMN action_index INTEGER NOT NULL DEFAULT 0",
            [],
        ) {
            Ok(_) => {}
            Err(e) if e.to_string().contains("duplicate column name") => {}
            Err(e) => return Err(e).context("migrating actions.action_index"),
        }
        Ok(Self { conn })
    }

    /// Open an existing index read-only — the RPC path. No DDL runs (the sync
    /// writer owns the schema), and the busy timeout absorbs writer contention
    /// instead of surfacing `SQLITE_BUSY` to clients.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .context("opening sqlite db read-only")?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        Ok(Self { conn })
    }

    /// Open an ephemeral in-memory index (tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    /// Look up the current entry for `name`, if any.
    pub fn lookup(&self, name: &str) -> Result<Option<NameChainEntry>> {
        lookup(&self.conn, name).map_err(Into::into)
    }

    /// The public registration record for `name`, if any.
    pub fn resolve_by_name(&self, name: &str) -> Result<Option<Registration>> {
        self.conn
            .query_row(
                "SELECT name, ua, txid, height, last_action FROM current_names WHERE name = ?1",
                params![name],
                row_to_registration,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Registrations currently bound to `ua` (reverse lookup), paginated.
    pub fn registrations_by_ua(&self, ua: &str, limit: u32, offset: u32) -> Result<Vec<Registration>> {
        self.query_registrations(
            "SELECT name, ua, txid, height, last_action FROM current_names
             WHERE ua = ?1 ORDER BY name LIMIT ?2 OFFSET ?3",
            params![ua, limit, offset],
        )
    }

    /// All registrations, paginated.
    pub fn list_registrations(&self, limit: u32, offset: u32) -> Result<Vec<Registration>> {
        self.query_registrations(
            "SELECT name, ua, txid, height, last_action FROM current_names
             ORDER BY name LIMIT ?1 OFFSET ?2",
            params![limit, offset],
        )
    }

    fn query_registrations(&self, sql: &str, p: impl rusqlite::Params) -> Result<Vec<Registration>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt
            .query_map(p, row_to_registration)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// The highest applied block height, or `None` if never synced.
    pub fn last_scanned_height(&self) -> Result<Option<u32>> {
        Ok(self.checkpoint()?.map(|c| u32::from(c.height)))
    }

    /// Number of names currently in the index.
    pub fn name_count(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM current_names", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// A filtered, newest-first page of the action log, plus the total number
    /// of matches (pre-pagination). `since_height` is strictly greater-than,
    /// per the indexer API contract.
    pub fn events(
        &self,
        name: Option<&str>,
        action: Option<Action>,
        since_height: Option<u32>,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Event>, u64)> {
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
            &format!("SELECT COUNT(*) FROM actions {WHERE}"),
            &p[..3],
            |r| r.get(0),
        )?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT rowid, name, action, ua, txid, height FROM actions {WHERE}
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
}

impl SqliteIndex {
    /// The last synced position, or `None` if never synced.
    pub fn checkpoint(&self) -> Result<Option<Cursor>> {
        let row: Option<(i64, Option<Vec<u8>>)> = self
            .conn
            .query_row("SELECT height, hash FROM sync_state WHERE id = 0", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .context("reading sync_state")?;
        Ok(row.and_then(|(height, hash)| {
            (height != 0).then(|| Cursor {
                height: BlockHeight::from_u32(height as u32),
                hash: hash.and_then(|v| v.try_into().ok()),
            })
        }))
    }

    /// Roll the index back to `to`, dropping every action above the fork.
    /// Called by the observer loop on reorg.
    pub fn rewind(&self, to: BlockHeight) -> Result<()> {
        let to_h = u32::from(to) as i64;
        let tx = self.conn.unchecked_transaction()?;
        // Drop every action above the fork. The prior action for each affected
        // name is below it and untouched, so `current_names` re-folds to the
        // correct pre-reorg state. Proof material above the fork proves
        // orphaned blocks — purge it with the actions.
        tx.execute("DELETE FROM actions WHERE height > ?1", params![to_h])?;
        tx.execute("DELETE FROM proof_material WHERE height > ?1", params![to_h])?;
        set_sync_state(&tx, to_h, None)?;
        tx.commit()?;
        Ok(())
    }

    /// Verify and record any Name Notes in `notes`, then advance the cursor to
    /// `at`. A note is recorded only if its memo parses as a ZNS action, the
    /// action fits the name's chain state, and its binding `cmx` checks out.
    /// Returns the recorded actions so the observer can materialize their
    /// proof context.
    pub fn apply_notes(&self, at: Cursor, notes: &[NameNote]) -> Result<Vec<Recorded>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut recorded = Vec::new();
        for n in notes {
            // The kernel parser is strict (exact field counts, DNS-label
            // names). Challenge/confirm memos are auth-flow traffic, not
            // index actions; everything else unparseable is not a Name Note.
            let Ok(ParsedMemo::Lifecycle { action, name, ua, prev_rcm: memo_prev }) =
                memo::parse_memo(n.memo.as_slice())
            else {
                continue;
            };

            // Names are sender-chosen, and `resolve` falls back to an
            // address lookup when the exact-name probe misses — so a name that
            // *is* a plausible UA string would permanently shadow reverse
            // lookup for that address. Reject the UA namespace outright.
            if shadows_ua_namespace(name) {
                tracing::debug!("ZNS name {:?} collides with the UA namespace", name);
                continue;
            }

            // NOTE(auth): the binding check below proves *integrity*, not
            // origin — the rcm chain is derivable from public data, so any
            // sender with the forked builder can mint a correctly-bound
            // action. Per DESIGN.md §9, auth (who may CLAIM/UPDATE/RELEASE)
            // is registry policy enforced *before* minting; the core protocol
            // gives the resolver no way to tell a registry mint from a
            // third-party note to addr_reg. If a resolver-side origin gate is
            // ever added (e.g. a registry signature), it belongs here.

            // The action must fit the name's history. `prev_rcm` is the current
            // tip's rcm — reconstructed from the log, *not* the memo. The fold
            // rule (kernel `chain`: CLAIM starts a fresh chain on an unseen or
            // released name; UPDATE/RELEASE extend a live tip) drops anything
            // ill-fitting.
            let tip = lookup(&tx, name)?.map(|t| t.tip());
            let Some(prev_rcm) = prev_rcm_for(tip.as_ref(), action) else {
                tracing::debug!("ZNS {:?} for {:?} does not fit chain state", action, name);
                continue;
            };

            // A cmx match proves the binding *and* that it extends `prev_rcm`.
            let Some(rcm) = verify::verify_binding(&n.note, n.cmx, action, name, ua, &prev_rcm)
            else {
                // The memo's disclosed witness is *not* an input above — the
                // canonical prev_rcm is our own tip. But on a mismatch it
                // distinguishes garbage from equivocation: a note that
                // verifies under its own disclosed witness while failing
                // under our tip means the registry minted off a different
                // predecessor — a fork (DESIGN.md §5).
                if let Some(claimed) = memo_prev.filter(|p| {
                    *p != prev_rcm
                        && verify::verify_binding(&n.note, n.cmx, action, name, ua, p).is_some()
                }) {
                    tracing::warn!(
                        name,
                        height = n.height,
                        claimed = hex::encode(claimed),
                        tip = hex::encode(prev_rcm),
                        "registry fork: Name Note extends a different predecessor than our tip"
                    );
                } else {
                    tracing::debug!("ZNS binding mismatch for {:?}", name);
                }
                continue;
            };

            record_action(&tx, name, ua, &rcm, &n.cmx, &n.txid, n.height, action, n.action_index)?;
            recorded.push(Recorded { txid: n.txid, height: n.height });
        }
        set_sync_state(&tx, u32::from(at.height) as i64, at.hash.as_ref().map(|h| h.as_slice()))?;
        tx.commit()?;
        Ok(recorded)
    }

    /// Store the proof context for a Name Note transaction (idempotent).
    pub fn insert_proof_material(
        &self,
        txid: &[u8; 32],
        height: u32,
        raw_tx: &[u8],
        header: &[u8],
        merkle_branch: &[[u8; 32]],
        merkle_index: u32,
    ) -> Result<()> {
        let branch: Vec<u8> = merkle_branch.iter().flatten().copied().collect();
        self.conn.execute(
            "INSERT OR IGNORE INTO proof_material
                 (txid, height, raw_tx, header, merkle_branch, merkle_index)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![txid.as_slice(), height as i64, raw_tx, header, branch, merkle_index as i64],
        )?;
        Ok(())
    }

    /// The proof context for a Name Note transaction, if materialized.
    pub fn proof_material(&self, txid: &[u8; 32]) -> Result<Option<ProofMaterial>> {
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
                            .map(|c| c.try_into().expect("exact 32-byte chunks"))
                            .collect(),
                        merkle_index: r.get::<_, i64>(3)? as u32,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// A name's full applied history in chain order — the rows behind the
    /// `chain` RPC and the `resolve` proof's current segment.
    pub fn chain_rows(&self, name: &str) -> Result<Vec<ChainRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT action, ua, height, txid, action_index FROM actions
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
}

// ---------- internals (operate on a Connection; Transaction derefs to it) ----------

/// The name's current chain tip — its latest action, *including* a `release`
/// (which the resolution view hides, but the fold rule needs to see).
fn lookup(conn: &Connection, name: &str) -> rusqlite::Result<Option<NameChainEntry>> {
    conn.query_row(
        "SELECT name, ua, rcm, action FROM actions
         WHERE name = ?1 ORDER BY height DESC, rowid DESC LIMIT 1",
        params![name],
        row_to_entry,
    )
    .optional()
}

/// Append a verified action to the log.
#[allow(clippy::too_many_arguments)]
fn record_action(
    conn: &Connection,
    name: &str,
    ua: &str,
    rcm: &[u8; 32],
    cmx: &[u8; 32],
    txid: &[u8; 32],
    height: u32,
    action: Action,
    action_index: usize,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO actions (name, height, action, ua, rcm, cmx, txid, action_index)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            name,
            height as i64,
            action_str(action),
            ua,
            rcm.as_slice(),
            cmx.as_slice(),
            txid.as_slice(),
            action_index as i64,
        ],
    )?;
    Ok(())
}

/// Whether `name` lives in the unified-address namespace (`u1…` mainnet,
/// `utest1…` testnet) and could therefore shadow a reverse lookup — the `ua`
/// column only ever holds UAs, so these are the only colliding prefixes.
fn shadows_ua_namespace(name: &str) -> bool {
    name.starts_with("u1") || name.starts_with("utest1")
}

fn set_sync_state(conn: &Connection, height: i64, hash: Option<&[u8]>) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO sync_state (id, height, hash) VALUES (0, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET height = excluded.height, hash = excluded.hash",
        params![height, hash],
    )?;
    Ok(())
}

fn row_to_entry(r: &Row) -> rusqlite::Result<NameChainEntry> {
    let rcm: Vec<u8> = r.get(2)?;
    Ok(NameChainEntry {
        name: r.get(0)?,
        ua: r.get(1)?,
        rcm: rcm.try_into().map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 0))?,
        last_action: parse_action(&r.get::<_, String>(3)?)?,
    })
}

fn row_to_registration(r: &Row) -> rusqlite::Result<Registration> {
    let txid: Vec<u8> = r.get(2)?;
    Ok(Registration {
        name: r.get(0)?,
        ua: r.get(1)?,
        txid: txid.try_into().map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 0))?,
        height: r.get::<_, i64>(3)? as u32,
        last_action: parse_action(&r.get::<_, String>(4)?)?,
    })
}

fn parse_action(s: &str) -> rusqlite::Result<Action> {
    match s {
        "claim" => Ok(Action::Claim),
        "update" => Ok(Action::Update),
        "release" => Ok(Action::Release),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(3, 0)),
    }
}

fn action_str(a: Action) -> &'static str {
    match a {
        Action::Claim => "claim",
        Action::Update => "update",
        Action::Release => "release",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_starts_empty() {
        let idx = SqliteIndex::open_in_memory().unwrap();
        assert_eq!(idx.last_scanned_height().unwrap(), None);
        assert_eq!(idx.name_count().unwrap(), 0);
        assert!(idx.checkpoint().unwrap().is_none());
    }

    // The fold rule itself (`prev_rcm_for`) is kernel code, tested in
    // `zns_verify::chain`; here we test the index machinery around it.

    // ---- the view + rewind, driven by the append half (no crypto) ----

    fn seed(idx: &SqliteIndex, name: &str, ua: &str, rcm: [u8; 32], height: u32, action: Action) {
        record_action(&idx.conn, name, ua, &rcm, &[0u8; 32], &[0u8; 32], height, action, 0)
            .unwrap();
    }

    #[test]
    fn rewind_restores_prior_action_below_the_fork() {
        let idx = SqliteIndex::open_in_memory().unwrap();
        seed(&idx, "alice", "u1a", [1u8; 32], 100, Action::Claim);
        seed(&idx, "alice", "u1b", [2u8; 32], 200, Action::Update);
        assert_eq!(idx.resolve_by_name("alice").unwrap().unwrap().ua, "u1b");

        // Reorg to 150 drops the UPDATE@200; the CLAIM@100 below the fork
        // survives, so the name reverts to its prior state rather than vanishing.
        idx.rewind(BlockHeight::from_u32(150)).unwrap();
        let reg = idx.resolve_by_name("alice").unwrap().expect("name must survive the reorg");
        assert_eq!(reg.ua, "u1a");
        assert_eq!(reg.last_action, Action::Claim);
    }

    #[test]
    fn released_name_can_be_reclaimed() {
        let idx = SqliteIndex::open_in_memory().unwrap();
        seed(&idx, "alice", "u1a", [1u8; 32], 100, Action::Claim);
        seed(&idx, "alice", "", [2u8; 32], 200, Action::Release);

        // Released → not a current registration.
        assert!(idx.resolve_by_name("alice").unwrap().is_none());
        assert_eq!(idx.name_count().unwrap(), 0);
        // ...but the tip is still visible to the fold, which treats it as free.
        let tip = idx.lookup("alice").unwrap().map(|t| t.tip());
        assert_eq!(prev_rcm_for(tip.as_ref(), Action::Claim), Some(zns_verify::ZERO_PREV_RCM));

        seed(&idx, "alice", "u1c", [3u8; 32], 300, Action::Claim);
        let reg = idx.resolve_by_name("alice").unwrap().unwrap();
        assert_eq!(reg.ua, "u1c");
        assert_eq!(reg.last_action, Action::Claim);
    }

    #[test]
    fn events_filters_and_paginates_newest_first() {
        let idx = SqliteIndex::open_in_memory().unwrap();
        seed(&idx, "alice", "u1a", [1u8; 32], 100, Action::Claim);
        seed(&idx, "bob", "u1b", [2u8; 32], 150, Action::Claim);
        seed(&idx, "alice", "u1c", [3u8; 32], 200, Action::Update);

        // Unfiltered: newest first, total = all rows.
        let (all, total) = idx.events(None, None, None, 50, 0).unwrap();
        assert_eq!(total, 3);
        assert_eq!(all.iter().map(|e| e.height).collect::<Vec<_>>(), vec![200, 150, 100]);

        // Name filter.
        let (alice, total) = idx.events(Some("alice"), None, None, 50, 0).unwrap();
        assert_eq!(total, 2);
        assert!(alice.iter().all(|e| e.name == "alice"));

        // Action filter.
        let (claims, total) = idx.events(None, Some(Action::Claim), None, 50, 0).unwrap();
        assert_eq!(total, 2);
        assert!(claims.iter().all(|e| e.action == Action::Claim));

        // `since_height` is strictly greater-than.
        let (since, _) = idx.events(None, None, Some(150), 50, 0).unwrap();
        assert_eq!(since.iter().map(|e| e.height).collect::<Vec<_>>(), vec![200]);

        // Pagination: `total` is pre-pagination, page picks the middle row.
        let (page, total) = idx.events(None, None, None, 1, 1).unwrap();
        assert_eq!(total, 3);
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].height, 150);
    }

    // ---- proof material: round trip + reorg purge ----

    #[test]
    fn proof_material_round_trips_and_purges_on_rewind() {
        let idx = SqliteIndex::open_in_memory().unwrap();
        let txid = [0xABu8; 32];
        let branch = [[1u8; 32], [2u8; 32]];
        idx.insert_proof_material(&txid, 200, b"rawtx", b"header", &branch, 1).unwrap();
        // Idempotent.
        idx.insert_proof_material(&txid, 200, b"rawtx", b"header", &branch, 1).unwrap();

        let m = idx.proof_material(&txid).unwrap().expect("stored");
        assert_eq!(m.raw_tx, b"rawtx");
        assert_eq!(m.header, b"header");
        assert_eq!(m.merkle_branch, branch.to_vec());
        assert_eq!(m.merkle_index, 1);

        // A rewind below the material's height purges it (it proves an
        // orphaned block); material at or below the fork survives.
        idx.rewind(BlockHeight::from_u32(150)).unwrap();
        assert!(idx.proof_material(&txid).unwrap().is_none());
    }

    #[test]
    fn chain_rows_come_back_in_chain_order() {
        let idx = SqliteIndex::open_in_memory().unwrap();
        seed(&idx, "alice", "u1a", [1u8; 32], 100, Action::Claim);
        seed(&idx, "alice", "u1b", [2u8; 32], 200, Action::Update);
        seed(&idx, "bob", "u1x", [3u8; 32], 150, Action::Claim);

        let rows = idx.chain_rows("alice").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].action, Action::Claim);
        assert_eq!(rows[1].action, Action::Update);
        assert_eq!(rows[1].ua, "u1b");
    }

    // ---- the UA-namespace guard ----

    #[test]
    fn ua_namespace_is_rejected_as_a_name() {
        assert!(shadows_ua_namespace("u1somethinglong"));
        assert!(shadows_ua_namespace("utest1somethinglong"));
        assert!(!shadows_ua_namespace("alice"));
        assert!(!shadows_ua_namespace("update")); // "u" alone is not the UA HRP
    }
}
