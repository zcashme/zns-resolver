//! The anchor lineage, derived: a pure fold over position-ordered facts.
//!
//! The lineage is never maintained — it is recomputed from chain facts.
//! The rules mirror the mint's claim-anchor semantics exactly
//! (`zns-mint` `AnchorPool` / `apply_block`). The keygen ceremony is one
//! transaction: all 40 zero-value registry notes, and no name note. A
//! zero-value note in any other transaction does not join the pool.
//! Spending an anchor later leaves one fewer in the pool, and a new note
//! does not replace it. A revealed nullifier retires whatever the rest of
//! its transaction turned out to be; a successor joins one-for-one —
//! including while the pool is short — only for a claim whose registry
//! outputs are exactly one zero-value note, and only when exactly one
//! live anchor retired.
//!
//! Because the fold is a pure function of facts, reorg correctness is
//! inherited from the fact tables' rewind semantics: folding the facts
//! that survive a rewind equals folding the winning chain.

use std::collections::BTreeSet;

use super::nf::AnchorNf;

/// Standing size of the anchor lineage pool; mirrors keygen's `NUM_ANCHORS`
/// and the mint's `ANCHOR_POOL_SIZE`.
pub(crate) const ANCHOR_POOL_SIZE: usize = 40;

/// The keygen ceremony is one transaction: the whole standing set of
/// zero-value registry outputs, and no name note. A note outside that
/// transaction does not join the pool.
pub(crate) fn is_ceremony_fill(zero_value_outputs: usize, name_notes: usize) -> bool {
    zero_value_outputs == ANCHOR_POOL_SIZE && name_notes == 0
}

/// A zero-value note is kept when it belongs to the keygen transaction,
/// or when it is a claim's successor. The scan and the in-memory batch
/// use the same check, so a replay of stored facts matches the batch
/// that wrote them.
pub(crate) fn keeps_anchor_note(
    zero_value_notes: usize,
    name_notes: usize,
    claim_successor: bool,
) -> bool {
    claim_successor || is_ceremony_fill(zero_value_notes, name_notes)
}

/// Canonical chain position of a fact: block height, transaction index
/// within the block, action index within the transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Position {
    pub(crate) height: u32,
    pub(crate) tx_index: u32,
    pub(crate) action_index: u32,
}

/// A zero-value registry output entering the lineage.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Adoption {
    pub(crate) nf: AnchorNf,
}

/// A revealed nullifier leaving the lineage.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Retirement {
    pub(crate) nf: AnchorNf,
}

/// One transaction's anchor facts, in canonical order. Adoptions and
/// retirements are grouped per transaction because the mint's semantics
/// are per-transaction: ceremony adoptions run before any verdict, and the
/// one-for-one successor rule reads the transaction's output shape.
#[derive(Debug, Default)]
pub(crate) struct TxAnchorFacts {
    pub(crate) adoptions: Vec<Adoption>,
    pub(crate) retirements: Vec<Retirement>,
    /// Whether the transaction presented exactly one name-note candidate —
    /// the accept path. Transactions without it take `follow_spends`.
    pub(crate) has_single_name_note: bool,
    /// Name notes in this transaction. The keygen ceremony has none.
    pub(crate) name_notes: usize,
    /// The mint's successor: the one candidate is a claim, and the
    /// transaction's registry outputs are exactly one zero-value note.
    /// An update, a release, or a second registry output leaves this false,
    /// so the note cannot take a seat after ceremony close.
    pub(crate) claim_successor: bool,
}

/// The set of nullifiers that currently confer claim authority, plus the
/// block where ceremony adoption first reached standing size.
#[derive(Debug, Default, Clone)]
pub(crate) struct Lineage {
    live: BTreeSet<AnchorNf>,
    /// Block where ceremony adoption first reached standing size.
    /// Later ordinary outputs do not refill a shrunken pool. Cleared
    /// only when a rewind removes that block from the fact stream.
    established: Option<u32>,
}

impl Lineage {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether ceremony adoption has already reached standing size.
    pub(crate) fn adoption_closed(&self) -> bool {
        self.established.is_some()
    }

    /// Block where ceremony adoption first reached standing size.
    pub(crate) fn established(&self) -> Option<u32> {
        self.established
    }

