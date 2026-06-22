//! Transactional core of the registry.

use std::collections::HashMap;

use tokio_rusqlite::rusqlite::{self as rusqlite, params, Connection, OptionalExtension, Row, Transaction};

use zns_verify::{Action, Tip};

use super::notes;
use super::{ChainPosition, Checkpoint, Event, NameNote, Registration};
use crate::sync::DecryptedNote;

/// Standalone version of the logic (will be called from the tokio-rusqlite writer closure).
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
/// write in Phase 2 are consistent because the serialized execution inside
/// the Registry impl is the sole mutator. No other code path can write to
/// `names` between Phase 1 and Phase 2.
///
/// The body will run inside a single tokio-rusqlite .call closure.
pub(super) fn apply_batch(
    conn: &Connection,
    scanned: ChainPosition,
    live: ChainPosition,
    decrypted: &[DecryptedNote],
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

        let Some(name_note) = notes::try_admit_name_note(n.memo.as_slice(), n, tip.as_ref())
        else {
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
    for nn in &admitted {
        insert_event(
            &tx,
            &nn.name,
            &nn.ua,
            &nn.prev_rcm,
            &nn.rcm,
            &nn.psi,
            &nn.cmx,
            &nn.txid,
            nn.height,
            nn.action,
            nn.action_index,
            &nn.raw_tx,
        )?;

        if nn.action == Action::Release {
            tx.execute("DELETE FROM names WHERE name = ?1", params![nn.name])?;
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
                    nn.name,
                    nn.height as i64,
                    action_str(nn.action),
                    nn.ua,
                    nn.prev_rcm.as_slice(),
                    nn.rcm.as_slice(),
                    nn.psi.as_slice(),
                    nn.cmx.as_slice(),
                    nn.txid.as_slice(),
                    nn.action_index as i64,
                    nn.raw_tx,
                ],
            )?;
        }
    }

    set_checkpoint_in_tx(
        &tx,
        Checkpoint {
            scanned_height: scanned.height,
            scanned_hash: scanned.hash,
            chain_tip_height: Some(live.height),
            chain_tip_hash: live.hash,
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

    for name in &affected {
        rebuild_name_tip(&tx, name)?;
    }

    set_checkpoint_in_tx(
        &tx,
        Checkpoint {
            scanned_height: fork_height,
            scanned_hash: None,
            chain_tip_height: None,
            chain_tip_hash: None,
        },
    )?;

    tx.commit()?;
    Ok(())
}

// ── reads (free functions; callable from any &Connection under WAL) ──────────

pub(super) fn checkpoint(conn: &Connection) -> rusqlite::Result<Option<Checkpoint>> {
    conn.query_row(
        "SELECT height, hash, chain_tip_height, chain_tip_hash FROM scan_state WHERE id = 0",
        [],
        row_to_checkpoint,
    )
    .optional()
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
) -> rusqlite::Result<Vec<Registration>> {
    let mut stmt = conn.prepare(
        "SELECT name, ua, txid, height, action FROM names
         WHERE ua = ?1 ORDER BY name LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt
        .query_map(params![ua, limit, offset], row_to_registration)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(super) fn list_registrations(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> rusqlite::Result<Vec<Registration>> {
    let mut stmt = conn.prepare(
        "SELECT name, ua, txid, height, action FROM names ORDER BY name LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt
        .query_map(params![limit, offset], row_to_registration)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
