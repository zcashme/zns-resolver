//! Name-note admission checks: the chain rule and binding verification.

use orchard::note::Note;
use zns_verify::{name_note_rseed, prev_rcm_for, Expiry, PrevRcm, Tip};

/// Checks the chain rule: the disclosed predecessor must be the current
/// tip's cmx. Returns that predecessor on success.
pub(crate) fn check_chain_rule(
    prev: Option<&Tip>,
    note: &zns_verify::NameNote<'_>,
) -> Option<[u8; 32]> {
    let expected_prev = prev_rcm_for(prev, note.action())?;
    (note.prev_rcm().unwrap_or(PrevRcm::ZERO).as_bytes() == &expected_prev).then_some(expected_prev)
}

/// Whether the rseed derived from the parsed record is the note's rseed, the
/// mint's `decrypt_name_notes` check. Decryption already tied the action's
/// cmx to that rseed.
pub(crate) fn binds(note: &zns_verify::NameNote<'_>, cand_note: &Note) -> bool {
    let expires_at = note.expires_at().unwrap_or(Expiry::NEVER);
    let prev = note.prev_rcm().unwrap_or(PrevRcm::ZERO);
    let terms = note.terms();
    let rseed = name_note_rseed(
        note.action().as_bytes(),
        note.name().as_str().as_bytes(),
        note.ua().as_str().as_bytes(),
        expires_at.field_bytes().as_bytes(),
        &terms[..note.term_count()],
        prev.as_bytes(),
    );
    cand_note.rseed().as_bytes() == &rseed
}