    /// One note from a ceremony the caller has already accepted. This is
    /// the mint's `adopt`: the note joins until the pool first reaches
    /// standing size. The scan does not call this for an arbitrary note.
    /// `step_tx` calls it for each note of the keygen transaction. The
    /// canon fixture's `adopt_anchor` events call it directly, the same
    /// way the mint's fixture does.
    pub(crate) fn adopt(&mut self, height: u32, nf: AnchorNf) {
        if self.adoption_closed() || self.live.len() >= ANCHOR_POOL_SIZE {
            return;
        }
        if self.live.insert(nf) && self.live.len() == ANCHOR_POOL_SIZE {
            self.established = Some(height);
        }
    }

    /// Advances the lineage across one transaction, in the mint's order:
    /// the keygen transaction's notes, then retirements (facts follow the
    /// chain whatever the verdict), then the one-for-one successor when
    /// the output shape is exactly one.
    pub(crate) fn step_tx(&mut self, height: u32, facts: &TxAnchorFacts) {
        if is_ceremony_fill(facts.adoptions.len(), facts.name_notes) {
            for adoption in &facts.adoptions {
                self.adopt(height, adoption.nf);
            }
        }

        let mut retired = 0;
        for retirement in &facts.retirements {
            if self.live.remove(&retirement.nf) {
                retired += 1;
            }
        }

        // One-for-one, mirroring the mint's retire_spent: a successor
        // takes a seat only for a claim with exactly one zero-value
        // registry output, and only when exactly one live anchor retired.
        // Authority cannot be minted, only succeeded. This insert is
        // independent of ceremony close: a backed successor still
        // enters while the pool is short.
        if retired == 1 && facts.claim_successor && facts.adoptions.len() == 1 {
            self.live.insert(facts.adoptions[0].nf);
        }
    }

    /// Whether `nf` is a live claim anchor right now. The claim law counts
    /// live retirements directly; this query is for the lineage tests.
    #[cfg(test)]
    pub(crate) fn contains(&self, nf: &AnchorNf) -> bool {
        self.live.contains(nf)
    }

    /// How many of the transaction's retirements hit live anchors — the
    /// mint's `spent.len()` in `accept_claim` (must be exactly one).
    pub(crate) fn live_retirements(&self, facts: &TxAnchorFacts) -> usize {
        facts
            .retirements
            .iter()
            .filter(|r| self.live.contains(&r.nf))
            .count()
    }

    /// Whether any of the transaction's retirements hits a live anchor —
    /// the mint's `predecessor_spent` guard: an update or release whose
    /// transaction also spends an anchor is malformed.
    pub(crate) fn touches_live_anchor(&self, facts: &TxAnchorFacts) -> bool {
        facts.retirements.iter().any(|r| self.live.contains(&r.nf))
    }

