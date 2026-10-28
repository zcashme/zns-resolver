//! ZNS-specific persistence on top of seer-sync's generic scan pipeline.

use std::error::Error;
use std::time::Duration;

use orchard::keys::FullViewingKey;
use seer_sync::sync::chain;
use seer_sync::sync::scan::WalletTx;
use seer_sync::{Account, Cursor as SeerCursor, Nullifiers};
use tokio::sync::watch;
use zcash_protocol::consensus::{BlockHeight, Network};

use crate::registry::{core, Db};

/// The chain head as last reported by lightwalletd. `None` when no server
/// answers, when a tip read or reconnect stalls, or after a second failed poll.
pub(crate) type ChainTip = watch::Receiver<Option<BlockHeight>>;

/// How long a tip poll or reconnect may take before the tip counts as stale.
/// A slow reconnect clears the tip but keeps going: `connect_auto` gives each
/// bundled server its own 30 s, so cancelling it would skip the fallbacks.
const TIP_TIMEOUT: Duration = Duration::from_secs(30);

/// Pause after an outage before the next reconnect.
const TIP_RETRY: Duration = Duration::from_secs(5);

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
    // One fast poll error is a blip: reconnect at once and keep the last tip.
    // A second consecutive error, or a stalled read, clears it.
    let mut poll_failed = false;

    loop {
        if client.is_none() {
            let mut connect = std::pin::pin!(chain::connect_auto(&network));
            client = match tokio::time::timeout(TIP_TIMEOUT, &mut connect).await {
                Ok(connected) => connected.ok(),
                Err(_) => {
                    tracing::warn!("reconnect is slow; clearing the tip while fallbacks are tried");
                    let _ = tip_tx.send(None);
                    connect.await.ok()
                }
            };
            if client.is_none() {
                tracing::warn!("no lightwalletd server for the tip publisher; retrying");
                let _ = tip_tx.send(None);
                poll_failed = false;
                tokio::time::sleep(TIP_RETRY).await;
                continue;
            }
        }
        let client_ref = client.as_mut().expect("checked above");

        match tokio::time::timeout(TIP_TIMEOUT, client_ref.latest_block()).await {
            Ok(Ok((height, _))) => {
                let _ = tip_tx.send(Some(height));
                poll_failed = false;
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            Ok(Err(error)) => {
                tracing::warn!(%error, "tip poll failed; reconnecting");
                client = None;
                if poll_failed {
                    let _ = tip_tx.send(None);
                    poll_failed = false;
                    tokio::time::sleep(TIP_RETRY).await;
                } else {
                    poll_failed = true;
                }
            }
            Err(_) => {
                tracing::warn!("tip poll timed out; reconnecting");
                let _ = tip_tx.send(None);
                poll_failed = false;
                client = None;
            }
        }
    }
}

impl Account for Registry {
    fn checkpoint(&self) -> Result<Option<SeerCursor>, Box<dyn Error + Send + Sync>> {
        let conn = self.db.lock();
        Ok(core::checkpoint(&conn)?)
    }

    fn birthday(&self) -> Result<BlockHeight, Box<dyn Error + Send + Sync>> {
        let conn = self.db.lock();
        Ok(core::birthday(&conn)?)
    }

    fn nullifiers(&self) -> Result<Nullifiers, Box<dyn Error + Send + Sync>> {
        let conn = self.db.lock();
        Ok(core::nullifiers(&conn)?)
    }

    fn rewind(&self, to: SeerCursor) -> Result<(), Box<dyn Error + Send + Sync>> {
        let conn = self.db.lock();
        core::rewind(&conn, to)?;
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
