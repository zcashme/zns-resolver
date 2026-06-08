//! ZNS memo parsing + binding verification.
//!
//! The resolver's trust model: an indexer can be stale or omit, but cannot
//! forge. [`verify_binding`] re-derives `(ψ, rcm)` from the claimed
//! `(action, name, ua, prev_rcm)` and recomputes the Sinsemilla `cmx`, comparing
//! it to the note's on-chain commitment.
//!
//! `prev_rcm` is supplied by the caller from its own reconstructed per-name tip
//! (it is *not* in the memo — see `DESIGN.md §5`). So a `cmx` match
//! simultaneously proves the binding *and* that the action correctly extends the
//! name's hash chain — verification and chain-advance are one step.

use group::ff::PrimeField;
use orchard::Note;
use pasta_curves::pallas;
use zns_verify::{note_commitment_cmx, zns_psi_rcm, Action};

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

/// A ZNS lifecycle action parsed from a memo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAction {
    /// CLAIM, UPDATE, or RELEASE.
    pub action: Action,
    /// The name.
    pub name: String,
    /// The bound UA (empty for RELEASE).
    pub ua: String,
}

/// Parse a memo's printable prefix as a ZNS lifecycle action.
///
/// Grammar (`DESIGN.md §6`): `ZNS:claim:<name>:<ua>`, `ZNS:update:<name>:<ua>`,
/// `ZNS:release:<name>`. Returns `None` for non-ZNS memos, the registry-only
/// `confirm` verb, or malformed input.
pub fn parse_memo(memo: &[u8]) -> Option<ParsedAction> {
    let mut parts = printable_prefix(memo)?.splitn(4, ':');
    if parts.next()? != "ZNS" {
        return None;
    }
    let verb = parts.next()?;
    let name = parts.next()?.to_string();
    let (action, ua) = match verb {
        "claim" => (Action::Claim, parts.next()?.to_string()),
        "update" => (Action::Update, parts.next()?.to_string()),
        "release" => (Action::Release, String::new()),
        _ => return None, // "confirm" and anything else aren't index actions.
    };
    (!name.is_empty()).then_some(ParsedAction { action, name, ua })
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

fn printable_prefix(memo: &[u8]) -> Option<&str> {
    let end = memo.iter().rposition(|b| *b != 0).map(|p| p + 1).unwrap_or(0);
    std::str::from_utf8(&memo[..end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claim_and_update() {
        assert_eq!(
            parse_memo(b"ZNS:claim:alice:u1xxx"),
            Some(ParsedAction { action: Action::Claim, name: "alice".into(), ua: "u1xxx".into() }),
        );
        assert_eq!(parse_memo(b"ZNS:update:alice:u1new").unwrap().action, Action::Update);
    }

    #[test]
    fn release_has_empty_ua() {
        let p = parse_memo(b"ZNS:release:alice").unwrap();
        assert_eq!(p.action, Action::Release);
        assert_eq!(p.ua, "");
    }

    #[test]
    fn rejects_confirm_and_malformed() {
        assert!(parse_memo(b"ZNS:confirm:alice:1234").is_none());
        assert!(parse_memo(b"not a zns memo").is_none());
        assert!(parse_memo(b"ZNS:claim:alice").is_none()); // CLAIM needs a ua
        assert!(parse_memo(b"ZNS:claim::u1xxx").is_none()); // empty name
    }
}