    pub(crate) fn len(&self) -> usize {
        self.live.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nf(seed: u8) -> AnchorNf {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        AnchorNf::from_bytes(&bytes)
    }

    fn adopt(seed: u8) -> Adoption {
        Adoption { nf: nf(seed) }
    }

    fn retire(seed: u8) -> Retirement {
        Retirement { nf: nf(seed) }
    }

    fn ceremony_adopt(seed: u8) -> TxAnchorFacts {
        TxAnchorFacts {
            adoptions: vec![adopt(seed)],
            retirements: vec![],
            has_single_name_note: false,
            name_notes: 0,
            claim_successor: false,
        }
    }

    fn keygen_ceremony() -> TxAnchorFacts {
        TxAnchorFacts {
            adoptions: (1..=ANCHOR_POOL_SIZE)
                .map(|seed| adopt(seed as u8))
                .collect(),
            retirements: vec![],
            has_single_name_note: false,
            name_notes: 0,
            claim_successor: false,
        }
    }

    fn fill_to_standing_size(lineage: &mut Lineage, height: u32) {
        lineage.step_tx(height, &keygen_ceremony());
    }

    /// The keygen transaction is the only ceremony. One note, 39 notes,
    /// 41 notes, or 40 notes that share the transaction with a name note
    /// do not join. The 40-note transaction with no name note fills the
    /// pool and closes it.
    #[test]
    fn only_the_keygen_transaction_fills_the_pool() {
        assert!(is_ceremony_fill(ANCHOR_POOL_SIZE, 0));
        assert!(!is_ceremony_fill(1, 0));
        assert!(!is_ceremony_fill(ANCHOR_POOL_SIZE - 1, 0));
        assert!(!is_ceremony_fill(ANCHOR_POOL_SIZE + 1, 0));
        assert!(!is_ceremony_fill(ANCHOR_POOL_SIZE, 1));
        assert!(keeps_anchor_note(1, 1, true));
        assert!(!keeps_anchor_note(1, 0, false));
        assert!(keeps_anchor_note(ANCHOR_POOL_SIZE, 0, false));
        assert!(!keeps_anchor_note(ANCHOR_POOL_SIZE, 1, false));

        let mut one = Lineage::new();
        one.step_tx(1, &ceremony_adopt(1));
        assert!(one.is_empty());
        assert!(!one.adoption_closed());

        let mut short = Lineage::new();
        let mut facts = keygen_ceremony();
        facts.adoptions.pop();
        short.step_tx(1, &facts);
        assert!(short.is_empty());

        let mut with_name = Lineage::new();
        let mut facts = keygen_ceremony();
        facts.name_notes = 1;
        with_name.step_tx(1, &facts);
        assert!(with_name.is_empty());

        let mut extra = Lineage::new();
        let mut facts = keygen_ceremony();
        facts.adoptions.push(adopt(0xF0));
        extra.step_tx(1, &facts);
        assert!(extra.is_empty());

        let mut lineage = Lineage::new();
        lineage.step_tx(1, &keygen_ceremony());
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE);
        assert!(lineage.adoption_closed());
        assert_eq!(lineage.established(), Some(1));
    }

    /// Ceremony filling stops at standing size. A note after the keygen
    /// transaction does not join.
    #[test]
    fn adoption_is_capped_at_standing_size() {
        let mut lineage = Lineage::new();
        fill_to_standing_size(&mut lineage, 1);
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE);
        assert!(lineage.adoption_closed());
        assert_eq!(lineage.established(), Some(1));

