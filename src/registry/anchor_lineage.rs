//! The anchor lineage, derived: a pure fold over position-ordered facts.
//!
//! The lineage is never maintained — it is recomputed from chain facts.
//! The starting anchors are the zero-value registry notes received in the
//! birthday block: the mint's anchor transaction. A zero-value note in
//! any later block joins only as an admitted claim's successor. A revealed
//! nullifier retires whatever the rest of its transaction turned out to be.
//!
//! Because the fold is a pure function of facts, reorg correctness is
//! inherited from the fact tables' rewind semantics: folding the facts
//! that survive a rewind equals folding the winning chain.

use std::collections::BTreeSet;

use super::nf::AnchorNf;

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
/// retirements are grouped per transaction: a transaction's retirements
/// land before its claim successor joins.
#[derive(Debug, Default)]
pub(crate) struct TxAnchorFacts {
    pub(crate) adoptions: Vec<Adoption>,
    pub(crate) retirements: Vec<Retirement>,
    /// Name notes in this transaction.
    pub(crate) name_notes: usize,
    /// The resolver admits this claim's successor. The output shape is one
    /// zero-value note, at least one live anchor retired, the transaction
    /// spent no name note, and the name was free. A duplicate on a live
    /// name, or a claim that also spends a name note, leaves this false:
    /// the anchor still retires, and the successor stays out.
    pub(crate) claim_successor: bool,
}

/// The set of nullifiers that currently confer claim authority, and the
/// birthday block whose zero-value registry notes started it.
#[derive(Debug, Clone)]
pub(crate) struct Lineage {
    live: BTreeSet<AnchorNf>,
    birthday: u32,
}

impl Lineage {
    pub(crate) fn new(birthday: u32) -> Self {
        Self {
            live: BTreeSet::new(),
            birthday,
        }
    }

    /// Whether a zero-value registry note at `height` is an anchor fact: a
    /// starting anchor in the birthday block, or a claim's successor. The
    /// scan and the in-memory batch use the same check, so a replay of
    /// stored facts matches the batch that wrote them.
    pub(crate) fn keeps_anchor_note(&self, height: u32, claim_successor: bool) -> bool {
        claim_successor || height == self.birthday
    }

    /// Advances the lineage across one transaction: the birthday block's
    /// notes, then retirements (facts follow the chain whatever the verdict),
    /// then the admitted claim's successor.
    pub(crate) fn step_tx(&mut self, height: u32, facts: &TxAnchorFacts) {
        if height == self.birthday {
            for adoption in &facts.adoptions {
                self.live.insert(adoption.nf);
            }
        }

        for retirement in &facts.retirements {
            self.live.remove(&retirement.nf);
        }

        // An admitted claim's successor joins. Admission already required a
        // live anchor spent, so authority is only ever succeeded.
        if facts.claim_successor && facts.adoptions.len() == 1 {
            self.live.insert(facts.adoptions[0].nf);
        }
    }

    /// Whether `nf` is a live claim anchor right now. Admission uses
    /// [`Self::spends_live_anchor`]; this query is for the lineage tests.
    #[cfg(test)]
    pub(crate) fn contains(&self, nf: &AnchorNf) -> bool {
        self.live.contains(nf)
    }

