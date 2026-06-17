//! Chain sync: lightwalletd compact blocks, reorg rewind, validator proofs.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClient;
use jsonrpsee::rpc_params;
use orchard::keys::PreparedIncomingViewingKey;
use seer_sync::chain::{self, ChainError, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::BlockHash;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use zcash_protocol::consensus::Network;

use crate::names::{BlockPos, Db, NameNote};
use crate::orchard::observe_batch;

pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(5);
pub(crate) const TIP_WATCH_INTERVAL: Duration = Duration::from_secs(10);


pub(crate) struct ValidatorClient {
    client: HttpClient,
}

struct BlockContext {
    header: Vec<u8>,
    txids: Vec<[u8; 32]>,
}

// ── proof material (derivability invariant) ─────────────────────────────────────
//
// Clients can audit bindings without trusting this resolver: given raw tx, block
// header, and merkle siblings they can re-derive inclusion and re-run binding checks.

impl ValidatorClient {
    pub(crate) fn new(url: &str) -> Result<Self> {
        let client = HttpClient::builder()
            .build(url)
            .context("validator RPC url")?;
        Ok(Self { client })
    }

    async fn block_context(&self, height: u32) -> Result<BlockContext> {
        let arg = height.to_string();

        let info: serde_json::Value = self
            .client
            .request("getblock", rpc_params![&arg, 1])
            .await
            .with_context(|| format!("getblock {height} (verbose)"))?;
        let txids = info
            .get("tx")
            .and_then(|t| t.as_array())
            .ok_or_else(|| anyhow!("getblock {height}: no tx list"))?
            .iter()
            .map(|v| {
                let hex_str = v.as_str().ok_or_else(|| anyhow!("non-string txid"))?;
                let mut bytes: [u8; 32] = hex::decode(hex_str)?
                    .try_into()
                    .map_err(|_| anyhow!("txid length"))?;
                bytes.reverse();
                Ok(bytes)
            })
            .collect::<Result<Vec<_>>>()?;

        let raw_hex: String = self
            .client
            .request("getblock", rpc_params![&arg, 0])
            .await
            .with_context(|| format!("getblock {height} (raw)"))?;
        let raw = hex::decode(raw_hex.trim()).context("raw block hex")?;
        let parsed = zcash_primitives::block::BlockHeader::read(&raw[..])
            .with_context(|| format!("block {height} header parse"))?;
        let mut header = Vec::new();
        parsed.write(&mut header)?;

        Ok(BlockContext { header, txids })
    }
}

/// Build a Bitcoin-style merkle inclusion path (double-SHA256 pairs).
fn merkle_branch(txids: &[[u8; 32]], index: usize) -> Vec<[u8; 32]> {
    assert!(index < txids.len(), "leaf index in range");
    let mut level: Vec<[u8; 32]> = txids.to_vec();
    let mut idx = index;
    let mut branch = Vec::new();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().expect("non-empty"));
        }
        let sibling = if idx % 2 == 1 {
            level[idx - 1]
        } else {
            level[idx + 1]
        };
        branch.push(sibling);
        level = level
            .chunks_exact(2)
            .map(|pair| sha256d(&pair[0], &pair[1]))
            .collect();
        idx /= 2;
    }
    branch
}

fn sha256d(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let first = Sha256::new()
        .chain_update(left)
        .chain_update(right)
        .finalize();
    Sha256::digest(first).into()
}

/// For each newly indexed name note, fetch block context from validator and persist proofs.
async fn materialize_proofs(
    db: &Db,
    validator: &ValidatorClient,
    indexed: &[NameNote],
) -> Result<()> {
    let mut heights: Vec<u32> = indexed.iter().map(|n| n.height).collect();
    heights.sort_unstable();
    heights.dedup();

    for height in heights {
        let ctx = validator.block_context(height).await?;
        for n in indexed.iter().filter(|n| n.height == height) {
            let Some(pos) = ctx.txids.iter().position(|t| *t == n.txid) else {
                anyhow::bail!(
                    "validator block {height} does not contain tx {}",
                    hex::encode(n.txid)
                );
            };
            let branch = merkle_branch(&ctx.txids, pos);
            db.insert_proof_material(
                &n.txid,
                height,
                &n.raw_tx,
                &ctx.header,
                &branch,
                pos as u32,
            )?;
        }
    }
    Ok(())
}

