The name-binding lives in `(ψ, rcm) → cmx`, not the memo - the memo is untrusted narration.

**Trust model:** stale or omit, never forge.

## Invariants

1. **Visibility** — Watch the registry inbox with its viewing key; trial-decrypt to find candidates, full-tx decrypt to recover memos.
2. **Transition** — Per-name chain: claim from zero, update/release extend the tip; reject illegal moves before crypto.
3. **Binding** — Recompute `cmx` from `(action, name, ua, prev_rcm)`; no match, no index entry.
4. **Derivability** — Serve tx, header, and merkle branch with answers; resolver verifies on index, proofs enable optional audit.

## Programming principles

**Mono-file.** 
**Thin consumer.** 
**Checkpoint after commit.** 
**Stale or omit, never forge.**
