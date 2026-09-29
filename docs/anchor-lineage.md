# The anchor lineage and the two-lane invariant

How the resolver decides which claims are backed, and why the decision is
a derived fold, not maintained state.

## The two lanes

Registry notes come in two families. They share a byte representation and
must never be compared — each is typed at the scan boundary
(`src/registry/nf.rs`).

| | anchor lane | name-chain lane |
|---|---|---|
| notes | ordinary (rseed self-consistent) | relaxed (ZNS commitment from memo) |
| nullifier | standard derivation | `zns_nullifier` (fork derivation) |
| type | `AnchorNf` | raw (PR #2: `TipNf`) |
| state | derived (`Lineage`) | the `names` row |

The scan surfaces both: ordinary notes in `ironwood_outputs`, relaxed
notes in `relaxed_ironwood_outputs` (seer-sync). Revealed spends are
grouped per transaction.

## The lineage is derived

The live anchor set — the nullifiers that currently confer claim
authority — is a pure function of position-ordered chain facts:

1. zero-value registry outputs **adopt** in canonical order
   (height, tx_index, action_index) until the pool first reaches
   standing size (`ANCHOR_POOL_SIZE = 40`, mirrors keygen's
   `NUM_ANCHORS`). The fold records that completion block; a later
   shrink does not reopen filling;
2. a revealed nullifier **retires** whatever the rest of its transaction
   turned out to be — including a candidate-free Registry spend;
3. a **successor** joins one-for-one — including while the pool is
   short — only when exactly one live anchor retired. Authority cannot
   be minted, only succeeded (mint lockstep: zns-mint #233). Ordinary
   zero-value outputs after close stay out.

`Lineage::step_tx` (`src/registry/anchor_lineage.rs`) folds one
transaction's facts in exactly that order. It is never persisted: each
batch folds the stored facts (`anchor_facts` table) into the state at
batch start, then steps the batch's own transactions, snapshotting the
pre-transaction state so every candidate is judged against the lineage its
transaction was judged against. Rewind drops the close only when the
completion block itself is removed from the fact stream.

Consequences:

- **No reorg machinery.** Rewind deletes facts above the fork and un-spends
  spends above it; folding the survivors equals folding the winning chain.
- **No cached pool rows.** Nothing can disagree with the facts.
- **Fail-closed birthday.** If the scan begins after the keygen ceremony,
  the pool never fills and every claim is rejected — never mis-admitted.

## The claim law

The mint's `accept_claim` (`zns-mint` `src/mint/registry.rs`), evaluated
at the candidate's transaction position:

1. the claim's own action spends a live anchor;
2. exactly one anchor is spent by the transaction;
3. no live name note is spent by the transaction;
4. the transaction creates exactly one zero-value successor;
5. the name is free (the chain rule: `prev_rcm_for(non-release tip, Claim)
   → None`).

Updates and releases take the same accept path with the mint's
`predecessor_spent` guard: a transition whose transaction also spends an
anchor is malformed.

## The canon contract

`tests/fixtures/canon-vectors-v1.json` is vendored from zns-mint
(provenance in `tests/fixtures/README.md`). All eight scenarios replay
into the fold; the live set and `adoption_closed` must equal the mint's
recorded snapshot after every event. Two implementations, one executable
contract.

## Known residue

- Implicit releases (a consumed tip without a valid successor; a spent
  foreign tip) land in the `implicit_releases` ledger rather than the
  event log — no memo, so the read-time memo-parse invariant stands.
  Surfacing them through the API is PR #2's remainder.
- The claim law lives in two repos by copy-paste (pinned by the fixture).
  The endgame is one law as a `zns-verify` kernel consumed by both.
- Clock checks (term expiry, liveness) are mint-side only — a clock-stale
  transition is admitted here when the mint would free the name. That is
  the M8 divergence, tracked separately.
- The mint's `retire_spent` adopts a rejected claim's successor into its
  pool (facts first). The fold reproduces this only for accepted-shape
  transactions; the poisoned entries are inert (attacker notes can never
  be spent) but grow the mint's pool unboundedly — mint-side hardening,
  separate ticket.
