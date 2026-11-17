//! ZNS name index in SQLite: one connection, written by sync, and an immutable
//! snapshot of it that queries read.

use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use rusqlite::Connection;
use zcash_protocol::consensus::Network;
use zns_verify::Action;

use self::snapshot::Snapshot;

mod anchor_lineage;
pub(crate) mod core;
mod nf;
mod notes;
pub(crate) mod snapshot;
pub(crate) mod storage;

// ── Db handle ───────────────────────────────────────────────────────────────

/// Cheap-clone handle to the ZNS database. Clones share the connection and
/// the published snapshot.
#[derive(Clone)]
pub(crate) struct Db {
    conn: Arc<Mutex<Connection>>,
    snapshot: Arc<ArcSwap<Snapshot>>,
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
        let snapshot = Snapshot::read(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            snapshot: Arc::new(ArcSwap::from_pointee(snapshot)),
        })
    }

    /// The connection, for sync. Recovers it when a previous holder panicked.
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| {
            tracing::error!("registry database lock was poisoned; recovering the connection");
            self.conn.clear_poison();
            poisoned.into_inner()
        })
    }

    /// Reads the committed state from `conn` and publishes it for queries.
    /// Sync calls this after each commit, while it still holds the connection.
    /// If the read fails, the previous snapshot stays published and the next
    /// commit publishes again.
    pub(crate) fn publish(&self, conn: &Connection) {
        match Snapshot::read(conn) {
            Ok(snapshot) => self.snapshot.store(Arc::new(snapshot)),
            Err(error) => tracing::error!(
                %error,
                "registry snapshot could not be read; queries keep the previous one"
            ),
        }
    }

    /// The last published snapshot.
    pub(crate) fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.load_full()
    }
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
