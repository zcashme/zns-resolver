//! ZNS name index in SQLite: one writer connection for sync, and read-only
//! connections for queries.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OpenFlags};
use zcash_protocol::consensus::Network;
use zns_verify::Action;

mod anchor_lineage;
pub(crate) mod core;
mod nf;
mod notes;
pub(crate) mod storage;

// ── Db handle ───────────────────────────────────────────────────────────────

/// How many read-only connections serve queries. A query holds one for its
/// read transaction, so this bounds how many queries run at once.
const READ_CONNECTIONS: usize = 4;

/// The path SQLite treats as a private in-memory database.
const IN_MEMORY: &str = ":memory:";

/// Cheap-clone handle to the ZNS database. Clones share the writer connection
/// and the read connections.
#[derive(Clone)]
pub(crate) struct Db(Arc<Connections>);

struct Connections {
    writer: Mutex<Connection>,
    /// Read-only connections to the same file. In WAL mode they read the last
    /// committed state while the writer works. Empty for an in-memory
    /// database, where every connection opens its own empty database; reads
    /// then go through the writer.
    readers: Vec<Mutex<Connection>>,
    next_reader: AtomicUsize,
}

impl Db {
    pub(crate) fn open(ufvk: &str, birthday: u32, db_path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(db_path)?;
        storage::install_schema(&conn)?;
        let net_str = match crate::NETWORK {
            Network::MainNetwork => "main",
            Network::TestNetwork => "test",
        };
        core::install_registry_config(&conn, ufvk, net_str, birthday)?;
        let readers = if db_path == IN_MEMORY {
            Vec::new()
        } else {
            (0..READ_CONNECTIONS)
                .map(|_| open_reader(db_path).map(Mutex::new))
                .collect::<rusqlite::Result<_>>()?
        };
        Ok(Self(Arc::new(Connections {
            writer: Mutex::new(conn),
            readers,
            next_reader: AtomicUsize::new(0),
        })))
    }

    /// The writer connection, for sync.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Connection> {
        lock_recovering(&self.0.writer)
    }

    /// Runs `read` in one read transaction on a read-only connection. Every
    /// query in it sees the same committed state, and none waits for the
    /// writer.
    pub(crate) fn read<T>(
        &self,
        read: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        let connections = &self.0;
        let conn = match connections.readers.len() {
            0 => lock_recovering(&connections.writer),
            n => {
                let next = connections.next_reader.fetch_add(1, Ordering::Relaxed);
                lock_recovering(&connections.readers[next % n])
            }
        };
        let transaction = conn.unchecked_transaction()?;
        let value = read(&transaction)?;
        transaction.commit()?;
        Ok(value)
    }
}

/// Locks `conn`, recovering it when a previous holder panicked.
fn lock_recovering(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    conn.lock().unwrap_or_else(|poisoned| {
        tracing::error!("registry database lock was poisoned; recovering the connection");
        conn.clear_poison();
        poisoned.into_inner()
    })
}

/// A read-only connection to the database at `db_path`, with the same
/// connection-local settings as the writer.
fn open_reader(db_path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch(storage::CONNECTION_SQL)?;
    Ok(conn)
}

// ── types ─────────────────────────────────────────────────────────────────────

/// Current registration: a name's live tip (absent from `names` if released).
#[derive(Debug, Clone)]
pub(crate) struct Registration {
    pub(crate) name: String,
    pub(crate) ua: String,
    /// Canonical memo field: `"none"` or decimal Unix seconds.
    pub(crate) expires_at: String,
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
    /// Canonical memo field: `"none"` or decimal Unix seconds.
    pub(crate) expires_at: String,
    pub(crate) txid: [u8; 32],
    pub(crate) height: u32,
    pub(crate) action_index: usize,
}
