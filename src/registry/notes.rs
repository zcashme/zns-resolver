//! Name-note admission helpers: the link gates, binding verification,
//! and the registry-fork warning.

use group::{Group, GroupEncoding};
use pasta_curves::arithmetic::CurveExt;
use pasta_curves::pallas;
use seer_sync::sync::decrypt::RelaxedIronwoodOutput;
use zns_verify::verify::verify_name_note_with_witness;
use zns_verify::{prev_rcm_for, ExtractedNoteCommitment as ZnsCmx, PrevRcm, Rho, Tip};

/// Checks a candidate's linkage to the name's live state: the chain rule
/// (disclosed `prev_rcm` must extend the tip) and the consumption link
/// (updates/releases must consume the admitted predecessor's nullifier).
/// Returns the expected `prev_rcm` on success.
pub(crate) fn check_name_link(
    prev: Option<&(Tip, [u8; 32])>,
    note: &zns_verify::NameNote<'_>,
    consumed_nf: &orchard::note::Nullifier,
) -> Option<[u8; 32]> {
    let expected_prev = prev_rcm_for(prev.map(|p| &p.0), note.action())?;
    if note.prev_rcm().unwrap_or(PrevRcm::ZERO).as_bytes() != &expected_prev {
        return None;
    }
    if !matches!(note, zns_verify::NameNote::Claim { .. }) {
        prev?
            .1
            .eq(&consumed_nf.to_bytes())
            .then_some(expected_prev)?;
    }
    Some(expected_prev)
}

/// Verifies a candidate's binding: recomputes the ZNS commitment from the
/// parsed transition and the note's parameters, and demands equality with
/// the published `cmx`. Returns the re-derived opening `(ψ, rcm)`.
pub(crate) fn verify_commitment(
    note: &zns_verify::NameNote<'_>,
    cand: &RelaxedIronwoodOutput,
) -> Option<(pallas::Base, pallas::Scalar)> {
    let (_, cand, _, _, _) = cand;
    let rho = Rho::from_bytes(&cand.note().rho().to_bytes())?;
    let cmx = ZnsCmx::from_bytes(&cand.cmx().to_bytes())?;
    let raw = cand.note().recipient().to_raw_address_bytes();
    let diversifier: [u8; 11] = raw[..11].try_into().expect("raw address is 43 bytes");
    let pk_d: [u8; 32] = raw[11..].try_into().expect("raw address is 43 bytes");
    let g_d = diversify_hash(&diversifier);
    let value = cand.note().value().inner();
    verify_name_note_with_witness(note, g_d, pk_d, value, rho, cmx)
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

fn diversify_hash(diversifier: &[u8; 11]) -> [u8; 32] {
    let hash = pallas::Point::hash_to_curve("z.cash:Orchard-gd");
    let point = hash(diversifier);
    if bool::from(point.is_identity()) {
        hash(&[]).to_bytes()
    } else {
        point.to_bytes()
    }
}
