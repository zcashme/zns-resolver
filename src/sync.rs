//! Maps seer-sync account callbacks to registry batch application.

mod batch;
mod tip;

pub(crate) use tip::live_tip;

use std::error::Error;
use std::sync::{Mutex, MutexGuard};

use seer_sync::sync::scan::WalletTx;
use seer_sync::{Account, Cursor as SeerCursor, Resume};
use zcash_primitives::transaction::{Transaction, TxId};
use zcash_protocol::consensus::BlockHeight;

use crate::registry::batch::BatchTx;
use crate::registry::Registry;

use self::batch::project_tx;

/// Adapts seer-sync's two callbacks to one registry batch application.
pub(crate) struct RegistryAdapter {
    registry: Registry,
    batch: Mutex<Option<ScannedBatch>>,
}

/// One set of transactions awaiting the matching compact blocks.
struct ScannedBatch {
    at: SeerCursor,
    transactions: Vec<BatchTx>,
}

impl RegistryAdapter {
    pub(crate) fn new(registry: Registry) -> Self {
        Self {
            registry,
            batch: Mutex::new(None),
        }
    }

    fn batch(&self) -> MutexGuard<'_, Option<ScannedBatch>> {
        self.batch.lock().unwrap_or_else(|poisoned| {
            self.batch.clear_poison();
            poisoned.into_inner()
        })
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

impl Account for RegistryAdapter {
    fn resume(&self) -> Result<Resume, Box<dyn Error + Send + Sync>> {
        Ok(self.registry.resume()?)
    }

    fn rewind(&self, to: BlockHeight) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.registry.rewind(to)?;
        Ok(())
    }

    fn apply_transactions(
        &self,
        at: SeerCursor,
        transactions: &[WalletTx],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut batch = self.batch();
        if batch.is_some() {
            tracing::warn!("replacing a scan batch that was never applied");
        }
        *batch = Some(ScannedBatch {
            at,
            transactions: transactions.iter().map(project_tx).collect(),
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
            .batch()
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
        self.registry
            .apply_batch(at, &batch.transactions, &block_times)?;
        Ok(())
    }
}
