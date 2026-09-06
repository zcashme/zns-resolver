//! Transactional core of the registry.

use std::collections::HashMap;

use orchard::keys::FullViewingKey;
use orchard::note::NoteCommitTrapdoor;
use orchard::note::Nullifier;
use rusqlite::{self as rusqlite, params, Connection, OptionalExtension, Row, Transaction};
use seer_sync::sync::scan::WalletTx;
use seer_sync::{Cursor, Nullifiers, Resume};
use zcash_primitives::block::BlockHash;
use zcash_protocol::consensus::BlockHeight;
use zns_verify::{Action, Memo, NameNote, PrimeField, Tip};

use super::notes;
use super::{Event, Registration};

pub(crate) fn install_registry_config(
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
/// Runs in one transaction. For each decrypted candidate: the gates (self-send
/// policy, protocol parse, chain rule, consumption link — the link gates via
/// `notes::check_name_link`, with the fork warning on failure), then the binding
/// verification (`notes::verify_commitment`), then the nullifier derivation.
/// Verified values are written immediately — the event row and the per-name
/// tip row — with the raw memo stored alongside. Tips are recorded in
/// `pending_tips` so a later note for the same name in the same batch sees
/// the updated tip.
///
/// Readers see either the pre-batch or post-batch state — never partial
/// (WAL snapshot isolation + atomic commit).
///
/// SAFETY (TOCTOU on the tip): the offline tip read and the tx writes run
/// inside the same serialized call. No other DB operation can interleave.
pub(crate) fn apply_batch(
    conn: &Connection,
    scanned: Cursor,
    transactions: &[WalletTx],
    fvk: &FullViewingKey,
) -> rusqlite::Result<()> {
    let mut pending_tips: HashMap<String, (Tip, [u8; 32])> = HashMap::new();
    let db_tx = conn.unchecked_transaction()?;

    // Persist received ironwood nullifiers.
    for tx_data in transactions {
        let txid = *tx_data.txid.as_ref();
        let height = u32::from(tx_data.height);
        for output in &tx_data.ironwood_outputs {
            if !output.is_sent {
                if let Some(nf) = output.nf {
                    db_tx.execute(
                        "INSERT OR IGNORE INTO watched_ironwood_notes (nullifier, txid, height, spent_height)
                         VALUES (?1, ?2, ?3, NULL)",
                        params![nf.to_bytes().as_slice(), txid.as_slice(), height as i64],
                    )?;
                }
            }
        }
    }

    // Mark spent ironwood nullifiers.
    for tx_data in transactions {
        let height = u32::from(tx_data.height);
        for spend in &tx_data.ironwood_spends {
            let nf_bytes = spend.nf.to_bytes();
            db_tx.execute(
                "UPDATE watched_ironwood_notes SET spent_height = ?1
                 WHERE nullifier = ?2 AND spent_height IS NULL",
                params![height as i64, nf_bytes.as_slice()],
            )?;
            db_tx.execute(
                "DELETE FROM names WHERE nullifier = ?1",
                params![nf_bytes.as_slice()],
            )?;
        }
    }
    // Admit name-note candidates: gates, then write at the moment of
    // verification. Verified values flow straight to the rows — there is
    // no intermediate type.
    for tx in transactions {
        let txid = *tx.txid.as_ref();
        let height = u32::from(tx.height);

        for candidate in &tx.relaxed_ironwood_outputs {
            let (_, cand, consumed_nf, _, is_sent) = candidate;

            // Gate: a name note exists only as a mint self-send.
            if !is_sent {
                continue;
            }
            let Some(memo) = candidate.3 else {
                continue;
            };
            let Ok(zns_memo) = Memo::from_bytes(&memo) else {
                continue;
            };
            // Gate: protocol parse. The kernel's structural rules are the
            // authority — invalid statements never become candidates.
            let Some(note) = NameNote::parse(&zns_memo).ok() else {
                continue;
            };
            let name = note.name().as_str().to_string();
            let ua = note.ua().as_str().to_string();
            // A release carries no expiry; the row records the canonical
            // "none" spelling.
            let expires_at = note
                .expires_at()
                .map(|e| e.field_bytes().to_string())
                .unwrap_or_else(|| "none".to_string());

            let prev = match pending_tips.get(&name) {
                Some(p) => Some(*p),
                None => read_tip_offline(&db_tx, &name)?,
            };

            // Gates: chain rule + consumption link. A failure here is either an
            // invalid-given-tip transition, a competing transition, or a registry
            // fork — warn_registry_fork discriminates and warns only on true forks.
            let Some(expected_prev) = notes::check_name_link(prev.as_ref(), &note, consumed_nf)
            else {
                notes::warn_registry_fork(candidate, note, height, prev.as_ref().map(|p| &p.0));
                continue;
            };

            // Gate: binding verification. The kernel recomputes the commitment
            // from the transition fields and demands equality with the published cmx.
            let Some((psi, rcm)) = notes::verify_commitment(&note, candidate) else {
                continue;
            };

            // Derived at admission, revealed at consumption.
            let Some(nullifier) = cand
                .note()
                .zns_nullifier(fvk, NoteCommitTrapdoor::from_inner(rcm), psi)
                .map(|n| n.to_bytes())
            else {
                continue;
            };

            let rcm_repr = rcm.to_repr();
            let psi_repr = psi.to_repr();
            let cmx = cand.cmx().to_bytes();

            insert_event(
                &db_tx,
                &name,
                &ua,
                &expires_at,
                note.action(),
                &expected_prev,
                &rcm_repr,
                &psi_repr,
                &cmx,
                &nullifier,
                &txid,
                height,
                candidate.0,
                &memo,
            )?;

            if note.action() == Action::Release {
                db_tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
            } else {
                db_tx.execute(
                    "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, action_index, memo)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT (name) DO UPDATE SET
                       height = excluded.height, action = excluded.action, ua = excluded.ua,
                       expires_at = excluded.expires_at,
                       prev_rcm = excluded.prev_rcm, rcm = excluded.rcm, psi = excluded.psi,
                       cmx = excluded.cmx, nullifier = excluded.nullifier,
                       txid = excluded.txid, action_index = excluded.action_index,
                       memo = excluded.memo",
                    params![
                        name,
                        height as i64,
                        note.action().as_str(),
                        ua,
                        expires_at,
                        expected_prev.as_slice(),
                        rcm_repr.as_slice(),
                        psi_repr.as_slice(),
                        cmx.as_slice(),
                        nullifier.as_slice(),
                        txid.as_slice(),
                        candidate.0 as i64,
                        memo.as_slice(),
                    ],
                )?;
            }

            pending_tips.insert(
                name,
                (
                    Tip {
                        action: note.action(),
                        rcm: rcm_repr,
                    },
                    nullifier,
                ),
            );
        }
    }

    set_checkpoint_in_tx(&db_tx, &scanned)?;
    db_tx.commit()?;
    Ok(())
}

pub(crate) fn rewind(conn: &Connection, fork_height: u32) -> rusqlite::Result<()> {
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

    // The sync position falls back to the fork height; the hash is fixed by
    // the next apply (seer-sync's rewind convention).
    tx.execute(
        "UPDATE registry_account SET sync_height = ?1, sync_hash = NULL WHERE id = 0",
        params![fork_height as i64],
    )?;

    tx.commit()?;
    Ok(())
}

// ── reads (free functions; called directly on a locked connection) ──────

pub(crate) fn resume(conn: &Connection) -> rusqlite::Result<Resume> {
    let checkpoint = checkpoint(conn)?;
    let ironwood: Vec<Nullifier> = ironwood_nullifiers(conn)?
        .into_iter()
        .filter_map(|bytes| Option::from(Nullifier::from_bytes(&bytes)))
        .collect();
    let birthday = birthday(conn)?;

    Ok(Resume {
        birthday: BlockHeight::from_u32(birthday),
        checkpoint,
        nullifiers: Nullifiers {
            sapling: vec![],
            orchard: vec![],
            ironwood,
        },
    })
}

fn birthday(conn: &Connection) -> rusqlite::Result<u32> {
    let b: i64 = conn.query_row(
        "SELECT birthday FROM registry_account WHERE id = 0",
        [],
        |row| row.get(0),
    )?;
    Ok(b as u32)
}

/// The sync position, decoded from the registry's account row. `None` = no
/// checkpoint yet (or one that failed to decode — healed by rescanning from
/// the birthday; the index replays idempotently).
pub(crate) fn checkpoint(conn: &Connection) -> rusqlite::Result<Option<Cursor>> {
    let row: Option<(Option<i64>, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT sync_height, sync_hash FROM registry_account WHERE id = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((height, hash)) = row else {
        return Ok(None);
    };
    match (height, hash) {
        (Some(h), Some(bytes)) if bytes.len() == 32 => Ok(Some(Cursor {
            height: BlockHeight::from_u32(h as u32),
            hash: BlockHash(bytes.as_slice().try_into().expect("32 bytes checked above")),
        })),
        (Some(_), Some(bytes)) => {
            tracing::warn!(
                bytes = bytes.len(),
                "sync position is corrupt; rescanning from the birthday"
            );
            Ok(None)
        }
        _ => Ok(None),
    }
}

/// The watch-set: every nullifier whose consumption we must detect —
/// unspent watched ironwood notes plus every admitted name's nullifier.
pub(crate) fn ironwood_nullifiers(conn: &Connection) -> rusqlite::Result<Vec<[u8; 32]>> {
    let mut statement = conn.prepare(
        "SELECT nullifier FROM watched_ironwood_notes WHERE spent_height IS NULL
         UNION
         SELECT nullifier FROM names",
    )?;
    let rows = statement.query_map([], |row| {
        let bytes: Vec<u8> = row.get(0)?;
        bytes.try_into().map_err(|_| corrupt_record())
    })?;
    rows.collect()
}

/// The registry's UFVK. The account row is guaranteed at open; a missing row
/// is a broken database and fails loudly.
pub(crate) fn registry_ufvk(conn: &Connection) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT ufvk FROM registry_account WHERE id = 0",
        [],
        |row| row.get(0),
    )
}

