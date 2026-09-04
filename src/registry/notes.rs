//! Parses and verifies `NameNote`s from decrypted memos.

use group::{Group, GroupEncoding};
use orchard::keys::FullViewingKey;
use orchard::note::ExtractedNoteCommitment;
use pasta_curves::arithmetic::CurveExt;
use pasta_curves::pallas;
use seer_sync::sync::scan::OrchardOutput;
use zns_verify::verify::verify_name_note_with_witness;
use zns_verify::{prev_rcm_for, ExtractedNoteCommitment as ZnsCmx, PrevRcm, PrimeField, Rho, Tip};

use super::NameNote;

/// Parse a candidate name memo (if valid format and does not shadow UA namespace).
/// The returned note borrows from `memo` and carries the *memo's* `prev_rcm`
/// (untrusted until the tip rule and the commitment check pass).
fn parse_memo(memo: &zns_verify::Memo) -> Option<zns_verify::NameNote<'_>> {
    let note = zns_verify::NameNote::parse(memo).ok()?;
    if shadows_ua_namespace(note.name().as_str()) {
        return None;
    }
    Some(note)
}

/// Lightweight extractor for the name only. Used by the batch applicator
/// to look up the current per-name tip before running verification.
/// Returns `None` for unparseable or invalid memos.
pub(super) fn name_from_memo(memo: &[u8]) -> Option<String> {
    let memo = zns_verify::Memo::from_bytes(memo).ok()?;
    parse_memo(&memo).map(|n| n.name().as_str().to_string())
}

pub(crate) fn try_admit_name_note(
    output: &OrchardOutput,
    txid: [u8; 32],
    height: u32,
    fvk: &FullViewingKey,
    tip: Option<&Tip>,
) -> Option<NameNote> {
    let memo = output.memo?;
    let zns_memo = zns_verify::Memo::from_bytes(&memo).ok()?;
    let note = parse_memo(&zns_memo)?;

    // The kernel hashes the *disclosed* predecessor into the commitment, so
    // the chain rule is enforced here: the disclosed `prev_rcm` (zero for a
    // claim) must exactly extend our current tip.
    let expected_prev = prev_rcm_for(tip, note.action())?;
    let disclosed = note.prev_rcm().unwrap_or(PrevRcm::ZERO);
    if disclosed.as_bytes() != &expected_prev {
        return None;
    }

    let rho = Rho::from_bytes(&output.note.rho().to_bytes())?;
    let cmx =
        ZnsCmx::from_bytes(&ExtractedNoteCommitment::from(output.note.commitment()).to_bytes())?;

    let raw = output.recipient.to_raw_address_bytes();
    let diversifier: [u8; 11] = raw[..11].try_into().expect("raw address is 43 bytes");
    let pk_d: [u8; 32] = raw[11..].try_into().expect("raw address is 43 bytes");
    let g_d = diversify_hash(&diversifier);
    let value = output.note.value().inner();

    let (psi, rcm) = verify_name_note_with_witness(&note, g_d, pk_d, value, rho, cmx)?;

    let nullifier = output.note.nullifier(fvk).to_bytes();
    let cmx_bytes = ExtractedNoteCommitment::from(output.note.commitment()).to_bytes();

    Some(NameNote {
        name: note.name().as_str().to_string(),
        ua: note.ua().as_str().to_string(),
        // A release carries no expiry; the row is deleted anyway and the
        // event logs the canonical "none" spelling.
        expires_at: note
            .expires_at()
            .map(|e| e.field_bytes().to_string())
            .unwrap_or_else(|| "none".to_string()),
        action: note.action(),
        prev_rcm: expected_prev,
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
    let Ok(zns_memo) = zns_verify::Memo::from_bytes(memo) else {
        return;
    };
    let Some(note) = parse_memo(&zns_memo) else {
        return;
    };

    let Some(expected_prev) = prev_rcm_for(tip, note.action()) else {
        return;
    };

    let claimed = note.prev_rcm().unwrap_or(PrevRcm::ZERO);
    let claimed_bytes = claimed.as_bytes();
    if claimed_bytes == &expected_prev {
        return;
    }

    let Some(rho) = Rho::from_bytes(&output.note.rho().to_bytes()) else {
        return;
    };
    let Some(cmx) =
        ZnsCmx::from_bytes(&ExtractedNoteCommitment::from(output.note.commitment()).to_bytes())
    else {
        return;
    };

    let raw = output.recipient.to_raw_address_bytes();
    let diversifier: [u8; 11] = raw[..11].try_into().expect("raw address is 43 bytes");
    let pk_d: [u8; 32] = raw[11..].try_into().expect("raw address is 43 bytes");
    let g_d = diversify_hash(&diversifier);
    let value = output.note.value().inner();

    let matches = verify_name_note_with_witness(&note, g_d, pk_d, value, rho, cmx).is_some();

    if !matches {
        return;
    }

    tracing::warn!(
        name = %note.name().as_str(),
        height,
        claimed = hex::encode(claimed_bytes),
        tip = hex::encode(expected_prev),
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
