//! Observes the ZNS registry account on chain.
//!
//! Turns batches of compact blocks into candidate decrypted notes
//! (trial decrypt + full tx fetch + OVK self-send proof).
//!
//! Binding verification and admission logic lives in `registry::notes`.

use std::collections::HashMap;

use orchard::keys::FullViewingKey;
use seer_sync::chain::ChainError;
use seer_sync::proto::CompactBlock;
use seer_sync::{parse_orchard, BlockHeight, TxId};

use crate::sync::chain::Connection;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BranchId, Parameters};
use zcash_protocol::memo::MemoBytes;

/// A decrypted ZNS name note candidate.
///
/// We immediately extract the raw `(g_d, pk_d, rho, value)` material from the
/// `orchard::Note` (using the ZNS-patched orchard) so that binding verification
/// can be done directly against `zns_verify` without threading the full note
/// object through the rest of the system.
pub(crate) struct DecryptedNote {
    /// Raw inputs required by `zns_verify` to recompute the note commitment.
    pub(crate) g_d: [u8; 32],
    pub(crate) pk_d: [u8; 32],
    pub(crate) rho: [u8; 32],
    pub(crate) value: u64,

    /// On-chain note commitment (cmx) observed in the compact block.
    pub(crate) cmx: [u8; 32],
    pub(crate) memo: MemoBytes,
    pub(crate) txid: [u8; 32],
    pub(crate) height: u32,
    /// Index of this Orchard action within the transaction's action list.
    pub(crate) action_index: usize,
    pub(crate) raw_tx: Vec<u8>,
}

/// Compact-block batch: trial-decrypt candidates, fetch raw tx, full decrypt.
/// Uses the account's FullViewingKey (for both receive and send/OVK self-send proof).
///
/// The `fvk` here must be derived from a full viewing key (UFVK). An incoming
/// viewing key is not sufficient for the mandatory OVK self-send check on ZNS memos.
pub(crate) async fn observe_batch(
    conn: &Connection,
    network: &impl Parameters,
    fvk: &FullViewingKey,
    blocks: &[CompactBlock],
) -> Result<Vec<DecryptedNote>, ChainError> {
    type Fetched = Option<(Transaction, u32, Vec<u8>)>;
    let mut fetched: HashMap<[u8; 32], Fetched> = HashMap::new();
    let mut out = Vec::new();

    for block in blocks {
        for tx in &block.vtx {
            let Ok(txid) = tx.txid[..].try_into() else {
                continue;
            };
            for (action_index, act) in tx.actions.iter().enumerate() {
                let Some(action) = parse_orchard(act) else {
                    continue;
                };
                if zns_verify::decrypt::try_compact_orchard(fvk, &action).is_none() {
                    continue;
                }

                if let std::collections::hash_map::Entry::Vacant(e) = fetched.entry(txid) {
                    let raw = conn
                        .fetch_raw_transaction(&TxId::from_bytes(txid))
                        .await
                        .map_err(|e| {
                            tracing::warn!(
                                txid = %hex::encode(txid),
                                error = %e,
                                "fetch raw transaction failed"
                            );
                            e
                        })?;
                    let height = raw.height as u32;
                    let parsed = Transaction::read(
                        &raw.data[..],
                        BranchId::for_height(network, BlockHeight::from_u32(height)),
                    )
                    .ok()
                    .map(|tx| (tx, height, raw.data));
                    e.insert(parsed);
                }

                let Some((parsed_tx, height, raw)) = fetched.get(&txid).and_then(|o| o.as_ref())
                else {
                    continue;
                };
                let Some(bundle) = parsed_tx.orchard_bundle() else {
                    continue;
                };
                let Some(action) = bundle.actions().get(action_index) else {
                    continue;
                };
                let Some((note, _recipient, memo)) =
                    zns_verify::decrypt::try_decrypt_orchard(action, fvk)
                else {
                    continue;
                };

                // Self-send proof using the FVK (OVK side). For ZNS name notes
                // (which must be 0-value self-sends), we require a matching send
                // recovery with the same memo.
                //
                // This only works if `fvk` came from a *full* viewing key (UFVK),
                // not a UIVK. The registry account must be configured with a UFVK.
                let has_self_send_proof =
                    zns_verify::decrypt::try_decrypt_orchard_sent(action, fvk).is_some();
                if memo.as_slice().starts_with(b"ZNS:") && !has_self_send_proof {
                    tracing::debug!(
                        txid = %hex::encode(txid),
                        "ZNS name note candidate had incoming view but no matching OVK send proof — ignoring"
                    );
                    continue;
                }

                // Extract the raw commitment inputs once. This lets us delegate
                // the actual binding check to zns-verify without threading the
                // full orchard::Note through the rest of the pipeline.
                let (g_d, pk_d) = note.recipient().zns_commitment_keys();
                let rho = note.rho().to_bytes();
                let value = note.value().inner();

                out.push(DecryptedNote {
                    g_d,
                    pk_d,
                    rho,
                    value,
                    cmx: action.cmx().to_bytes(),
                    memo,
                    txid,
                    height: *height,
                    action_index,
                    raw_tx: raw.clone(),
                });
            }
        }
    }

    Ok(out)
}
