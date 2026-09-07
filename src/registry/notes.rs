//! Name-note admission checks: the chain rule and binding verification.

use group::{Group, GroupEncoding};
use pasta_curves::arithmetic::CurveExt;
use pasta_curves::pallas;
use seer_sync::sync::decrypt::RelaxedIronwoodOutput;
use zns_verify::verify::verify_name_note_with_witness;
use zns_verify::{prev_rcm_for, ExtractedNoteCommitment as ZnsCmx, PrevRcm, Rho, Tip};

/// Checks the chain rule: the disclosed `prev_rcm` must extend the current
/// tip. Returns the expected `prev_rcm` on success.
pub(crate) fn check_chain_rule(
    prev: Option<&Tip>,
    note: &zns_verify::NameNote<'_>,
) -> Option<[u8; 32]> {
    let expected_prev = prev_rcm_for(prev, note.action())?;
    (note.prev_rcm().unwrap_or(PrevRcm::ZERO).as_bytes() == &expected_prev).then_some(expected_prev)
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

fn diversify_hash(diversifier: &[u8; 11]) -> [u8; 32] {
    let hash = pallas::Point::hash_to_curve("z.cash:Orchard-gd");
    let point = hash(diversifier);
    if bool::from(point.is_identity()) {
        hash(&[]).to_bytes()
    } else {
        point.to_bytes()
    }
}
