//! End-to-end binding verification.
//!
//! The resolver scans with the *relaxed* decrypt (the ZIP-212 `cmx` check is
//! dropped so Name Notes are visible at all), which means [`verify_binding`] is
//! the sole gate keeping a forged claim out of the index. This test exercises
//! that gate through a real [`orchard::Note`] — proving the `(g_d, pk_d, ρ)`
//! extraction is wired correctly — and proves a genuine note verifies while any
//! tampered claim over the same on-chain `cmx` is rejected.
//!
//! The "genuine" `cmx` is derived exactly as the registry mints it (via the
//! `zns-verify` kernel), so the accept case is faithful to a real mint; the
//! kernel's own `note_commitment_cmx` is anchored non-circularly by the pinned
//! vector in `zns-verify`.

use group::ff::PrimeField;
use orchard::keys::{FullViewingKey, Scope, SpendingKey};
use orchard::note::{RandomSeed, Rho};
use orchard::value::NoteValue;
use orchard::Note;
use pasta_curves::pallas;
use seer_sync::BlockHeight;
use zcash_protocol::memo::MemoBytes;

use zns_resolver::index::{Cursor, NameNote, SqliteIndex};
use zns_resolver::verify::verify_binding;
use zns_resolver::ZERO_PREV_RCM;
use zns_verify::{note_commitment_cmx, zns_psi_rcm, Action};

/// A value-0 Orchard note to a deterministic recipient — the shape of a ZNS
/// Name Note. Its `rseed` is a decoy (ZNS notes derive `(ψ, rcm)` from the
/// registration tuple, not the seed); only `recipient` and `ρ` feed the binding.
fn name_note() -> Note {
    let sk = SpendingKey::from_bytes([1u8; 32]).into_option().expect("valid spending key");
    let fvk = FullViewingKey::from(&sk);
    let recipient = fvk.address_at(0u32, Scope::External);

    // `[1u8; 32]` is canonical for the Pallas base field (top byte 0x01 < the
    // modulus), so both constructors succeed deterministically.
    let rho = Rho::from_bytes(&[1u8; 32]).into_option().expect("valid rho");
    let rseed = RandomSeed::from_bytes([2u8; 32], &rho).into_option().expect("valid rseed");
    Note::from_parts(recipient, NoteValue::from_raw(0), rho, rseed)
        .into_option()
        .expect("valid note")
}

/// The on-chain `cmx` a registry mint would publish for `(action, name, ua,
/// prev_rcm)` over this note — i.e. what the relaxed scanner sees on chain.
fn genuine_cmx(note: &Note, action: Action, name: &str, ua: &str, prev_rcm: &[u8; 32]) -> [u8; 32] {
    let (psi, rcm) = zns_psi_rcm(action.as_bytes(), name.as_bytes(), ua.as_bytes(), prev_rcm);
    let (g_d, pk_d) = note.recipient().zns_commitment_keys();
    let rho = pallas::Base::from_repr(note.rho().to_bytes()).expect("rho is a canonical base");
    let cmx = note_commitment_cmx(g_d, pk_d, note.value().inner(), rho, psi, rcm)
        .expect("commitment is not the identity");
    cmx.to_repr()
}

#[test]
fn genuine_name_note_verifies() {
    let note = name_note();
    let (action, name, ua, prev) = (Action::Claim, "alice", "u1xxx", ZERO_PREV_RCM);
    let cmx = genuine_cmx(&note, action, name, ua, &prev);

    // Verification re-derives `(ψ, rcm)` and recomputes `cmx` from the note's own
    // `(g_d, pk_d, ρ)`; it must match and return the note's `rcm`.
    let (_, expected_rcm) = zns_psi_rcm(action.as_bytes(), name.as_bytes(), ua.as_bytes(), &prev);
    assert_eq!(
        verify_binding(&note, cmx, action, name, ua, &prev),
        Some(expected_rcm.to_repr()),
    );
}

/// A CLAIM and its UPDATE landing in the *same* scan batch must apply in chain
/// order: `apply_notes` folds sequentially against each name's tip, so the
/// UPDATE only fits once the CLAIM is recorded. (Regression guard for the
/// observer bug where `recover_notes` regrouped a batch by txid — here the
/// UPDATE's txid sorts *before* the CLAIM's, which under txid ordering would
/// silently and permanently drop the UPDATE.)
#[test]
fn same_batch_claim_then_update_applies_in_chain_order() {
    let idx = SqliteIndex::open_in_memory().unwrap();
    let note = name_note();

    let claim_cmx = genuine_cmx(&note, Action::Claim, "alice", "u1old", &ZERO_PREV_RCM);
    let (_, claim_rcm) =
        zns_psi_rcm(Action::Claim.as_bytes(), b"alice", b"u1old", &ZERO_PREV_RCM);
    let prev = claim_rcm.to_repr();
    let update_cmx = genuine_cmx(&note, Action::Update, "alice", "u1new", &prev);

    let memo = |s: &str| MemoBytes::from_bytes(s.as_bytes()).unwrap();
    let batch = [
        NameNote {
            note,
            cmx: claim_cmx,
            memo: memo("ZNS:claim:alice:u1old"),
            txid: [0xFF; 32], // sorts after the UPDATE's txid
            height: 100,
            action_index: 0,
            tx_bytes: vec![],
        },
        NameNote {
            note,
            cmx: update_cmx,
            memo: memo("ZNS:update:alice:u1new"),
            txid: [0x01; 32],
            height: 105,
            action_index: 0,
            tx_bytes: vec![],
        },
    ];
    idx.apply_notes(Cursor { height: BlockHeight::from_u32(105), hash: None }, &batch).unwrap();

    let reg = idx.resolve_by_name("alice").unwrap().expect("claim must be indexed");
    assert_eq!(reg.ua, "u1new", "same-batch UPDATE must apply on top of its CLAIM");
    assert_eq!(reg.last_action, Action::Update);
}

#[test]
fn tampered_claims_are_rejected() {
    let note = name_note();
    let (action, name, ua, prev) = (Action::Claim, "alice", "u1xxx", ZERO_PREV_RCM);
    // The attacker keeps the genuine on-chain `cmx` but lies about the claim.
    let cmx = genuine_cmx(&note, action, name, ua, &prev);

    // Swapped UA: the resolver can't be tricked into pointing "alice" elsewhere.
    assert_eq!(verify_binding(&note, cmx, action, name, "u1evil", &prev), None);
    // Swapped name.
    assert_eq!(verify_binding(&note, cmx, action, "bob", ua, &prev), None);
    // Wrong action verb.
    assert_eq!(verify_binding(&note, cmx, Action::Update, name, ua, &prev), None);
    // Wrong `prev_rcm` — i.e. claiming to extend a different chain position.
    assert_eq!(verify_binding(&note, cmx, action, name, ua, &[9u8; 32]), None);
}