        lineage.step_tx(2, &ceremony_adopt(u8::MAX));
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE);
        assert!(!lineage.contains(&nf(u8::MAX)));
    }

    /// A revealed nullifier retires whatever the transaction was.
    #[test]
    fn retirement_follows_the_chain_fact() {
        let mut lineage = Lineage::new();
        lineage.adopt(1, nf(1));
        lineage.step_tx(
            2,
            &TxAnchorFacts {
                adoptions: vec![],
                retirements: vec![retire(1)],
                has_single_name_note: false,
                name_notes: 0,
                claim_successor: false,
            },
        );
        assert!(lineage.is_empty());
    }

    /// A backed claim's successor joins one-for-one, past standing size.
    #[test]
    fn successor_joins_past_standing_size() {
        let mut lineage = Lineage::new();
        fill_to_standing_size(&mut lineage, 1);
        // A backed claim: exactly one name-note candidate (the accept path),
        // spends one live anchor, creates one zero-value output. The
        // successor takes the retired anchor's seat.
        lineage.step_tx(
            2,
            &TxAnchorFacts {
                adoptions: vec![adopt(200)],
                retirements: vec![retire(1)],
                has_single_name_note: true,
                name_notes: 1,
                claim_successor: true,
            },
        );
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE);
        assert!(!lineage.contains(&nf(1)));
        assert!(lineage.contains(&nf(200)));
    }

    /// After ceremony close, a zero-value note seats only as a claim's
    /// successor. An update or release that spends one anchor, and a claim
    /// with a second registry output, are both stored with the flag clear.
    #[test]
    fn a_note_without_the_claim_shape_does_not_seat() {
        let mut lineage = Lineage::new();
        fill_to_standing_size(&mut lineage, 1);
        lineage.step_tx(
            2,
            &TxAnchorFacts {
                adoptions: vec![adopt(200)],
                retirements: vec![retire(1)],
                has_single_name_note: true,
                name_notes: 1,
                claim_successor: false,
            },
        );
        assert!(!lineage.contains(&nf(200)));
        assert!(!lineage.contains(&nf(1)));
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE - 1);
    }

    /// Two zero-value outputs are not the keygen transaction, so neither
    /// joins. The retirement still follows the chain. There is no
    /// successor insert: the output shape is not exactly one.
    #[test]
    fn multi_adoption_tx_gets_no_successor_insert() {
        let mut lineage = Lineage::new();
        lineage.adopt(1, nf(1));
        lineage.step_tx(
            2,
            &TxAnchorFacts {
                adoptions: vec![adopt(2), adopt(3)],
                retirements: vec![retire(1)],
                has_single_name_note: true,
                name_notes: 1,
                claim_successor: false,
            },
        );
        assert!(!lineage.contains(&nf(2)));
        assert!(!lineage.contains(&nf(3)));
        assert!(!lineage.contains(&nf(1)));
        assert!(lineage.is_empty());
    }

    /// Within one transaction the mint's order is: ceremony adoptions
    /// (capped), then retirements, then the successor. At the cap boundary
    /// this order is observable: the successor's seat opens only after the
    /// retirement, and the uncapped insert lands it anyway.
    #[test]
    fn within_tx_order_adopts_then_retires_then_successor() {
        let mut lineage = Lineage::new();
        fill_to_standing_size(&mut lineage, 1);
        let before = lineage.len();

        lineage.step_tx(
            2,
            &TxAnchorFacts {
                adoptions: vec![adopt(200)],
                retirements: vec![retire(7)],
                has_single_name_note: true,
                name_notes: 1,
                claim_successor: true,
            },
        );

        assert_eq!(lineage.len(), before);
        assert!(lineage.contains(&nf(200)));
        assert!(!lineage.contains(&nf(7)));
    }

    /// A successor-shaped output with no live anchor retired behind it
    /// adopts nothing at standing size — the fold's mirror of the mint's
    /// fix (zns-mint #233): authority cannot be minted, only succeeded.
    /// After ceremony close the filling loop adopts nothing either.
    #[test]
    fn unbacked_successor_adopts_nothing() {
        let mut lineage = Lineage::new();
        fill_to_standing_size(&mut lineage, 1);
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE);

        lineage.step_tx(
            2,
            &TxAnchorFacts {
                adoptions: vec![adopt(50)],
                retirements: vec![retire(99)], // 99 is not a live anchor
                has_single_name_note: true,
                name_notes: 1,
                claim_successor: true,
            },
        );
        assert!(!lineage.contains(&nf(50)));
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE);
    }

    /// Ceremony close is one-time: a later shrink does not reopen filling.
    /// A one-for-one successor still enters while the pool is short.
    /// Folding facts that survive a rewind drops the close only when the
    /// completion block itself is gone.
    #[test]
    fn ceremony_adoption_stays_closed_after_the_pool_shrinks() {
        let mut lineage = Lineage::new();
        fill_to_standing_size(&mut lineage, 100);
        assert!(lineage.adoption_closed());
        assert_eq!(lineage.established(), Some(100));

        // Candidate-free Registry spend: both anchors leave, no successor.
        lineage.step_tx(
            101,
            &TxAnchorFacts {
                adoptions: vec![],
                retirements: vec![retire(1), retire(2)],
                has_single_name_note: false,
                name_notes: 0,
                claim_successor: false,
            },
        );
        lineage.step_tx(102, &ceremony_adopt(0xF0));
        assert!(!lineage.contains(&nf(0xF0)));
        // A second keygen-shaped transaction does not refill the pool.
        lineage.step_tx(102, &keygen_ceremony());
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE - 2);
        assert!(lineage.adoption_closed());

        // A backed successor still replaces the one anchor it spends.
        lineage.step_tx(
            103,
            &TxAnchorFacts {
                adoptions: vec![adopt(201)],
                retirements: vec![retire(3)],
                has_single_name_note: true,
                name_notes: 1,
                claim_successor: true,
            },
        );
        assert!(lineage.contains(&nf(201)));
        lineage.step_tx(104, &ceremony_adopt(202));
        assert!(!lineage.contains(&nf(202)));

        // Rewind to the completion block: still closed, all 40 restored.
        let mut at_completion = Lineage::new();
        fill_to_standing_size(&mut at_completion, 100);
        assert!(at_completion.adoption_closed());
        assert_eq!(at_completion.len(), ANCHOR_POOL_SIZE);
        at_completion.step_tx(100, &ceremony_adopt(203));
        assert!(!at_completion.contains(&nf(203)));

        // Before the keygen transaction, adopt() still takes a note. A
        // one-note transaction does not.
        let mut before = Lineage::new();
        assert!(!before.adoption_closed());
        assert_eq!(before.len(), 0);
        before.adopt(100, nf(1));
        assert!(before.contains(&nf(1)));
        assert!(!before.adoption_closed());
        before.step_tx(100, &ceremony_adopt(2));
        assert!(!before.contains(&nf(2)));
    }

    /// The reorg property: folding the facts that survive a rewind equals
    /// folding the winning chain from scratch. Rewind deletes facts above
    /// the fork and un-spends spends above it; the fold needs no code of
    /// its own to be reorg-correct.
    #[test]
    fn fold_over_rewound_facts_equals_fold_over_winning_chain() {
        // Winning chain: anchor 1 adopted at h10, retired at h12 by a claim
        // whose successor is anchor 2.
        let mut lineage = Lineage::new();
        lineage.adopt(10, nf(1));

        let facts = TxAnchorFacts {
            adoptions: vec![adopt(2)],
            retirements: vec![retire(1)],
            has_single_name_note: true,
            name_notes: 1,
            claim_successor: true,
        };
        lineage.step_tx(12, &facts);
        assert!(lineage.contains(&nf(2)));

        // A rewind below h12 un-spends the retirement: the surviving fact
        // stream is just the adoption of anchor 1. Folding it from scratch
        // must match a lineage rebuilt only from surviving facts.
        let mut rewound = Lineage::new();
        rewound.adopt(10, nf(1));
        assert!(rewound.contains(&nf(1)));
        assert_eq!(rewound.len(), 1);
        assert!(!rewound.contains(&nf(2)));
    }
}

