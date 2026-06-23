//! Parses and verifies `NameNote`s from decrypted memos.

use zns_verify::pallas;
use zns_verify::verify::verify_name_note_with_witness;
use zns_verify::{parse_name_note, prev_rcm_for, PrimeField, Tip};

use crate::sync::DecryptedNote;

use super::NameNote;

/// Parse a candidate name memo (if valid format and does not shadow UA namespace).
/// The returned note carries the *memo's* `prev_rcm` (untrusted until verified).
fn parse_memo(memo: &[u8]) -> Option<zns_verify::NameNote<'_>> {
    let Ok(note) = parse_name_note(memo) else {
        return None;
    };
    if shadows_ua_namespace(&note.name) {
        return None;
    }
    Some(note)
}

/// Lightweight extractor for the name only. Used by the batch applicator
/// to look up the current per-name tip before running verification.
/// Returns `None` for unparseable or invalid memos.
pub(super) fn name_from_memo(memo: &[u8]) -> Option<String> {
    parse_memo(memo).map(|n| n.name.to_string())
}

/// Validates a name memo and, if the binding to the on-chain `cmx` is correct
/// and it extends the current per-name tip, constructs a verified `NameNote`.
///
/// The binding check is performed directly with the zns-verify kernel using
/// material extracted from the decrypted note.
pub(super) fn try_admit_name_note(
    memo: &[u8],
    n: &DecryptedNote,
    tip: Option<&Tip>,
) -> Option<NameNote> {
    let note = parse_memo(memo)?;

    let prev_rcm = prev_rcm_for(tip, note.action)?;

    let rho = pallas::Base::from_repr(n.rho).into_option()?;
    let expected = pallas::Base::from_repr(n.cmx).into_option()?;

    let (psi, rcm) = verify_name_note_with_witness(
        note.action.as_bytes(),
        note.name.as_bytes(),
        note.ua.as_bytes(),
        &prev_rcm,
        n.g_d,
        n.pk_d,
        n.value,
        rho,
        expected,
    )?;

    Some(NameNote {
        name: note.name.to_string(),
        ua: note.ua.to_string(),
        action: note.action,
        prev_rcm,
        rcm: rcm.to_repr(),
        psi: psi.to_repr(),
        cmx: n.cmx,
        txid: n.txid,
        height: n.height,
        action_index: n.action_index,
    })
}

/// If binding failed but memo's `prev_rcm` witness would verify, log a possible fork.
pub(super) fn warn_registry_fork(memo: &[u8], n: &DecryptedNote, tip: Option<&Tip>) {
    let Some(note) = parse_memo(memo) else {
        return;
    };

    let Some(prev_rcm) = prev_rcm_for(tip, note.action) else {
        return;
    };

    let claimed = note.prev_rcm;
    if claimed == prev_rcm {
        return;
    }

    let rho = match pallas::Base::from_repr(n.rho).into_option() {
        Some(v) => v,
        None => return,
    };
    let expected = match pallas::Base::from_repr(n.cmx).into_option() {
        Some(v) => v,
        None => return,
    };

    let matches = verify_name_note_with_witness(
        note.action.as_bytes(),
        note.name.as_bytes(),
        note.ua.as_bytes(),
        &claimed,
        n.g_d,
        n.pk_d,
        n.value,
        rho,
        expected,
    )
    .is_some();

    if !matches {
        return;
    }

    tracing::warn!(
        name = %note.name,
        height = n.height,
        claimed = hex::encode(claimed),
        tip = hex::encode(prev_rcm),
        "registry fork: note extends a different predecessor than our tip"
    );
}

/// Reject names that could be mistaken for Zcash unified addresses.
fn shadows_ua_namespace(name: &str) -> bool {
    name.starts_with("u1") || name.starts_with("utest1")
}