pub(crate) fn name_count(conn: &Connection) -> rusqlite::Result<u64> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
    Ok(n as u64)
}

pub(crate) fn resolve_by_name(
    conn: &Connection,
    name: &str,
) -> rusqlite::Result<Option<Registration>> {
    conn.query_row(
        "SELECT name, ua, txid, height, action, memo FROM names WHERE name = ?1",
        params![name],
        registration_from_row,
    )
    .optional()
}

pub(crate) fn registrations_by_ua(
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
        "SELECT name, ua, txid, height, action, memo FROM names
         WHERE ua = ?1 ORDER BY name LIMIT ?2 OFFSET ?3",
    )?;
    let rows = stmt
        .query_map(params![ua, limit, offset], registration_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total as u64))
}

pub(crate) fn list_registrations(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> rusqlite::Result<(Vec<Registration>, u64)> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM names", [], |r| r.get(0))?;
    let mut stmt = conn.prepare(
        "SELECT name, ua, txid, height, action, memo FROM names ORDER BY name LIMIT ?1 OFFSET ?2",
    )?;
    let rows = stmt
        .query_map(params![limit, offset], registration_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((rows, total as u64))
}

pub(crate) fn events(
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
        action.map(|a| a.as_str()),
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
        "SELECT rowid, name, action, ua, txid, height, action_index, memo FROM name_events {WHERE}
         ORDER BY height DESC, rowid DESC LIMIT ?4 OFFSET ?5"
    ))?;
    let events = stmt
        .query_map(p, |row| {
            let id: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let action: String = row.get(2)?;
            let ua: String = row.get(3)?;
            let txid: Vec<u8> = row.get(4)?;
            let height: i64 = row.get(5)?;
            let action_index: i64 = row.get(6)?;
            let memo: Vec<u8> = row.get(7)?;
            let zns_memo = Memo::from_bytes(&memo).map_err(|_| corrupt_record())?;
            let note = parse_stored_record(&zns_memo, &name, &ua).ok_or(corrupt_record())?;
            let txid: [u8; 32] = txid.try_into().map_err(|_| corrupt_record())?;
            Ok(Event {
                id,
                name: note.name().as_str().to_string(),
                action: Action::from_bytes(action.as_bytes()).ok_or(corrupt_record())?,
                ua: note.ua().as_str().to_string(),
                txid,
                height: height as u32,
                action_index: action_index as usize,
                expires_at: note
                    .expires_at()
                    .map(|e| e.field_bytes().to_string())
                    .unwrap_or_else(|| "none".to_string()),
            })
        })?
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

