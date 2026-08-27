//! Transactional core of the registry.

use std::collections::HashMap;

use rusqlite::{self as rusqlite, params, Connection, OptionalExtension, Row, Transaction};
use seer_sync::Cursor;

use zns_verify::{Action, Tip};

use super::notes;
use super::{Checkpoint, Event, NameNote, Registration};
use crate::sync::DecryptedNote;

/// Standalone version of the logic (will be called from inside a
/// tokio_rusqlite::Connection::call closure).
pub(super) fn install_registry_config(
    conn: &Connection,
    ufvk: &str,
    network: &str,
    birthday: u32,
) -> rusqlite::Result<()> {
    if let Some((stored_ufvk, stored_net, stored_birthday)) = registry_config(conn)? {
        if stored_ufvk != ufvk {
            tracing::warn!(
                stored = %stored_ufvk,
                "registry_account ufvk already set; not changing"
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

    conn.execute(
        "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, ?1, ?2, ?3)",
        params![ufvk, network, birthday as i64],
    )?;
    Ok(())
}

/// The main write path.
///
/// Phase 1 (no transaction): for each decrypted note, read the name's tip
/// (from the in-batch `pending_tips` map, or from the committed DB state via
/// a plain `SELECT`) and run `notes::try_admit_name_note` (which parses
/// the memo, performs the ZNS binding verification directly via zns-verify,
/// and returns a fully constructed `NameNote` on success). Admitted notes are
/// collected and their new tips recorded in `pending_tips` so a later note
/// for the same name in the same batch sees the updated tip.
///
/// Phase 2 (one transaction): write all admitted events, upsert/delete the
/// per-name tip rows, and advance `scan_state` in the same transaction.
/// Readers see either the pre-batch or post-batch state — never partial
/// (WAL snapshot isolation + atomic commit).
///
/// SAFETY (TOCTOU on the tip): the offline tip read in Phase 1 and the tx
/// write in Phase 2 run inside the same serialized tokio_rusqlite call.
/// No other DB operation can interleave.
pub(super) fn apply_batch(
    conn: &Connection,
    scanned: Cursor,
    live: Cursor,
    decrypted: &[DecryptedNote],
    received_nullifiers: &[([u8; 32], [u8; 32], u32)],
    spent_nullifiers: &[([u8; 32], u32)],
) -> rusqlite::Result<Vec<NameNote>> {
    let mut pending_tips: HashMap<String, Tip> = HashMap::new();
    let mut admitted: Vec<NameNote> = Vec::new();

    for n in decrypted {
        let Some(name) = notes::name_from_memo(n.memo.as_slice()) else {
            continue;
        };

        let tip: Option<Tip> = match pending_tips.get(&name) {
            Some(t) => Some(*t),
            None => read_tip_offline(conn, &name)?,
        };

        let Some(name_note) = notes::try_admit_name_note(n.memo.as_slice(), n, tip.as_ref()) else {
            notes::warn_registry_fork(n.memo.as_slice(), n, tip.as_ref());
            continue;
        };

        pending_tips.insert(
            name_note.name.clone(),
            Tip {
                action: name_note.action,
                rcm: name_note.rcm,
            },
        );
        admitted.push(name_note);
    }

    let tx = conn.unchecked_transaction()?;
    for (nullifier, txid, height) in received_nullifiers {
        tx.execute(
            "INSERT OR IGNORE INTO watched_ironwood_notes (nullifier, txid, height, spent_height)
             VALUES (?1, ?2, ?3, NULL)",
            params![nullifier.as_slice(), txid.as_slice(), *height as i64],
        )?;
    }
    for (nullifier, height) in spent_nullifiers {
        tx.execute(
            "UPDATE watched_ironwood_notes SET spent_height = ?1
             WHERE nullifier = ?2 AND spent_height IS NULL",
            params![*height as i64, nullifier.as_slice()],
        )?;
        // A consumed live Name Note is no longer resolvable unless this same
        // transaction supplies a verified successor below.
        tx.execute(
            "DELETE FROM names WHERE nullifier = ?1",
            params![nullifier.as_slice()],
        )?;
    }
    for nn in &admitted {
        insert_event(
            &tx,
            &nn.name,
            &nn.ua,
            &nn.prev_rcm,
            &nn.rcm,
            &nn.psi,
            &nn.cmx,
            &nn.nullifier,
            &nn.txid,
            nn.height,
            nn.action,
            nn.action_index,
        )?;

        if nn.action == Action::Release {
            tx.execute("DELETE FROM names WHERE name = ?1", params![nn.name])?;
        } else {
            tx.execute(
                "INSERT INTO names (name, height, action, ua, prev_rcm, rcm, psi, cmx, nullifier, txid, action_index)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT (name) DO UPDATE SET
                   height = excluded.height, action = excluded.action, ua = excluded.ua,
                   prev_rcm = excluded.prev_rcm, rcm = excluded.rcm, psi = excluded.psi,
                   cmx = excluded.cmx, nullifier = excluded.nullifier,
                   txid = excluded.txid, action_index = excluded.action_index",
                params![
                    nn.name,
                    nn.height as i64,
                    action_str(nn.action),
                    nn.ua,
                    nn.prev_rcm.as_slice(),
                    nn.rcm.as_slice(),
                    nn.psi.as_slice(),
                    nn.cmx.as_slice(),
                    nn.nullifier.as_slice(),
                    nn.txid.as_slice(),
                    nn.action_index as i64,
                ],
            )?;
        }
    }

    set_checkpoint_in_tx(
        &tx,
        Checkpoint {
            scanned_height: u32::from(scanned.height),
            scanned_hash: Some(scanned.hash.0),
            chain_tip_height: Some(u32::from(live.height)),
            chain_tip_hash: Some(live.hash.0),
        },
    )?;
    tx.commit()?;
    Ok(admitted)
}

/// Standalone version.
pub(super) fn rewind(conn: &Connection, fork_height: u32) -> rusqlite::Result<()> {
    let tx = conn.unchecked_transaction()?;

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
        "DELETE FROM watched_ironwood_notes WHERE height > ?1",
        params![fork_height as i64],
    )?;
    tx.execute(
        "UPDATE watched_ironwood_notes SET spent_height = NULL WHERE spent_height > ?1",
        params![fork_height as i64],
    )?;

    for name in &affected {
        rebuild_name_tip(&tx, name)?;
    }

    tx.execute("DELETE FROM scan_state WHERE id = 0", [])?;

    tx.commit()?;
    Ok(())
}

// ── reads (free functions; run inside the single serialized connection) ──────

pub(super) fn checkpoint(conn: &Connection) -> rusqlite::Result<Option<Checkpoint>> {
    conn.query_row(
        "SELECT height, hash, chain_tip_height, chain_tip_hash FROM scan_state WHERE id = 0",
        [],
        row_to_checkpoint,
    )
    .optional()
}

pub(super) fn ironwood_nullifiers(conn: &Connection) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut out = Vec::new();
    for query in [
        "SELECT nullifier FROM watched_ironwood_notes WHERE spent_height IS NULL",
        "SELECT nullifier FROM names",
    ] {
        let mut statement = conn.prepare(query)?;
        let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        for row in rows {
            let bytes: [u8; 32] = row?
                .try_into()
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
            if !out.contains(&bytes) {
                out.push(bytes);
            }
        }
    }
    Ok(out)
}

pub(super) fn registry_ufvk(conn: &Connection) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT ufvk FROM registry_account WHERE id = 0",
        [],
        |row| row.get(0),
    )
    .optional()
}

