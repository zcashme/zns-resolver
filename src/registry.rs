//! ZNS name index in SQLite (`registry_account`, `scan_state`, `name_events`, `names`).
//!
//! `registry_account` stores the UIVK + the network + scan birthday used to initialize.
//!
//! All SQLite I/O runs on one dedicated thread. [`Registry`] is a cloneable handle.
//! The transactional core lives in `core`, the actor surface in `handle`,
//! the durable schema description in `storage`, and read queries in `queries`.
//!
//! Key invariants are documented and enforced in `core.rs`:
//! - apply_batch + names updates + checkpoint advance happen in a single transaction.
//! - The binding gate in `lifecycle` is the precise point at which a candidate
//!   becomes recorded (graduation).
//! - `names` is a derived projection of the latest non-release event per name.

use zns_verify::Action;

mod core;
mod handle;
mod lifecycle;
mod queries;
mod storage;

// The main public (pub(crate)) surface for the rest of the crate.
pub(crate) use handle::Registry;

// ── types ─────────────────────────────────────────────────────────────────────
// These are kept in the module root (no dedicated types module) as they
// form the documented interface between the registry and its callers
// (sync, jsonrpc, main). This keeps the data model visible without extra files.

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
/// This is the internal result of successful admission. Primarily used
/// by the caller to count how many names were indexed from a batch.
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
