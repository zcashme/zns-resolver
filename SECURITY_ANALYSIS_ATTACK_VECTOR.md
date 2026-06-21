# Security Review + Architecture Analysis: ZNS Name Note Injection Attack Vector

**Role**: Security Review Engineer + Code Architect  
**Date**: 2026-06-21 (analysis performed)  
**Scope**: Primary focus on `zns-resolver` (this workspace) + relevant cross-cuts into `zns-verify`, `seer-sync`, mint-side scanner, and the custom `orchard` crate.  
**Input**: `zns-mint/ATTACK_VECTOR_PROMPT.md` + `docs/name-note-authorization.md`

---

## 1. Attack Vector Summary (as described)

An attacker can directly craft and broadcast valid 0-value Orchard Name Notes (`ZNS:claim/update/release` with 5-field prev_rcm form) addressed to the public registry address.

Because:
- The registry address is derived from a public viewing key (UIVK/UFVK).
- `(ψ, rcm)` derivation via `zns_psi_rcm` + Sinsemilla `note_commitment_cmx` is completely public / deterministic.
- Anyone can create a V5 tx that emits an output to a known Orchard address (no spend from the registry's notes is required).
- The resolver (and mint scanner) only perform IVK trial-decryption + binding verification + `prev_rcm_for` transition rule.

Result: attacker can:
- (A) Inject fresh names (using `ZERO_PREV_RCM`).
- (B) Hijack live names by copying the current `rcm` tip.
- (C) Poison the mint daemon's `names` tip and in-flight state.
- (D) Do all of the above with no cryptographic attestation that the note was created by an authorized signer.

No code path requires:
- A spend from a registry-controlled note in the same bundle.
- Recovery of the `out_ciphertext` under the registry's OVK.
- Any other provenance (nullifier set intersection, specific funding tx, etc.).

---

## 2. Current State of zns-resolver (as of this analysis)

### 2.1 Authorization Model — None (IVK-only)

- `main.rs:39`: hardcoded `UIVK` string.
- `orchard.rs:32`: `orchard_ivk` accepts UIVK *or* UFVK but **always reduces to `PreparedIncomingViewingKey`** (IVK only).
- `observe_batch` (orchard.rs:92-122): uses only the relaxed `zns_verify::decrypt::{try_compact_orchard, try_decrypt_orchard}` — **pure IVK receive path**.
- **No OVK usage anywhere** in the resolver crate.

```rust
// orchard.rs:39
if let Ok(ufvk) = ... {
    ... fvk.to_ivk(Scope::External)  // only IVK ever used
}
```

### 2.2 Admission Gate (the critical path)

**Full call chain for an attacker-controlled note**:

1. `sync/engine.rs:186` → `observe_batch`
2. `registry/core.rs:107` → `lifecycle_claim_from_memo` (accepts any 5-field Lifecycle memo)
3. `lifecycle.rs:48` → `prev_rcm_for(tip, action)` (public rule from zns-verify)
4. `lifecycle.rs:49` → `verify_binding` (public `(ψ,rcm)` + `cmx` match)
5. `core.rs:154` → `ON CONFLICT DO UPDATE` on `names` (last-writer wins)
6. Event is always appended to `name_events`

**Specific weaknesses called out in the prompt**:

- `warn_registry_fork` (lifecycle.rs:61) **only fires when the memo's disclosed prev_rcm differs from tip-derived**. An attacker who supplies the *correct current tip rcm* triggers zero warning.
- `try_admit_name_note` deliberately ignores `claim.memo_prev_rcm` and uses the *indexer's computed* `prev_rcm_for`. Attacker just matches it.
- No cross-check that the creating action spent a registry note or that `out_ciphertext` is recoverable.
- `apply_batch` is purely append + upsert; ordering within a batch or across heights is last-writer (SQL).

### 2.3 Storage & Provenance

From `storage.rs` + `core.rs`:
- `names` and `name_events` store `psi`, `rcm`, `prev_rcm`, `cmx`, `raw_tx`, `action_index`, `txid`, height.
- This is actually *good* for verifiability (clients can re-run `zns-verify`).
- But it means an injected note is fully indistinguishable from a legitimate one in the public data model.

`registry_account` table only stores the UIVK string (misnomer; will become FVK).

### 2.4 Reorg Handling

- Shallow (<30 blocks): deletes future events, rebuilds `names` tip from surviving `name_events` (highest rowid wins).
- Deep: full table wipe.

An injected note that lands in `name_events` can survive shallow reorgs and become the "canonical" tip if the real mint note is on the losing fork side or arrives later in processing order.

### 2.5 API Surface

- `status` leaks the configured key (`uivk` field).
- `events` / `resolve` will happily return attacker-controlled bindings.
- `raw_tx` + binding material are stored and will be exposed (per pending.md).

### 2.6 Compilation / Integration Debt (found during review)

Current tree does **not** compile:

```
error[E0432]: unresolved imports `zns_verify::chain`, `zns_verify::parse_memo_validated`
```

- `lifecycle.rs` imports `chain::prev_rcm_for` + `parse_memo_validated(memo, network)`.
- Current `zns-verify` only exports `prev_rcm_for`, `Tip`, `ZERO_PREV_RCM` at root (from `memo`).
- `parse_memo` (not `_validated`) exists and accepts both request and name-note forms.

This is a real integration hazard. Any security fix must also repair the call sites.

---

## 3. Mint-Side Exposure (for completeness)

- `zns-mint/chain/src/scanner.rs` + `mint/src/scan.rs:291` (`apply_name_note`) have analogous paths:
  - `NameNoteCandidate` → relaxed IVK decrypt → `recover_memo` → `verify_name_note_decrypted` using the *memo-supplied* `prev_rcm`.
  - Then `derive_psi_rcm` + `apply_mint` (blind upsert).
- The "skip our own notes because they have prev_rcm" heuristic also happily ingests attacker notes.
- Mint's `names` tip corruption affects future policy dispatch, challenge clearing, etc.

The resolver and mint are both vulnerable today under the exact scenario in the prompt.

---

## 4. Intended Mitigation (from docs/name-note-authorization.md)

1. Dedicated ZIP-32 "name-note spending account".
2. All authoritative Name Notes are **0-value self-sends** from that account to itself.
3. Resolvers receive the **Full Viewing Key (UFVK / FVK)** of that account only.
4. A note is authorized only if **both** hold:
   - **Receive check**: decrypts under the account's IVK (addressed to the account).
   - **Send check**: the note's `out_ciphertext` recovers under the account's OVK (proves creation by someone with the FVK).
5. Combine with existing binding + `prev_rcm_for`.

This gives cryptographic provenance: only the holder of the corresponding spending key can produce notes whose outgoing ciphertexts are recoverable under that OVK.

Additional implications:
- Funding: auto-top-up from treasury into the name-note account (fees ~10k zat per action).
- Key boundaries:
  - Name-note FVK → can be shared with public resolvers.
  - OTP/challenge account FVK → must **never** be shared.
- Pure UIVK mode is no longer supported for production.

---

## 5. Primitives Available Today

- Custom `orchard` crate (with `unsafe-zns`, `add_zns_output`):
  - `add_zns_output(ovk: Option<OutgoingViewingKey>, recipient, value=0, memo, rcm, psi)` — legitimate path already supplies `ovk`.
  - `try_output_recovery_with_ovk` (via `zcash_note_encryption`).

- `seer-sync/src/decrypt.rs` already implements:
  ```rust
  try_decrypt_orchard_sent(action, ovk) -> Option<(Note, Address, MemoBytes)>
  ```
  using `try_output_recovery_with_ovk(..., &action.encrypted_note().out_ciphertext, cv_net)`.

- `zns-verify` (decrypt feature) currently provides only the IVK relaxed paths. No OVK support yet.

- Legit creation (signer):
  ```rust
  let ovk = Some(registry_fvk.to_ovk(Scope::External));
  builder.add_zns_output(ovk, recipient /* the registry addr */, NoteValue::from_raw(0), ...);
  ```

For a self-send check we will also want to verify that the recovered recipient address is controlled by the same FVK (or at minimum that the incoming recipient == outgoing recipient).

---

## 6. Architectural Observations & Recommendations

**Strengths (defense in depth already present):**
- Extremely strict memo grammar (`parse_memo`) duplicated in `zns-core` and `zns-verify`.
- Binding verification is pure public math + separate from authorization.
- `prev_rcm_for` is tiny, auditable state machine.
- Storing `raw_tx` + full binding material is the right direction for client-side verification.
- Clear separation: resolver does *binding verification only*; mint does policy + spend.

**Weaknesses / Debt:**
- Authorization currently == "decryptable + binds + follows transition rule". This is address + math only.
- No provenance on the creating action.
- Resolver still uses UIVK constant + IVK reduction.
- API drift between zns-verify and resolver (currently unbuildable).
- Hardcoded everything in `main.rs`.
- No OVK path in the resolver's `orchard.rs` or `zns_verify::decrypt`.
- Schema still calls the column `uivk`.
- (fixed) Dead marketplace/signature fields removed from API responses.
- Reorg handling can entrench attacker notes.

**Design recommendations for the fix:**
1. Change resolver config from UIVK string → UFVK/FVK string for the name-note account.
2. Extend (or add to) `zns-verify` a dual-decrypt helper that returns success only on *both* IVK receive + OVK send recovery. Or do it in resolver using seer-sync primitives + orchard types.
3. In `try_admit_name_note` / new `is_authorized_self_send`, require:
   - IVK hit.
   - OVK hit on the same action.
   - (Strong) recovered recipient address matches the account's derived address (or at least incoming recipient == outgoing recipient).
   - value == 0 (name notes are defined as 0-value).
4. Update `DecryptedNote` or add `AuthorizedNameNote` type that carries proof of the send-side recovery.
5. Update storage schema comment + `registry_account` (consider storing the FVK or a hash fingerprint for auditing).
6. Expose the binding material (per pending.md) so clients can do independent `zns-verify` even if they distrust the resolver's key config.
7. Consider also hardening the mint scanner with the same dual check (or at least a "known output from our recent spends" heuristic + the OVK check).
8. Add a clear error / metric when a decrypted note fails the self-send check (helps detect probing).

**Broader considerations:**
- Once FVK self-sends are enforced, rotating the name-note spending account requires coordinated resolver updates + a new "genesis" for the name chain (or a migration event).
- The OTP/challenge account must remain completely separate (different FVK never shared).
- Public resolvers become high-value targets only for their view of history, not for the ability to mint — the FVK alone cannot spend.
- Light clients / `zns-verify` direct users will still need the raw tx or compact proofs; the resolver change does not affect them negatively.

---

## 7. Immediate Next Steps (when task assigned)

- Decide exact API for the self-send check (inside zns-verify? thin wrapper in resolver?).
- Repair the current build break as part of the change (update imports to `zns_verify::{prev_rcm_for, ...}` and decide on memo validation strategy).
- Implement UFVK path + dual (receive + send) check.
- Add appropriate tracing for "accepted via self-send auth" vs "rejected (no ovk recovery)".
- Update `main.rs`, `orchard.rs`, `lifecycle.rs`, `core.rs` (schema + handling), `handle.rs`, `jsonrpc.rs` (status field rename?).
- Update docs, `pending.md`, `name-note-authorization.md`.
- Consider adding a small test harness (even if integration) once the primitives exist.
- Evaluate whether the mint daemon needs a parallel change for defense-in-depth.

---

## 8. Files of Highest Security Relevance (for focused changes)

**Resolver:**
- `src/main.rs`
- `src/orchard.rs`
- `src/registry/lifecycle.rs`
- `src/registry/core.rs`
- `src/registry/storage.rs`
- `src/jsonrpc.rs` (status exposure)
- `src/registry/handle.rs`

**Shared:**
- `zns-verify/src/verify.rs` (decrypt module) — may need OVK support
- `seer-sync/src/decrypt.rs` (already has `*_sent`)
- `zns-mint/...` scanner + apply paths (for consistency)

**Docs:**
- `docs/name-note-authorization.md`
- `pending.md`
- `zns-mint/ATTACK_VECTOR_PROMPT.md` (reference)

---

**Status**: Attack vector fully internalized. Code paths traced. Primitives located. Implementation gaps and debt identified. Ready to implement or review the fix when the concrete task is assigned.
