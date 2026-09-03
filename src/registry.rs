//! ZNS name index in SQLite (single-connection architecture).

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use zcash_protocol::consensus::Network;
use zns_verify::Action;

pub(crate) mod core;
mod notes;
pub(crate) mod storage;

// ── Db handle ───────────────────────────────────────────────────────────────

/// Cheap-clone handle to the ZNS database.
/// Cloning shares the same locked connection.
#[derive(Clone)]
pub(crate) struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub(crate) fn open(
        network: Network,
        ufvk: &str,
        birthday: u32,
        db_path: &str,
    ) -> rusqlite::Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(storage::SCHEMA_SQL)?;
        let net_str = if network == Network::MainNetwork {
            "main"
        } else {
            "test"
        };
        core::install_registry_config(&conn, ufvk, net_str, birthday)?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap()
    }
}

// ── types ─────────────────────────────────────────────────────────────────────

/// Persisted `scan_state` row.
pub(crate) struct Checkpoint {
    pub(crate) scanned_height: u32,
    pub(crate) scanned_hash: Option<[u8; 32]>,
    pub(crate) chain_tip_height: Option<u32>,
    pub(crate) chain_tip_hash: Option<[u8; 32]>,
}

/// A verified ZNS name note:
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
    pub(crate) nullifier: [u8; 32],
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
