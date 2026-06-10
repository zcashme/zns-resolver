//! ZNS binding verification — the resolver's glue over the `zns-verify` kernel.
//!
//! The resolver's trust model: an indexer can be stale or omit, but cannot
//! forge. [`verify_binding`] re-derives `(ψ, rcm)` from the claimed
//! `(action, name, ua, prev_rcm)` and recomputes the Sinsemilla `cmx`, comparing
//! it to the note's on-chain commitment.
//!
//! Memo parsing and the per-name transition rule live in the kernel
//! (`zns_verify::memo`, `zns_verify::chain`) — they are protocol, shared
//! verbatim with the registry and the proof verifier (`DESIGN.md §17`).
//!
//! `prev_rcm` is supplied by the caller from its own reconstructed per-name tip
//! (it is *not* in the memo — see `DESIGN.md §5`). So a `cmx` match
//! simultaneously proves the binding *and* that the action correctly extends the
//! name's hash chain — verification and chain-advance are one step.

use group::ff::PrimeField;
use orchard::Note;
use pasta_curves::pallas;
use zns_verify::{note_commitment_cmx, zns_psi_rcm, Action, Tip};

/// A name's current index entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameChainEntry {
    /// The name.
    pub name: String,
    /// The UA currently bound to it (empty once released).
    pub ua: String,
    /// `rcm` of the latest Name Note — the `prev_rcm` for the next action.
    pub rcm: [u8; 32],
    /// The kind of the latest action.
    pub last_action: Action,
}

impl NameChainEntry {
    /// This entry as the kernel fold rule's [`Tip`].
    pub fn tip(&self) -> Tip {
        Tip { action: self.last_action, rcm: self.rcm }
    }
}

/// Verify that a scanned Name Note binds `(action, name, ua)` to `prev_rcm`,
/// returning its `rcm` (the next action's `prev_rcm`) on success.
///
/// Re-derives `(ψ, rcm)`, recomputes the Sinsemilla `cmx` over the note's
/// `(g_d, pk_d, value, ρ)` plus `(ψ, rcm)`, and returns `Some(rcm)` iff it
/// equals the on-chain `cmx`. `None` means the note does not bind to this claim
/// under this `prev_rcm` — a forgery, a stale tip, or not a Name Note.
pub fn verify_binding(
    note: &Note,
    on_chain_cmx: [u8; 32],
    action: Action,
    name: &str,
    ua: &str,
    prev_rcm: &[u8; 32],
) -> Option<[u8; 32]> {
    let (g_d, pk_d) = note.recipient().zns_commitment_keys();
    let rho = pallas::Base::from_repr(note.rho().to_bytes()).into_option()?;
    let expected = pallas::Base::from_repr(on_chain_cmx).into_option()?;

    let (psi, rcm) = zns_psi_rcm(action.as_bytes(), name.as_bytes(), ua.as_bytes(), prev_rcm);
    let cmx = note_commitment_cmx(g_d, pk_d, note.value().inner(), rho, psi, rcm)?;
    (cmx == expected).then(|| rcm.to_repr())
}
