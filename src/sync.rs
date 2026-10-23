//! ZNS-specific persistence on top of seer-sync's generic scan pipeline.

use std::error::Error;
use std::time::Duration;

use orchard::keys::FullViewingKey;
use seer_sync::sync::chain::LwdClient;
use seer_sync::sync::scan::WalletTx;
use seer_sync::{Account, Cursor as SeerCursor, Resume};
use tokio::sync::watch;
use zcash_protocol::consensus::{BlockHeight, Network};

use crate::registry::{core, Db};

/// The chain head as last reported by lightwalletd: `None` while no server
/// answers.
pub(crate) type ChainTip = watch::Receiver<Option<BlockHeight>>;

/// How long a tip poll or reconnect may take before it counts as failed.
const TIP_TIMEOUT: Duration = Duration::from_secs(30);

/// The resolver's local registry replica as a seer-sync account.
pub(crate) struct Registry {
    pub(crate) db: Db,
    pub(crate) fvk: FullViewingKey,
}

/// The network path: observes the chain head live and publishes it to status
/// readers. Separate from the indexer — the tip is an observation, never
/// correctness state, so it is never persisted.
pub(crate) async fn live_tip(tip_tx: watch::Sender<Option<BlockHeight>>, network: Network) {
    let mut client = None;

    loop {
        if client.is_none() {
            client = tokio::time::timeout(TIP_TIMEOUT, LwdClient::connect_auto(&network))
                .await
                .ok()
                .and_then(Result::ok);
            if client.is_none() {
                tracing::warn!("no lightwalletd server for the tip publisher; retrying");
                let _ = tip_tx.send(None);
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        }
        let client_ref = client.as_mut().expect("checked above");

        // A failed poll reconnects at once, so a blip never shows as unsynced.
        match tokio::time::timeout(TIP_TIMEOUT, client_ref.latest_block()).await {
            Ok(Ok((height, _))) => {
                let _ = tip_tx.send(Some(height));
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "tip poll failed; reconnecting");
                client = None;
            }
            Err(_) => {
                tracing::warn!("tip poll timed out; reconnecting");
                client = None;
            }
        }
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
        // Fold the snapshot without the connection so a name query is not
        // stuck behind the replay. The indexer is the only writer, so the
        // snapshot still matches the batch.
        let facts = {
            let conn = self.db.lock();
            core::lineage_facts(&conn)?
        };
        let lineage = core::fold_lineage(facts);
        let conn = self.db.lock();
        core::apply_batch(&conn, at, transactions, &self.fvk, lineage)?;
        Ok(())
    }
}
