//! The chain-observer scan loop.
//!
//! seer-sync's standard engine applies the ZIP-212 commitment check and so
//! drops Name Notes; instead of asking it to relax that, the resolver drives
//! its own loop over seer-sync's *toolkit* — the block stream, action parsing,
//! and commitment firehose — and supplies the relaxed decrypt itself from
//! `zns-verify`. Per batch of blocks:
//!
//! 1. trial-decrypt every Orchard action with the relaxed compact path (cheap,
//!    no fetch) to find candidates addressed to `ivk`;
//! 2. fetch each candidate's full transaction and relaxed-decrypt it — the AEAD
//!    tag authenticates the note and yields the memo compact blocks truncate;
//! 3. hand the verified-decrypt candidates to the index, which checks each
//!    binding's `cmx` and folds it into the name log.
//!
//! A `Reorg` from the stream rolls the index back and re-scans forward.

use std::collections::HashMap;

use anyhow::Result;
use futures::StreamExt;
use orchard::keys::PreparedIncomingViewingKey;
use seer_sync::sync::chain::{self, ChainError, LwdClient, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::{parse_orchard, CompactBlock};
use seer_sync::BlockHeight;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BranchId, Network};

use crate::index::{Cursor, NameNote, SqliteIndex};

/// Scan from the index's checkpoint (or `birthday` if empty) to the chain tip,
/// applying every verified Name Note found.
pub async fn sync_to_tip(
    index: &SqliteIndex,
    client: LwdClient,
    network: &Network,
    ivk: &PreparedIncomingViewingKey,
    birthday: u32,
) -> Result<()> {
    let mut fetch_client = client.clone();
    let tip = chain::tip_height(&mut fetch_client).await?;

    let mut rewind_by: u32 = 1;
    loop {
        let (start, seam) = match index.checkpoint()? {
            Some(c) => (u32::from(c.height) + 1, c.hash),
            None => (birthday, None),
        };
        if start > tip {
            return Ok(());
        }

        let mut stream = chain::blocks(client.clone(), start, tip, DEFAULT_CHUNK_OUTPUTS, seam);
        loop {
            match stream.next().await {
                None => return Ok(()),
                Some(Ok(batch)) => {
                    let Some(last) = batch.last() else { continue };
                    let at = Cursor {
                        height: BlockHeight::from_u32(last.height as u32),
                        // A malformed hash becomes `None` (skip the next seam
                        // check), never a bogus value that would fake a reorg.
                        hash: last.hash[..].try_into().ok(),
                    };

                    let candidates = scan_name_notes(&batch, ivk);
                    let notes = recover_notes(&mut fetch_client, network, ivk, candidates).await?;
                    index.apply_notes(at, &notes)?;
                    rewind_by = 1;
                }
                Some(Err(ChainError::Reorg(at))) => {
                    index.rewind(BlockHeight::from_u32(at.saturating_sub(rewind_by)))?;
                    rewind_by = rewind_by.saturating_mul(2);
                    break;
                }
                Some(Err(e)) => return Err(e.into()),
            }
        }
    }
}

/// A compact-block hit: which action in which transaction decrypted to `ivk`.
struct Candidate {
    txid: [u8; 32],
    action_index: usize,
}

/// Relaxed-decrypt every Orchard action in `blocks`, collecting those addressed
/// to `ivk`. The compact path carries no AEAD tag, so these are candidates —
/// authentication and the memo come from [`recover_notes`].
fn scan_name_notes(blocks: &[CompactBlock], ivk: &PreparedIncomingViewingKey) -> Vec<Candidate> {
    let mut out = Vec::new();
    for block in blocks {
        for tx in &block.vtx {
            let Ok(txid) = tx.txid[..].try_into() else { continue };
            for (action_index, act) in tx.actions.iter().enumerate() {
                let Some(action) = parse_orchard(act) else { continue };
                if zns_verify::decrypt::try_compact_orchard(ivk, &action).is_some() {
                    out.push(Candidate { txid, action_index });
                }
            }
        }
    }
    out
}

/// Fetch each candidate's full transaction and relaxed-decrypt it, recovering
/// the authenticated note + memo. Candidates whose full ciphertext fails the
/// AEAD tag (compact-path false positives) are dropped here.
///
/// Notes are returned in `candidates` order — the chain order
/// (`height, tx_index, action_index`) [`scan_name_notes`] found them in.
/// [`SqliteIndex::apply_notes`](crate::index::SqliteIndex::apply_notes) folds
/// actions sequentially against each name's tip, so reordering (e.g. by txid)
/// would silently drop a same-batch UPDATE that follows its CLAIM and change
/// who wins a same-batch claim race.
async fn recover_notes(
    client: &mut LwdClient,
    network: &Network,
    ivk: &PreparedIncomingViewingKey,
    candidates: Vec<Candidate>,
) -> Result<Vec<NameNote>> {
    // Each full transaction is fetched and parsed once; `None` caches a parse
    // failure so it isn't refetched for its remaining candidates.
    let mut fetched: HashMap<[u8; 32], Option<(Transaction, u32)>> = HashMap::new();

    let mut out = Vec::new();
    for c in candidates {
        if let std::collections::hash_map::Entry::Vacant(e) = fetched.entry(c.txid) {
            let raw = chain::fetch_raw_transaction(client, &c.txid).await?;
            let height = raw.height as u32;
            let parsed = Transaction::read(
                &raw.data[..],
                BranchId::for_height(network, BlockHeight::from_u32(height)),
            )
            .ok()
            .map(|tx| (tx, height));
            e.insert(parsed);
        }
        let Some((tx, height)) = fetched.get(&c.txid).and_then(|o| o.as_ref()) else { continue };
        let Some(bundle) = tx.orchard_bundle() else { continue };
        let Some(action) = bundle.actions().get(c.action_index) else { continue };
        let Some((note, _recipient, memo)) = zns_verify::decrypt::try_decrypt_orchard(action, ivk)
        else {
            continue;
        };
        out.push(NameNote { note, cmx: action.cmx().to_bytes(), memo, txid: c.txid, height: *height });
    }
    Ok(out)
}

/// Derive the external Orchard incoming viewing key from a unified viewing key
/// encoding (`uivk1…` or a UFVK). Name Notes are encrypted to this key.
pub fn orchard_ivk(network: &Network, encoding: &str) -> Result<PreparedIncomingViewingKey> {
    use orchard::keys::Scope;
    use zcash_keys::keys::{UnifiedFullViewingKey, UnifiedIncomingViewingKey};

    if let Ok(ufvk) = UnifiedFullViewingKey::decode(network, encoding) {
        if let Some(fvk) = ufvk.orchard() {
            return Ok(PreparedIncomingViewingKey::new(&fvk.to_ivk(Scope::External)));
        }
    }
    if let Ok(uivk) = UnifiedIncomingViewingKey::decode(network, encoding) {
        if let Some(ivk) = uivk.orchard() {
            return Ok(PreparedIncomingViewingKey::new(ivk));
        }
    }
    anyhow::bail!("no Orchard incoming viewing key found in the provided key")
}
