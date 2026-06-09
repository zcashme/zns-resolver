//! SQLite name index + the `seer-sync` [`Account`] impl that drives it.
//!
//! The blockchain is the state machine; this index is a materialized view of it:
//!  - `actions`    — append-only log, one row per applied Name Note. The
//!    canonical per-name history. The `current_names` view folds it to the
//!    latest non-released action per name (the resolution answer).
//!  - `sync_state` — last applied `(height, hash)`; the resume cursor.
//!
//! Reorgs are seer-sync's job: it calls [`Account::rewind`], which deletes
//! actions above the fork. Because the log retains history, the prior action
//! for each affected name is still present, so the view re-folds correctly at
//! any rewind depth — no separate reorg buffer is needed.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use incrementalmerkletree::{Position, Retention};
use orchard::tree::MerkleHashOrchard;
use rusqlite::{params, Connection, OptionalExtension, Row};
use seer_sync::db::commitment_tree;
use seer_sync::sync::scan::{Commitment, Note as ScannedNote, Pool, ShieldedNote, Spend};
use seer_sync::sync::{Account, AccountError, Cursor};
use seer_sync::BlockHeight;

use crate::verify::{self, NameChainEntry};
use zns_verify::{Action, ZERO_PREV_RCM};

/// Tree checkpoints retained for reorg rollback — comfortably beyond seer-sync's
/// exponential rewind backoff.
const MAX_CHECKPOINTS: usize = 100;

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

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS actions (
    name    TEXT NOT NULL,
    height  INTEGER NOT NULL,
    action  TEXT NOT NULL CHECK (action IN ('claim','update','release')),
    ua      TEXT NOT NULL,
    rcm     BLOB NOT NULL,
    cmx     BLOB NOT NULL,
    txid    BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS actions_name_height_idx ON actions(name, height);

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
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA_SQL).context("applying schema")?;
        commitment_tree::init_tables(&conn).context("creating commitment-tree tables")?;
        Ok(Self { conn })
    }

    /// Open an ephemeral in-memory index (tests).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA_SQL)?;
        commitment_tree::init_tables(&conn)?;
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
        Ok(self.checkpoint().map(|c| u32::from(c.height)))
    }

    /// Number of names currently in the index.
    pub fn name_count(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM current_names", [], |r| r.get(0))?;
        Ok(n as u64)
    }
}

