The name-binding lives in `(ψ, rcm) → cmx`, not the memo - the memo is untrusted narration.

**Trust model:** stale or omit, never forge.

## Invariants

1. **Visibility** — Watch the registry inbox with its viewing key; trial-decrypt to find candidates, full-tx decrypt to recover memos.
2. **Transition** — Per-name chain: claim from zero, update/release extend the tip; reject illegal moves before crypto.
3. **Binding** — Recompute `cmx` from `(action, name, ua, prev_rcm)`; no match, no index entry.
4. **Derivability** — Serve tx, header, and merkle branch with answers; resolver verifies on index, proofs enable optional audit.

**Ingest (`registry::apply_batch`):** `lifecycle_claim_from_memo` → candidate claims; `try_admit_name_note` → transition + binding. Only the latter indexes a row.

## Programming principles

**Binary modules:** `sync`, `orchard`, `registry`, `jsonrpc` (no `lib.rs`). 
**Checkpoint after commit.** 
**Stale or omit, never forge.**
