//! Chain sync: lightwalletd compact blocks and reorg rewind.

use std::time::Duration;

use futures::StreamExt;
use orchard::keys::PreparedIncomingViewingKey;
use seer_sync::chain::{self, ChainError, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::BlockHash;
use tokio::sync::watch;
use zcash_protocol::consensus::Network;

use crate::registry::{Cursor, Db};
use crate::orchard::observe_batch;

pub(crate) const RETRY_DELAY: Duration = Duration::from_secs(5);
pub(crate) const TIP_WATCH_INTERVAL: Duration = Duration::from_secs(10);

/// Owned lightwalletd URL — pass a clone into tasks; RPC via `seer_sync::chain`.
#[derive(Clone)]
pub(crate) struct LwdSession(String);

impl LwdSession {
    pub(crate) fn new(url: String) -> Self {
        Self(url)
    }

    pub(crate) fn url(&self) -> &str {
        &self.0
    }

    pub(crate) async fn poll_tip(&self) -> Result<Cursor, ChainError> {
        let mut client = chain::connect(self.url()).await?;
        chain::tip(&mut client).await
    }
}

/// Polls lightwalletd for the chain tip and notifies the sync worker when it moves.
pub(crate) async fn run_tip_watcher(
    session: LwdSession,
    interval: Duration,
    tip_sender: watch::Sender<Cursor>,
) {
    loop {
        match session.poll_tip().await {
            Ok(tip) => {
                let _ = tip_sender.send(tip);
            }
            Err(e) => eprintln!("tip watcher: {e}"),
        }
        tokio::time::sleep(interval).await;
    }
}

pub(crate) async fn run_sync_loop(
    session: LwdSession,
    db_path: &'static str,
    network: Network,
    scan_birthday: u32,
    ivk: PreparedIncomingViewingKey,
    mut tip_rx: watch::Receiver<Cursor>,
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

        let client = match chain::connect(session.url()).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("lightwalletd: {e}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let mut fetch_client = client.clone();

        let live = *tip_rx.borrow_and_update();

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