impl Account for SqliteIndex {
    fn checkpoint(&self) -> Option<Cursor> {
        let (height, hash): (i64, Option<Vec<u8>>) = self
            .conn
            .query_row("SELECT height, hash FROM sync_state WHERE id = 0", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .ok()??;
        (height != 0).then(|| Cursor {
            height: BlockHeight::from_u32(height as u32),
            hash: hash.and_then(|v| v.try_into().ok()),
        })
    }

    fn rewind(&self, to: BlockHeight) -> Result<(), AccountError> {
        let to_h = u32::from(to);
        let tx = self.conn.unchecked_transaction()?;

        // The commitment tree must roll back in lockstep, but it can only
        // truncate to one of its per-batch checkpoints. Find the latest
        // checkpoint at or below the fork and rewind *both* the tree and the
        // actions log to that height — possibly a little below `to`, but the two
        // stay consistent (seer-sync simply re-scans forward from here).
        let checkpoint_at_or_below: Option<u32> = tx
            .query_row(
                "SELECT MAX(checkpoint_id) FROM orchard_tree_checkpoints WHERE checkpoint_id <= ?1",
                params![to_h],
                |r| r.get::<_, Option<u32>>(0),
            )
            .optional()?
            .flatten();

        let effective = match checkpoint_at_or_below {
            Some(c) => {
                let mut tree = commitment_tree::orchard_tree(&tx, MAX_CHECKPOINTS)?;
                tree.truncate_to_checkpoint(&BlockHeight::from_u32(c))?;
                drop(tree);
                c
            }
            None => {
                // No checkpoint at/below the fork: any tree state is entirely
                // above it, so reset the tree (it rebuilds on re-scan).
                tx.execute_batch(
                    "DELETE FROM orchard_tree_checkpoint_marks_removed;
                     DELETE FROM orchard_tree_checkpoints;
                     DELETE FROM orchard_tree_shards;
                     DELETE FROM orchard_tree_cap;",
                )?;
                to_h
            }
        };

        // Drop every action above the effective fork. The prior action for each
        // affected name is below it and untouched, so `current_names` re-folds to
        // the correct pre-reorg state.
        tx.execute("DELETE FROM actions WHERE height > ?1", params![effective as i64])?;
        set_sync_state(&tx, effective as i64, None)?;
        tx.commit()?;
        Ok(())
    }

    fn owns_nf(&self, _pool: Pool, _nf: &[u8; 32]) -> Result<bool, AccountError> {
        // The resolver scans incoming-only with addr_reg's IVK; it derives no
        // nullifiers and tracks no spends.
        Ok(false)
    }

    fn apply(&self, at: Cursor, notes: &[ScannedNote], _spends: &[Spend]) -> Result<(), AccountError> {
        let tx = self.conn.unchecked_transaction()?;
        for n in notes {
            let ShieldedNote::Orchard(note) = &n.note else { continue };
            let (Some(cmx), Some(memo)) = (n.cmx, n.memo.as_ref()) else { continue };
            let Some(parsed) = verify::parse_memo(memo.as_slice()) else { continue };

            // The action must fit the name's history. `prev_rcm` is the current
            // tip's rcm — reconstructed from the log, *not* the memo. The fold
            // rule (CLAIM starts a fresh chain on an unseen or released name;
            // UPDATE/RELEASE extend a live tip) drops anything ill-fitting.
            let tip = lookup(&tx, &parsed.name)?;
            let Some(prev_rcm) = prev_rcm_for(tip.as_ref(), parsed.action) else {
                tracing::debug!(
                    "ZNS {:?} for {:?} does not fit chain state",
                    parsed.action,
                    parsed.name
                );
                continue;
            };

            // A cmx match proves the binding *and* that it extends `prev_rcm`.
            let Some(rcm) =
                verify::verify_binding(note, cmx, parsed.action, &parsed.name, &parsed.ua, &prev_rcm)
            else {
                tracing::debug!("ZNS binding mismatch for {:?}", parsed.name);
                continue;
            };

            record_action(
                &tx,
                &parsed.name,
                &parsed.ua,
                &rcm,
                &cmx,
                &n.txid,
                u32::from(n.height),
                parsed.action,
            )?;
        }
        set_sync_state(&tx, u32::from(at.height) as i64, at.hash.as_ref().map(|h| h.as_slice()))?;
        tx.commit()?;
        Ok(())
    }

    fn wants_commitments(&self) -> bool {
        true
    }

    fn apply_commitments(&self, at: Cursor, commitments: &[Commitment]) -> Result<(), AccountError> {
        let tx = self.conn.unchecked_transaction()?;

        // Positions to retain witnesses for: every Name Note `cmx` this index has
        // recorded. `apply` runs before this in the same batch, so the batch's
        // new Name Notes are already present. The set is small (a name service),
        // so loading it whole is cheap. NOTE: tree update and `apply` are
        // separate transactions; a crash between them can leave the tree one
        // batch behind. That is fail-safe — a stale/short tree yields a witness
        // whose root won't match the anchor, so the producer's self-check drops
        // it rather than serving a wrong proof.
        let marked: HashSet<[u8; 32]> = {
            let mut stmt = tx.prepare("SELECT cmx FROM actions")?;
            let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
            let mut set = HashSet::new();
            for cmx in rows {
                if let Ok(bytes) = <[u8; 32]>::try_from(cmx?.as_slice()) {
                    set.insert(bytes);
                }
            }
            set
        };

        let mut tree = commitment_tree::orchard_tree(&tx, MAX_CHECKPOINTS)?;
        if let Some(first) = commitments.first() {
            let mut leaves = Vec::with_capacity(commitments.len());
            for c in commitments {
                let leaf = MerkleHashOrchard::from_bytes(&c.cmx)
                    .into_option()
                    .ok_or("non-canonical cmx in commitment firehose")?;
                let retention = if marked.contains(&c.cmx) {
                    Retention::Marked
                } else {
                    Retention::Ephemeral
                };
                leaves.push((leaf, retention));
            }
            tree.batch_insert(Position::from(first.position), leaves.into_iter())?;
        }
        // Checkpoint every batch height so `rewind` always has a target.
        tree.checkpoint(at.height)?;
        drop(tree);

        tx.commit()?;
        Ok(())
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
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO actions (name, height, action, ua, rcm, cmx, txid)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            name,
            height as i64,
            action_str(action),
            ua,
            rcm.as_slice(),
            cmx.as_slice(),
            txid.as_slice(),
        ],
    )?;
    Ok(())
}

