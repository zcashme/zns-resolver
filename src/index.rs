//! SQLite name index + the `seer-sync` [`Account`] impl that drives it.
//!
//! Two tables:
//!  - `names`      — current state per name (the lookup answer).
//!  - `sync_state` — last applied `(height, hash)`; the resume cursor.
//!
//! Reorgs are seer-sync's job: it calls [`Account::rewind`], which drops names
//! above the fork. There is no local reorg buffer.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use seer_sync::sync::scan::{Note as ScannedNote, Pool, ShieldedNote, Spend};
use seer_sync::sync::{Account, AccountError, Cursor};
use seer_sync::BlockHeight;

use crate::verify::{self, NameChainEntry};
use zns_verify::{Action, ZERO_PREV_RCM};

/// The SQLite-backed ZNS name index.
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

    /// The highest applied block height, or `None` if never synced.
    pub fn last_scanned_height(&self) -> Result<Option<u32>> {
        Ok(self.checkpoint().map(|c| u32::from(c.height)))
    }

    /// Number of names currently in the index.
    pub fn name_count(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
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
        let h = u32::from(to) as i64;
        self.conn
            .execute("DELETE FROM names WHERE height > ?1", params![h])?;
        set_sync_state(&self.conn, h, None)?;
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

            // prev_rcm is this name's reconstructed tip (not the memo). The
            // action must fit the chain: CLAIM only on an unseen name,
            // UPDATE/RELEASE only on a known one.
            let prev = lookup(&tx, &parsed.name)?;
            let prev_rcm = match (parsed.action, &prev) {
                (Action::Claim, None) => ZERO_PREV_RCM,
                (Action::Update | Action::Release, Some(p)) => p.rcm,
                _ => {
                    tracing::debug!(
                        "ZNS {:?} for {:?} does not fit chain state",
                        parsed.action,
                        parsed.name
                    );
                    continue;
                }
            };

            // A cmx match proves the binding *and* that it extends `prev_rcm`.
            let Some(rcm) =
                verify::verify_binding(note, cmx, parsed.action, &parsed.name, &parsed.ua, &prev_rcm)
            else {
                tracing::debug!("ZNS binding mismatch for {:?}", parsed.name);
                continue;
            };

            tx.execute(
                "INSERT INTO names (name, ua, rcm, cmx, height, last_action)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(name) DO UPDATE SET
                     ua = excluded.ua, rcm = excluded.rcm, cmx = excluded.cmx,
                     height = excluded.height, last_action = excluded.last_action",
                params![
                    parsed.name,
                    parsed.ua,
                    rcm.as_slice(),
                    cmx.as_slice(),
                    u32::from(n.height) as i64,
                    action_str(parsed.action),
                ],
            )?;
        }
        set_sync_state(&tx, u32::from(at.height) as i64, at.hash.as_ref().map(|h| h.as_slice()))?;
        tx.commit()?;
        Ok(())
    }
}

// ---------- internals (operate on a Connection; Transaction derefs to it) ----------

fn lookup(conn: &Connection, name: &str) -> rusqlite::Result<Option<NameChainEntry>> {
    conn.query_row(
        "SELECT name, ua, rcm, last_action FROM names WHERE name = ?",
        params![name],
        row_to_entry,
    )
    .optional()
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
}
