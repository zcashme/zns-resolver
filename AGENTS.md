# AGENTS.md

## Commands

```bash
# Build / run the resolver (requires sibling crates at ../)
cargo build
cargo run

# Tests
cargo test
cargo clippy
cargo fmt -- --check

# Probe lightwalletd compatibility (useful when adding servers or debugging fetch)
cargo run --example probe_lwd_block
cargo run --example probe_lwd_block -- https://testnet.zec.rocks:443
```

The binary is a long-running daemon. It opens `zns-resolver.sqlite` (WAL mode), connects to the configured lightwalletd, and serves JSON-RPC on 127.0.0.1:8080 by default.

**Important**: The crate uses local path dependencies (`seer-sync`, `zns-verify`, `zns-orchard`). You must be inside the `zns-resolver` directory with the sibling crates at `../seer-sync`, `../zns-verify`, `../zns-orchard`.

Rust toolchain is pinned to 1.85.1 (see `librustzcash/rust-toolchain.toml` for reference and `Cargo.toml`).

## Configuration (currently hardcoded)

All critical constants live in `src/main.rs`:

- `UIVK` — the registry inbox viewing key (currently a testnet UIVK/UFVK).
- `NETWORK` — `TestNetwork`.
- `LIGHTWALLETD` — `https://testnet.zec.rocks:443`.
- `DB_PATH` — `zns-resolver.sqlite`.
- `RPC_ADDR` — `127.0.0.1:8080`.
- `SCAN_BIRTHDAY` — 4_000_000 (must be before first ZNS activity on the chosen network).

Changing any of these (especially for mainnet) requires a compatible viewing key and an appropriate birthday. The `install_registry_config` path is idempotent but warns on mismatch.

## Architecture

ZNS binds human-readable names to Orchard unified addresses by posting specially formatted memos in Orchard notes sent to a well-known registry inbox. The resolver is a **view-key-only indexer**:

1. Syncs compact blocks via lightwalletd (using `seer-sync`).
2. Trial-decrypts candidate Orchard notes using the registry IVK.
3. Fully decrypts + fetches raw tx for binding verification.
4. Enforces cryptographic binding + per-name transition rules.
5. Maintains a queryable SQLite index.
6. Serves a small JSON-RPC read API.

### High-level boot (main.rs)

- Initialize tracing (INFO level).
- Start the `Registry` actor (dedicated DB thread + `SyncSender<Op>` handle).
- Install (or verify) singleton registry config (UIVK + network + birthday).
- Spawn the JSON-RPC server (non-blocking).
- Run the sync loop until Ctrl-C.
- Graceful shutdown of the registry actor + join its thread.

### Sync Engine (sync.rs + orchard.rs)

- `run_sync_loop` is an infinite retry loop with reconnects.
- Resumes from `scan_state` checkpoint (or `SCAN_BIRTHDAY`).
- Polls tip until the local height is caught up.
- Streams compact blocks in chunks (`seer_sync::chain::blocks`).
- For each batch:
  - `observe_batch`: trial decrypt (compact), fetch raw txs for hits, full decrypt, collect `DecryptedNote` (note + on-chain cmx + memo + metadata).
  - `registry.apply_batch`: verify + index.
- Live tip is re-fetched before processing to detect concurrent tip movement.
- Reorgs are surfaced by `seer-sync` as `ChainError::Reorg(at)`.
- On reorg: `handle_reorg` calls `registry.rewind`, doubles `rewind_by` (exponential backoff on repeated reorgs).

Key timings:
- `RETRY_DELAY` = 5s
- `TIP_POLL_INTERVAL` = 10s

### Orchard Verification Layer (orchard.rs)

- `orchard_ivk`: accepts either UFVK or UIVK encoding, extracts the Orchard IVK as `PreparedIncomingViewingKey`.
- `observe_batch`: trial-decrypts with `zns_verify::decrypt::try_compact_orchard`, then for candidates:
  - Fetches the full transaction via gRPC.
  - Parses with the correct `BranchId`.
  - Fully decrypts the action.
  - Emits `DecryptedNote` carrying the *on-chain* `cmx` (the binding target).