/// The `prev_rcm` an `action` must extend given the name's current `tip`, or
/// `None` if the action does not fit the chain. This is the protocol's
/// transition rule as a pure fold over the tip:
///  - CLAIM starts a fresh chain (zero genesis) on an unseen *or* released name.
///  - UPDATE/RELEASE extend a live (non-released) tip, chaining off its `rcm`.
fn prev_rcm_for(tip: Option<&NameChainEntry>, action: Action) -> Option<[u8; 32]> {
    match (action, tip) {
        (Action::Claim, None) => Some(ZERO_PREV_RCM),
        (Action::Claim, Some(t)) if t.last_action == Action::Release => Some(ZERO_PREV_RCM),
        (Action::Update | Action::Release, Some(t)) if t.last_action != Action::Release => {
            Some(t.rcm)
        }
        _ => None,
    }
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
        assert!(idx.checkpoint().is_none());
    }

    fn entry(action: Action, rcm: [u8; 32]) -> NameChainEntry {
        NameChainEntry { name: "alice".into(), ua: "u1".into(), rcm, last_action: action }
    }

    // ---- the fold rule (#prev_rcm_for): pure, no crypto, no DB ----

    #[test]
    fn fold_claim_fits_unseen_or_released_name() {
        // Fresh name: CLAIM extends the zero genesis.
        assert_eq!(prev_rcm_for(None, Action::Claim), Some(ZERO_PREV_RCM));
        // Released name is free: a new CLAIM is allowed, again from genesis.
        let released = entry(Action::Release, [9u8; 32]);
        assert_eq!(prev_rcm_for(Some(&released), Action::Claim), Some(ZERO_PREV_RCM));
        // Live name: a second CLAIM does not fit.
        let live = entry(Action::Claim, [1u8; 32]);
        assert_eq!(prev_rcm_for(Some(&live), Action::Claim), None);
    }

    #[test]
    fn fold_update_release_need_a_live_tip() {
        let live = entry(Action::Claim, [7u8; 32]);
        assert_eq!(prev_rcm_for(Some(&live), Action::Update), Some([7u8; 32]));
        assert_eq!(prev_rcm_for(Some(&live), Action::Release), Some([7u8; 32]));
        // No tip, or a released tip: nothing to extend.
        assert_eq!(prev_rcm_for(None, Action::Update), None);
        let released = entry(Action::Release, [7u8; 32]);
        assert_eq!(prev_rcm_for(Some(&released), Action::Update), None);
    }

    // ---- the view + rewind, driven by the append half (no crypto) ----

    fn seed(idx: &SqliteIndex, name: &str, ua: &str, rcm: [u8; 32], height: u32, action: Action) {
        record_action(&idx.conn, name, ua, &rcm, &[0u8; 32], &[0u8; 32], height, action).unwrap();
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
        assert_eq!(
            prev_rcm_for(idx.lookup("alice").unwrap().as_ref(), Action::Claim),
            Some(ZERO_PREV_RCM),
        );

        seed(&idx, "alice", "u1c", [3u8; 32], 300, Action::Claim);
        let reg = idx.resolve_by_name("alice").unwrap().unwrap();
        assert_eq!(reg.ua, "u1c");
        assert_eq!(reg.last_action, Action::Claim);
    }

    // ---- commitment-tree wiring: firehose → marked Name Note witnessed ----

    #[test]
    fn apply_commitments_marks_indexed_name_notes() {
        let idx = SqliteIndex::open_in_memory().unwrap();
        // A canonical cmx for a Name Note that `apply` would have recorded.
        let name_cmx = [3u8; 32];
        seed_cmx(&idx, "alice", name_cmx, 100, Action::Claim);

        // The batch's firehose: 4 commitments, position 2 is the Name Note.
        let commitments: Vec<Commitment> = (0u64..4)
            .map(|i| Commitment {
                position: i,
                cmx: if i == 2 { name_cmx } else { [(i as u8) + 10; 32] },
            })
            .collect();
        idx.apply_commitments(Cursor { height: BlockHeight::from_u32(100), hash: None }, &commitments)
            .unwrap();

        // The marked Name Note position is witnessable; an unmarked one is pruned.
        let tx = idx.conn.unchecked_transaction().unwrap();
        let tree = commitment_tree::orchard_tree(&tx, MAX_CHECKPOINTS).unwrap();
        let h = BlockHeight::from_u32(100);
        assert!(tree.witness_at_checkpoint_id(Position::from(2), &h).unwrap().is_some());
        assert!(tree.witness_at_checkpoint_id(Position::from(1), &h).is_err());
    }

    fn seed_cmx(idx: &SqliteIndex, name: &str, cmx: [u8; 32], height: u32, action: Action) {
        record_action(&idx.conn, name, "u1", &[1u8; 32], &cmx, &[0u8; 32], height, action).unwrap();
    }

    #[test]
    fn rewind_truncates_tree_in_lockstep() {
        let idx = SqliteIndex::open_in_memory().unwrap();
        let name_cmx = [3u8; 32];
        seed_cmx(&idx, "alice", name_cmx, 100, Action::Claim);

        // Batch @100: the Name Note at position 1. Batch @200: another leaf.
        idx.apply_commitments(
            Cursor { height: BlockHeight::from_u32(100), hash: None },
            &[
                Commitment { position: 0, cmx: [10u8; 32] },
                Commitment { position: 1, cmx: name_cmx },
            ],
        )
        .unwrap();
        idx.apply_commitments(
            Cursor { height: BlockHeight::from_u32(200), hash: None },
            &[Commitment { position: 2, cmx: [12u8; 32] }],
        )
        .unwrap();

        // Reorg to 150 → rolls the tree back to the checkpoint @100.
        idx.rewind(BlockHeight::from_u32(150)).unwrap();

        let tx = idx.conn.unchecked_transaction().unwrap();
        let tree = commitment_tree::orchard_tree(&tx, MAX_CHECKPOINTS).unwrap();
        // Checkpoint @100 still witnesses the Name Note; @200 is gone.
        assert!(tree
            .witness_at_checkpoint_id(Position::from(1), &BlockHeight::from_u32(100))
            .unwrap()
            .is_some());
        assert!(tree.root_at_checkpoint_id(&BlockHeight::from_u32(200)).unwrap().is_none());
    }
}