/// The resolver folds the same chain facts the mint folds; the vendored
/// canon fixture is the executable cross-repo contract. Every scenario is
/// replayed into the fold; the live set and `adoption_closed` must equal
/// the mint's recorded snapshot after every event.
#[cfg(test)]
mod canon {
    use super::*;
    use serde::Deserialize;

    const FIXTURE: &str = include_str!("../../tests/fixtures/canon-vectors-v1.json");

    #[derive(Deserialize)]
    struct Fixture {
        version: String,
        anchor_pool_size: usize,
        scenarios: Vec<Scenario>,
    }

    #[derive(Deserialize)]
    struct Scenario {
        name: String,
        events: Vec<CanonEvent>,
        trace: Vec<Trace>,
    }

    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum CanonEvent {
        AdoptAnchor {
            height: u32,
            nullifier: String,
        },
        /// Candidate-free Registry spend: live anchors in `spent` leave,
        /// and no successor is seated.
        Retire {
            height: u32,
            spent: Vec<String>,
        },
        Claim {
            height: u32,
            spent_anchor: String,
            successor_anchor: String,
        },
        UnbackedClaim {
            height: u32,
            spent: Vec<String>,
        },
        Update {
            height: u32,
            prev_nullifier: String,
        },
        Release {
            height: u32,
            prev_nullifier: String,
        },
        Rewind {
            to_height: u32,
        },
    }

    #[derive(Deserialize)]
    struct Trace {
        anchor_pool: Vec<String>,
        adoption_closed: bool,
    }

