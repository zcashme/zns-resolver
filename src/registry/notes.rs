//! Name-note admission checks: the chain rule and binding verification.

use orchard::note::Note;
use zns_verify::{name_note_rseed, prev_for, Memo, NameNote, PrevCmx, Tip};

/// Checks the chain rule: the disclosed predecessor must be the current
/// tip's cmx. Returns that predecessor on success.
pub(crate) fn check_chain_rule(prev: Option<&Tip>, note: &NameNote<'_>) -> Option<[u8; 32]> {
    let expected_prev = prev_for(prev, note.action())?;
    (note.prev().unwrap_or(PrevCmx::ZERO).as_bytes() == &expected_prev).then_some(expected_prev)
}

/// Whether the rseed derived from the memo is the note's rseed, the mint's
/// `decrypt_name_notes` check. Decryption already tied the action's cmx to
/// that rseed.
pub(crate) fn binds(memo: &Memo, note: &NameNote<'_>, cand_note: &Note) -> bool {
    let Some(fields) = memo.fields() else {
        return false;
    };
    let prev = note.prev().unwrap_or(PrevCmx::ZERO);
    cand_note.rseed().as_bytes() == &name_note_rseed(fields, prev.as_bytes())
}
