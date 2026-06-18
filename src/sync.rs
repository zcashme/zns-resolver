//! Chain sync: lightwalletd compact blocks and reorg rewind.

use std::time::Duration;

use futures::StreamExt;
use orchard::keys::PreparedIncomingViewingKey;
use seer_sync::chain::{self, ChainError, LwdClient, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::proto::CompactBlock;
use seer_sync::BlockHash;
use zcash_protocol::consensus::Network;

use crate::orchard::observe_batch;
use crate::registry::{Cursor, Registry, RegistryError};

pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(5);
pub(crate) const TIP_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Connected lightwalletd client
struct Lwd(LwdClient);

impl Lwd {
    async fn connect(url: &str) -> Result<Self, ChainError> {
        chain::connect(url).await.map(Self)
    }

    async fn reconnect(&mut self, url: &str) -> Result<(), ChainError> {
        self.0 = chain::connect(url).await?;
        Ok(())
    }

    fn fork(&self) -> LwdClient {
        self.0.clone()
    }

    async fn tip(&mut self) -> Result<Cursor, ChainError> {
        chain::tip(&mut self.0).await
    }
}

pub(crate) async fn run_sync_loop(
    lightwalletd: &'static str,
    registry: Registry,
    network: Network,
    scan_birthday: u32,
    ivk: PreparedIncomingViewingKey,
) {
    let mut lwd = connect_with_retry(lightwalletd).await;
    let mut rewind_by = 1u32;

    loop {
        let resume = match get_resume_point(&registry, scan_birthday) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "checkpoint read failed");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let tip = match wait_until_caught_up(&mut lwd, resume.start).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "tip poll failed");
                let _ = lwd.reconnect(lightwalletd).await;
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        match drive_range(
            &mut lwd,
            &registry,
            network,
            &ivk,
            resume,
            tip,
            &mut rewind_by,
        ).await {
            RangeOutcome::Completed => {}
            RangeOutcome::Reorg { at } => {
                handle_reorg(&registry, at, &mut rewind_by);
            }
            RangeOutcome::NeedsReconnect => {
                let _ = lwd.reconnect(lightwalletd).await;
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ResumePoint {
    start: u32,
    seam: Option<BlockHash>,
}

async fn connect_with_retry(url: &str) -> Lwd {
    loop {
        match Lwd::connect(url).await {
            Ok(s) => return s,
            Err(e) => {
                tracing::warn!(error = %e, "lightwalletd connect failed");
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
}

fn get_resume_point(
    registry: &Registry,
    scan_birthday: u32,
) -> Result<ResumePoint, RegistryError> {
    let checkpoint = registry.checkpoint()?;
    let (start, seam) = match checkpoint {
        Some(c) => (
            c.scanned_height.saturating_add(1),
            c.scanned_hash.map(BlockHash),
        ),
        None => (scan_birthday, None),
    };
    Ok(ResumePoint { start, seam })
}

async fn wait_until_caught_up(
    lwd: &mut Lwd,
    start: u32,
) -> Result<Cursor, ChainError> {
    let mut live = lwd.tip().await?;
    while live.0 == 0 || start > live.0 {
        tokio::time::sleep(TIP_POLL_INTERVAL).await;
        live = lwd.tip().await?;
    }
    Ok(live)
}

#[derive(Debug)]
enum RangeOutcome {
    Completed,
    Reorg { at: u32 },
    NeedsReconnect,
}

async fn drive_range(
    lwd: &mut Lwd,
    registry: &Registry,
    network: Network,
    ivk: &PreparedIncomingViewingKey,
    resume: ResumePoint,
    tip: Cursor,
    rewind_by: &mut u32,
) -> RangeOutcome {
    let mut fetch_client = lwd.fork();
    let mut stream = chain::blocks(lwd.fork(), resume.start, tip.0, DEFAULT_CHUNK_OUTPUTS, resume.seam);

    loop {
        match stream.next().await {
            None => return RangeOutcome::Completed,
            Some(Ok(batch)) => {
                if let Err(outcome) = process_batch(
                    lwd,
                    registry,
                    network,
                    ivk,
                    &mut fetch_client,
                    &batch,
                    rewind_by,
                ).await {
                    return outcome;
                }
            }
            Some(Err(ChainError::Reorg(at))) => {
                return RangeOutcome::Reorg { at };
            }
            Some(Err(e)) => {
                tracing::warn!(error = %e, "block stream failed");
                return RangeOutcome::NeedsReconnect;
            }
        }
    }
}

async fn process_batch(
    lwd: &mut Lwd,
    registry: &Registry,
    network: Network,
    ivk: &PreparedIncomingViewingKey,
    fetch_client: &mut LwdClient,
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
            return Err(RangeOutcome::NeedsReconnect);
        }
    };

    match observe_batch(fetch_client, &network, ivk, batch).await {
        Ok(decrypted) => {
            let n_decrypt = decrypted.len();
            match registry.apply_batch(network, scanned, live, decrypted) {
                Ok(indexed) => {
                    *rewind_by = 1;
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
                    return Err(RangeOutcome::NeedsReconnect);
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "observe batch failed");
            return Err(RangeOutcome::NeedsReconnect);
        }
    }

    Ok(())
}

fn handle_reorg(registry: &Registry, at: u32, rewind_by: &mut u32) {
    let rewind_to = at.saturating_sub(*rewind_by);
    let scanned = registry
        .checkpoint()
        .ok()
        .flatten()
        .map(|c| c.scanned_height)
        .unwrap_or(0);
    tracing::warn!(at, rewind_to, "chain reorg");
    if let Err(e) = registry.rewind(rewind_to, scanned) {
        tracing::error!(error = %e, "rewind failed");
    }
    *rewind_by = rewind_by.saturating_mul(2);
}