    /// Whether the transaction spends a live anchor: a registry note only the
    /// mint can spend, so a claim's proof that the mint made it.
    pub(crate) fn spends_live_anchor(&self, facts: &TxAnchorFacts) -> bool {
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

    /// The birthday block the tests' lineages start from.
    const BIRTHDAY: u32 = 1;

    /// How many starting anchors the tests' birthday block holds.
    const STARTING_ANCHORS: u8 = 40;

    fn zero_value_notes(seeds: impl IntoIterator<Item = u8>) -> TxAnchorFacts {
        TxAnchorFacts {
            adoptions: seeds.into_iter().map(adopt).collect(),
            retirements: vec![],
            name_notes: 0,
            claim_successor: false,
        }
    }

    /// A lineage started from the birthday block's anchors `1..=STARTING_ANCHORS`.
    fn started() -> Lineage {
        let mut lineage = Lineage::new(BIRTHDAY);
        lineage.step_tx(BIRTHDAY, &zero_value_notes(1..=STARTING_ANCHORS));
        lineage
    }

    /// The birthday block's zero-value notes are the starting anchors,
    /// however many there are. A zero-value note in any later block joins
    /// only as a claim's successor.
    #[test]
    fn only_the_birthday_block_starts_the_pool() {
        let lineage = Lineage::new(BIRTHDAY);
        assert!(lineage.keeps_anchor_note(BIRTHDAY, false));
        assert!(!lineage.keeps_anchor_note(BIRTHDAY + 1, false));
        assert!(lineage.keeps_anchor_note(BIRTHDAY + 1, true));

        let mut lineage = started();
        assert_eq!(lineage.len(), usize::from(STARTING_ANCHORS));
        lineage.step_tx(BIRTHDAY + 1, &zero_value_notes(101..=140));
        assert_eq!(lineage.len(), usize::from(STARTING_ANCHORS));
        assert!(!lineage.contains(&nf(101)));
    }

    /// A revealed nullifier retires whatever the transaction was.
    #[test]
    fn retirement_follows_the_chain_fact() {
        let mut lineage = Lineage::new(BIRTHDAY);
        lineage.step_tx(BIRTHDAY, &zero_value_notes([1]));
        lineage.step_tx(
            BIRTHDAY + 1,
            &TxAnchorFacts {
                adoptions: vec![],
                retirements: vec![retire(1)],
                name_notes: 0,
                claim_successor: false,
            },
        );
        assert!(lineage.is_empty());
    }

    /// An admitted claim's successor takes the place of the anchor it spent.
    #[test]
    fn a_claims_successor_joins() {
        let mut lineage = started();
        lineage.step_tx(
            BIRTHDAY + 1,
            &TxAnchorFacts {
                adoptions: vec![adopt(200)],
                retirements: vec![retire(1)],
                name_notes: 1,
                claim_successor: true,
            },
        );
        assert_eq!(lineage.len(), usize::from(STARTING_ANCHORS));
        assert!(!lineage.contains(&nf(1)));
        assert!(lineage.contains(&nf(200)));
    }

    /// After the birthday block, a zero-value note seats only as a claim's
    /// successor. An update or release that spends one anchor, and a claim
    /// with a second registry output, are both stored with the flag clear.
    #[test]
    fn a_note_without_the_claim_shape_does_not_seat() {
        let mut lineage = started();
        lineage.step_tx(
            BIRTHDAY + 1,
            &TxAnchorFacts {
                adoptions: vec![adopt(200)],
                retirements: vec![retire(1)],
                name_notes: 1,
                claim_successor: false,
            },
        );
        assert!(!lineage.contains(&nf(200)));
        assert!(!lineage.contains(&nf(1)));
        assert_eq!(lineage.len(), usize::from(STARTING_ANCHORS) - 1);
    }

    /// Two zero-value outputs after the birthday block are not a claim's
    /// successor, so neither joins. The retirement still follows the chain.
    #[test]
    fn multi_adoption_tx_gets_no_successor_insert() {
        let mut lineage = Lineage::new(BIRTHDAY);
        lineage.step_tx(BIRTHDAY, &zero_value_notes([1]));
        lineage.step_tx(
            BIRTHDAY + 1,
            &TxAnchorFacts {
                adoptions: vec![adopt(2), adopt(3)],
                retirements: vec![retire(1)],
                name_notes: 1,
                claim_successor: false,
            },
        );
        assert!(!lineage.contains(&nf(2)));
        assert!(!lineage.contains(&nf(3)));
        assert!(!lineage.contains(&nf(1)));
        assert!(lineage.is_empty());
    }

    /// Within one transaction, the spent anchor retires before the successor
    /// joins, so a claim that spends one anchor keeps the pool's size.
    #[test]
    fn within_tx_order_retires_then_successor() {
        let mut lineage = started();
        let before = lineage.len();

        lineage.step_tx(
            BIRTHDAY + 1,
            &TxAnchorFacts {
                adoptions: vec![adopt(200)],
                retirements: vec![retire(7)],
                name_notes: 1,
                claim_successor: true,
            },
        );

        assert_eq!(lineage.len(), before);
        assert!(lineage.contains(&nf(200)));
        assert!(!lineage.contains(&nf(7)));
    }

    /// The reorg property: folding the facts that survive a rewind equals
    /// folding the winning chain from scratch. Rewind deletes facts above
    /// the fork and un-spends spends above it; the fold needs no code of
    /// its own to be reorg-correct.
    #[test]
    fn fold_over_rewound_facts_equals_fold_over_winning_chain() {
        // Winning chain: anchor 1 adopted at the birthday h10, retired at h12
        // by a claim whose successor is anchor 2.
        let mut lineage = Lineage::new(10);
        lineage.step_tx(10, &zero_value_notes([1]));

        let facts = TxAnchorFacts {
            adoptions: vec![adopt(2)],
            retirements: vec![retire(1)],
            name_notes: 1,
            claim_successor: true,
        };
        lineage.step_tx(12, &facts);
        assert!(lineage.contains(&nf(2)));

        // A rewind below h12 un-spends the retirement: the surviving fact
        // stream is just the adoption of anchor 1. Folding it from scratch
        // must match a lineage rebuilt only from surviving facts.
        let mut rewound = Lineage::new(10);
        rewound.step_tx(10, &zero_value_notes([1]));
        assert!(rewound.contains(&nf(1)));
        assert_eq!(rewound.len(), 1);
        assert!(!rewound.contains(&nf(2)));
    }
}

/// The resolver folds the same chain facts the mint folds; the vendored
/// canon fixture is the executable cross-repo contract. Every scenario whose
/// anchors are adopted in one block is replayed into the fold, with that block
/// as the birthday; the live set must equal the mint's recorded snapshot after
/// every event. The two ceremony scenarios exercise the mint's adoption cap,
/// which the resolver does not have: it takes the birthday block's anchors.
#[cfg(test)]
mod canon {
    use super::*;
    use serde::Deserialize;

