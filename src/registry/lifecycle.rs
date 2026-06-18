//! Memo parsing and admission control for name lifecycle events.
//!
//! Per the trust model: the memo is untrusted narration. The binding
//! (ψ, rcm) → cmx is what actually authorizes a name transition.
//!
//! Ingest flow inside `apply_batch`:
//!   lifecycle_claim_from_memo → candidate (untrusted)
//!   try_admit_name_note        → transition check + binding check
//! Only success from the second step results in an indexed row.

use zcash_protocol::consensus::Parameters;
use zns_verify::{chain::prev_rcm_for, parse_memo_validated, Action, ParsedMemo, Tip};

use crate::orchard::{verify_binding, DecryptedNote};

/// Candidate fields parsed from a canonical lifecycle memo — **untrusted** until binding passes.
pub(super) struct LifecycleClaim {
    pub(super) action: Action,
    pub(super) name: String,
    pub(super) ua: String,
    /// Optional memo witness; transition uses index tip `prev_rcm`, not this field.
    pub(super) memo_prev_rcm: Option<[u8; 32]>,
}

/// Extract indexing claims from memo. Does not admit a name note (see [`try_admit_name_note`]).
pub(super) fn lifecycle_claim_from_memo(memo: &[u8], network: &impl Parameters) -> Option<LifecycleClaim> {
    let Ok(ParsedMemo::Lifecycle {
        action,
        name,
        ua,
        prev_rcm,
    }) = parse_memo_validated(memo, network)
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
        memo_prev_rcm: prev_rcm,
    })
}

/// Admission gate: legal transition on our per-name chain + ZNS binding to on-chain `cmx`.
pub(super) fn try_admit_name_note(
    claim: &LifecycleClaim,
    n: &DecryptedNote,
    tip: Option<&Tip>,
) -> Option<([u8; 32], [u8; 32], [u8; 32])> {
    let prev_rcm = prev_rcm_for(tip, claim.action)?;
    let (psi, rcm) = verify_binding(
        &n.note,
        n.cmx,
        claim.action,
        &claim.name,
        &claim.ua,
        &prev_rcm,
    )?;
    Some((prev_rcm, psi, rcm))
}

/// If binding failed but memo's `prev_rcm` witness would verify, log a possible fork.
pub(super) fn warn_registry_fork(claim: &LifecycleClaim, n: &DecryptedNote, tip: Option<&Tip>) {
    let Some(prev_rcm) = prev_rcm_for(tip, claim.action) else {
        return;
    };
    let Some(claimed) = claim.memo_prev_rcm.filter(|p| {
        *p != prev_rcm
            && verify_binding(
                &n.note,
                n.cmx,
                claim.action,
                &claim.name,
                &claim.ua,
                p,
            )
            .is_some()
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
