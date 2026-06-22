//! The sync loop itself.

use std::time::Duration;

use futures::StreamExt;
use orchard::keys::FullViewingKey;
use seer_sync::chain::{ChainError, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::proto::CompactBlock;
use seer_sync::BlockHash;
use zcash_protocol::consensus::Network;

use crate::orchard::observe_batch;
use crate::registry::{BatchOutcome, ChainPosition, Cursor, Registry};
use crate::sync::chain::Lwd;
use crate::sync::SyncError;

const TIP_POLL_INTERVAL: Duration = Duration::from_secs(10);
const REORG_REWIND_INITIAL: u32 = 1;

pub(crate) async fn run_sync_loop(
    lightwalletd: &'static str,
    registry: Registry,
    network: Network,
    scan_birthday: u32,
    fvk: &FullViewingKey,
) -> Result<(), SyncError> {
    let mut lwd = Lwd::connect(lightwalletd).await;
    let mut rewind_by = REORG_REWIND_INITIAL;

    loop {
        let (start, seam) = match resume_from_checkpoint(&registry, scan_birthday) {
            Ok(resume) => resume,
            Err(e) => {
                tracing::warn!(error = %e, "checkpoint read failed");
                continue;
            }
        };

        let tip = match wait_for_tip(&mut lwd, start).await {
            Ok(tip) => tip,
            Err(e) => {
                tracing::warn!(error = %e, "tip poll failed");
                lwd.reconnect().await;
                continue;
            }
        };

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
            Ok(RangeOutcome::Done) => {}

            Ok(RangeOutcome::Reorg { at }) => {
                let rewind_to = at.saturating_sub(rewind_by);

                tracing::warn!(at, rewind_to, "chain reorg");

                if let Err(e) = registry.rewind(rewind_to).await {
                    tracing::error!(error = %e, "rewind failed");
                    // Registry errors during rewind are logged; we double the rewind step
                    // and continue. A permanent registry failure will surface on the
                    // next batch instead.
                }

                rewind_by = rewind_by.saturating_mul(2);
            }
            Ok(RangeOutcome::Reconnect) => {
                lwd.reconnect().await;
            }
            Err(e) => return Err(e),
            Ok(RangeOutcome::Fatal(e)) => return Err(e),
        }
    }
}

fn resume_from_checkpoint(
    registry: &Registry,
    scan_birthday: u32,
) -> Result<(u32, Option<BlockHash>), crate::registry::RegistryError> {
    let info = registry.get_resume_info(scan_birthday)?;
    let seam = info.seam_hash.map(BlockHash);
    Ok((info.start_height, seam))
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
    /// Fatal error that should cause the entire sync loop to terminate.
    Fatal(SyncError),
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
) -> Result<RangeOutcome, SyncError> {
    let mut fetch_client = lwd.fork();
    let mut stream =
        seer_sync::chain::blocks(lwd.fork(), start, tip.0, DEFAULT_CHUNK_OUTPUTS, seam);

    loop {
        match stream.next().await {
            None => return Ok(RangeOutcome::Done),
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
                    match outcome {
                        RangeOutcome::Fatal(e) => return Err(e),
                        other => return Ok(other),
                    }
                }
            }
            Some(Err(ChainError::Reorg(at))) => return Ok(RangeOutcome::Reorg { at }),
            Some(Err(e)) => {
                tracing::warn!(error = %e, "block stream failed");
                return Ok(RangeOutcome::Reconnect);
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
    let scanned: ChainPosition = (last.height as u32, last.hash[..].try_into().ok()).into();

    let live_cursor = match lwd.tip().await {
        Ok(tip) => tip,
        Err(e) => {
            tracing::warn!(error = %e, "tip poll failed during sync");
            return Err(RangeOutcome::Reconnect);
        }
    };
    let live: ChainPosition = live_cursor.into();

    match observe_batch(fetch_client, &network, fvk, batch).await {
        Ok(decrypted) => {
            let n_decrypt = decrypted.len();
            match registry.apply_batch(decrypted, scanned, live).await {
                Ok(BatchOutcome { indexed }) => {
                    *rewind_by = REORG_REWIND_INITIAL;
                    tracing::info!(
                        height = scanned.height,
                        tip = live.height,
                        decrypted = n_decrypt,
                        indexed,
                        "batch applied"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "apply_batch failed");
                    // Any failure talking to the registry (including the writer thread
                    // exiting) is fatal; we surface it via the tiny RegistryError wrapper.
                    return Err(RangeOutcome::Fatal(SyncError::Registry(e)));
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
