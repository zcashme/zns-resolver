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

/// Atomic snapshot of the three DB fields read by the `status` RPC, taken from
/// a single read transaction so they are consistent relative to each other.
pub(crate) struct StatusSnapshot {
    pub(crate) checkpoint: Option<Checkpoint>,
    pub(crate) uivk: Option<String>,
    pub(crate) name_count: u64,
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
    ShuttingDown,
    /// The background writer thread has exited (typically via panic). No further
    /// writes are possible; this is fatal for the resolver.
    WriterDead,
}

impl From<rusqlite::Error> for RegistryError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e)
    }
}

impl std::error::Error for RegistryError {}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "{e}"),
            Self::Disconnected => write!(f, "registry writer thread disconnected"),
            Self::ShuttingDown => write!(f, "registry is shutting down"),
            Self::WriterDead => write!(f, "registry writer thread has exited"),
        }
    }
}
