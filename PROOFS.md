# Proof bundles — the resolver's self-verifying answers

`DESIGN.md §19.7`: *ZcashName beats old ZNS if and only if verification runs in
the wallet.* A resolver that serves bare `{ ua }` answers reintroduces the
single point of trust this project exists to remove, no matter how honestly it
indexed. So every resolution answer carries the material for the wallet to
re-derive it from chain data: **proofs, not verdicts** (`DESIGN.md §11.4`).

This document is the implementation contract for that surface: the wire shape
the resolver serves, the walk the wallet runs, and what each side must hold.
The verification walk ships in `zns-verify` (feature `proof`) so the resolver,
the wallet SDK, and any future watchdog share one implementation.

---

## 1. What is served

A **proof chain** for a name is the name's `rcm`-linked Name Notes in chain
order, each wrapped with enough chain context to verify it independently:

- `resolve(query, limit, offset, with_proof = true)` — each registration entry
  gains a `proof` field containing the name's **current segment**: the links
  from its most recent CLAIM-from-zero genesis to the tip. Earlier
  claim/release cycles do not bear on the current binding (each CLAIM restarts
  the hash chain from `ZERO_PREV_RCM`), so they are not served here.
- `chain(name)` — the **full history**: every applied action for the name,
  all segments, same link shape. This is `DESIGN.md §11.4`'s `/chain/:name`,
  for auditing and longest-chain comparison.

The method names and entry shapes stay wire-compatible with `zcashname-sdk`;
`proof` is additive.

## 2. The link

```jsonc
{
  "action": "claim",          // claim | update | release
  "ua": "u1…",                // the claimed binding target ("" for release)
  "height": 419201,
  "txid": "hex",              // convenience pointer (derived; not trusted)
  "action_index": 0,          // which Orchard action in the tx is the Name Note
  "tx": "hex",                // the full raw transaction
  "header": "hex",            // the raw block header (PoW, hashMerkleRoot)
  "merkle_branch": ["hex"],   // txid → header.hashMerkleRoot (sha256d pairs)
  "merkle_index": 3           // the tx's leaf position in the block
}
```

`action` and `ua` are the resolver's only *claims*; everything else is chain
artifact. The claims need no separate authentication because the verification
walk hashes them: wrong claims produce a wrong `rcm`, hence a `cmx` mismatch
(`DESIGN.md §4`). The note's opening is **not** served — `ρ` is the action's
own `nf` field, the recipient `(g_d, pk_d)` and value are spec constants of
`addr_reg` (§4 below), and `prev_rcm` falls out of the walk. The resolver
cannot assert anything the wallet does not recompute.

## 3. The walk (wallet side)

For each link, genesis → tip (`zns_verify::proof::verify_chain`, pure, no IO):

1. **Parse `tx`**; parsing recomputes the ZIP-244 `txid`.
2. **Merkle inclusion**: fold `txid` up `merkle_branch` at `merkle_index`;
   the result must equal the header's `hashMerkleRoot`.
3. **Anchor**: hash the header → block hash. Collect
   `(height, block_hash)` — the kernel does **not** decide whether this header
   is in the real chain; see §5.
4. **Read the action** at `action_index`: its `nf` is the Name Note's `ρ`
   (the circuit constrains `ρ_new = nf_old`, `DESIGN.md §4`), its `cmx` is the
   commitment to check.
5. **Fold rule** (`DESIGN.md §5`): the first link must be a CLAIM
   (`prev_rcm = ZERO_PREV_RCM`); each later link extends the previous link's
   recomputed `rcm`; nothing extends a RELEASE except, in `chain` mode, a
   fresh CLAIM-from-zero.
6. **Binding**: `verify_name_note(action, name, ua, prev_rcm, g_d, pk_d, v,
   ρ, cmx)` — re-derive `(ψ, rcm)`, recompute the Sinsemilla commitment,
   compare to the on-chain `cmx`.

The tip link's `ua` is the resolution. Across several resolvers, the wallet
keeps the **longest valid chain** (`DESIGN.md §19.4`): a stale answer is a
provable prefix of the honest one; a forged answer fails step 6.

**Verify, don't decrypt.** The wallet never trial-decrypts the Name Note. The
relaxed AEAD decrypt (`zns-verify` feature `decrypt`) would only re-read the
memo — a restatement of the same claims the `cmx` recompute already
authenticates — at the cost of pulling cipher crates into every SDK build. The
commitment *is* the binding; decryption is a scanner concern, not a verifier
concern.