    const FIXTURE: &str = include_str!("../../tests/fixtures/canon-vectors-v1.json");

    /// Scenarios that adopt anchors across several blocks and cap the pool.
    const CEREMONY_SCENARIOS: [&str; 2] = ["ceremony_closes_once", "ceremony_fill"];

    #[derive(Deserialize)]
    struct Fixture {
        version: String,
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
            expect: String,
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
    }

    fn fold(birthday: u32, applied: &[Applied]) -> Lineage {
        let mut order: Vec<&Applied> = applied.iter().collect();
        order.sort_by_key(|a| (a.height, a.tx_index));
        let mut lineage = Lineage::new(birthday);
        for applied in order {
            lineage.step_tx(applied.height, &applied.facts);
        }
        lineage
    }

    fn assert_snapshot(lineage: &Lineage, trace: &Trace) {
        assert_eq!(lineage.len(), trace.anchor_pool.len(), "pool size");
        for hex in &trace.anchor_pool {
            assert!(lineage.contains(&hex32(hex)), "missing anchor {hex}");
        }
    }

    #[test]
    fn canon_vectors_pin_the_fold() {
        let fixture: Fixture = serde_json::from_str(FIXTURE).expect("fixture parses");
        assert_eq!(fixture.version, "canon-vectors-v1");

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

        for scenario in &fixture.scenarios {
            if CEREMONY_SCENARIOS.contains(&scenario.name.as_str()) {
                continue;
            }
            // The scenario's anchors are adopted in one block: its birthday.
            let birthday = scenario
                .events
                .iter()
                .find_map(|event| match event {
                    CanonEvent::AdoptAnchor { height, .. } => Some(*height),
                    _ => None,
                })
                .expect("a scenario adopts its starting anchors");
            let mut applied: Vec<Applied> = Vec::new();
            for (i, event) in scenario.events.iter().enumerate() {
                let (height, facts) = match event {
                    // The fixture calls the mint's adopt() once per note, all
                    // in the birthday block, where step_tx takes them.
                    CanonEvent::AdoptAnchor { height, nullifier } => (
                        *height,
                        TxAnchorFacts {
                            adoptions: vec![Adoption {
                                nf: hex32(nullifier),
                            }],
                            retirements: vec![],
                            name_notes: 0,
                            claim_successor: false,
                        },
                    ),
                    CanonEvent::Retire { height, spent } => (
                        *height,
                        TxAnchorFacts {
                            adoptions: vec![],
                            retirements: spent
                                .iter()
                                .map(|s| Retirement { nf: hex32(s) })
                                .collect(),
                            name_notes: 0,
                            claim_successor: false,
                        },
                    ),
                    CanonEvent::Claim {
                        height,
                        spent_anchor,
                        successor_anchor,
                        expect,
                    } => (
                        *height,
                        TxAnchorFacts {
                            adoptions: vec![Adoption {
                                nf: hex32(successor_anchor),
                            }],
                            retirements: vec![Retirement {
                                nf: hex32(spent_anchor),
                            }],
                            name_notes: 1,
                            // A rejected claim retires its anchor and leaves
                            // the successor out. The fixture records that
                            // verdict; the scan computes the same one.
                            claim_successor: expect == "accepted",
                        },
                    ),
                    CanonEvent::UnbackedClaim { height, spent } => (
                        *height,
                        TxAnchorFacts {
                            adoptions: vec![],
                            retirements: spent
                                .iter()
                                .map(|s| Retirement { nf: hex32(s) })
                                .collect(),
                            name_notes: 1,
                            claim_successor: false,
                        },
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
                            name_notes: 1,
                            claim_successor: false,
                        },
                    ),
                    CanonEvent::Rewind { to_height } => {
                        applied.retain(|a| a.height <= *to_height);
                        let lineage = fold(birthday, &applied);
                        assert_snapshot(&lineage, &scenario.trace[i]);
                        continue;
                    }
                };
                applied.push(Applied {
                    height,
                    tx_index: i as u32,
                    facts,
                });
                let lineage = fold(birthday, &applied);
                assert_snapshot(&lineage, &scenario.trace[i]);
            }
        }
    }
}
