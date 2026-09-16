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
    /// The address carried by this action's Name Note — the binding it establishes or ends.
    pub address: String,
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

// ── conversions: registry rows → API DTOs (total and context-free) ──────────

/// Renders a registry tip as the public record served by `resolve` and
/// `list_names`.
impl From<Registration> for NameRecord {
    fn from(reg: Registration) -> Self {
        NameRecord {
            name: reg.name,
            address: reg.ua,
            txid: TxId::from_bytes(reg.txid).as_hex(),
            height: u64::from(reg.height),
            last_action: reg.last_action.as_str().to_string(),
            expires_at: reg.expires_at,
        }
    }
}

/// Renders one registry log row as the public entry served by `events`.
impl From<Event> for NameEvent {
    fn from(e: Event) -> Self {
        NameEvent {
            id: e.id,
            name: e.name,
            action: e.action.as_str().to_string(),
            txid: TxId::from_bytes(e.txid).as_hex(),
            height: u64::from(e.height),
            action_index: e.action_index as u64,
            address: e.ua,
            expires_at: e.expires_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zns_verify::Action;

    /// Asymmetric first/last bytes so the expected string catches byte-order
    /// mistakes: `TxId::as_hex` renders reversed hex (block-explorer
    /// convention).
    fn sample_txid() -> [u8; 32] {
        let mut txid = [0u8; 32];
        txid[0] = 0x01;
        txid[31] = 0x2a;
        txid
    }

    #[test]
    fn registration_becomes_the_public_record() {
        let reg = Registration {
            name: "alice".to_string(),
            ua: "utest1abc".to_string(),
            expires_at: "1735689600".to_string(),
            txid: sample_txid(),
            height: 3_000_000,
            last_action: Action::Update,
        };

        let rec: NameRecord = reg.into();
        assert_eq!(rec.name, "alice");
        assert_eq!(rec.address, "utest1abc");
        assert_eq!(rec.txid, format!("2a{}01", "00".repeat(30)));
        assert_eq!(rec.height, 3_000_000);
        assert_eq!(rec.last_action, "update");
        assert_eq!(rec.expires_at, "1735689600");
    }

    #[test]
    fn event_becomes_the_public_log_entry() {
        let e = Event {
            id: 7,
            name: "bob".to_string(),
            action: Action::Claim,
            ua: "utest1def".to_string(),
            expires_at: "none".to_string(),
            txid: sample_txid(),
            height: u32::MAX,
            action_index: 3,
        };

        let ev: NameEvent = e.into();
        assert_eq!(ev.id, 7);
        assert_eq!(ev.name, "bob");
        assert_eq!(ev.action, "claim");
        assert_eq!(ev.address, "utest1def");
        assert_eq!(ev.expires_at, "none");
        assert_eq!(ev.txid, format!("2a{}01", "00".repeat(30)));
        assert_eq!(ev.height, u64::from(u32::MAX));
        assert_eq!(ev.action_index, 3);
    }
}
