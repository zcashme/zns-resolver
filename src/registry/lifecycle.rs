//! Memo parsing and admission control for ZNS name lifecycle events.

use zns_verify::pallas;
use zns_verify::verify::verify_name_note_with_witness;
use zns_verify::{parse_name_note, prev_rcm_for, Action, NameNote, PrimeField, Tip};

use crate::orchard::DecryptedNote;

/// Candidate fields parsed from a canonical lifecycle memo — **untrusted** until binding passes.
pub(super) struct LifecycleClaim {
    pub(super) action: Action,
    pub(super) name: String,
    pub(super) ua: String,
    /// Optional memo witness; transition uses index tip `prev_rcm`, not this field.
    pub(super) memo_prev_rcm: Option<[u8; 32]>,
}

/// Extract indexing claims from memo. Does not admit a name note (see [`try_admit_name_note`]).
pub(super) fn lifecycle_claim_from_memo(
    memo: &[u8],
) -> Option<LifecycleClaim> {
    let Ok(NameNote {
        action,
        name,
        ua,
        prev_rcm,
    }) = parse_name_note(memo)
    else {
        return None;
    };
    if shadows_ua_namespace(name) {
        return None;
    }
    Some(LifecycleClaim {
        action,
        name: name.to_string(),
        ua: ua.to_string(),
        memo_prev_rcm: Some(prev_rcm),
    })
}

/// Admission gate: legal transition on our per-name chain + ZNS binding to on-chain `cmx`.
///
/// The binding check is performed directly with the zns-verify kernel using
/// material extracted from the decrypted note.
pub(super) fn try_admit_name_note(
    claim: &LifecycleClaim,
    n: &DecryptedNote,
    tip: Option<&Tip>,
) -> Option<([u8; 32], [u8; 32], [u8; 32])> {
    let prev_rcm = prev_rcm_for(tip, claim.action)?;

    let rho = pallas::Base::from_repr(n.rho).into_option()?;
    let expected = pallas::Base::from_repr(n.cmx).into_option()?;

    let (psi, rcm) = verify_name_note_with_witness(
        claim.action.as_bytes(),
        claim.name.as_bytes(),
        claim.ua.as_bytes(),
        &prev_rcm,
        n.g_d,
        n.pk_d,
        n.value,
        rho,
        expected,
    )?;

    Some((prev_rcm, psi.to_repr(), rcm.to_repr()))
}

/// If binding failed but memo's `prev_rcm` witness would verify, log a possible fork.
pub(super) fn warn_registry_fork(claim: &LifecycleClaim, n: &DecryptedNote, tip: Option<&Tip>) {
    let Some(prev_rcm) = prev_rcm_for(tip, claim.action) else {
        return;
    };

    let Some(claimed) = claim.memo_prev_rcm.and_then(|p| {
        if p == prev_rcm {
            return None;
        }
        let rho = pallas::Base::from_repr(n.rho).into_option()?;
        let expected = pallas::Base::from_repr(n.cmx).into_option()?;

        let matches = verify_name_note_with_witness(
            claim.action.as_bytes(),
            claim.name.as_bytes(),
            claim.ua.as_bytes(),
            &p,
            n.g_d,
            n.pk_d,
            n.value,
            rho,
            expected,
        )
        .is_some();

        matches.then_some(p)
    }) else {
        return;
    };

    tracing::warn!(
        name = %claim.name,
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