## 4. Spec constants

The walk needs three published constants of the namespace, supplied by the SDK
configuration, not by the resolver:

| Constant     | Value                                            |
|--------------|--------------------------------------------------|
| `addr_reg`   | the registry Orchard address → `(g_d, pk_d)`     |
| `v`          | `0` — Name Notes are value-0 (`zns-mint` signer) |
| genesis      | `ZERO_PREV_RCM = [0u8; 32]`                      |

## 5. PoW policy belongs to the caller

`verify_chain` returns `{ ua, anchors: [(height, block_hash)] }`. Whether
those block hashes sit in the canonical chain is the wallet's question, and
wallets differ: a light client matches them against the compact-block headers
it already syncs; a full-node-backed consumer asks its node; a watchdog may
check work directly. Baking one policy into the kernel would make the others
impossible — so the kernel proves *"this chain of bindings is internally valid
and committed under these headers"* and hands the headers up.

## 6. Where the resolver gets proof material

The scan loop (lightwalletd compact blocks) cannot produce Merkle branches —
compact blocks omit transparent-only txids, and branches need the block's full
txid list. So the resolver takes a validator RPC endpoint (`--validator-rpc`,
zebrad/zcashd):

- `getblock(height, 1)` → the block's txid list → branch + index;
- the raw header (`getblockheader` / raw-block prefix);
- the raw transaction (lightwalletd `GetTransaction`, already a dependency).

Material is **materialized at apply time** — right after a batch's actions
record, the observer fetches each affected block's context and stores the
result in SQLite (`proof_material`, keyed by txid). This keeps the RPC
handlers strictly read-only (they only ever join the cache) and costs one
validator round-trip pair per Name Note block — names are rare, so this is
noise next to the scan itself. The raw transaction comes free: note recovery
already fetched it. Material is immutable under finality; rows above a reorg
rewind are purged with the actions they prove. Without `--validator-rpc` the
resolver still indexes and serves bare answers; `with_proof`/`chain` requests
fail with a typed error (`-32011`).

The `actions` log additionally persists `action_index`.

## 7. Trust analysis

With this surface a malicious resolver can:

- **withhold** — serve a prefix (stale-but-genuine) or nothing. Caught by any
  honest peer's longer chain, or the wallet's self tail-scan
  (`DESIGN.md §19.4`); never believed over a longer valid chain.

It cannot:

- **forge** a binding — step 6 fails without the registry's minted `cmx`;
- **fabricate** chain context — steps 1–3 tie the `cmx` to a PoW header the
  wallet checks against its own view;
- **reorder or splice** history — the `rcm` chain (step 5) admits exactly one
  valid order per segment.

This is the §19.6 posture: a resolver is a block explorer, not an authority.

## 8. The disclosed witness, and cross-component gaps (resolved)

Specifying this contract surfaced three gaps; their resolutions are now in
the code:

1. **The mint wrote a zero memo into Name Notes** (`[0u8; 512]` in both
   signer builders), so the resolver — which indexes from the Name Note's own
   memo — would have indexed nothing. Fixed: both builders write the
   canonical memo via `zns_core::memo::encode_name_note`.
2. **The canonical Name Note memo form is pinned per `DESIGN.md §6`,
   *including* the `prev_rcm` witness:**
   `ZNS:<action>:<name>:<ua>:<prev_rcm_hex>` (64 lowercase hex; RELEASE keeps
   `ua` positional and empty). The witness is load-bearing against lying
   resolvers: the commitment already binds `prev_rcm` as a hash input, so
   disclosing it on-chain makes every single Name Note verifiable
   **standalone** — a scanner needs no reconstructed chain prefix. That is
   what gives the tail-scan backstop teeth (a wallet that spots a fresh
   UPDATE on-chain verifies it locally and rejects a resolver's stale
   prefix), and what `/openings` and §12 single-note fraud proofs rest on.
   An *indexer* still derives the canonical `prev_rcm` from its own tip; a
   note that verifies under its disclosed witness but not under the tip is
   **registry fork evidence**, which the resolver now logs loudly.
3. **The grammar parser was triplicated and disagreed** (mint, resolver, and
   the `DESIGN.md` sketch — `ZNS:update:alice:u1x:extra` parsed to different
   UAs in mint vs resolver). The canonical strict parser now lives in
   `zns_verify::memo` (no_std, exact field counts, DNS-label names); the
   resolver uses it. The mint's lenient request parser is still its own —
   migrating it to the strict semantics is open work.
