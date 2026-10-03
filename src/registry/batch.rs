//! Transaction facts consumed by registry admission.

use orchard::note::Nullifier;
use orchard::note_encryption::CandidateNote;
use zcash_primitives::transaction::TxId;
use zcash_protocol::consensus::BlockHeight;

/// Only the transaction facts used by the registry's admission rules.
pub(crate) struct BatchTx {
    pub(crate) txid: TxId,
    pub(crate) height: BlockHeight,
    pub(crate) tx_index: u32,
    pub(crate) ironwood_outputs: Vec<BatchOutput>,
    pub(crate) ironwood_spends: Vec<BatchSpend>,
    pub(crate) relaxed_ironwood_outputs: Vec<BatchCandidate>,
}

pub(crate) struct BatchOutput {
    pub(crate) index: u32,
    pub(crate) value: u64,
    pub(crate) nf: Option<Nullifier>,
    pub(crate) is_sent: bool,
}

pub(crate) struct BatchSpend {
    pub(crate) index: u32,
    pub(crate) nf: Nullifier,
}

pub(crate) struct BatchCandidate {
    pub(crate) action_index: usize,
    pub(crate) note: CandidateNote,
    pub(crate) memo: Option<[u8; 512]>,
}
