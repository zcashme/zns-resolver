//! ZNS-specific persistence on top of seer-sync's generic scan pipeline.

use std::error::Error;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use orchard::keys::FullViewingKey;
use seer_sync::sync::chain::LwdClient;
use seer_sync::sync::scan::WalletTx;
use seer_sync::{Account, Cursor as SeerCursor, Resume};
use tokio::sync::watch;
use zcash_primitives::transaction::{Transaction, TxId};
use zcash_protocol::consensus::{BlockHeight, Network};

use crate::registry::scanned::{ScannedBatch, ScannedTx};
use crate::registry::{core, Db};

/// The resolver's local registry replica as a seer-sync account.
pub(crate) struct Registry {
    pub(crate) db: Db,
    pub(crate) fvk: FullViewingKey,
    pub(crate) pending: Mutex<Option<ScannedBatch>>,
}

impl Registry {
    fn pending(&self) -> MutexGuard<'_, Option<ScannedBatch>> {
        self.pending.lock().unwrap_or_else(|poisoned| {
            self.pending.clear_poison();
            poisoned.into_inner()
        })
    }
}

/// The network path: observes the chain head live and publishes it to status
/// readers. Separate from the indexer — the tip is an observation, never
/// correctness state, so it is never persisted.
pub(crate) async fn live_tip(tip_tx: watch::Sender<Option<u32>>, network: Network) {
    let mut client = LwdClient::connect_auto(network).await.ok();

    loop {
        if client.is_none() {
            client = LwdClient::connect_auto(network).await.ok();
            if client.is_none() {
                tracing::warn!("no lightwalletd server for the tip publisher; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        }
        let client_ref = client.as_mut().expect("checked above");

        match client_ref.latest_block().await {
            Ok((height, _)) => {
                let _ = tip_tx.send(Some(u32::from(height)));
            }
            Err(error) => {
                tracing::warn!(%error, "tip poll failed; reconnecting");
                client = LwdClient::connect_auto(network).await.ok();
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

#[derive(Debug)]
struct ScanBatchError(&'static str);

impl std::fmt::Display for ScanBatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl Error for ScanBatchError {}

impl Account for Registry {
    fn resume(&self) -> Result<Resume, Box<dyn Error + Send + Sync>> {
        let conn = self.db.lock();
        Ok(core::resume(&conn)?)
    }

    fn rewind(&self, to: BlockHeight) -> Result<(), Box<dyn Error + Send + Sync>> {
        let conn = self.db.lock();
        core::rewind(&conn, u32::from(to))?;
        Ok(())
    }

    fn apply_transactions(
        &self,
        at: SeerCursor,
        transactions: &[WalletTx],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut pending = self.pending();
        if pending.is_some() {
            tracing::warn!("replacing a scan batch that was never applied");
        }
        *pending = Some(ScannedBatch {
            at,
            transactions: transactions.iter().map(ScannedTx::from_wallet).collect(),
        });
        Ok(())
    }

    fn apply_blocks(
        &self,
        at: SeerCursor,
        blocks: &[seer_sync::proto::CompactBlock],
        _full_txs: &[(TxId, BlockHeight, Transaction)],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let batch = self
            .pending()
            .take()
            .ok_or(ScanBatchError("compact blocks arrived with no scan batch"))?;
        if batch.at != at {
            return Err(
                ScanBatchError("compact blocks do not match the pending scan batch").into(),
            );
        }
        let mut block_times = Vec::with_capacity(blocks.len());
        for block in blocks {
            let height = u32::try_from(block.height)
                .map_err(|_| ScanBatchError("compact block height does not fit"))?;
            if block.time != 0 {
                block_times.push((height, u64::from(block.time)));
            }
        }
        // Fold the snapshot without the connection so a name query is not
        // stuck behind the replay. The indexer is the only writer, so the
        // snapshot still matches the batch.
        let facts = {
            let conn = self.db.lock();
            core::lineage_facts(&conn)?
        };
        let lineage = core::fold_lineage(facts);
        let conn = self.db.lock();
        core::apply_batch(
            &conn,
            at,
            &batch.transactions,
            &block_times,
            &self.fvk,
            lineage,
        )?;
        Ok(())
    }
}