/// Plain `SELECT` of a name's live state — the tip plus the stored nullifier
/// (for the consumption link). No transaction: used by `apply_batch` Phase 1.
/// Safe alongside the Phase 2 tx because the serialized execution inside the
/// Registry impl is the sole mutator.
fn read_tip_offline(conn: &Connection, name: &str) -> rusqlite::Result<Option<(Tip, [u8; 32])>> {
    conn.query_row(
        "SELECT action, rcm, nullifier FROM names WHERE name = ?1",
        params![name],
        |row| {
            let action =
                Action::from_bytes(row.get::<_, String>(0)?.as_bytes()).ok_or(corrupt_record())?;
            let rcm: Vec<u8> = row.get(1)?;
            let rcm: [u8; 32] = rcm.try_into().map_err(|_| corrupt_record())?;
            let nullifier: Vec<u8> = row.get(2)?;
            let nullifier: [u8; 32] = nullifier.try_into().map_err(|_| corrupt_record())?;
            Ok((Tip { action, rcm }, nullifier))
        },
    )
    .optional()
}

fn set_checkpoint_in_tx(tx: &Transaction<'_>, scanned: &Cursor) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE registry_account SET sync_height = ?1, sync_hash = ?2 WHERE id = 0",
        params![u32::from(scanned.height) as i64, scanned.hash.0.as_slice()],
    )?;
    Ok(())
}