pub(super) fn name_count(conn: &Connection) -> rusqlite::Result<u64> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
    Ok(n as u64)
}

pub(super) fn resolve_by_name(
    conn: &Connection,
    name: &str,
) -> rusqlite::Result<Option<Registration>> {
    conn.query_row(
        "SELECT name, ua, txid, height, action FROM names WHERE name = ?1",
        params![name],
        row_to_registration,
    )
    .optional()
}

pub(super) fn registrations_by_ua(
    conn: &Connection,
    ua: &str,
    limit: u32,
    offset: u32,
) -> rusqlite::Result<(Vec<Registration>, u64)> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM names WHERE ua = ?1",
        params![ua],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT name, ua, txid, height, action FROM names
         WHERE ua = ?1 ORDER BY name LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt
        .query_map(params![ua, limit, offset], row_to_registration)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total as u64))
}

pub(super) fn list_registrations(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> rusqlite::Result<(Vec<Registration>, u64)> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "SELECT name, ua, txid, height, action FROM names ORDER BY name LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt
        .query_map(params![limit, offset], row_to_registration)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total as u64))
}

pub(super) fn events(
    conn: &Connection,
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

    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM name_events {WHERE}"),
        &p[..3],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT rowid, name, action, ua, txid, height, action_index FROM name_events {WHERE}
         ORDER BY height DESC, rowid DESC LIMIT ?4 OFFSET ?5"
    ))?;
    let events = stmt
        .query_map(p, row_to_event)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((events, total as u64))
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn registry_config(conn: &Connection) -> rusqlite::Result<Option<(String, String, i64)>> {
    conn.query_row(
        "SELECT ufvk, network, birthday FROM registry_account WHERE id = 0",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
}

/// Plain `SELECT` of a name's tip — no transaction. Used by `apply_batch`
/// Phase 1. Safe alongside the Phase 2 tx because the serialized execution
/// inside the Registry impl is the sole mutator.
fn read_tip_offline(conn: &Connection, name: &str) -> rusqlite::Result<Option<Tip>> {
    conn.query_row(
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
}

fn set_checkpoint_in_tx(tx: &Transaction<'_>, state: Checkpoint) -> rusqlite::Result<()> {
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
        Vec<u8>,
        i64,
        i64,
    )> = tx
        .query_row(
            "SELECT name, height, action, ua, prev_rcm, rcm, psi, cmx, nullifier, txid, action_index
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
        Some((
            name,
            height,
            action,
            ua,
            prev_rcm,
            rcm,
            psi,
            cmx,
            nullifier,
            txid,
            action_index,
        )) => {
            if action == "release" {
                tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
            } else {
                tx.execute(
                    "INSERT INTO names (name, height, action, ua, prev_rcm, rcm, psi, cmx, nullifier, txid, action_index)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT (name) DO UPDATE SET
                       height = excluded.height, action = excluded.action, ua = excluded.ua,
                       prev_rcm = excluded.prev_rcm, rcm = excluded.rcm, psi = excluded.psi,
                       cmx = excluded.cmx, nullifier = excluded.nullifier,
                       txid = excluded.txid, action_index = excluded.action_index",
                    params![
                        name,
                        height,
                        action,
                        ua,
                        prev_rcm,
                        rcm,
                        psi,
                        cmx,
                        nullifier,
                        txid,
                        action_index,
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
    nullifier: &[u8; 32],
    txid: &[u8; 32],
    height: u32,
    action: Action,
    action_index: usize,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO name_events (name, height, action, ua, prev_rcm, rcm, psi, cmx, nullifier, txid, action_index)
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
            nullifier.as_slice(),
            txid.as_slice(),
            action_index as i64,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::storage::SCHEMA_SQL;
    use zcash_primitives::block::BlockHash;
    use zcash_protocol::consensus::BlockHeight;

    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn
    }

    fn cursor(height: u32, hash_byte: u8) -> Cursor {
        Cursor {
            height: BlockHeight::from_u32(height),
            hash: BlockHash([hash_byte; 32]),
        }
    }

    #[test]
    fn apply_batch_persists_a_complete_cursor() {
        let conn = database();
        let cursor = cursor(42, 7);

        apply_batch(&conn, cursor, cursor, &[], &[], &[]).unwrap();

        let checkpoint = checkpoint(&conn).unwrap().unwrap();
        assert_eq!(checkpoint.scanned_height, 42);
        assert_eq!(checkpoint.scanned_hash, Some([7; 32]));
        assert_eq!(checkpoint.chain_tip_height, Some(42));
        assert_eq!(checkpoint.chain_tip_hash, Some([7; 32]));
    }

    #[test]
    fn rewind_removes_the_checkpoint_and_rolled_back_nullifiers() {
        let conn = database();
        let cursor = cursor(42, 7);
        let received = [([1; 32], [2; 32], 42)];
        apply_batch(&conn, cursor, cursor, &[], &received, &[]).unwrap();

        rewind(&conn, 41).unwrap();

        assert!(checkpoint(&conn).unwrap().is_none());
        let watched: u64 = conn
            .query_row("SELECT COUNT(*) FROM watched_ironwood_notes", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(watched, 0);
    }
}
