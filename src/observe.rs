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
use seer_sync::chain::{self, ChainError, LwdClient, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::proto::CompactBlock;
use seer_sync::{parse_orchard, BlockHash, BlockHeight, TxId};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BranchId, Parameters};

use crate::index::{Cursor, NameNote, Recorded, SqliteIndex};
use crate::proof::{merkle_branch, ValidatorClient};

/// Scan from the index's checkpoint (or `birthday` if empty) to the chain tip,
/// applying every verified Name Note found.
///
/// With a `validator` client, every recorded action's proof context (header +
/// Merkle branch, `PROOFS.md §6`) is materialized right after its batch
/// applies; without one, the resolver still indexes but serves bare answers.
pub async fn sync_to_tip(
    index: &SqliteIndex,
    client: LwdClient,
    network: &impl Parameters,
    ivk: &PreparedIncomingViewingKey,
    birthday: u32,
    validator: Option<&ValidatorClient>,
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

        let mut stream =
            chain::blocks(client.clone(), start, tip, DEFAULT_CHUNK_OUTPUTS, seam.map(BlockHash));
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
                    let recorded = index.apply_notes(at, &notes)?;
                    if let Some(validator) = validator {
                        materialize_proofs(index, validator, &notes, &recorded).await?;
                    }
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
    network: &impl Parameters,
    ivk: &PreparedIncomingViewingKey,
    candidates: Vec<Candidate>,
) -> Result<Vec<NameNote>> {
    // Each full transaction is fetched and parsed once; `None` caches a parse
    // failure so it isn't refetched for its remaining candidates. The raw
    // bytes ride along: they are the proof bundle's `tx` (`PROOFS.md §2`).
    type Fetched = Option<(Transaction, u32, Vec<u8>)>;
    let mut fetched: HashMap<[u8; 32], Fetched> = HashMap::new();

    let mut out = Vec::new();
    for c in candidates {
        if let std::collections::hash_map::Entry::Vacant(e) = fetched.entry(c.txid) {
            let raw = chain::fetch_raw_transaction(client, &TxId::from_bytes(c.txid)).await?;
            let height = raw.height as u32;
            let parsed = Transaction::read(
                &raw.data[..],
                BranchId::for_height(network, BlockHeight::from_u32(height)),
            )
            .ok()
            .map(|tx| (tx, height, raw.data));
            e.insert(parsed);
        }
        let Some((tx, height, raw)) = fetched.get(&c.txid).and_then(|o| o.as_ref()) else {
            continue;
        };
        let Some(bundle) = tx.orchard_bundle() else { continue };
        let Some(action) = bundle.actions().get(c.action_index) else { continue };
        let Some((note, _recipient, memo)) = zns_verify::decrypt::try_decrypt_orchard(action, ivk)
        else {
            continue;
        };
        out.push(NameNote {
            note,
            cmx: action.cmx().to_bytes(),
            memo,
            txid: c.txid,
            height: *height,
            action_index: c.action_index,
            tx_bytes: raw.clone(),
        });
    }
    Ok(out)
}

/// Materialize the proof context for each recorded action: per block, the
/// header + txid list from the validator RPC, then the Merkle branch for each
/// Name Note transaction. Idempotent — already-materialized txids are
/// skipped by the `INSERT OR IGNORE`.
async fn materialize_proofs(
    index: &SqliteIndex,
    validator: &ValidatorClient,
    notes: &[NameNote],
    recorded: &[Recorded],
) -> Result<()> {
    // One validator round-trip pair per distinct block.
    let mut heights: Vec<u32> = recorded.iter().map(|r| r.height).collect();
    heights.sort_unstable();
    heights.dedup();

    for height in heights {
        let ctx = validator.block_context(height).await?;
        for r in recorded.iter().filter(|r| r.height == height) {
            let Some(pos) = ctx.txids.iter().position(|t| *t == r.txid) else {
                anyhow::bail!(
                    "validator block {height} does not contain Name Note tx {}",
                    hex::encode(r.txid)
                );
            };
            let branch = merkle_branch(&ctx.txids, pos);
            let tx_bytes = notes
                .iter()
                .find(|n| n.txid == r.txid)
                .map(|n| n.tx_bytes.as_slice())
                .expect("recorded action came from this batch's notes");
            index.insert_proof_material(
                &r.txid,
                height,
                tx_bytes,
                &ctx.header,
                &branch,
                pos as u32,
            )?;
        }
    }
    Ok(())
}

/// Derive the external Orchard incoming viewing key from a unified viewing key
/// encoding (`uivk1…` or a UFVK). Name Notes are encrypted to this key.
pub fn orchard_ivk(network: &impl Parameters, encoding: &str) -> Result<PreparedIncomingViewingKey> {
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
