//! Nullifier provenance. Two nullifier families share one byte
//! representation and must never be compared: the anchor lineage (standard
//! derivation, ordinary-lane notes) and name-chain tips (`zns_nullifier`,
//! relaxed-lane notes). Each family gets its own type; conversions happen
//! only at the scan boundary.

/// A revealed nullifier of an ordinary-lane registry note — the anchor
/// lineage.
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

/// A name-chain tip nullifier: `zns_nullifier` on a relaxed-lane note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TipNf([u8; 32]);

impl TipNf {
    /// Adopts the admission nullifier from `zns_nullifier`.
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
