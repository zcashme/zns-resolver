//! Parses and verifies `NameNote`s from decrypted memos.

use group::{Group, GroupEncoding};
use orchard::keys::FullViewingKey;
use orchard::note::ExtractedNoteCommitment;
use pasta_curves::arithmetic::CurveExt;
use pasta_curves::pallas;
use seer_sync::sync::scan::OrchardOutput;
use zns_verify::verify::verify_name_note_with_witness;
use zns_verify::{parse_name_note, prev_rcm_for, PrimeField, Tip};

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

pub(crate) fn try_admit_name_note(
    output: &OrchardOutput,
    txid: [u8; 32],
    height: u32,
    fvk: &FullViewingKey,
    tip: Option<&Tip>,
) -> Option<NameNote> {
    let memo = output.memo?;
    let note = parse_memo(&memo)?;

    let prev_rcm = prev_rcm_for(tip, note.action)?;

    let rho = zns_verify::pallas::Base::from_repr(output.note.rho().to_bytes()).into_option()?;
    let cmx = output.note.commitment();
    let expected = zns_verify::pallas::Base::from_repr(
        ExtractedNoteCommitment::from(cmx).to_bytes(),
    )
    .into_option()?;

    let raw = output.recipient.to_raw_address_bytes();
    let diversifier: [u8; 11] = raw[..11].try_into().expect("raw address is 43 bytes");
    let pk_d: [u8; 32] = raw[11..].try_into().expect("raw address is 43 bytes");
    let g_d = diversify_hash(&diversifier);
    let value = output.note.value().inner();

    let (psi, rcm) = verify_name_note_with_witness(
        note.action.as_bytes(),
        note.name.as_bytes(),
        note.ua.as_bytes(),
        &prev_rcm,
        g_d,
        pk_d,
        value,
        rho,
        expected,
    )?;

    let nullifier = output.note.nullifier(fvk).to_bytes();
    let cmx_bytes = ExtractedNoteCommitment::from(output.note.commitment()).to_bytes();

    Some(NameNote {
        name: note.name.to_string(),
        ua: note.ua.to_string(),
        action: note.action,
        prev_rcm,
        rcm: rcm.to_repr(),
        psi: psi.to_repr(),
        cmx: cmx_bytes,
        txid,
        height,
        action_index: output.index as usize,
        nullifier,
    })
}

pub(crate) fn warn_registry_fork(
    memo: &[u8],
    output: &OrchardOutput,
    _txid: [u8; 32],
    height: u32,
    tip: Option<&Tip>,
) {
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

    let rho = match zns_verify::pallas::Base::from_repr(output.note.rho().to_bytes()).into_option() {
        Some(v) => v,
        None => return,
    };
    let expected = match zns_verify::pallas::Base::from_repr(
        ExtractedNoteCommitment::from(output.note.commitment()).to_bytes(),
    )
    .into_option()
    {
        Some(v) => v,
        None => return,
    };

    let raw = output.recipient.to_raw_address_bytes();
    let diversifier: [u8; 11] = raw[..11].try_into().expect("raw address is 43 bytes");
    let pk_d: [u8; 32] = raw[11..].try_into().expect("raw address is 43 bytes");
    let g_d = diversify_hash(&diversifier);
    let value = output.note.value().inner();

    let matches = verify_name_note_with_witness(
        note.action.as_bytes(),
        note.name.as_bytes(),
        note.ua.as_bytes(),
        &claimed,
        g_d,
        pk_d,
        value,
        rho,
        expected,
    )
    .is_some();

    if !matches {
        return;
    }

    tracing::warn!(
        name = %note.name,
        height,
        claimed = hex::encode(claimed),
        tip = hex::encode(prev_rcm),
        "registry fork: note extends a different predecessor than our tip"
    );
}

/// Reject names that could be mistaken for Zcash unified addresses.
fn shadows_ua_namespace(name: &str) -> bool {
    name.starts_with("u1") || name.starts_with("utest1")
}

fn diversify_hash(diversifier: &[u8; 11]) -> [u8; 32] {
    let hash = pallas::Point::hash_to_curve("z.cash:Orchard-gd");
    let point = hash(diversifier);
    if bool::from(point.is_identity()) {
        hash(&[]).to_bytes()
    } else {
        point.to_bytes()
    }
}
