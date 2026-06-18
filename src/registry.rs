//! ZNS name index in SQLite

use zns_verify::Action;

mod core;
mod handle;
mod lifecycle;
mod storage;

// The main public (pub(crate)) surface for the rest of the crate.
pub(crate) use handle::Registry;

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
