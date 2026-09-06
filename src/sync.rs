//! ZNS-specific persistence on top of seer-sync's generic scan pipeline.

use std::error::Error;
use std::time::Duration;

use orchard::keys::FullViewingKey;
use seer_sync::sync::chain::LwdClient;
use seer_sync::sync::scan::WalletTx;
use seer_sync::{Account, Cursor as SeerCursor, Resume};
use tokio::sync::watch;
use zcash_protocol::consensus::BlockHeight;

use crate::registry::{core, Db};
use crate::Registry;

/// The network path: observes the chain head live and publishes it to status
/// readers. Separate from the indexer — the tip is an observation, never
/// correctness state, so it is never persisted.
pub(crate) async fn live_tip(tip_tx: watch::Sender<Option<u32>>) {
    let mut client = LwdClient::connect_auto(crate::NETWORK).await.ok();

    loop {
        if client.is_none() {
            client = LwdClient::connect_auto(crate::NETWORK).await.ok();
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
                client = LwdClient::connect_auto(crate::NETWORK).await.ok();
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// Runs the name indexer forever: drives seer-sync's scan pipeline so newly
/// published name notes are verified and indexed as they arrive.
pub(crate) async fn run_indexer(db: Db, ufvk: &str, fvk: FullViewingKey, birthday: u32) {
    tracing::info!(network = ?crate::NETWORK, birthday, "starting indexer");
    let account = Registry { db, fvk };

    // seer-sync's run loops internally until an error; any return is a
    // restart, never a hot loop.
    loop {
        if let Err(error) = seer_sync::run(ufvk, crate::NETWORK, &account).await {
            tracing::warn!(%error, "sync error; reconnecting");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

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
        let conn = self.db.lock();
        core::apply_batch(&conn, at, transactions, &self.fvk)?;
        Ok(())
    }
}
