# ZNS Resolver — Pending Work

This resolver focuses on **Orchard binding verification**: it watches the registry UIVK, verifies deterministic `(ψ, rcm)` bindings against on-chain `cmx`, and maintains a name index.

It deliberately does **not** implement the full signed-action ZNS model (no admin Ed25519 key, no nonces/signatures on actions, no marketplace).

Hard-coded deployment configuration is currently acceptable.

## Currently Implemented (core binding verification)

- Scans Orchard notes via registry UIVK using lightwalletd + seer-sync
- Parses `ZNS:claim` / `update` / `release` memos only
- Verifies deterministic `(ψ, rcm)` bindings against on-chain `cmx`
- Stores binding material (`psi`, `rcm`, `prev_rcm`, `cmx`) + `raw_tx` + `action_index` for every event
- Maintains current name tips + full event history in SQLite
- Basic JSON-RPC: `resolve`, `status` (with `uivk` + sync details), `events` (and a stub `listings`)
- Reorg handling (shallow + full reset)

## High-Leverage Gaps (builds on existing verification)

- [ ] **Expose binding material in the public API**
  - `psi`, `rcm`, `prev_rcm`, `cmx`, `raw_tx`, and `action_index` are already computed and stored.
  - Clients should be able to get the current tip's binding values directly so they can perform their own verification with `zns-verify`.
  - Add fields to the `resolve` and `events` responses.

## API Surface Cleanup

- [ ] **`listings` endpoint and `listed` count are dead weight**
  - Always empty / 0. Marketplace support is not planned for this resolver.

- [ ] **Dead fields in responses**
  - `RegistrationEntry` and `EventEntry` contain `nonce`, `signature`, `pubkey`, `listing`, and `price`.
  - All are always zero/None because this resolver does not track signed actions.
  - Either remove the fields or clearly mark them as unused.

## Verifiability Features

These are large and mostly out of scope for this focused binding verifier:

- [ ] **Merkle proofs / state roots**
  - No Merkle tree or state roots. See sibling `zns-zecnames` for a reference implementation.

- [ ] **Full proof bundle support**
  - Only `raw_tx` is stored. No block headers, tx Merkle branches, or `ProofLink` structures.

- [ ] **Lightweight tip verification**
  - Once binding material is exposed (see above), clients can already get what they need for direct `zns-verify` calls on the tip. Historical access may still be useful.

## Configuration & Operations

- [ ] Better structured logging and metrics
  - Current logging is minimal (`tracing_subscriber::fmt()` at INFO).

- [ ] Mainnet support
  - Currently hardcoded to testnet values. May be acceptable as hard-coded per-deployment.

## Intentionally Not Pursued (different scope)

This resolver focuses on binding verification only. The following are not planned:

- Marketplace actions (LIST, DELIST, BUY, SETPRICE)
- Nonces, signatures, or pubkeys on actions
- Admin Ed25519 key / admin-signed vs sovereign distinction
- Pricing tiers
- `address` (registry payment address) in status

## Nice-to-Have / Longer Term

- [ ] Expose binding material for historical events (not just current tips)
- [ ] Optional `with_proof` / raw binding data on `resolve`
- [ ] Pagination + filtering improvements
- [ ] Prometheus / health endpoint
- [ ] Better deep reorg handling (current full reset on >30 blocks is blunt)

## References

- `zns-verify` — the binding verification kernel (especially `zns_psi_rcm`, `verify_binding`, memo parsing)
- `zns-zecnames` — reference for Merkle proofs / state roots (intentionally not implemented here)
- Sibling crates for lightwalletd access (`seer-sync`)

---

Update this file as the scope and priorities evolve. This resolver is intentionally narrower than the full public ZNS indexer.