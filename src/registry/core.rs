//! Transactional core of the registry.
//!
//! This module owns `DbConn` and all logic that must execute inside
//! SQLite transactions. The key invariants live here:
//!
//! - apply_batch: all admitted events + names updates + checkpoint written
//!   in ONE transaction, then commit. Checkpoint must not advance without
//!   the events.
//! - name_tip_in_tx sees uncommitted writes from the current batch (for
//!   correct prev_rcm chaining within the batch).
//! - rewind + rebuild_name_tip run in the same tx; rebuild must produce
//!   the same projection as the normal ingest path.
//!
//! Only the dedicated writer thread (in handle) calls into this.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use zcash_protocol::consensus::Parameters;

use zns_verify::{Action, Tip};

use super::storage;
use super::{Checkpoint, Cursor, NameNote, Registration, Event};  // types live in parent for now
use crate::orchard::DecryptedNote;
use super::lifecycle;

const REORG_SHALLOW_MAX: u32 = 30;

pub(super) struct DbConn {
    conn: Connection,
}

impl DbConn {
    pub(super) fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(storage::SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    pub(super) fn install_registry_config(
        &self,
        uivk: &str,
        network: &str,
        birthday: u32,
    ) -> rusqlite::Result<()> {
        if let Some((stored_uivk, stored_net, stored_birthday)) = self.registry_config()? {
            if stored_uivk != uivk {
                tracing::warn!(
                    stored = %stored_uivk,
                    "registry_account uivk already set; not changing"
                );
            }
            if stored_net != network {
                tracing::warn!(
                    stored = %stored_net,
                    "registry_account network already set; not changing"
                );
            }
            if stored_birthday != birthday as i64 {
                tracing::warn!(
                    stored = stored_birthday,
                    "registry_account birthday already set; not changing"
                );
            }
            return Ok(());
        }

        self.conn.execute(
            "INSERT INTO registry_account (id, uivk, network, birthday) VALUES (0, ?1, ?2, ?3)",
            params![uivk, network, birthday as i64],
        )?;
        Ok(())
    }

    pub(super) fn registry_uivk(&self) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT uivk FROM registry_account WHERE id = 0",
                [],
                |row| row.get(0),
            )
            .optional()
    }

    fn registry_config(&self) -> rusqlite::Result<Option<(String, String, i64)>> {
        self.conn
            .query_row(
                "SELECT uivk, network, birthday FROM registry_account WHERE id = 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
    }

    pub(super) fn checkpoint(&self) -> rusqlite::Result<Option<Checkpoint>> {
        self.conn
            .query_row(
                "SELECT height, hash, chain_tip_height, chain_tip_hash FROM scan_state WHERE id = 0",
                [],
                row_to_checkpoint,
            )
            .optional()
            .map_err(Into::into)
    }

    /// The main write path. Everything for the batch + the checkpoint advance
    /// happens inside one transaction.
    pub(super) fn apply_batch(
        &self,
        network: &impl Parameters,
        scanned: Cursor,
        live: Cursor,
        decrypted: &[DecryptedNote],
    ) -> rusqlite::Result<Vec<NameNote>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut indexed = Vec::new();

        for n in decrypted {
            let Some(claim) = lifecycle::lifecycle_claim_from_memo(n.memo.as_slice(), network) else {
                continue;
            };

            let tip = self.name_tip_in_tx(&tx, &claim.name)?;
            let Some((prev_rcm, psi, rcm)) =
                lifecycle::try_admit_name_note(&claim, n, tip.as_ref())
            else {
                lifecycle::warn_registry_fork(&claim, n, tip.as_ref());
                continue;
            };

            let name_note = NameNote {
                name: claim.name.clone(),
                ua: claim.ua.clone(),
                action: claim.action,
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

    pub(super) fn rewind(&self, fork_height: u32, scanned_height: u32) -> rusqlite::Result<()> {
        let depth = scanned_height.saturating_sub(fork_height);
        let tx = self.conn.unchecked_transaction()?;

        if depth > REORG_SHALLOW_MAX {
            tx.execute("DELETE FROM name_events", [])?;
            tx.execute("DELETE FROM names", [])?;
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

    // --- read methods also exposed via the actor, but implemented on conn ---
    pub(super) fn resolve_by_name(&self, name: &str) -> rusqlite::Result<Option<Registration>> {
        self.conn
            .query_row(
                "SELECT name, ua, txid, height, action FROM names WHERE name = ?1",
                params![name],
                row_to_registration,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(super) fn registrations_by_ua(
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

    pub(super) fn list_registrations(&self, limit: u32, offset: u32) -> rusqlite::Result<Vec<Registration>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, ua, txid, height, action FROM names ORDER BY name LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(params![limit, offset], row_to_registration)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub(super) fn name_count(&self) -> rusqlite::Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    pub(super) fn events(
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
            "SELECT rowid, name, action, ua, txid, height, action_index FROM name_events {WHERE}
             ORDER BY height DESC, rowid DESC LIMIT ?4 OFFSET ?5"
        ))?;
        let events = stmt
            .query_map(p, |r| row_to_event(r))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((events, total as u64))
    }

    // --- helpers that must be called inside a transaction ---

    fn name_tip_in_tx(
        &self,
        tx: &Transaction<'_>,
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
        tx: &Transaction<'_>,
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

/// After deleting post-fork events, set `names` to the highest surviving event
/// for this name (or delete the row if the tip was a release).
fn rebuild_name_tip(tx: &Transaction<'_>, name: &str) -> rusqlite::Result<()> {
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
    conn: &Transaction<'_>,
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

fn row_to_event(row: &Row<'_>) -> rusqlite::Result<Event> {
    let txid: Vec<u8> = row.get(4)?;
    Ok(Event {
        id: row.get(0)?,
        name: row.get(1)?,
        action: parse_action(&row.get::<_, String>(2)?)?,
        ua: row.get(3)?,
        txid: txid
            .try_into()
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, 0))?,
        height: row.get::<_, i64>(5)? as u32,
        action_index: row.get::<_, i64>(6)? as usize,
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