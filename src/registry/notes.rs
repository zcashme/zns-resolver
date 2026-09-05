//! Parses and verifies `NameNote`s from decrypted memos.

use group::{Group, GroupEncoding};
use orchard::keys::FullViewingKey;
use orchard::note::NoteCommitTrapdoor;
use pasta_curves::arithmetic::CurveExt;
use pasta_curves::pallas;
use seer_sync::sync::decrypt::RelaxedIronwoodOutput;
use zns_verify::verify::verify_name_note_with_witness;
use zns_verify::{prev_rcm_for, ExtractedNoteCommitment as ZnsCmx, PrevRcm, PrimeField, Rho, Tip};

use super::NameNote;

/// Parse a candidate name memo (if valid format and does not shadow UA namespace).
/// The returned note borrows from `memo` and carries the *memo's* `prev_rcm`
/// (untrusted until the tip rule and the commitment check pass).
pub(super) fn parse_memo(memo: &zns_verify::Memo) -> Option<zns_verify::NameNote<'_>> {
    let note = zns_verify::NameNote::parse(memo).ok()?;
    if shadows_ua_namespace(note.name().as_str()) {
        return None;
    }
    Some(note)
}

/// Attempt to admit a decrypted Name Note candidate.
///
/// `prev` is the name's live state: the tip the note must extend plus the
/// predecessor's derived nullifier (for the consumption link on
/// updates/releases; claims start a fresh chain and check nothing).
pub(crate) fn try_admit_name_note(
    candidate: &RelaxedIronwoodOutput,
    note: zns_verify::NameNote<'_>,
    txid: [u8; 32],
    height: u32,
    fvk: &FullViewingKey,
    prev: Option<&(Tip, [u8; 32])>,
) -> Option<NameNote> {
    let (idx, cand, consumed_nf, _, is_sent) = candidate;

    // A name note exists only as a mint self-send; `is_sent` is
    // OVK-established by the scan.
    if !is_sent {
        return None;
    }

    // The disclosed predecessor must be exactly what the live tip demands.
    let expected_prev = prev_rcm_for(prev.map(|p| &p.0), note.action())?;
    let disclosed = note.prev_rcm().unwrap_or(PrevRcm::ZERO);
    if disclosed.as_bytes() != &expected_prev {
        return None;
    }

    // Consumption link: an update/release must consume the admitted
    // predecessor (its action reveals the stored nullifier).
    if !matches!(note, zns_verify::NameNote::Claim { .. }) {
        let (_, prev_nullifier) = prev?;
        if consumed_nf.to_bytes() != *prev_nullifier {
            return None;
        }
    }

    let rho = Rho::from_bytes(&cand.note().rho().to_bytes())?;
    let cmx = ZnsCmx::from_bytes(&cand.cmx().to_bytes())?;

    let raw = cand.note().recipient().to_raw_address_bytes();
    let diversifier: [u8; 11] = raw[..11].try_into().expect("raw address is 43 bytes");
    let pk_d: [u8; 32] = raw[11..].try_into().expect("raw address is 43 bytes");
    let g_d = diversify_hash(&diversifier);
    let value = cand.note().value().inner();

    let (psi, rcm) = verify_name_note_with_witness(&note, g_d, pk_d, value, rho, cmx)?;

    // Derived at admission, revealed at consumption.
    let nullifier = cand
        .note()
        .zns_nullifier(fvk, NoteCommitTrapdoor::from_inner(rcm), psi)?
        .to_bytes();

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
        cmx: cand.cmx().to_bytes(),
        txid,
        height,
        action_index: *idx,
        nullifier,
    })
}

/// Warn on a registry fork: a candidate whose binding verifies but whose
/// disclosed predecessor differs from our live tip. Called after a failed
/// admission; all other rejection paths stay silent.
pub(crate) fn warn_registry_fork(
    candidate: &RelaxedIronwoodOutput,
    note: zns_verify::NameNote<'_>,
    height: u32,
    tip: Option<&Tip>,
) {
    let (_, cand, _, _, _) = candidate;

    let Some(expected_prev) = prev_rcm_for(tip, note.action()) else {
        return;
    };
    let claimed = note.prev_rcm().unwrap_or(PrevRcm::ZERO);
    let claimed_bytes = claimed.as_bytes();
    if claimed_bytes == &expected_prev {
        return;
    }

    let Some(rho) = Rho::from_bytes(&cand.note().rho().to_bytes()) else {
        return;
    };
    let Some(cmx) = ZnsCmx::from_bytes(&cand.cmx().to_bytes()) else {
        return;
    };

    let raw = cand.note().recipient().to_raw_address_bytes();
    let diversifier: [u8; 11] = raw[..11].try_into().expect("raw address is 43 bytes");
    let pk_d: [u8; 32] = raw[11..].try_into().expect("raw address is 43 bytes");
    let g_d = diversify_hash(&diversifier);
    let value = cand.note().value().inner();

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
