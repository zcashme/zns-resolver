# ZNS Resolver — External Code Review Brief

`zns-resolver` watches a the ZcashName Mint's Name Notes account of Orchard note memos, verifies them, indexes them in SQLite, and serves them

## Scope of Review

Architecture & boundaries, plus general code quality.

#### 1. Sync engine — `src/sync.rs` (199 LOC)

---

#### 2. Registry — `src/registry.rs` (94) + `src/registry/{handle,core,notes,storage}.rs`

---
#### 3. JSON-RPC — `src/jsonrpc.rs` (26) + `src/jsonrpc/{service,records}.rs`

---

## ZNS Protocol Architecture Overview

The **mint** (`zns-mint`) is the registry-writer: it
watches the chain for user request memos like `ZNS:claim:alice:ua1...`, runs a
challenge-response auth flow, and then authors and broadcasts an Orchard note
(Name Note) that records the binding. 

A Name Note is an Orchard shielded note whose memo carries the canonical form
`ZNS:<verb>:<name>:<ua>:<prev_rcm>` — for example
`ZNS:claim:alice:u1...:<64 hex>`. The `prev_rcm` is a 32-byte witness tucked
into the memo so any scanner can verify that one note on its own, without first
rebuilding the name's whole history. The note's on-chain commitment (`cmx`)
cryptographically binds the action, name, address, and `prev_rcm` together
through a BLAKE2b-derived `(ψ, rcm)` and the Sinsemilla note commitment.

Each name has a chain: `claim` starts it (from a zero `prev_rcm`), `update` and
`release` extend the live tip. 

## What the resolver's task is

`zns-resolver` is a Zcash Name Service resolver. There's a shielded "registry
inbox" account on Zcash. We hold its full viewing key (UFVK), watch the chain,
and decrypt Orchard notes whose memos start with `ZNS:`. Each of those memos
carries a name → unified-address binding. We verify the binding against the
on-chain note commitment, store the tip per name in SQLite, and expose it all
over a JSON-RPC HTTP API for resolution and event history.

One-way data flow (reorgs handled separately):

```
lightwalletd ──▶ seer-sync engine ──▶ Batch of decrypted notes
                  (drives the scan)         │
                                             ▼
                                    sync::notes_from_batch
                                    (filter Orchard + "ZNS:" memo)
                                             │
                                             ▼
                                    registry::apply_batch
                                    (verify binding via zns-verify,
                                     update name tips + event log,
                                     advance scan_state in one tx)
                                             │
                                             ▼
                                    SQLite (WAL, single connection)
                                             │
                                             ▼
                                    jsonrpc handlers (resolve / events / status)
                                             │
                                             ▼
                                     JSON-RPC client on 127.0.0.1:8080
```
## Scope of Review: Architecture & Boundaries and General Code Quality

#### 1. Sync engine — `src/sync.rs` (199 LOC)

---

#### 2. Registry — `src/registry.rs` (94) + `src/registry/{handle,core,notes,storage}.rs`

---
#### 3. JSON-RPC — `src/jsonrpc.rs` (26) + `src/jsonrpc/{service,records}.rs`
