//! Project seer-sync wallet transactions into registry admission facts.

use seer_sync::sync::scan::WalletTx;

use crate::registry::batch::{BatchCandidate, BatchOutput, BatchSpend, BatchTx};

pub(super) fn project_tx(tx: &WalletTx) -> BatchTx {
    BatchTx {
        txid: tx.txid,
        height: tx.height,
        tx_index: tx.tx_index,
        ironwood_outputs: tx
            .ironwood_outputs
            .iter()
            .map(|output| BatchOutput {
                index: output.index,
                value: output.note.value().inner(),
                nf: output.nf,
                is_sent: output.is_sent,
            })
            .collect(),
        ironwood_spends: tx
            .ironwood_spends
            .iter()
            .map(|spend| BatchSpend {
                index: spend.index,
                nf: spend.nf,
            })
            .collect(),
        relaxed_ironwood_outputs: tx
            .relaxed_ironwood_outputs
            .iter()
            .map(|(action_index, note, _, memo, _)| BatchCandidate {
                action_index: *action_index,
                note: *note,
                memo: *memo,
            })
            .collect(),
    }
}
