//! Resolver-owned facts retained between seer-sync's scan callbacks.

use orchard::note::Nullifier;
use orchard::note_encryption::CandidateNote;
use seer_sync::sync::scan::WalletTx;
use seer_sync::Cursor;
use zcash_primitives::transaction::TxId;
use zcash_protocol::consensus::BlockHeight;

/// The one scan batch waiting for its compact-block timestamps.
pub(crate) struct ScannedBatch {
    pub(crate) at: Cursor,
    pub(crate) transactions: Vec<ScannedTx>,
}

/// Only the transaction facts used by the registry's admission rules.
pub(crate) struct ScannedTx {
    pub(crate) txid: TxId,
    pub(crate) height: BlockHeight,
    pub(crate) tx_index: u32,
    pub(crate) ironwood_outputs: Vec<ScannedOutput>,
    pub(crate) ironwood_spends: Vec<ScannedSpend>,
    pub(crate) relaxed_ironwood_outputs: Vec<ScannedCandidate>,
}

pub(crate) struct ScannedOutput {
    pub(crate) index: u32,
    pub(crate) value: u64,
    pub(crate) nf: Option<Nullifier>,
    pub(crate) is_sent: bool,
}

pub(crate) struct ScannedSpend {
    pub(crate) index: u32,
    pub(crate) nf: Nullifier,
}

pub(crate) struct ScannedCandidate {
    pub(crate) action_index: usize,
    pub(crate) note: CandidateNote,
    pub(crate) memo: Option<[u8; 512]>,
}

impl ScannedTx {
    pub(crate) fn from_wallet(tx: &WalletTx) -> Self {
        Self {
            txid: tx.txid,
            height: tx.height,
            tx_index: tx.tx_index,
            ironwood_outputs: tx
                .ironwood_outputs
                .iter()
                .map(|output| ScannedOutput {
                    index: output.index,
                    value: output.note.value().inner(),
                    nf: output.nf,
                    is_sent: output.is_sent,
                })
                .collect(),
            ironwood_spends: tx
                .ironwood_spends
                .iter()
                .map(|spend| ScannedSpend {
                    index: spend.index,
                    nf: spend.nf,
                })
                .collect(),
            relaxed_ironwood_outputs: tx
                .relaxed_ironwood_outputs
                .iter()
                .map(|(action_index, note, _, memo, _)| ScannedCandidate {
                    action_index: *action_index,
                    note: *note,
                    memo: *memo,
                })
                .collect(),
        }
    }
}
