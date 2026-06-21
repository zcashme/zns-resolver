# ZNS Resolver — Pending Work

This document tracks what is still missing to make `zns-resolver` a complete, production-grade ZNS indexer/resolver that matches the expectations of the TypeScript SDK, the web app, and the full protocol.

## Currently Implemented (core binding verification)

- Scans Orchard notes via registry UIVK using lightwalletd + seer-sync
- Parses `ZNS:claim` / `update` / `release` memos
- Verifies deterministic `(ψ, rcm)` bindings against on-chain `cmx`
- Maintains current name tips + full event history in SQLite
- Basic JSON-RPC: `resolve`, `status`, `events`
- Reorg handling (shallow + full reset)

## High-Priority Gaps (API Compatibility)

- [ ] **Expose binding material in the public API**
  - Currently `psi`, `rcm`, `prev_rcm`, and `cmx` are computed and stored but never returned.
  - Clients (wallets, light clients) want these values for fast, direct verification of the current tip without fetching full proof bundles.
  - Consider adding fields to `Registration` / `Event` responses (and possibly a `raw_binding` or similar object).

- [ ] **`listings` is a stub**
  - Always returns empty. Marketplace listings are not indexed or served.

- [ ] **Status response is incomplete**
  - `listed` is always `0`
  - Missing entirely: `pricing`, `address` (registry payment address)

- [ ] **Registration objects are incomplete**
  - `nonce`, `signature`, `pubkey`, and `listing` are always zero/None.
  - This breaks SDK expectations for `Registration`, sovereign names, and listing state.

## Verifiability Features (the big missing pieces)

- [ ] **Merkle proofs from a trusted state root**
  - No Merkle tree is maintained over the set of current registrations.
  - No state root is computed or stored at heights.
  - The SDK's `resolveNameWithProof()` + `verifyProof()` cannot be supported.
  - See the implementation in `../zns-zecnames/src/merkle.rs` + `rpc.rs` (sorted leaves, `hash_leaf`, `merkle_root`, `merkle_path`, `state_root` table) for the expected shape.

- [ ] **Proof bundle support (raw artifacts)**
  - The resolver already stores `raw_tx`.
  - It does not currently serve the full `ProofLink` structures expected by `zns-verify::proof` (raw tx + block header + tx Merkle branch + action_index + claims).
  - Reference: `zns-verify/src/proof.rs` and the contract described in `PROOFS.md` (when it exists).

- [ ] **Lightweight tip verification path**
  - Many clients will want just the latest `(psi, rcm, cmx, height, action_index)` for a name so they can call `zns_verify` functions directly.
  - Currently there is no supported way to get this data from the resolver.

## Configuration & Operations

- [ ] Remove all hard-coded values from `main.rs`:
  - UIVK
  - Network (currently forced to TestNetwork)
  - lightwalletd URL
  - Scan birthday
  - Database path
  - RPC listen address
- [ ] Support mainnet (via features, config file, or env vars)
- [ ] Better structured logging and metrics for a real indexer

## Richer Protocol Support

- [ ] Index and surface marketplace actions:
  - LIST, DELIST, BUY
  - SETPRICE (for dynamic pricing tiers)
- [ ] Track nonce advancement and signatures on actions
- [ ] Distinguish admin-signed registrations vs sovereign (user-signed) registrations
- [ ] Store and return pricing configuration (tiers + nonce + height)

## Nice-to-Have / Longer Term

- [ ] Optional `with_proof` flag on `resolve` (or a dedicated method)
- [ ] Serve historical proof bundles for any past name event (not just the tip)
- [ ] Cross-check / pin known good state roots (or support multiple independent roots)
- [ ] Pagination + filtering improvements to match the current OpenRPC spec
- [ ] Prometheus / health endpoint beyond the current minimal status
- [ ] Proper handling of deep reorgs (currently limited)

## References

- `zns-verify` — the verification kernel (especially `proof.rs` and `chain.rs`)
- `zns-zecnames` (sibling crate) — contains a working Merkle state root + proof implementation
- TypeScript SDK (`zcashname-sdk`) — `resolveNameWithProof`, `Status`, `Registration`, `MerkleProof`
- `zcashname/lib/zns/` and the web app — expected shapes for listings, pricing, events
- Running an indexer docs in the web app (`content/indexer/running.mdx`)

---

This file should be updated as items are implemented. When the major gaps above are closed, the resolver should be able to serve as a drop-in or self-hosted equivalent to the public endpoints (`light.zcash.me/zns-*`).