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
| type | `AnchorNf` | `TipNf` |
| state | derived (`Lineage`) | the `names` row |

The scan surfaces both: ordinary notes in `ironwood_outputs`, relaxed
notes in `relaxed_ironwood_outputs` (seer-sync). Revealed spends are
grouped per transaction.

## The lineage is derived

The live anchor set — the nullifiers that currently confer claim
authority — is a pure function of position-ordered chain facts:

1. The ceremony is one transaction that brings all 40 zero-value notes
   at once, and no name note. Those notes fill the pool. The fold records
   that block. Spending an anchor later leaves one fewer in the pool, and
   a new note does not replace it. A note sent
   on its own does not count, so the first 40 gifts cannot close
   adoption. The mint still counts every zero-value note. This resolver
   does not.
2. a revealed nullifier **retires** whatever the rest of its transaction
   turned out to be — including a candidate-free Registry spend;
3. a **successor** joins one-for-one — including while the pool is
   short — only for a claim whose registry outputs are exactly one
   zero-value note, and only when exactly one live anchor retired.
   A note cannot create a new claim permission on its own. It only
   replaces the one anchor this transaction spent (zns-mint #233). An update, a release, or a second registry output leaves the
   successor unset. Ordinary zero-value outputs after close stay out.

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

1. exactly one anchor is spent by the transaction;
2. no live name note is spent by the transaction;
3. the transaction's registry outputs are exactly one note, and that
   note is zero-value;
4. the name is free (the chain rule: `prev_rcm_for(non-release tip, Claim)
   → None`).

The anchor spend and the name note are not the same action. The mint's
builder places fee inputs between them, so requiring the name note's own
nullifier to be the anchor rejects claims the mint has already broadcast.

Updates and releases take the same accept path with the mint's
`predecessor_spent` guard: the transaction's nullifiers must spend this
name's live tip, no other live name, and no anchor. The predecessor spend
is not the name note's own action; fee inputs sit between them, same as a claim.

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
- A clock-due update ends the binding. Compact-block times are stored,
  and each block is judged at the median of its trailing eleven (the
  mint's MTP). The clocks are the predecessor's term and
  `confirmed_mtp + LIVENESS_INTERVAL`. A release stays legal after
  either clock. The first ten scanned blocks have no MTP, so an update
  there follows the chain rule alone.
- The mint's `retire_spent` adopts a rejected claim's successor into its
  pool (facts first). The fold reproduces this only for accepted-shape
  transactions; the poisoned entries are inert (attacker notes can never
  be spent) but grow the mint's pool unboundedly — mint-side hardening,
  separate ticket.