/// After deleting post-fork events, set `names` to the highest surviving event
/// for this name (or delete the row if the tip was a release). The surviving
/// memo must still parse and agree with its columns — a corrupt record fails
/// the rewind loudly.
fn rebuild_name_tip(tx: &Transaction<'_>, name: &str) -> rusqlite::Result<()> {
    let row = tx
        .query_row(
            "SELECT action, memo, txid, height, action_index, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier
             FROM name_events WHERE name = ?1
             ORDER BY height DESC, rowid DESC LIMIT 1",
            params![name],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                    row.get::<_, Vec<u8>>(7)?,
                    row.get::<_, Vec<u8>>(8)?,
                    row.get::<_, Vec<u8>>(9)?,
                    row.get::<_, Vec<u8>>(10)?,
                    row.get::<_, Vec<u8>>(11)?,
                ))
            },
        )
        .optional()?;

    tx.execute("DELETE FROM names WHERE name = ?1", params![name])?;
    let Some((
        action_col,
        memo,
        txid_b,
        height,
        action_index,
        ua_b,
        expires_col,
        prev_b,
        rcm_b,
        psi_b,
        cmx_b,
        nullifier_b,
    )) = row
    else {
        return Ok(());
    };

    // The restored record must still parse and agree with its columns.
    let zns_memo = Memo::from_bytes(&memo).map_err(|_| corrupt_record())?;
    if parse_stored_record(&zns_memo, name, &ua_b).is_none() {
        return Err(corrupt_record());
    }
    let action = Action::from_bytes(action_col.as_bytes()).ok_or(corrupt_record())?;

    if matches!(action, Action::Claim | Action::Update) {
        tx.execute(
            "INSERT INTO names (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, action_index, memo)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                name,
                height,
                action_col,
                ua_b,
                expires_col,
                prev_b,
                rcm_b,
                psi_b,
                cmx_b,
                nullifier_b,
                txid_b,
                action_index,
                memo,
            ],
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn insert_event(
    tx: &Transaction<'_>,
    name: &str,
    ua: &str,
    expires_at: &str,
    action: Action,
    prev_rcm: &[u8; 32],
    rcm: &[u8; 32],
    psi: &[u8; 32],
    cmx: &[u8; 32],
    nullifier: &[u8; 32],
    txid: &[u8; 32],
    height: u32,
    action_index: usize,
    memo: &[u8],
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO name_events (name, height, action, ua, expires_at, prev_rcm, rcm, psi, cmx, nullifier, txid, action_index, memo)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            name,
            height as i64,
            action.as_str(),
            ua,
            expires_at,
            prev_rcm,
            rcm,
            psi,
            cmx,
            nullifier,
            txid,
            action_index as i64,
            memo,
        ],
    )?;
    Ok(())
}

