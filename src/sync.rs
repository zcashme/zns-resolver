//! Chain sync: lightwalletd compact blocks and reorg rewind.

use std::time::Duration;

use futures::StreamExt;
use orchard::keys::PreparedIncomingViewingKey;
use seer_sync::chain::{self, ChainError, LwdClient, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::BlockHash;
use zcash_protocol::consensus::Network;

use crate::orchard::observe_batch;
use crate::registry::{Cursor, Registry};

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
}

async fn wait_until_caught_up(lwd: &mut Lwd, start: u32) -> Result<Cursor, ChainError> {
    loop {
        let live = chain::tip(&mut lwd.0).await?;
        if live.0 != 0 && start <= live.0 {
            return Ok(live);
        }
        tokio::time::sleep(TIP_POLL_INTERVAL).await;
    }
}

pub(crate) async fn run_sync_loop(
    lightwalletd: &'static str,
    registry: Registry,
    network: Network,
    scan_birthday: u32,
    ivk: PreparedIncomingViewingKey,
) {
    let mut rewind_by = 1u32;
    let mut lwd = loop {
        match Lwd::connect(lightwalletd).await {
            Ok(s) => break s,
            Err(e) => {
                tracing::warn!(error = %e, "lightwalletd connect failed");
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    };

    loop {
        let checkpoint = match registry.checkpoint() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "checkpoint read failed");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let live = match chain::tip(&mut lwd.0).await {
            Ok(tip) => tip,
            Err(e) => {
                tracing::warn!(error = %e, "tip poll failed");
                if lwd.reconnect(lightwalletd).await.is_err() {
                    tokio::time::sleep(RETRY_DELAY).await;
                }
                continue;
            }
        };

        let (start, seam) = match checkpoint {
            Some(c) => (
                c.scanned_height.saturating_add(1),
                c.scanned_hash.map(BlockHash),
            ),
            None => (scan_birthday, None),
        };

        let live = if live.0 == 0 || start > live.0 {
            match wait_until_caught_up(&mut lwd, start).await {
                Ok(tip) => tip,
                Err(e) => {
                    tracing::warn!(error = %e, "tip wait failed");
                    let _ = lwd.reconnect(lightwalletd).await;
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
            }
        } else {
            live
        };

        let mut fetch_client = lwd.fork();
        let mut stream = chain::blocks(lwd.fork(), start, live.0, DEFAULT_CHUNK_OUTPUTS, seam);

        loop {
            match stream.next().await {
                None => break,
                Some(Ok(batch)) => {
                    let Some(last) = batch.last() else {
                        continue;
                    };
                    let scanned = (last.height as u32, last.hash[..].try_into().ok());

                    let live = match chain::tip(&mut lwd.0).await {
                        Ok(tip) => tip,
                        Err(e) => {
                            tracing::warn!(error = %e, "tip poll failed during sync");
                            let _ = lwd.reconnect(lightwalletd).await;
                            break;
                        }
                    };

                    match observe_batch(&mut fetch_client, &network, &ivk, &batch).await {
                        Ok(decrypted) => {
                            let n_decrypt = decrypted.len();
                            match registry.apply_batch(network, scanned, live, decrypted) {
                                Ok(indexed) => {
                                    rewind_by = 1;
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
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "observe batch failed");
                            let _ = lwd.reconnect(lightwalletd).await;
                            break;
                        }
                    }
                }
                Some(Err(ChainError::Reorg(at))) => {
                    let rewind_to = at.saturating_sub(rewind_by);
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
                    rewind_by = rewind_by.saturating_mul(2);
                    break;
                }
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "block stream failed");
                    let _ = lwd.reconnect(lightwalletd).await;
                    break;
                }
            }
        }
    }
}
