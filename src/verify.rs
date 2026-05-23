//! Layer 2: ZNS-specific binding verification and chain advancement.
//!
//! Given a decrypted memo + the Orchard action it came from,
//! [`verify_zns_action`] cryptographically confirms that the memo's
//! `(action, name, ua, prev_rcm)` tuple actually produced the on-chain
//! `cmx`. This is the "binding check" that gives the resolver its trust
//! model: an indexer can omit or be stale, but cannot forge.
//!
//! [`valid_chain_advance`] then validates protocol-level rules: CLAIM
//! requires no prior entry for the name; UPDATE/RELEASE require
//! `prev_rcm` matches the previous entry's `rcm`.

use anyhow::{anyhow, Result};
use orchard::{
    note::{ExtractedNoteCommitment, NoteCommitTrapdoor, RandomSeed, Rho},
    value::NoteValue,
    Note,
};
use pasta_curves::pallas;
use rand::{rngs::OsRng, RngCore};
use zns_verify::{zns_psi_rcm, Action, Memo};

/// A ZNS action that has been decrypted, parsed, and cryptographically
/// verified to bind to a specific on-chain `cmx`.
///
/// Construct via [`verify_zns_action`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidZnsAction {
    /// CLAIM, UPDATE, or RELEASE.
    pub action: Action,
    /// The registered name.
    pub name: String,
    /// The Unified Address this name binds to.
    pub ua: String,
    /// The `rcm` of this action's Name Note (becomes the next action's `prev_rcm`).
    pub rcm: [u8; 32],
    /// The Orchard `cmx` of this action's Name Note. Always 32 bytes.
    pub cmx: [u8; 32],
}

/// A row in the name → latest action mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameChainEntry {
    /// The name.
    pub name: String,
    /// The latest UA bound to this name.
    pub ua: String,
    /// The `rcm` of the latest action — required as `prev_rcm` for any
    /// further UPDATE/RELEASE on this name.
    pub rcm: [u8; 32],
    /// The kind of the latest action.
    pub last_action: Action,
}

/// Verify that a decrypted memo + the Orchard action it came from form a
/// valid ZNS binding.
///
/// Steps:
///  1. Parse the memo's printable prefix as a canonical ZNS memo.
///  2. Re-derive `(ψ, rcm)` from `(action, name, ua, prev_rcm)`.
///  3. Recompute `cmx` from `(recipient, value=0, ρ, ψ, rcm)` using
///     orchard's reference [`Note::commitment_with_randomness`].
///  4. Compare to the action's published `cmx`. Match → valid; mismatch → `None`.
///
/// Returns `None` if any step fails (not a ZNS memo, derivation mismatch,
/// or commitment at the identity).
pub fn verify_zns_action<A>(
    action: &orchard::Action<A>,
    recipient: &orchard::Address,
    memo: &[u8; seer_sync::decrypt::MEMO_SIZE],
) -> Option<ValidZnsAction> {
    let memo_str = printable_prefix(memo)?;
    let parsed = Memo::parse(memo_str)?;

    let (psi, rcm) = zns_psi_rcm(
        parsed.action.as_bytes(),
        parsed.name.as_bytes(),
        parsed.ua.as_bytes(),
        &parsed.prev_rcm,
    );

    let recomputed = recompute_cmx(*recipient, action.rho(), psi, rcm).ok()?;
    let on_chain: [u8; 32] = action.cmx().into();
    if recomputed != on_chain {
        return None;
    }

    let rcm_bytes: [u8; 32] = rcm_to_bytes(rcm);
    Some(ValidZnsAction {
        action: parsed.action,
        name: parsed.name,
        ua: parsed.ua,
        rcm: rcm_bytes,
        cmx: on_chain,
    })
}

/// The decision returned by [`valid_chain_advance`].
#[derive(Debug, PartialEq, Eq)]
pub enum ChainAdvanceResult {
    /// The candidate is a valid advancement; the caller should write it as
    /// the new latest entry for this name.
    Accept,
    /// The candidate is invalid; the caller should drop it. The string is
    /// a human-readable reason for logging.
    Reject(String),
}

