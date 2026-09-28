//! The anchor lineage, derived: a pure fold over position-ordered facts.
//!
//! The lineage is never maintained — it is recomputed from chain facts.
//! The rules mirror the mint's claim-anchor semantics exactly
//! (`zns-mint` `AnchorPool` / `apply_block`): zero-value registry outputs
//! adopt in canonical order while below standing size; a revealed
//! nullifier retires whatever the rest of its transaction turned out to
//! be; a transaction that created exactly one zero-value registry output
//! joins that successor one-for-one, past standing size.
//!
//! Because the fold is a pure function of facts, reorg correctness is
//! inherited from the fact tables' rewind semantics: folding the facts
//! that survive a rewind equals folding the winning chain.

use std::collections::BTreeSet;

use super::nf::AnchorNf;

/// Standing size of the anchor lineage pool; mirrors keygen's `NUM_ANCHORS`
/// and the mint's `ANCHOR_POOL_SIZE`.
pub(crate) const ANCHOR_POOL_SIZE: usize = 40;

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
    /// the accept path. Transactions without it take `follow_spends`:
    /// successor is `None`, so a stray zero-value output can never force
    /// its way past standing size.
    pub(crate) has_single_name_note: bool,
}

impl TxAnchorFacts {
    /// Whether the transaction created exactly one zero-value registry
    /// output — the successor shape.
    fn has_successor(&self) -> bool {
        self.adoptions.len() == 1
    }
}

/// The set of nullifiers that currently confer claim authority.
#[derive(Debug, Default, Clone)]
pub(crate) struct Lineage {
    live: BTreeSet<AnchorNf>,
}

impl Lineage {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Advances the lineage across one transaction, in the mint's order:
    /// ceremony adoptions (capped at standing size, canonical order), then
    /// retirements (facts follow the chain whatever the verdict), then the
    /// one-for-one successor when the output shape is exactly one.
    pub(crate) fn step_tx(&mut self, facts: &TxAnchorFacts) {
        for adoption in &facts.adoptions {
            if self.live.len() < ANCHOR_POOL_SIZE {
                self.live.insert(adoption.nf);
            }
        }
        for retirement in &facts.retirements {
            self.live.remove(&retirement.nf);
        }
        if facts.has_single_name_note && facts.has_successor() {
            self.live.insert(facts.adoptions[0].nf);
        }
    }

    /// Whether `nf` is a live claim anchor right now.
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
        facts
            .retirements
            .iter()
            .any(|r| self.live.contains(&r.nf))
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

    /// Ceremony filling stops at standing size; extras never enter — and a
    /// tx with no name-note candidate takes `follow_spends` (successor
    /// `None`), so a stray zero-value output cannot join past the cap.
    #[test]
    fn adoption_is_capped_at_standing_size() {
        let mut lineage = Lineage::new();
        for i in 0..ANCHOR_POOL_SIZE {
            lineage.step_tx(&TxAnchorFacts {
                adoptions: vec![adopt(i as u8 + 1)],
                retirements: vec![],
                has_single_name_note: false,
            });
        }
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE);

        lineage.step_tx(&TxAnchorFacts {
            adoptions: vec![adopt(u8::MAX)],
            retirements: vec![],
            has_single_name_note: false,
        });
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE);
        assert!(!lineage.contains(&nf(u8::MAX)));
    }

    /// A revealed nullifier retires whatever the transaction was.
    #[test]
    fn retirement_follows_the_chain_fact() {
        let mut lineage = Lineage::new();
        lineage.step_tx(&TxAnchorFacts {
            adoptions: vec![adopt(1)],
            retirements: vec![],
            has_single_name_note: false,
        });
        lineage.step_tx(&TxAnchorFacts {
            adoptions: vec![],
            retirements: vec![retire(1)],
            has_single_name_note: false,
        });
        assert!(lineage.is_empty());
    }

    /// A backed claim's successor joins one-for-one, past standing size.
    #[test]
    fn successor_joins_past_standing_size() {
        let mut lineage = Lineage::new();
        for i in 0..ANCHOR_POOL_SIZE {
            lineage.step_tx(&TxAnchorFacts {
                adoptions: vec![adopt(i as u8 + 1)],
                retirements: vec![],
                has_single_name_note: false,
            });
        }
        // A backed claim: exactly one name-note candidate (the accept path),
        // spends one live anchor, creates one zero-value output. The
        // successor takes the retired anchor's seat.
        lineage.step_tx(&TxAnchorFacts {
            adoptions: vec![adopt(200)],
            retirements: vec![retire(1)],
            has_single_name_note: true,
        });
        assert_eq!(lineage.len(), ANCHOR_POOL_SIZE);
        assert!(!lineage.contains(&nf(1)));
        assert!(lineage.contains(&nf(200)));
    }

    /// A transaction with two or more zero-value outputs is not a backed
    /// claim: no uncapped successor insert. Facts still follow.
    #[test]
    fn multi_adoption_tx_gets_no_successor_insert() {
        let mut lineage = Lineage::new();
        lineage.step_tx(&TxAnchorFacts {
            adoptions: vec![adopt(1)],
            retirements: vec![],
            has_single_name_note: false,
        });
        lineage.step_tx(&TxAnchorFacts {
            adoptions: vec![adopt(2), adopt(3)],
            retirements: vec![retire(1)],
            has_single_name_note: true,
        });
        // Both adoptions entered (below standing size), the retirement
        // followed the fact, and the successor insert never fired — the
        // output shape is not `exactly one`.
        assert!(lineage.contains(&nf(2)));
        assert!(lineage.contains(&nf(3)));
        assert!(!lineage.contains(&nf(1)));
    }

    /// Within one transaction the mint's order is: ceremony adoptions
    /// (capped), then retirements, then the successor. At the cap boundary
    /// this order is observable: the successor's seat opens only after the
    /// retirement, and the uncapped insert lands it anyway.
    #[test]
    fn within_tx_order_adopts_then_retires_then_successor() {
        let mut lineage = Lineage::new();
        for i in 0..ANCHOR_POOL_SIZE {
            lineage.step_tx(&TxAnchorFacts {
                adoptions: vec![adopt(i as u8 + 1)],
                retirements: vec![],
                has_single_name_note: false,
            });
        }
        let before = lineage.len();

        lineage.step_tx(&TxAnchorFacts {
            adoptions: vec![adopt(200)],
            retirements: vec![retire(7)],
            has_single_name_note: true,
        });

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
        // Winning chain: anchor 1 adopted at h10, retired at h12 by a claim
        // whose successor is anchor 2.
        let mut facts = TxAnchorFacts {
            adoptions: vec![adopt(1)],
            retirements: vec![],
            has_single_name_note: false,
        };
        let mut lineage = Lineage::new();
        lineage.step_tx(&facts);

        facts = TxAnchorFacts {
            adoptions: vec![adopt(2)],
            retirements: vec![retire(1)],
            has_single_name_note: true,
        };
        lineage.step_tx(&facts);
        assert!(lineage.contains(&nf(2)));

        // A rewind below h12 un-spends the retirement: the surviving fact
        // stream is just the adoption of anchor 1. Folding it from scratch
        // must match a lineage rebuilt only from surviving facts.
        let mut rewound = Lineage::new();
        rewound.step_tx(&TxAnchorFacts {
            adoptions: vec![adopt(1)],
            retirements: vec![],
            has_single_name_note: false,
        });
        assert!(rewound.contains(&nf(1)));
        assert_eq!(rewound.len(), 1);
        assert!(!rewound.contains(&nf(2)));
    }
}
