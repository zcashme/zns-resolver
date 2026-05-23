//! The `ScanCallback` implementation that bridges seer-sync's generic
//! decrypt loop into our ZNS-specific verification.

use seer_sync::scan::ScanCallback;

use crate::verify::{verify_zns_action, ValidZnsAction};

/// `ScanCallback` that accepts only AEAD decrypts whose memo encodes a
/// canonical ZNS binding that cryptographically reconstructs the action's
/// on-chain `cmx`.
pub struct ZnsCallback;

impl ScanCallback for ZnsCallback {
    type Hit = ZnsHit;

    fn on_decrypt<A>(
        &mut self,
        action: &orchard::Action<A>,
        recipient: &orchard::Address,
        memo: &[u8; seer_sync::decrypt::MEMO_SIZE],
    ) -> Option<Self::Hit> {
        let valid = verify_zns_action(action, recipient, memo)?;
        // Also remember the prev_rcm encoded in the memo, since the chain-
        // advance check needs it independently of the binding check.
        let prev_rcm = parse_prev_rcm_from_memo(memo)?;
        Some(ZnsHit { valid, prev_rcm })
    }
}

/// One accepted ZNS hit: the verified action plus the `prev_rcm` value its
/// memo claimed to point back to. The index layer uses `prev_rcm` to decide
/// whether this action validly extends the name's chain.
#[derive(Debug, Clone)]
pub struct ZnsHit {
    /// The verified action.
    pub valid: ValidZnsAction,
    /// The `prev_rcm` value encoded in the memo, used as the chain-advance
    /// key against the previously-stored entry's `rcm`.
    pub prev_rcm: [u8; 32],
}

fn parse_prev_rcm_from_memo(memo: &[u8; seer_sync::decrypt::MEMO_SIZE]) -> Option<[u8; 32]> {
    let end = memo.iter().rposition(|b| *b != 0).map(|p| p + 1).unwrap_or(0);
    let s = std::str::from_utf8(&memo[..end]).ok()?;
    let parsed = zns_verify::Memo::parse(s)?;
    Some(parsed.prev_rcm)
}