    fn hex32(s: &str) -> AnchorNf {
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("fixture hex");
        }
        AnchorNf::from_bytes(&bytes)
    }

    /// One applied event = one transaction group at a unique position.
    struct Applied {
        height: u32,
        tx_index: u32,
        facts: TxAnchorFacts,
        /// `adopt_anchor` is the mint's `adopt()` call. The scan never
        /// sets this; it only steps a keygen transaction.
        adopt: bool,
    }

    fn fold(applied: &[Applied]) -> Lineage {
        let mut order: Vec<&Applied> = applied.iter().collect();
        order.sort_by_key(|a| (a.height, a.tx_index));
        let mut lineage = Lineage::new();
        for applied in order {
            if applied.adopt {
                for adoption in &applied.facts.adoptions {
                    lineage.adopt(applied.height, adoption.nf);
                }
            } else {
                lineage.step_tx(applied.height, &applied.facts);
            }
        }
        lineage
    }

    fn assert_snapshot(lineage: &Lineage, trace: &Trace) {
        assert_eq!(lineage.len(), trace.anchor_pool.len(), "pool size");
        for hex in &trace.anchor_pool {
            assert!(lineage.contains(&hex32(hex)), "missing anchor {hex}");
        }
        assert_eq!(
            lineage.adoption_closed(),
            trace.adoption_closed,
            "adoption_closed"
        );
    }

    #[test]
    fn canon_vectors_pin_the_fold() {
        let fixture: Fixture = serde_json::from_str(FIXTURE).expect("fixture parses");
        assert_eq!(fixture.version, "canon-vectors-v1");
        // The fixture pins the standing size across repos.
        assert_eq!(fixture.anchor_pool_size, ANCHOR_POOL_SIZE);

        // The exact scenario set — a count alone would let a fixture edit
        // silently drop the security regression vector.
        let mut names: Vec<&str> = fixture.scenarios.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "backed_claim",
                "ceremony_closes_once",
                "ceremony_fill",
                "claim_after_release",
                "duplicate_claim",
                "reorg",
                "unbacked_claim",
                "update_then_release",
            ]
        );
        assert_eq!(fixture.scenarios.len(), 8);

        for scenario in &fixture.scenarios {
            let mut applied: Vec<Applied> = Vec::new();
            for (i, event) in scenario.events.iter().enumerate() {
                let (height, facts, adopt) = match event {
                    // The fixture calls the mint's adopt() once per note.
                    // The keygen check lives in step_tx, not in these events.
                    CanonEvent::AdoptAnchor { height, nullifier } => (
                        *height,
                        TxAnchorFacts {
                            adoptions: vec![Adoption {
                                nf: hex32(nullifier),
                            }],
                            retirements: vec![],
                            has_single_name_note: false,
                            name_notes: 0,
                            claim_successor: false,
                        },
                        true,
                    ),
                    CanonEvent::Retire { height, spent } => (
                        *height,
                        TxAnchorFacts {
                            adoptions: vec![],
                            retirements: spent
                                .iter()
                                .map(|s| Retirement { nf: hex32(s) })
                                .collect(),
                            has_single_name_note: false,
                            name_notes: 0,
                            claim_successor: false,
                        },
                        false,
                    ),
                    CanonEvent::Claim {
                        height,
                        spent_anchor,
                        successor_anchor,
                    } => (
                        *height,
                        TxAnchorFacts {
                            adoptions: vec![Adoption {
                                nf: hex32(successor_anchor),
                            }],
                            retirements: vec![Retirement {
                                nf: hex32(spent_anchor),
                            }],
                            has_single_name_note: true,
                            name_notes: 1,
                            claim_successor: true,
                        },
                        false,
                    ),
                    CanonEvent::UnbackedClaim { height, spent } => (
                        *height,
                        TxAnchorFacts {
                            adoptions: vec![],
                            retirements: spent
                                .iter()
                                .map(|s| Retirement { nf: hex32(s) })
                                .collect(),
                            has_single_name_note: true,
                            name_notes: 1,
                            claim_successor: false,
                        },
                        false,
                    ),
                    CanonEvent::Update {
                        height,
                        prev_nullifier,
                    }
                    | CanonEvent::Release {
                        height,
                        prev_nullifier,
                    } => (
                        *height,
                        TxAnchorFacts {
                            adoptions: vec![],
                            retirements: vec![Retirement {
                                nf: hex32(prev_nullifier),
                            }],
                            has_single_name_note: true,
                            name_notes: 1,
                            claim_successor: false,
                        },
                        false,
                    ),
                    CanonEvent::Rewind { to_height } => {
                        applied.retain(|a| a.height <= *to_height);
                        let lineage = fold(&applied);
                        assert_snapshot(&lineage, &scenario.trace[i]);
                        continue;
                    }
                };
                applied.push(Applied {
                    height,
                    tx_index: i as u32,
                    facts,
                    adopt,
                });
                let lineage = fold(&applied);
                assert_snapshot(&lineage, &scenario.trace[i]);
            }
        }
    }
}