/// Decide whether `candidate` is a valid advancement of `prev` (the name's
/// last-known entry, or `None` if the name has never been seen).
///
/// Rules:
///  - CLAIM is valid only if `prev` is `None`. A second CLAIM is rejected.
///  - UPDATE and RELEASE are valid only if `prev` is `Some` and its `rcm`
///    matches `candidate.prev_rcm` — i.e., the chain hasn't forked under
///    this name.
///  - RELEASE leaves the name in a terminal state; the caller may choose
///    to delete the entry rather than mark it released. This function
///    accepts RELEASE; the policy is up to the caller.
pub fn valid_chain_advance(
    prev: Option<&NameChainEntry>,
    candidate: &ValidZnsAction,
    candidate_prev_rcm: &[u8; 32],
) -> ChainAdvanceResult {
    use Action::*;
    match (candidate.action, prev) {
        (Claim, None) => ChainAdvanceResult::Accept,
        (Claim, Some(p)) => ChainAdvanceResult::Reject(format!(
            "duplicate CLAIM for name {:?}; already bound to {}",
            candidate.name, p.ua
        )),
        (Update | Release, None) => ChainAdvanceResult::Reject(format!(
            "{:?} for unknown name {:?}",
            candidate.action, candidate.name
        )),
        (Update | Release, Some(p)) => {
            if p.rcm == *candidate_prev_rcm {
                ChainAdvanceResult::Accept
            } else {
                ChainAdvanceResult::Reject(format!(
                    "prev_rcm mismatch for name {:?}: candidate refs {} but last is {}",
                    candidate.name,
                    hex::encode(candidate_prev_rcm),
                    hex::encode(p.rcm),
                ))
            }
        }
    }
}

// ---------- internals ----------

fn printable_prefix(memo: &[u8]) -> Option<&str> {
    let end = memo.iter().rposition(|b| *b != 0).map(|p| p + 1).unwrap_or(0);
    std::str::from_utf8(&memo[..end]).ok()
}

fn recompute_cmx(
    recipient: orchard::Address,
    rho: Rho,
    psi: pallas::Base,
    rcm: pallas::Scalar,
) -> Result<[u8; 32]> {
    let note = loop {
        let mut rseed_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut rseed_bytes);
        if let Some(rseed) = RandomSeed::from_bytes(rseed_bytes, &rho).into_option() {
            if let Some(n) =
                Note::from_parts(recipient, NoteValue::from_raw(0), rho, rseed).into_option()
            {
                break n;
            }
        }
    };
    let cmx_point = note
        .commitment_with_randomness(psi, NoteCommitTrapdoor::from_scalar(rcm))
        .into_option()
        .ok_or_else(|| anyhow!("commitment_with_randomness landed at identity"))?;
    let cmx_extracted = ExtractedNoteCommitment::from(cmx_point);
    Ok((&cmx_extracted).into())
}

fn rcm_to_bytes(rcm: pallas::Scalar) -> [u8; 32] {
    use group::ff::PrimeField;
    rcm.to_repr().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_advance_first_claim_accepted() {
        let candidate = ValidZnsAction {
            action: Action::Claim,
            name: "alice".to_string(),
            ua: "u1xxx".to_string(),
            rcm: [1u8; 32],
            cmx: [2u8; 32],
        };
        assert_eq!(
            valid_chain_advance(None, &candidate, &[0u8; 32]),
            ChainAdvanceResult::Accept,
        );
    }

    #[test]
    fn chain_advance_duplicate_claim_rejected() {
        let prev = NameChainEntry {
            name: "alice".to_string(),
            ua: "u1xxx".to_string(),
            rcm: [1u8; 32],
            last_action: Action::Claim,
        };
        let candidate = ValidZnsAction {
            action: Action::Claim,
            name: "alice".to_string(),
            ua: "u1evil".to_string(),
            rcm: [9u8; 32],
            cmx: [3u8; 32],
        };
        matches!(
            valid_chain_advance(Some(&prev), &candidate, &[0u8; 32]),
            ChainAdvanceResult::Reject(_)
        );
    }

    #[test]
    fn chain_advance_update_with_matching_prev_rcm_accepted() {
        let prev = NameChainEntry {
            name: "alice".to_string(),
            ua: "u1old".to_string(),
            rcm: [7u8; 32],
            last_action: Action::Claim,
        };
        let candidate = ValidZnsAction {
            action: Action::Update,
            name: "alice".to_string(),
            ua: "u1new".to_string(),
            rcm: [8u8; 32],
            cmx: [9u8; 32],
        };
        assert_eq!(
            valid_chain_advance(Some(&prev), &candidate, &[7u8; 32]),
            ChainAdvanceResult::Accept,
        );
    }

    #[test]
    fn chain_advance_update_with_wrong_prev_rcm_rejected() {
        let prev = NameChainEntry {
            name: "alice".to_string(),
            ua: "u1old".to_string(),
            rcm: [7u8; 32],
            last_action: Action::Claim,
        };
        let candidate = ValidZnsAction {
            action: Action::Update,
            name: "alice".to_string(),
            ua: "u1new".to_string(),
            rcm: [8u8; 32],
            cmx: [9u8; 32],
        };
        matches!(
            valid_chain_advance(Some(&prev), &candidate, &[0u8; 32]),
            ChainAdvanceResult::Reject(_)
        );
    }
}
