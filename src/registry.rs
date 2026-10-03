//! ZNS name index in SQLite (single-connection architecture).

use std::sync::{Arc, Mutex};

use orchard::keys::FullViewingKey;
use rusqlite::Connection;
use zcash_protocol::consensus::Network;
use zns_verify::Action;

mod anchor_lineage;
pub(crate) mod batch;
pub(crate) mod core;
mod nf;
mod notes;
pub(crate) mod storage;

/// The local registry replica and the key used to verify its name notes.
pub(crate) struct Registry {
    pub(crate) db: Db,
    pub(crate) fvk: FullViewingKey,
}

// ── Db handle ───────────────────────────────────────────────────────────────

/// Cheap-clone handle to the ZNS database.
/// Cloning shares the same locked connection.
#[derive(Clone)]
pub(crate) struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub(crate) fn open(ufvk: &str, birthday: u32, db_path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(db_path)?;
        storage::install_schema(&conn)?;
        let net_str = if crate::NETWORK == Network::MainNetwork {
            "main"
        } else {
            "test"
        };
        core::install_registry_config(&conn, ufvk, net_str, birthday)?;
        core::drop_payment_notes(&conn)?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    /// Recovers the connection when a previous holder panicked.
    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|poisoned| {
            tracing::error!("registry database lock was poisoned; recovering the connection");
            self.0.clear_poison();
            poisoned.into_inner()
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_recovers_after_the_mutex_is_poisoned() {
        let db = Db::open("ufvk", 1, ":memory:").unwrap();
        let other = db.clone();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = other.lock();
            panic!("poison the registry lock");
        }));
        assert!(panicked.is_err());

        {
            let conn = db.lock();
            let names: i64 = conn
                .query_row("SELECT COUNT(*) FROM names", [], |row| row.get(0))
                .unwrap();
            assert_eq!(names, 0);
        }
        assert!(!db.0.is_poisoned());
        let _again = db.lock();
    }
}
