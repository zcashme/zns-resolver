//! Nullifier roles. Anchors and name-chain tips are both registry notes
//! with standard nullifiers, but they confer different authority: an anchor
//! backs a claim, a tip backs its name's next transition. Each role gets its
//! own type; conversions happen only at the scan boundary.

/// A revealed nullifier of a registry note in the anchor lineage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct AnchorNf([u8; 32]);

impl AnchorNf {
    /// Adopts a nullifier from the scan.
    pub(crate) fn from_scan(nf: orchard::note::Nullifier) -> Self {
        Self(nf.to_bytes())
    }

    /// Reads a nullifier back from the fact table.
    pub(crate) fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self(*bytes)
    }

    /// The bytes as revealed on chain and stored in the fact table.
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A name-chain tip nullifier: the nullifier of a name's admitted note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TipNf([u8; 32]);

impl TipNf {
    /// Adopts the admitted note's nullifier from the scan.
    pub(crate) fn from_scan(nf: orchard::note::Nullifier) -> Self {
        Self(nf.to_bytes())
    }

    /// Reads a nullifier back from the names table.
    pub(crate) fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self(*bytes)
    }

    /// A revealed spend, read as a name-chain nullifier. The same bytes
    /// may also be an [`AnchorNf`]; the two types do not compare.
    pub(crate) fn from_revealed(anchor: &AnchorNf) -> Self {
        Self(*anchor.as_bytes())
    }

    /// The bytes stored for this tip.
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
