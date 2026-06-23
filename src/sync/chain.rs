//! Lightwalletd connection management owned by the sync module.

use std::time::Duration;

use futures::Stream;

use seer_sync::chain::{self, ChainError};
use seer_sync::proto::{CompactBlock, RawTransaction};
use seer_sync::{BlockHash, TxId};

// Network-selected lightwalletd endpoints (re-exported from crate::network).
use crate::network::LIGHTWALLETD_ENDPOINTS;

/// CompactBlock client from lightwalletd (just the underlying client).
pub(crate) type LwdClient = seer_sync::chain::LwdClient;

/// A managed connection to lightwalletd.
pub(crate) struct Connection {
    client: LwdClient,
}

impl Connection {
    pub(crate) async fn connect() -> Self {
        loop {
            for &url in LIGHTWALLETD_ENDPOINTS {
                match chain::connect(url).await {
                    Ok(client) => return Self { client },
                    Err(e) => {
                        tracing::warn!(error = %e, "lightwalletd connect failed");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }

    pub(crate) async fn reconnect(&mut self) {
        loop {
            for &url in LIGHTWALLETD_ENDPOINTS {
                match chain::connect(url).await {
                    Ok(client) => {
                        self.client = client;
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "lightwalletd reconnect failed");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }

    /// Returns a stream of CompactBlock batches.
    pub(crate) fn create_block_stream(
        &self,
        from: u32,
        to: u32,
        max_outputs: usize,
        prev_hash: Option<BlockHash>,
    ) -> impl Stream<Item = Result<Vec<CompactBlock>, ChainError>> {
        let client = self.client.clone();
        seer_sync::chain::blocks(client, from, to, max_outputs, prev_hash)
    }

    /// Fetches a full raw transaction.
    pub(crate) async fn fetch_raw_transaction(
        &self,
        txid: &TxId,
    ) -> Result<RawTransaction, ChainError> {
        let mut client = self.client.clone();
        chain::fetch_raw_transaction(&mut client, txid).await
    }

    pub(crate) async fn tip(&mut self) -> Result<(u32, Option<[u8; 32]>), ChainError> {
        chain::tip(&mut self.client).await
    }
}
