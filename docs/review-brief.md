# ZNS Resolver — External Code Review Brief

`zns-resolver` watches a the ZcashName Mint's Name Notes account of Orchard note memos, verifies them, indexes them in SQLite, and serves them
## Scope of Review: Architecture and Code Quality


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

## Repo layout

| File | LOC | Responsibility |
|---|---|---|
| `src/main.rs` | 48 | Bootstrap: logging, open registry, start RPC, run sync loop. |
| `src/network.rs` | 74 | Compile-time mainnet/testnet selection; sole `#[cfg(feature=...)]` site. Exports `UFVK`, `NETWORK`, `DB_PATH`, `SCAN_BIRTHDAY`. |
| `src/sync.rs` | 206 | Long-running sync loop; `seer_sync::Account` impl adapter; `notes_from_batch` memo filter. |
| `src/registry.rs` | 98 | Module root + boundary types (`ChainPosition`, `ResumeInfo`, `Checkpoint`, `NameNote`, `Registration`, `Event`, `RegistryError`). |
| `src/registry/handle.rs` | 165 | `Registry` newtype wrapping one `tokio_rusqlite::AsyncConnection`; async API surface. |
| `src/registry/core.rs` | 508 | Transactional write path (`apply_batch`, `rewind`) + all read functions + row mappers. |
| `src/registry/notes.rs` | 130 | Memo parsing + calls into `zns-verify` for binding verification; fork-warning heuristic. |
| `src/registry/storage.rs` | 57 | SQL schema as a `const &str`. |
| `src/jsonrpc.rs` | 26 | Module root + `serve_rpc` starter. |
| `src/jsonrpc/handlers.rs` | 186 | `ZnsApi` trait (jsonrpsee proc macro) + `JsonRpcApi` impl. |
| `src/jsonrpc/models.rs` | 101 | Public DTOs (`NameRecord`, `NameEvent`, `Paginated`, `Status`) + conversion helpers. |
