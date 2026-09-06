//! ZNS JSON-RPC interface.
//!

use serde::Serialize;
use zcash_protocol::TxId;

use crate::registry::{Event, Registration};

/// A currently active name registration (the "tip" for a name).
#[derive(Debug, Clone, Serialize)]
pub struct NameRecord {
    /// The registered human-readable name.
    pub name: String,
    /// The shielded (unified) address the name currently resolves to.
    pub address: String,
    /// The transaction that produced the current binding.
    pub txid: String,
    /// Block height of the transaction that produced this binding.
    pub height: u64,
    /// The last lifecycle action that produced this state ("claim", "update", or "release").
    pub last_action: String,
    /// Canonical Name Note `expires_at`: `"none"` or a decimal Unix timestamp.
    pub expires_at: String,
}

/// One entry in the immutable event log for names.
#[derive(Debug, Clone, Serialize)]
pub struct NameEvent {
    /// Monotonic identifier for this event (stable for this name's history).
    pub id: i64,
    pub name: String,
    pub action: String,
    pub txid: String,
    pub height: u64,
    /// Index of this action within the block (for ordering when multiple
    /// actions for the same name occur in one block).
    pub action_index: u64,
    /// The address bound by this action (present on claim/update, absent on release).
    pub address: Option<String>,
    /// Canonical Name Note `expires_at`: `"none"` or a decimal Unix timestamp.
    pub expires_at: String,
}

/// Paginated result envelope used by list-style methods.
#[derive(Debug, Clone, Serialize)]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub total: u64,
    /// The limit that was applied (after server caps).
    pub limit: u64,
    /// The offset that was applied.
    pub offset: u64,
}

/// Current operational status of the resolver.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// Height up to which we have verified and indexed name bindings.
    pub synced_height: u64,
    /// Whether indexing has reached the chain head (as of the last tip poll).
    pub synced: bool,
    /// The viewing key (as a string) used to observe name bindings.
    /// Exposed so clients can verify they are talking to the expected resolver.
    pub viewing_key: String,
    /// Total number of currently registered names.
    pub registered: u64,
}

// ── conversion helpers (private to the jsonrpc module) ───────────────────────

pub(super) fn to_name_record(reg: Registration) -> NameRecord {
    NameRecord {
        name: reg.name,
        address: reg.ua,
        txid: TxId::from_bytes(reg.txid).to_string(),
        height: reg.height as u64,
        last_action: reg.last_action.as_str().to_string(),
        expires_at: reg.expires_at,
    }
}

pub(super) fn to_name_event(e: Event) -> NameEvent {
    NameEvent {
        id: e.id,
        name: e.name,
        action: e.action.as_str().to_string(),
        txid: TxId::from_bytes(e.txid).to_string(),
        height: e.height as u64,
        action_index: e.action_index as u64,
        address: (!e.ua.is_empty()).then_some(e.ua),
        expires_at: e.expires_at,
    }
}
