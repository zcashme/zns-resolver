//! Chain sync: lightwalletd compact blocks, reorg rewind, zebrad proof material.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use jsonrpsee::core::client::ClientT;
use jsonrpsee::http_client::HttpClient;
use jsonrpsee::rpc_params;
use orchard::keys::PreparedIncomingViewingKey;
use seer_sync::chain::{self, ChainError, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::BlockHash;
use serde::Deserialize;
use tokio::sync::watch;
use zcash_primitives::block::BlockHeader;
use zcash_protocol::consensus::Network;
use zns_verify::proof::{merkle_branch, verify_link_inclusion};

use crate::names::{BlockPos, Db, NameNote};
use crate::orchard::observe_batch;

pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(5);
pub(crate) const TIP_WATCH_INTERVAL: Duration = Duration::from_secs(10);

/// zebrad JSON-RPC (`getblock`) for proof material — tested on regtest; same RPC shape as zcashd.
pub(crate) struct ZebradClient {
    client: HttpClient,
}

struct BlockContext {
    header: Vec<u8>,
    txids: Vec<[u8; 32]>,
}

#[derive(Debug, Deserialize)]
struct GetBlockObject {
    tx: Vec<String>,
}

// ── proof material (derivability invariant) ─────────────────────────────────────
//
// Clients can audit bindings without trusting this resolver: given raw tx, block
// header, and merkle siblings they can re-derive inclusion and re-run binding checks.
// Stale or omit, never forge: failed fetches or failed inclusion checks skip the row.

impl ZebradClient {
    pub(crate) fn new(url: &str) -> Result<Self> {
        let client = HttpClient::builder()
            .build(url)
            .context("zebrad RPC url")?;
        Ok(Self { client })
    }

    async fn block_context(&self, height: u32) -> Result<BlockContext> {
        let arg = height.to_string();

        let block: GetBlockObject = self
            .client
            .request("getblock", rpc_params![&arg, 1])
            .await
            .with_context(|| format!("zebrad getblock {height} verbosity=1"))?;
        let txids = block
            .tx
            .iter()
            .map(|hex_str| txid_from_display_hex(hex_str))
            .collect::<Result<Vec<_>>>()?;

        let raw_hex: String = self
            .client
            .request("getblock", rpc_params![&arg, 0])
            .await
            .with_context(|| format!("zebrad getblock {height} verbosity=0"))?;
        let raw = hex::decode(raw_hex.trim()).context("zebrad getblock raw hex")?;
        let parsed = BlockHeader::read(&raw[..])
            .with_context(|| format!("block {height} header parse"))?;
        let mut header = Vec::new();
        parsed.write(&mut header)?;

        Ok(BlockContext { header, txids })
    }
}

fn txid_from_display_hex(hex_str: &str) -> Result<[u8; 32]> {
    let mut bytes: [u8; 32] = hex::decode(hex_str)?
        .try_into()
        .map_err(|_| anyhow!("txid length"))?;
    bytes.reverse();
    Ok(bytes)
}

/// For each newly indexed name note, fetch block context from zebrad and persist proofs.
async fn materialize_proofs(
    db: &Db,
    network: &Network,
    zebrad: &ZebradClient,
    indexed: &[NameNote],
) -> Result<()> {
    let mut heights: Vec<u32> = indexed.iter().map(|n| n.height).collect();
    heights.sort_unstable();
    heights.dedup();

    for height in heights {
        let ctx = match zebrad.block_context(height).await {
            Ok(ctx) => ctx,
            Err(e) => {
                tracing::warn!(height, error = %e, "proof material: zebrad block context");
                continue;
            }
        };

        for n in indexed.iter().filter(|n| n.height == height) {
            let Some(pos) = ctx.txids.iter().position(|t| *t == n.txid) else {
                tracing::warn!(
                    height,
                    txid = %hex::encode(n.txid),
                    "proof material: tx not in zebrad block"
                );
                continue;
            };
            let branch = merkle_branch(&ctx.txids, pos);
            let merkle_index = pos as u32;

            if let Err(e) = verify_link_inclusion(
                network,
                0,
                height,
                &n.raw_tx,
                &ctx.header,
                &branch,
                merkle_index,
            ) {
                tracing::warn!(
                    height,
                    txid = %hex::encode(n.txid),
                    error = %e,
                    "proof material: inclusion check failed"
                );
                continue;
            }

            if let Err(e) = db.insert_proof_material(
                &n.txid,
                height,
                &n.raw_tx,
                &ctx.header,
                &branch,
                merkle_index,
            ) {
                tracing::warn!(
                    height,
                    txid = %hex::encode(n.txid),
                    error = %e,
                    "proof material: db insert"
                );
            }
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
    zebrad: Option<ZebradClient>,
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
                        Ok(decrypted) => match db.apply_batch(&network, scanned, live, &decrypted) {
                            Ok(indexed) => {
                                if let Some(ref zebrad) = zebrad {
                                    if let Err(e) =
                                        materialize_proofs(&db, &network, zebrad, &indexed).await
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