//! ZNS JSON-RPC interface.
//!

use serde::Serialize;
use zns_verify::Action;

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
    /// Best known chain tip height from the lightwalletd we are following.
    pub chain_tip_height: u64,
    /// Whether we are currently caught up with the chain tip.
    pub synced: bool,
    /// How many blocks behind the chain tip we are (0 when synced).
    pub blocks_behind: u64,
    /// The viewing key (as a string) used to observe name bindings.
    /// Exposed so clients can verify they are talking to the expected resolver.
    pub viewing_key: String,
    /// Total number of currently registered names.
    pub registered: u64,
}

// ── conversion helpers (private to the jsonrpc module) ───────────────────────

pub(super) fn action_name(a: Action) -> &'static str {
    match a {
        Action::Claim => "claim",
        Action::Update => "update",
        Action::Release => "release",
    }
}

pub(super) fn to_name_record(reg: Registration) -> NameRecord {
    NameRecord {
        name: reg.name,
        address: reg.ua,
        txid: hex::encode(reg.txid),
        height: reg.height as u64,
        last_action: action_name(reg.last_action).to_string(),
    }
}

pub(super) fn to_name_event(e: Event) -> NameEvent {
    NameEvent {
        id: e.id,
        name: e.name,
        action: action_name(e.action).to_string(),
        txid: hex::encode(e.txid),
        height: e.height as u64,
        action_index: e.action_index as u64,
        address: (!e.ua.is_empty()).then_some(e.ua),
    }
}
