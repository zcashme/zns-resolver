//! The sync loop itself.

use std::time::Duration;

use futures::StreamExt;
use orchard::keys::FullViewingKey;
use seer_sync::chain::{ChainError, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::proto::CompactBlock;
use seer_sync::BlockHash;
use zcash_protocol::consensus::Network;

use crate::orchard::observe_batch;
use crate::registry::{Cursor, Registry};
use crate::sync::chain::Lwd;
use crate::sync::status::SyncStatus;

const TIP_POLL_INTERVAL: Duration = Duration::from_secs(10);
const REORG_REWIND_INITIAL: u32 = 1;

pub(crate) async fn run_sync_loop(
    lightwalletd: &'static str,
    registry: Registry,
    network: Network,
    scan_birthday: u32,
    fvk: &FullViewingKey,
) {
    let mut lwd = Lwd::connect(lightwalletd).await;
    let mut rewind_by = REORG_REWIND_INITIAL;

    loop {
        let (start, seam) = match resume_from_checkpoint(&registry, scan_birthday) {
            Ok(resume) => resume,
            Err(e) => {
                tracing::warn!(error = %e, "checkpoint read failed");
                registry.set_sync_status(SyncStatus::error("checkpoint read failed"));
                continue;
            }
        };

        let tip = match wait_for_tip(&mut lwd, start).await {
            Ok(tip) => tip,
            Err(e) => {
                tracing::warn!(error = %e, "tip poll failed");
                registry.set_sync_status(SyncStatus::error(format!("tip poll failed: {e}")));
                lwd.reconnect().await;
                continue;
            }
        };

        registry.set_sync_status(SyncStatus::catching_up(start, tip.0));

        match drive_range(
            &mut lwd,
            &registry,
            network,
            fvk,
            start,
            seam,
            tip,
            &mut rewind_by,
        )
        .await
        {
            RangeOutcome::Done => {
                registry.set_sync_status(SyncStatus::caught_up(tip.0));
            }
            RangeOutcome::Reorg { at } => {
                let rewind_to = at.saturating_sub(rewind_by);
                let scanned = registry
                    .checkpoint()
                    .ok()
                    .flatten()
                    .map(|c| c.scanned_height)
                    .unwrap_or(0);

                tracing::warn!(at, rewind_to, "chain reorg");
                registry.set_sync_status(SyncStatus::catching_up(rewind_to, tip.0));

                if let Err(e) = registry.rewind(rewind_to, scanned) {
                    tracing::error!(error = %e, "rewind failed");
                    registry.set_sync_status(SyncStatus::error(format!("rewind failed: {e}")));
                }

                rewind_by = rewind_by.saturating_mul(2);
            }
            RangeOutcome::Reconnect => {
                registry.set_sync_status(SyncStatus::error("block stream failed, reconnecting"));
                lwd.reconnect().await;
            }
        }
    }
}

fn resume_from_checkpoint(
    registry: &Registry,
    scan_birthday: u32,
) -> Result<(u32, Option<BlockHash>), crate::registry::RegistryError> {
    let checkpoint = registry.checkpoint()?;
    let start = checkpoint
        .as_ref()
        .map(|c| c.scanned_height.saturating_add(1))
        .unwrap_or(scan_birthday);
    let seam = checkpoint.and_then(|c| c.scanned_hash.map(BlockHash));
    Ok((start, seam))
}

async fn wait_for_tip(lwd: &mut Lwd, start: u32) -> Result<Cursor, ChainError> {
    let mut tip = lwd.tip().await?;
    while tip.0 == 0 || start > tip.0 {
        tokio::time::sleep(TIP_POLL_INTERVAL).await;
        tip = lwd.tip().await?;
    }
    Ok(tip)
}

#[derive(Debug)]
enum RangeOutcome {
    Done,
    Reorg { at: u32 },
    Reconnect,
}

async fn drive_range(
    lwd: &mut Lwd,
    registry: &Registry,
    network: Network,
    fvk: &FullViewingKey,
    start: u32,
    seam: Option<BlockHash>,
    tip: Cursor,
    rewind_by: &mut u32,
) -> RangeOutcome {
    let mut fetch_client = lwd.fork();
    let mut stream =
        seer_sync::chain::blocks(lwd.fork(), start, tip.0, DEFAULT_CHUNK_OUTPUTS, seam);

    loop {
        match stream.next().await {
            None => return RangeOutcome::Done,
            Some(Ok(batch)) => {
                if let Err(outcome) = process_batch(
                    lwd,
                    registry,
                    network,
                    fvk,
                    &mut fetch_client,
                    &batch,
                    rewind_by,
                )
                .await
                {
                    return outcome;
                }
            }
            Some(Err(ChainError::Reorg(at))) => return RangeOutcome::Reorg { at },
            Some(Err(e)) => {
                tracing::warn!(error = %e, "block stream failed");
                return RangeOutcome::Reconnect;
            }
        }
    }
}

async fn process_batch(
    lwd: &mut Lwd,
    registry: &Registry,
    network: Network,
    fvk: &FullViewingKey,
    fetch_client: &mut seer_sync::chain::LwdClient,
    batch: &[CompactBlock],
    rewind_by: &mut u32,
) -> Result<(), RangeOutcome> {
    let Some(last) = batch.last() else {
        return Ok(());
    };
    let scanned = (last.height as u32, last.hash[..].try_into().ok());

    let live = match lwd.tip().await {
        Ok(tip) => tip,
        Err(e) => {
            tracing::warn!(error = %e, "tip poll failed during sync");
            return Err(RangeOutcome::Reconnect);
        }
    };

    match observe_batch(fetch_client, &network, fvk, batch).await {
        Ok(decrypted) => {
            let n_decrypt = decrypted.len();
            match registry.apply_batch(scanned, live, decrypted) {
                Ok(indexed) => {
                    *rewind_by = REORG_REWIND_INITIAL;
                    registry.set_sync_status(SyncStatus::catching_up(scanned.0, live.0));
                    tracing::info!(
                        height = scanned.0,
                        tip = live.0,
                        decrypted = n_decrypt,
                        indexed = indexed.len(),
                        "batch applied"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "apply_batch failed");
                    return Err(RangeOutcome::Reconnect);
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "observe batch failed");
            return Err(RangeOutcome::Reconnect);
        }
    }

    Ok(())
}
