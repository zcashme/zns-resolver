//! Layer 4: SQLite-backed index for name → (cmx, ua, height).
//!
//! Narrow three-table schema:
//!  - `names`         — current state per name (the lookup answer).
//!  - `scan_state`    — last-scanned height + chain tip seen.
//!  - `recent_blocks` — reorg buffer (rolling height/hash pairs).
//!
//! All state-mutating operations happen in per-block transactions: a
//! single block's hits + chain-state updates + buffer extension all
//! commit atomically, or none do.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

use seer_sync::scan::ReorgBuffer;

use crate::verify::{NameChainEntry, ValidZnsAction};
use zns_verify::Action;

/// The SQLite-backed ZNS index.
pub struct SqliteIndex {
    conn: Connection,
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS names (
    name        TEXT NOT NULL PRIMARY KEY,
    ua          TEXT NOT NULL,
    rcm         BLOB NOT NULL,
    cmx         BLOB NOT NULL,
    height      INTEGER NOT NULL,
    last_action TEXT NOT NULL CHECK (last_action IN ('claim','update','release'))
);

CREATE INDEX IF NOT EXISTS names_height_idx ON names(height);

CREATE TABLE IF NOT EXISTS scan_state (
    id                  INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
    last_scanned_height INTEGER NOT NULL,
    chain_tip_seen      INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS recent_blocks (
    height INTEGER NOT NULL PRIMARY KEY,
    hash   BLOB NOT NULL
);
"#;

impl SqliteIndex {
    /// Open or create the SQLite database at `path`. Applies schema
    /// migrations and enables WAL mode for concurrent readers.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).context("opening sqlite db")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA_SQL)
            .context("applying schema")?;
        Ok(Self { conn })
    }

    /// Open or create at the given path; in-memory if path is `:memory:`.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    /// The highest fully-scanned block height. `None` if the index has
    /// never been scanned.
    pub fn last_scanned_height(&self) -> Result<Option<u32>> {
        let h: Option<i64> = self
            .conn
            .query_row(
                "SELECT last_scanned_height FROM scan_state WHERE id = 0",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(h.map(|x| x as u32))
    }

    /// Look up the current entry for `name`, if any.
    pub fn lookup(&self, name: &str) -> Result<Option<NameChainEntry>> {
        self.conn
            .query_row(
                "SELECT name, ua, rcm, last_action FROM names WHERE name = ?",
                params![name],
                |r| {
                    let rcm_blob: Vec<u8> = r.get(2)?;
                    let rcm: [u8; 32] = rcm_blob
                        .try_into()
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 0))?;
                    let action_s: String = r.get(3)?;
                    let last_action = match action_s.as_str() {
                        "claim" => Action::Claim,
                        "update" => Action::Update,
                        "release" => Action::Release,
                        _ => return Err(rusqlite::Error::IntegralValueOutOfRange(3, 0)),
                    };
                    Ok(NameChainEntry {
                        name: r.get(0)?,
                        ua: r.get(1)?,
                        rcm,
                        last_action,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Load the ReorgBuffer from persisted recent_blocks rows.
    pub fn load_reorg_buffer(&self, max_depth: usize) -> Result<ReorgBuffer> {
        let mut stmt = self
            .conn
            .prepare("SELECT height, hash FROM recent_blocks ORDER BY height ASC")?;
        let entries: Result<Vec<(u32, [u8; 32])>, _> = stmt
            .query_map([], |r| {
                let h: i64 = r.get(0)?;
                let hash_blob: Vec<u8> = r.get(1)?;
                let hash: [u8; 32] = hash_blob
                    .try_into()
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(1, 0))?;
                Ok((h as u32, hash))
            })?
            .collect();
        Ok(ReorgBuffer::load(entries?, max_depth))
    }

    /// Commit one block's worth of state changes atomically.
    ///
    /// Applies all `hits` (rejecting any that fail `valid_chain_advance`
    /// against the current state), updates `last_scanned_height` and
    /// `chain_tip_seen`, and persists the `(height, hash)` to the reorg
    /// buffer table. Either all of these succeed or none do.
    pub fn commit_block(
        &mut self,
        height: u32,
        block_hash: [u8; 32],
        hits: &[(ValidZnsAction, [u8; 32])],  // (hit, prev_rcm from memo)
        chain_tip_seen: u32,
    ) -> Result<BlockCommitOutcome> {
        let tx = self.conn.transaction()?;
        let mut applied = 0;
        let mut rejected = Vec::new();

        for (hit, prev_rcm) in hits {
            // Re-read inside the transaction for consistency.
            let prev = tx
                .query_row(
                    "SELECT name, ua, rcm, last_action FROM names WHERE name = ?",
                    params![hit.name],
                    |r| {
                        let rcm_blob: Vec<u8> = r.get(2)?;
                        let rcm: [u8; 32] = rcm_blob
                            .try_into()
                            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, 0))?;
                        let action_s: String = r.get(3)?;
                        let last_action = match action_s.as_str() {
                            "claim" => Action::Claim,
                            "update" => Action::Update,
                            "release" => Action::Release,
                            _ => return Err(rusqlite::Error::IntegralValueOutOfRange(3, 0)),
                        };
                        Ok(NameChainEntry {
                            name: r.get(0)?,
                            ua: r.get(1)?,
                            rcm,
                            last_action,
                        })
                    },
                )
                .optional()?;

            let decision = crate::verify::valid_chain_advance(prev.as_ref(), hit, prev_rcm);
            match decision {
                crate::verify::ChainAdvanceResult::Accept => {
                    tx.execute(
                        "INSERT INTO names (name, ua, rcm, cmx, height, last_action)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                         ON CONFLICT(name) DO UPDATE SET
                             ua = excluded.ua,
                             rcm = excluded.rcm,
                             cmx = excluded.cmx,
                             height = excluded.height,
                             last_action = excluded.last_action",
                        params![
                            hit.name,
                            hit.ua,
                            hit.rcm.as_slice(),
                            hit.cmx.as_slice(),
                            height as i64,
                            action_as_str(hit.action),
                        ],
                    )?;
                    applied += 1;
                }
                crate::verify::ChainAdvanceResult::Reject(reason) => {
                    rejected.push(reason);
                }
            }
        }

        // Scan state.
        tx.execute(
            "INSERT INTO scan_state (id, last_scanned_height, chain_tip_seen)
             VALUES (0, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET
                 last_scanned_height = excluded.last_scanned_height,
                 chain_tip_seen = excluded.chain_tip_seen",
            params![height as i64, chain_tip_seen as i64],
        )?;

        // Reorg buffer (just append; pruning happens in rewind_to / pruning task).
        tx.execute(
            "INSERT OR REPLACE INTO recent_blocks (height, hash) VALUES (?1, ?2)",
            params![height as i64, block_hash.as_slice()],
        )?;

        tx.commit()?;

        Ok(BlockCommitOutcome { applied, rejected })
    }

    /// Rewind to (and including) `fork_height - 1`: deletes all rows at
    /// heights `>= fork_height` from `names` and `recent_blocks`, and
    /// resets `last_scanned_height` to `fork_height - 1`.
    ///
    /// **Caveat**: this drops only the *latest* per-name entries that
    /// happened at or after `fork_height`. If a name was UPDATEd at
    /// height H >= fork_height after a CLAIM at height H' < fork_height,
    /// the row gets deleted entirely (not rolled back to the CLAIM
    /// state). To restore the prior state correctly, the caller must
    /// re-scan from `fork_height` after rewinding.
    pub fn rewind_to(&mut self, fork_height: u32) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM names WHERE height >= ?1",
            params![fork_height as i64],
        )?;
        tx.execute(
            "DELETE FROM recent_blocks WHERE height >= ?1",
            params![fork_height as i64],
        )?;
        tx.execute(
            "INSERT INTO scan_state (id, last_scanned_height, chain_tip_seen)
             VALUES (0, ?1, ?1)
             ON CONFLICT(id) DO UPDATE SET
                 last_scanned_height = excluded.last_scanned_height",
            params![(fork_height.saturating_sub(1)) as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Count of names currently in the index.
    pub fn name_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
        Ok(n as u64)
    }
}

/// Outcome of one [`SqliteIndex::commit_block`] call.
#[derive(Debug)]
pub struct BlockCommitOutcome {
    /// Number of ZNS hits applied (Accept path).
    pub applied: usize,
    /// Human-readable rejection reasons (Reject path).
    pub rejected: Vec<String>,
}

fn action_as_str(a: Action) -> &'static str {
    match a {
        Action::Claim => "claim",
        Action::Update => "update",
        Action::Release => "release",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hit(name: &str, ua: &str, rcm_byte: u8, cmx_byte: u8, action: Action) -> ValidZnsAction {
        ValidZnsAction {
            action,
            name: name.to_string(),
            ua: ua.to_string(),
            rcm: [rcm_byte; 32],
            cmx: [cmx_byte; 32],
        }
    }

    #[test]
    fn open_in_memory_creates_schema() {
        let idx = SqliteIndex::open_in_memory().unwrap();
        assert_eq!(idx.last_scanned_height().unwrap(), None);
        assert_eq!(idx.name_count().unwrap(), 0);
    }

    #[test]
    fn commit_block_applies_hits_and_updates_state() {
        let mut idx = SqliteIndex::open_in_memory().unwrap();
        let hits = vec![(make_hit("alice", "u1xxx", 1, 2, Action::Claim), [0u8; 32])];
        let outcome = idx.commit_block(100, [99u8; 32], &hits, 100).unwrap();
        assert_eq!(outcome.applied, 1);
        assert!(outcome.rejected.is_empty());
        assert_eq!(idx.last_scanned_height().unwrap(), Some(100));
        let entry = idx.lookup("alice").unwrap().unwrap();
        assert_eq!(entry.ua, "u1xxx");
    }

    #[test]
    fn commit_block_rejects_invalid_update() {
        let mut idx = SqliteIndex::open_in_memory().unwrap();
        // First a CLAIM.
        let claim = vec![(make_hit("alice", "u1old", 1, 2, Action::Claim), [0u8; 32])];
        idx.commit_block(100, [1u8; 32], &claim, 100).unwrap();

        // Then an UPDATE with wrong prev_rcm.
        let bad_update = vec![(make_hit("alice", "u1evil", 9, 9, Action::Update), [99u8; 32])];
        let outcome = idx.commit_block(101, [2u8; 32], &bad_update, 101).unwrap();
        assert_eq!(outcome.applied, 0);
        assert_eq!(outcome.rejected.len(), 1);

        // alice should still resolve to u1old.
        let entry = idx.lookup("alice").unwrap().unwrap();
        assert_eq!(entry.ua, "u1old");
    }

    #[test]
    fn rewind_to_drops_above() {
        let mut idx = SqliteIndex::open_in_memory().unwrap();
        let claim = vec![(make_hit("alice", "u1xxx", 1, 2, Action::Claim), [0u8; 32])];
        idx.commit_block(100, [1u8; 32], &claim, 100).unwrap();
        let update = vec![(make_hit("alice", "u1new", 2, 3, Action::Update), [1u8; 32])];
        idx.commit_block(101, [2u8; 32], &update, 101).unwrap();
        assert!(idx.lookup("alice").unwrap().is_some());

        idx.rewind_to(101).unwrap();
        assert!(idx.lookup("alice").unwrap().is_none());
        assert_eq!(idx.last_scanned_height().unwrap(), Some(100));
    }
}
