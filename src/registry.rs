//! ZNS name index in SQLite (`registry_account`, `scan_state`, `name_events`, `names`).
//!
//! All SQLite I/O runs on one dedicated thread. [`Registry`] is a cloneable handle;
//! [`DbConn`] (private) owns the `Connection`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::JoinHandle;

use rusqlite::{params, Connection, OptionalExtension, Row};
use zcash_protocol::consensus::{Network, Parameters};
use zns_verify::{
    chain::prev_rcm_for, parse_memo_validated, Action, ParsedMemo, Tip,
};

use crate::orchard::{verify_binding, DecryptedNote};

const QUEUE_CAP: usize = 256;
const REORG_SHALLOW_MAX: u32 = 30;

// SQLite schema:
//   registry_account — singleton: registry inbox UIVK (set once at create, not scan state)
//   scan_state       — checkpoint after commit (how far we've scanned)
//   name_events      — append-only history of every verified lifecycle event
//   names            — materialized current tip per name (fast resolve)
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

/// One verified lifecycle row from `name_events` (event log + per-name chain).
#[derive(Debug, Clone)]
pub(crate) struct Event {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) action: Action,
    pub(crate) ua: String,
    pub(crate) txid: [u8; 32],
    pub(crate) height: u32,
    pub(crate) action_index: usize,
}

#[derive(Debug)]
pub(crate) enum RegistryError {
    Db(rusqlite::Error),
    Disconnected,
}

impl From<rusqlite::Error> for RegistryError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "{e}"),
            Self::Disconnected => write!(f, "registry db thread disconnected"),
        }
    }
}

/// Cloneable handle to the registry index — enqueues work on the dedicated DB thread.
#[derive(Clone)]
pub(crate) struct Registry {
    tx: SyncSender<Op>,
}

enum Op {
    InstallRegistryUivk {
        uivk: String,
        reply: Sender<Result<(), rusqlite::Error>>,
    },
    ApplyBatch {
        network: Network,
        scanned: Cursor,
        live: Cursor,
        decrypted: Vec<DecryptedNote>,
        reply: Sender<Result<Vec<NameNote>, rusqlite::Error>>,
    },
    Rewind {
        fork_height: u32,
        scanned_height: u32,
        reply: Sender<Result<(), rusqlite::Error>>,
    },
    Checkpoint {
        reply: Sender<Result<Option<Checkpoint>, rusqlite::Error>>,
    },
    RegistryUivk {
        reply: Sender<Result<Option<String>, rusqlite::Error>>,
    },
    ResolveByName {
        name: String,
        reply: Sender<Result<Option<Registration>, rusqlite::Error>>,
    },
    RegistrationsByUa {
        ua: String,
        limit: u32,
        offset: u32,
        reply: Sender<Result<Vec<Registration>, rusqlite::Error>>,
    },
    ListRegistrations {
        limit: u32,
        offset: u32,
        reply: Sender<Result<Vec<Registration>, rusqlite::Error>>,
    },
    NameCount {
        reply: Sender<Result<u64, rusqlite::Error>>,
    },
    Events {
        name: Option<String>,
        action: Option<Action>,
        since_height: Option<u32>,
        limit: u32,
        offset: u32,
        reply: Sender<Result<(Vec<Event>, u64), rusqlite::Error>>,
    },
    Shutdown,
}

struct DbConn {
    conn: Connection,
}

fn recv_db_reply<T>(rx: mpsc::Receiver<Result<T, rusqlite::Error>>) -> Result<T, RegistryError> {
    rx.recv()
        .map_err(|_| RegistryError::Disconnected)?
        .map_err(RegistryError::from)
}

impl Registry {
    pub(crate) fn start(path: PathBuf) -> Result<(Self, JoinHandle<()>), rusqlite::Error> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (op_tx, op_rx) = mpsc::sync_channel(QUEUE_CAP);

        let join = std::thread::spawn(move || {
            match DbConn::open(&path) {
                Ok(mut db) => {
                    let _ = ready_tx.send(Ok(()));
                    run_db_thread(&mut db, op_rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        });

        match ready_rx.recv() {
            Ok(Ok(())) => Ok((Registry { tx: op_tx }, join)),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
                Some("registry db thread exited before ready".into()),
            )),
        }
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.tx.send(Op::Shutdown);
    }