// ── tip watcher ───────────────────────────────────────────────────────────────

/// Polls lightwalletd for the chain tip and notifies the sync worker when it moves.
pub(crate) async fn run_tip_watcher(url: &'static str, interval: Duration, tx: watch::Sender<BlockPos>) {
    loop {
        match chain::connect(url).await {
            Ok(mut client) => match chain::tip(&mut client).await {
                Ok(tip) => {
                    let _ = tx.send(tip);
                }
                Err(e) => eprintln!("tip watcher: {e}"),
            },
            Err(e) => eprintln!("tip watcher connect: {e}"),
        }
        tokio::time::sleep(interval).await;
    }
}


pub(crate) async fn run_sync_loop(
    lightwalletd: &'static str,
    db_path: &'static str,
    network: Network,
    scan_birthday: u32,
    ivk: PreparedIncomingViewingKey,
    validator: Option<ValidatorClient>,
    mut tip_rx: watch::Receiver<BlockPos>,
) {
    let mut rewind_by = 1u32;

    loop {
        let db = match Db::open_for_indexer(db_path) {
            Ok(db) => db,
            Err(e) => {
                eprintln!("database: {e}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let checkpoint = match db.checkpoint() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("checkpoint: {e}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let client = match chain::connect(lightwalletd).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("lightwalletd: {e}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let mut fetch_client = client.clone();

        let live = *tip_rx.borrow_and_update();

        // Resume from checkpoint+1; `seam` hash lets seer-sync detect reorgs at the boundary.
        let (start, seam) = match checkpoint {
            Some(c) => (
                c.scanned_height.saturating_add(1),
                c.scanned_hash.map(BlockHash),
            ),
            None => (scan_birthday, None),
        };

        if live.0 == 0 || start > live.0 {
            if tip_rx.changed().await.is_err() {
                eprintln!("tip watcher stopped");
                break;
            }
            continue;
        }

        let mut stream = chain::blocks(client, start, live.0, DEFAULT_CHUNK_OUTPUTS, seam);

        // Inner loop: process block batches until stream ends or an error breaks out.
        loop {
            match stream.next().await {
                None => break,
                Some(Ok(batch)) => {
                    let Some(last) = batch.last() else {
                        continue;
                    };
                    let scanned = (last.height as u32, last.hash[..].try_into().ok());

                    match observe_batch(&mut fetch_client, &network, &ivk, &batch).await {
                        Ok(decrypted) => match db.apply_batch(scanned, live, &decrypted) {
                            Ok(indexed) => {
                                if let Some(ref validator) = validator {
                                    if let Err(e) =
                                        materialize_proofs(&db, validator, &indexed).await
                                    {
                                        eprintln!("proofs: {e}");
                                    }
                                }
                                rewind_by = 1;
                                tracing::info!(
                                    height = scanned.0,
                                    tip = live.0,
                                    decrypted = decrypted.len(),
                                    indexed = indexed.len(),
                                    "batch applied"
                                );
                            }
                            Err(e) => {
                                eprintln!("apply: {e}");
                                break;
                            }
                        },
                        Err(e) => {
                            eprintln!("observe: {e}");
                            break;
                        }
                    }
                }
                // Chain hash mismatch at seam → rewind and retry outer loop.
                Some(Err(ChainError::Reorg(at))) => {
                    let rewind_to = at.saturating_sub(rewind_by);
                    let scanned = db
                        .checkpoint()
                        .ok()
                        .flatten()
                        .map(|c| c.scanned_height)
                        .unwrap_or(0);
                    eprintln!("reorg at {at}, rewind to {rewind_to}");
                    if let Err(e) = db.rewind(rewind_to, scanned) {
                        eprintln!("rewind: {e}");
                    }
                    rewind_by = rewind_by.saturating_mul(2);
                    break;
                }
                Some(Err(e)) => {
                    eprintln!("scan: {e}");
                    break;
                }
            }
        }
    }
}
