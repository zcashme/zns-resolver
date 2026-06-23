//! ZNS name index in SQLite.
//!

use tokio_rusqlite::rusqlite;
use zns_verify::Action;

mod core;
mod handle;
mod notes;
mod storage;

pub(crate) use handle::Registry;

// ── supporting types (part of the boundary) ───────────────────────────────────

/// Chain Cursor (height + optional hash).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChainPosition {
    pub height: u32,
    pub hash: Option<[u8; 32]>,
}

impl From<(u32, Option<[u8; 32]>)> for ChainPosition {
    fn from((height, hash): (u32, Option<[u8; 32]>)) -> Self {
        Self { height, hash }
    }
}

impl From<ChainPosition> for (u32, Option<[u8; 32]>) {
    fn from(pos: ChainPosition) -> Self {
        (pos.height, pos.hash)
    }
}

/// Information needed by the sync loop to resume scanning.
#[derive(Clone, Debug)]
pub(crate) struct ResumeInfo {
    pub start_height: u32,
    pub seam_hash: Option<[u8; 32]>,
}

/// Outcome of applying one batch of name notes.
#[derive(Clone, Debug)]
pub(crate) struct BatchOutcome {
    pub indexed: usize,
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
    // raw_tx no longer stored (per refactor decision)
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

/// Tiny wrapper around rusqlite errors.
///
/// The only purpose of this type today is to give the registry a distinct error
/// type in public APIs. It transparently surfaces the underlying rusqlite error.
/// Extra variants or impls can be added later if needed.
#[derive(thiserror::Error, Debug)]
#[error(transparent)]
pub(crate) struct RegistryError(#[from] rusqlite::Error);
