# ZNS Resolver — Test & Benchmark Plan

This crate currently ships **no `tests/` and no `benches/`**. This document is
a concrete, prioritized menu of what to add.

## Critical prerequisite

`zns-resolver` is a **binary, not a library** — there is no `src/lib.rs`,
only `src/main.rs`, and every module is `pub(crate)`. Integration tests and
benches compile against the crate's *public* API, so today they can import
**nothing**; the only thing they could do is spawn the binary as a subprocess
and hit `127.0.0.1:8080`.

Before any of the below lands, split into:

- `src/lib.rs` — re-exports `registry`, `jsonrpc`, `sync`, `network`, plus a
  `pub async fn run() -> Result<(), SyncError>` that does what `main` does today.
- `src/main.rs` — thin wrapper that calls `lib::run()`.

`main.rs` is already 48 lines, so this is low risk and unblocks everything
below. It also lets the JSON-RPC handlers and the registry types be exercised
directly rather than through a live socket.

A `Registry::open_path(path: impl Into<PathBuf>)` constructor is also needed
so tests can use `tempfile`-managed SQLite files (or `:memory:`) instead of
the compile-time `DB_PATH` that `start()` hard-codes.

---

## Tests (`tests/`)

### Registry write path — highest value

1. **`registry_apply_batch.rs`** — the capstone. Build a `DecryptedNote`
   whose `cmx` is recomputed from `(g_d, pk_d, value, rho, ψ, rcm)` via
   `zns_verify::zns_psi_rcm` + `note_commitment_cmx` so it is internally
   consistent, feed it through `apply_batch`, and assert:
   - the name appears in `resolve_by_name`,
   - the event is in `events`,
   - `scan_state` advanced to the scanned position.

2. **`registry_lifecycle.rs`** — Claim → Update → Release → re-Claim.
   Assert each tip transition, that `Release` deletes the `names` row but
   keeps the event log, and that Claim-on-live and Update-with-stale-`prev_rcm`
   are rejected (the chain rule from `zns-verify`).

3. **`registry_rewind.rs`** — apply batches at H, H+1, H+2; `rewind(H)`;
   assert `scan_state.scanned_height == H`, events with `height > H` are gone,
   and each affected name's `names` row is rebuilt to the highest surviving
   event (or deleted if that event was a release).

4. **`registry_resume.rs`** — fresh DB → `start_height == birthday,
   seam_hash == None`; after a batch → `start_height == scanned + 1,
   seam_hash == Some(...)`; after rewind → consistent with the new tip.

### Registry reads

5. **`registry_queries.rs`** — `resolve_by_name` (hit / miss),
   `registrations_by_ua` (multi-match + pagination), `list_registrations`
   (ordering + limit/offset), `events` with `name` / `action` /
   `since_height` filters and `total`, `name_count`.

### Registry config / network

6. **`registry_config.rs`** — first `install_registry_config` inserts;
   a second call with a *different* UFVK / network / birthday is a no-op
   (this is the guard against mainnet/testnet DB reuse).

7. **`network_feature_flags.rs`** — assert the active cfg's
   `DB_PATH` / `NETWORK` constants; both-features-on is a `compile_error!`,
   exercised by a CI build matrix job rather than a runtime test.

### Sync filter

8. **`memo_filter.rs`** — `sync::notes_from_batch`: non-Orchard notes are
   skipped, Orchard-without-`ZNS:` is skipped, Orchard-with-`ZNS:` yields a
   `DecryptedNote` with the right `g_d` / `pk_d` / `rho` / `cmx` /
   `action_index`. Needs `seer-sync` to expose `ShieldedNote::Orchard`
   construction (or a `test-dependencies` feature).

9. **`shadows_ua_namespace.rs`** — names starting `u1` / `utest1` are
   rejected. Currently private, so test indirectly via `apply_batch` with
   `name = "u1evil"` and assert no row is admitted.

### JSON-RPC

10. **`jsonrpc_api.rs`** — start `serve_rpc` on an ephemeral port, drive
    with a `jsonrpsee` client:
    - `resolve` empty → list,
    - by-name → single record,
    - by-UA → reverse lookup,
    - `status` fields,
    - `events` filters + pagination,
    - invalid `action` → empty page,
    - `limit` clamped to 500.

11. **`jsonrpc_serialization.rs`** — golden JSON shapes for `NameRecord` /
    `NameEvent` / `Paginated` / `Status`; assert `to_name_event` emits
    `address: null` for release (public contract guard).

---

## Benchmarks (`benches/`, criterion)

`seer-sync` already uses `criterion`, so the harness is familiar.

1. **`apply_batch.rs`** — the hot loop. Pre-compute N valid
   `DecryptedNote`s for N ∈ {1, 10, 100, 1000}; throughput in notes/sec.
   Subgroup: isolate `try_admit_name_note` (crypto) vs the SQL transaction
   (DB) to attribute cost. **Highest value bench.**

2. **`verify_name_note.rs`** — micro-benches for `zns_psi_rcm` (BLAKE2b),
   `note_commitment_cmx` (Sinsemilla), and the full
   `verify_name_note_with_witness`. These dominate `apply_batch` and are
   the first place to look if the resolver is slow.

3. **`notes_from_batch.rs`** — `Batch` with K Orchard actions, fraction `f`
   carrying `ZNS:`; throughput in actions/sec and filtered-notes/sec.
   Captures the cost of scanning the bulk of mainnet Orchard traffic.

4. **`registry_reads.rs`** — populate 10k / 100k names; bench
   `resolve_by_name`, `registrations_by_ua`, `list_registrations`,
   `events` with filters. The user-visible latency surface.

5. **`rewind.rs`** — populate 100k events across N names; bench
   `rewind(H-10)` (shallow reorg) vs `rewind(0)` (deep). The
   `rebuild_name_tip` per-name loop is `O(affected names)` — quantify it.

6. **`jsonrpc_end_to_end.rs`** — start `serve_rpc` in-process, drive a
   `jsonrpsee` client; bench `resolve` / `status` / `events` round-trips.
   Captures `block_in_place` + `block_on` + serde overhead on top of raw
   DB reads.

7. **`memo_parse.rs`** — `parse_name_note` throughput as a baseline for
   `notes_from_batch`.

---

## Dev-dependencies to add

- `criterion` — bench harness (match `seer-sync`).
- `tempfile` — per-test SQLite files via the new `Registry::open_path`.
- `jsonrpsee` with the `client` feature — to drive `serve_rpc` in tests/benches.
- `zns-verify` (already a dependency) — its `zns_psi_rcm` / `note_commitment_cmx`
  are re-exported and let tests mint internally consistent `DecryptedNote`s
  without spinning up real Orchard keys.

## Suggested order

1. lib/bin split + `Registry::open_path`.
2. `registry_apply_batch.rs` — proves the seam works and is the capstone.
3. `registry_lifecycle.rs` + `registry_rewind.rs` — the chain-rule and reorg
   invariants, the two things that would actually be dangerous to break.
4. `apply_batch` + `verify_name_note` benches — the performance surface.
5. Everything else.
