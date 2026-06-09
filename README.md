# zns-resolver

ZcashName (ZNS) resolver: a thin chain observer that verifies ZNS Name Note
bindings and maintains a name → UA index, served over JSON-RPC.

The blockchain is the state machine; this crate is a materialized view of it.
The resolver scans Orchard outputs to the registry address (`addr_reg`) with
its **public IVK**, relaxed-decrypts them with the [`zns-verify`] kernel
(standard ZIP-212 scanning discards Name Notes by design — see
`DESIGN.md §8`), verifies each note's `rcm`-encoded binding, and folds the
result into an append-only SQLite action log.

## How it works

```
lightwalletd ──block stream──▶ observe ──candidates──▶ relaxed decrypt (AEAD)
                                                            │ note + memo
                                                            ▼
                  current_names view ◀──fold── actions log ◀── verify-on-apply
                        │                                   (cmx binding check)
                        ▼
                jsonrpsee RPC: resolve / status / events
```

- **`verify`** — pure crypto: memo grammar parsing + binding verification.
  A Sinsemilla `cmx` recomputed over `(action, name, ua, prev_rcm)` must equal
  the on-chain commitment; since `prev_rcm` comes from the resolver's own
  reconstructed chain tip, one comparison proves the binding *and* that the
  action extends the name's hash chain (`DESIGN.md §4–5`).
- **`observe`** — the scan loop over [`seer-sync`]'s toolkit (block stream,
  action parsing). seer-sync stays ZNS-blind; the relaxed decrypt lives
  entirely in `zns-verify`. Notes are applied in chain order
  `(height, tx_index, action_index)`.
- **`index`** — the SQLite store. `actions` is an append-only log;
  `current_names` folds it to the latest non-released action per name. A reorg
  is one `DELETE` above the fork — prior state re-emerges from the log.
- **`http`** — a resolution-only subset of the live ZNS indexer's JSON-RPC
  surface, wire-compatible with `zcashname-sdk`: `resolve`, `status`,
  `events`, plus stub `listings` (ZcashName has no marketplace).

## Usage

```sh
# Follow the chain tip and serve the RPC (testnet defaults):
zns-resolve serve --uivk uivk1... [--addr 127.0.0.1:8080] [--birthday <height>]

# One-shot scan to tip:
zns-resolve sync --uivk uivk1...

# Local queries:
zns-resolve lookup alice
zns-resolve status
```

`--mainnet`, `--lightwalletd <url>`, and `--db <path>` are global flags.

## Trust model

A resolver can be **stale or omit, but cannot forge a binding**: every served
record passed the `cmx` check against on-chain data. Two known limits:

- **Origin is not authenticated.** The binding proves integrity, not who sent
  the note; auth (who may CLAIM/UPDATE/RELEASE) is registry policy enforced
  before minting (`DESIGN.md §9`). See `NOTE(auth)` in
  `SqliteIndex::apply_notes`.
- **Query privacy** is the connection's: lookups are plain JSON-RPC over HTTP.

## Build & test

```sh
cargo test
```

Path dependencies: [`seer-sync`] (sibling checkout), [`zns-verify`], and the
`zns-orchard` fork (patched over crates.io `orchard`; its `unsafe-zns` feature
means *protocol*-unsafe — exposed commitment internals — not memory-unsafe).

[`seer-sync`]: ../seer-sync
[`zns-verify`]: ../zns-verify