    pub(crate) fn install_registry_uivk(&self, uivk: &str) -> Result<(), RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::InstallRegistryUivk {
                uivk: uivk.to_string(),
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn apply_batch(
        &self,
        network: Network,
        scanned: Cursor,
        live: Cursor,
        decrypted: Vec<DecryptedNote>,
    ) -> Result<Vec<NameNote>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::ApplyBatch {
                network,
                scanned,
                live,
                decrypted,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn rewind(&self, fork_height: u32, scanned_height: u32) -> Result<(), RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::Rewind {
                fork_height,
                scanned_height,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn checkpoint(&self) -> Result<Option<Checkpoint>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::Checkpoint { reply })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn registry_uivk(&self) -> Result<Option<String>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::RegistryUivk { reply })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn resolve_by_name(&self, name: &str) -> Result<Option<Registration>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::ResolveByName {
                name: name.to_string(),
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn registrations_by_ua(
        &self,
        ua: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Registration>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::RegistrationsByUa {
                ua: ua.to_string(),
                limit,
                offset,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn list_registrations(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Registration>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::ListRegistrations {
                limit,
                offset,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn name_count(&self) -> Result<u64, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::NameCount { reply })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn events(
        &self,
        name: Option<&str>,
        action: Option<Action>,
        since_height: Option<u32>,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Event>, u64), RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::Events {
                name: name.map(str::to_string),
                action,
                since_height,
                limit,
                offset,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }
}

fn run_db_thread(db: &mut DbConn, rx: Receiver<Op>) {
    while let Ok(op) = rx.recv() {
        match op {
            Op::Shutdown => break,
            Op::InstallRegistryUivk { uivk, reply } => {
                let _ = reply.send(db.install_registry_uivk(&uivk));
            }
            Op::ApplyBatch {
                network,
                scanned,
                live,
                decrypted,
                reply,
            } => {
                let _ = reply.send(db.apply_batch(&network, scanned, live, &decrypted));
            }
            Op::Rewind {
                fork_height,
                scanned_height,
                reply,
            } => {
                let _ = reply.send(db.rewind(fork_height, scanned_height));
            }
            Op::Checkpoint { reply } => {
                let _ = reply.send(db.checkpoint());
            }
            Op::RegistryUivk { reply } => {
                let _ = reply.send(db.registry_uivk());
            }
            Op::ResolveByName { name, reply } => {
                let _ = reply.send(db.resolve_by_name(&name));
            }
            Op::RegistrationsByUa {
                ua,
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(db.registrations_by_ua(&ua, limit, offset));
            }
            Op::ListRegistrations {
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(db.list_registrations(limit, offset));
            }
            Op::NameCount { reply } => {
                let _ = reply.send(db.name_count());
            }
            Op::Events {
                name,
                action,
                since_height,
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(db.events(name.as_deref(), action, since_height, limit, offset));
            }
        }
    }
}

impl DbConn {
    fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self { conn })
    }

    fn install_registry_uivk(&self, uivk: &str) -> rusqlite::Result<()> {
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

    fn registry_uivk(&self) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT uivk FROM registry_account WHERE id = 0",
                [],
                |row| row.get(0),
            )
            .optional()
    }

    fn checkpoint(&self) -> rusqlite::Result<Option<Checkpoint>> {
        self.conn
            .query_row(
                "SELECT height, hash, chain_tip_height, chain_tip_hash FROM scan_state WHERE id = 0",
                [],
                row_to_checkpoint,
            )
            .optional()
            .map_err(Into::into)
    }

    fn apply_batch(
        &self,
        network: &impl Parameters,
        scanned: Cursor,
        live: Cursor,
        decrypted: &[DecryptedNote],
    ) -> rusqlite::Result<Vec<NameNote>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut indexed = Vec::new();

        for n in decrypted {
            let Some(claim) = lifecycle_claim_from_memo(n.memo.as_slice(), network) else {
                continue;
            };

            let tip = self.name_tip_in_tx(&tx, &claim.name)?;
            let Some((prev_rcm, psi, rcm)) =
                try_admit_name_note(&claim, n, tip.as_ref())
            else {
                warn_registry_fork(&claim, n, tip.as_ref());
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

    fn rewind(&self, fork_height: u32, scanned_height: u32) -> rusqlite::Result<()> {
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
            "SELECT rowid, name, action, ua, txid, height, action_index FROM name_events {WHERE}
             ORDER BY height DESC, rowid DESC LIMIT ?4 OFFSET ?5"
        ))?;
        let events = stmt
            .query_map(p, |r| row_to_event(r))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((events, total as u64))
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
    conn: &rusqlite::Transaction<'_>,
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

/// Candidate fields parsed from a canonical lifecycle memo — **untrusted** until binding passes.
struct LifecycleClaim {
    action: Action,
    name: String,
    ua: String,
    /// Optional memo witness; transition uses index tip `prev_rcm`, not this field.
    memo_prev_rcm: Option<[u8; 32]>,
}

/// Extract indexing claims from memo. Does not admit a name note (see [`try_admit_name_note`]).
fn lifecycle_claim_from_memo(memo: &[u8], network: &impl Parameters) -> Option<LifecycleClaim> {
    let Ok(ParsedMemo::Lifecycle {
        action,
        name,
        ua,
        prev_rcm,
    }) = parse_memo_validated(memo, network)
    else {
        return None;
    };
    if shadows_ua_namespace(name) {
        return None;
    }
    Some(LifecycleClaim {
        action,
        name: name.to_string(),
        ua: ua.to_string(),
        memo_prev_rcm: prev_rcm,
    })
}

/// Admission gate: legal transition on our per-name chain + ZNS binding to on-chain `cmx`.
fn try_admit_name_note(
    claim: &LifecycleClaim,
    n: &DecryptedNote,
    tip: Option<&Tip>,
) -> Option<([u8; 32], [u8; 32], [u8; 32])> {
    let prev_rcm = prev_rcm_for(tip, claim.action)?;
    let (psi, rcm) = verify_binding(
        &n.note,
        n.cmx,
        claim.action,
        &claim.name,
        &claim.ua,
        &prev_rcm,
    )?;
    Some((prev_rcm, psi, rcm))
}

/// If binding failed but memo's `prev_rcm` witness would verify, log a possible fork.
fn warn_registry_fork(claim: &LifecycleClaim, n: &DecryptedNote, tip: Option<&Tip>) {
    let Some(prev_rcm) = prev_rcm_for(tip, claim.action) else {
        return;
    };
    let Some(claimed) = claim.memo_prev_rcm.filter(|p| {
        *p != prev_rcm
            && verify_binding(
                &n.note,
                n.cmx,
                claim.action,
                &claim.name,
                &claim.ua,
                p,
            )
            .is_some()
    }) else {
        return;
    };
    tracing::warn!(
        name = %claim.name,
        height = n.height,
        claimed = hex::encode(claimed),
        tip = hex::encode(prev_rcm),
        "registry fork: note extends a different predecessor than our tip"
    );
}

/// Reject names that could be mistaken for Zcash unified addresses.
fn shadows_ua_namespace(name: &str) -> bool {
    name.starts_with("u1") || name.starts_with("utest1")
}

