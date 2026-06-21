//! Registry Orchard decrypt and `cmx` binding verification.

use std::collections::HashMap;

use anyhow::{Context, Result};
use group::ff::PrimeField;
use orchard::keys::FullViewingKey;
use pasta_curves::pallas;
use seer_sync::chain::{self, LwdClient};
use seer_sync::proto::CompactBlock;
use seer_sync::{parse_orchard, BlockHeight, TxId};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BranchId, Parameters};
use zcash_protocol::memo::MemoBytes;
use zns_verify::{note_commitment_cmx, zns_psi_rcm, Action};

/// A registry Orchard note we decrypted — carries the note (for binding
/// verification) and memo (untrusted, parsed separately).
pub(crate) struct DecryptedNote {
    pub(crate) note: orchard::Note,
    /// On-chain note commitment (cmx) from the action; must match our recomputation.
    pub(crate) cmx: [u8; 32],
    pub(crate) memo: MemoBytes,
    pub(crate) txid: [u8; 32],
    pub(crate) height: u32,
    /// Index of this Orchard action within the transaction's action list.
    pub(crate) action_index: usize,
    pub(crate) raw_tx: Vec<u8>,
}

/// Returns `(psi, rcm)` byte reprs if the note commitment matches on-chain cmx.
pub(crate) fn verify_binding(
    note: &orchard::Note,
    on_chain_cmx: [u8; 32],
    action: Action,
    name: &str,
    ua: &str,
    prev_rcm: &[u8; 32],
) -> Option<([u8; 32], [u8; 32])> {
    let (g_d, pk_d) = note.recipient().zns_commitment_keys();
    let rho = pallas::Base::from_repr(note.rho().to_bytes()).into_option()?;
    let expected = pallas::Base::from_repr(on_chain_cmx).into_option()?;

    let (psi, rcm) = zns_psi_rcm(action.as_bytes(), name.as_bytes(), ua.as_bytes(), prev_rcm);
    let cmx = note_commitment_cmx(g_d, pk_d, note.value().inner(), rho, psi, rcm)?;
    (cmx == expected).then(|| (psi.to_repr(), rcm.to_repr()))
}

/// Compact-block batch: trial-decrypt candidates, fetch raw tx, full decrypt.
/// Uses the account's FullViewingKey (for both receive and send/OVK self-send proof).
pub(crate) async fn observe_batch(
    client: &mut LwdClient,
    network: &impl Parameters,
    fvk: &FullViewingKey,
    blocks: &[CompactBlock],
) -> Result<Vec<DecryptedNote>> {
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
                    let raw = chain::fetch_raw_transaction(client, &TxId::from_bytes(txid))
                        .await
                        .with_context(|| format!("fetch tx {}", hex::encode(txid)))?;
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
                let has_self_send_proof =
                    zns_verify::decrypt::try_decrypt_orchard_sent(action, fvk).is_some();
                if memo.as_slice().starts_with(b"ZNS:") && !has_self_send_proof {
                    tracing::debug!(
                        txid = %hex::encode(txid),
                        "ZNS name note candidate had incoming view but no matching OVK send proof — ignoring"
                    );
                    continue;
                }

                out.push(DecryptedNote {
                    note,
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