- `verify_binding`: recomputes the ZNS-specific `(psi, rcm)` from action + name + ua + prev_rcm, derives the expected note commitment, and requires exact match against the on-chain cmx. Returns the `(psi, rcm)` only on success.

**Trust model** (repeated in several places):
> The memo is untrusted narration. The binding `(ψ, rcm) → cmx` is what actually authorizes a name transition.

### Registry Actor + DB (registry/*)

The registry is a classic actor:

- `Registry` (handle.rs): cheap `Clone` handle. All operations send an `Op` over a bounded `SyncSender` (cap 256).
- One OS thread owns the `rusqlite::Connection` (`core::DbConn`).
- All SQLite work (including reads for RPC) is serialized on that thread. This avoids `Send` issues and gives simple serializable transactions.
- `shutdown()` sends `Op::Shutdown` and the thread exits.

**Tables** (storage.rs, applied with `execute_batch` on open):

- `registry_account` (id=0 singleton): uivk, network, birthday.
- `scan_state` (id=0 singleton): resumability checkpoint + last known chain tip.
- `name_events`: append-only source of truth. `PRIMARY KEY (name, height)`. Stores full verified binding material + raw_tx.
- `names`: current tip projection (one row per live name). Absent row == released. Updated with `INSERT ... ON CONFLICT DO UPDATE` or `DELETE` on release.

**Critical write path** (`core.rs:apply_batch`):
- Opens one `unchecked_transaction`.
- For every decrypted note in the batch:
  1. `lifecycle_claim_from_memo` (untrusted candidate from memo).
  2. `name_tip_in_tx` (sees prior writes in the *same* tx — important for multi-note batches).
  3. `try_admit_name_note` = `prev_rcm_for` (transition rule) + `verify_binding` (crypto).
  4. Only on success: `insert_event`, mutate `names` (or DELETE on Release), collect for return.
- After the loop: `set_checkpoint_in_tx` (advances scanned height + tip).
- Commit.

**Reorg / rewind invariants** (core.rs):
- Shallow (`scanned_height - fork_height <= REORG_SHALLOW_MAX` = 30): delete events after fork, `rebuild_name_tip` for every affected name, reset checkpoint to fork point.
- Deep: truncate `name_events`, `names`, `scan_state`.
- `rebuild_name_tip` must produce *exactly* the same `names` row that normal ingest would have produced for the highest surviving event (or delete on release tip). This is a correctness contract.

**Lifecycle rules** (lifecycle.rs + zns-verify):
- `shadows_ua_namespace`: names starting with `u1` / `utest1` are rejected (to avoid confusion with addresses).
- `Action::Claim | Update | Release`.
- `prev_rcm_for` from `zns_verify::chain` implements the per-name hash chain rule.

The modularization state is documented in:
- `REGISTRY_MODULARIZATION_DISCOVERY.md`
- `REGISTRY_REFACTOR_SYNTHESIS.md`

Current split (as of this writing):
- `handle.rs` — actor surface + dispatch
- `core.rs` — transactional heart + invariants (all reads + writes)
- `lifecycle.rs` — memo parsing + admission
- `storage.rs` — SCHEMA_SQL

### JSON-RPC API (jsonrpc.rs)

Uses `jsonrpsee` server + proc macro. All methods are `blocking` (run on jsonrpsee thread pool) and delegate to the `Registry` handle.

Methods:
- `resolve(query, limit?, offset?)` — exact name match, UA prefix reverse lookup, or full list when query empty.
- `status()` — sync progress, registered count, uivk, etc. (listings and admin fields are stubs).
- `events(name?, action?, since_height?, limit?, offset?)` — paginated event log.
- `listings(...)` — stub (always empty).