/// The error for a record that fails the read-time check: its memo no longer
/// parses, or disagrees with its identity columns. Corrupt and must not be
/// served.
fn corrupt_record() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        Some("registry record is corrupt: memo and columns disagree".to_string()),
    )
}

/// Parses a stored memo and cross-checks it against the record's identity
/// columns. A record whose memo no longer parses or disagrees with its
/// columns is corrupt and must not be served.
fn parse_stored_record<'a>(
    zns_memo: &'a zns_verify::Memo,
    name: &str,
    ua: &str,
) -> Option<zns_verify::NameNote<'a>> {
    let note = NameNote::parse(zns_memo).ok()?;
    if note.name().as_str() != name || note.ua().as_str() != ua {
        return None;
    }
    Some(note)
}

/// Builds a `Registration` from a `names` record: the memo is parsed and
/// cross-checked against the identity columns before anything is served.
fn registration_from_row(r: &Row<'_>) -> rusqlite::Result<Registration> {
    let name: String = r.get(0)?;
    let ua: String = r.get(1)?;
    let txid: Vec<u8> = r.get(2)?;
    let height: i64 = r.get(3)?;
    let action: String = r.get(4)?;
    let memo: Vec<u8> = r.get(5)?;

    let zns_memo = Memo::from_bytes(&memo).map_err(|_| corrupt_record())?;
    let Some(note) = parse_stored_record(&zns_memo, &name, &ua) else {
        return Err(corrupt_record());
    };
    let txid: [u8; 32] = txid.try_into().map_err(|_| corrupt_record())?;
    Ok(Registration {
        name: note.name().as_str().to_string(),
        ua: note.ua().as_str().to_string(),
        expires_at: note
            .expires_at()
            .map(|e| e.field_bytes().to_string())
            .unwrap_or_else(|| "none".to_string()),
        txid,
        height: height as u32,
        last_action: Action::from_bytes(action.as_bytes()).ok_or(corrupt_record())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::storage::SCHEMA_SQL;

    fn database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_SQL).unwrap();
        conn
    }

    fn insert_checkpoint(conn: &Connection, height: u32, hash_byte: u8) {
        conn.execute(
            "UPDATE registry_account SET sync_height = ?1, sync_hash = ?2 WHERE id = 0",
            params![height as i64, vec![hash_byte; 32]],
        )
        .unwrap();
    }

    #[test]
    fn checkpoint_persists_the_sync_position() {
        let conn = database();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, 'ufvk', 'test', 1)",
            [],
        )
        .unwrap();
        insert_checkpoint(&conn, 42, 7);

        let scanned = checkpoint(&conn).unwrap().unwrap();
        assert_eq!(scanned.height, BlockHeight::from_u32(42));
        assert_eq!(scanned.hash.0, [7; 32]);
    }

    #[test]
    fn rewind_resets_the_sync_position_to_the_fork_height() {
        let conn = database();
        conn.execute(
            "INSERT INTO registry_account (id, ufvk, network, birthday) VALUES (0, 'ufvk', 'test', 1)",
            [],
        )
        .unwrap();
        insert_checkpoint(&conn, 42, 7);
        conn.execute(
            "INSERT INTO watched_ironwood_notes (nullifier, txid, height, spent_height)
             VALUES (?1, ?2, 42, NULL)",
            params![vec![1u8; 32], vec![2u8; 32]],
        )
        .unwrap();

        rewind(&conn, 41).unwrap();

        let position = checkpoint(&conn).unwrap();
        assert!(position.is_none()); // NULL hash: the next apply fixes it; a
                                     // restart meanwhile rescans from the birthday.
        let watched: u64 = conn
            .query_row("SELECT COUNT(*) FROM watched_ironwood_notes", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(watched, 0);
    }
}