Responses use hex for txids/blobs. Action names are uppercase in the wire format (`"CLAIM"`, `"UPDATE"`, `"RELEASE"`).

### Data Types (pub(crate) surface between modules)

Defined at the top of `src/registry.rs` (and re-exported) for visibility:
- `Cursor`, `Checkpoint`, `NameNote`, `Registration`, `Event`, `RegistryError`.

## Key Invariants & Gotchas

1. **Checkpoint must not advance without the events.** Both happen inside the same committed transaction in `apply_batch`.
2. **Single-writer-thread is mandatory.** Do not give any other thread a `Connection` or attempt concurrent writes.
3. **Binding is the authority.** If `verify_binding` fails, the note is ignored even if the memo looks perfect. `warn_registry_fork` only fires when the memo's claimed prev_rcm would have verified against a *different* tip.
4. **`rebuild_name_tip` must be semantically identical** to the projection logic in the normal ingest path.
5. `names` row is absent after a release; do not treat a release row as a live registration.
6. `name_events` keeps history forever (except on deep reorg). Use it for audit / dispute resolution.
7. Action strings are lowercase in the DB (`claim`/`update`/`release`) but uppercase on the RPC wire.
8. `action_index` is the 0-based index inside the Orchard bundle of the transaction.
9. Raw tx is stored for every event (space cost accepted for auditability).
10. The registry inbox UIVK is the root of trust for *what* we scan. Anyone who can derive a note to that IVK can attempt a binding.

## Testing

- `cargo test` — currently thin (most logic is exercised via integration with real chain data).
- `cargo run --example probe_lwd_block` — exercises the lower-level compact block + raw tx fetch path against public servers. Use when validating new lightwalletd endpoints or changes in seer-sync.
- For deeper testing you will typically need the sibling `zns-verify` and `zns-orchard` test suites plus a local regtest or testnet faucet that can emit valid ZNS memos.

There is no built-in regtest harness in this crate today.

## Crate Versions & Constraints

Pinned in `Cargo.toml` (and resolved in `Cargo.lock`):

- Rust 1.85.1 minimum.
- `rusqlite` 0.32 (bundled).
- `jsonrpsee` 0.26 (server + macros).
- `tonic` 0.14 + `tls-ring` (same pattern as many modern Zcash light clients).
- `zcash_protocol`, `zcash_keys`, `zcash_primitives` at specific versions for compatibility with the rest of the ZNS workspace.
- Local `orchard` patch to `../zns-orchard` (with `unsafe-zns` feature).
- `seer-sync` and `zns-verify` are workspace siblings — their exact versions and features matter.

When bumping Zcash crates, re-verify that `zns-verify` binding construction and `observe_batch` decryption still produce matching `cmx` values.

## Ignored / Special Paths

- `jsonrpsee-docs/` — vendored copy of the jsonrpsee crate for reference. Do not edit.
- `librustzcash/` — vendored reference copy of the Zcash Rust crates (has its own `AGENTS.md` with strict contribution rules). Not a direct build dep of this binary.
- `target/` — build artifacts.
- `REGISTRY_MODULARIZATION_DISCOVERY.md` and `REGISTRY_REFACTOR_SYNTHESIS.md` — historical design documents. Once the registry split is finalized they may be archived or turned into normal module docs.
- `zns-resolver.sqlite` and `zns-resolver.sqlite-*` (WAL/SHM) — runtime database. Never commit.

## Contribution Notes

- Keep changes focused on the correctness of the binding gate and the per-name chain.
- When touching the write path (`apply_batch`, `rewind`, `rebuild_name_tip`), add or update comments that restate the invariant being preserved.
- Schema changes require a strategy (current code has no migration framework beyond re-applying `CREATE IF NOT EXISTS`).
- RPC changes must keep backward compatibility for existing consumers (or bump a version that is coordinated with the rest of the ZNS ecosystem).

This file is the source of truth for agent context. Update it when architecture, invariants, or operational procedures change.