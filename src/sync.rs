//! ZNS-specific persistence on top of seer-sync's generic scan pipeline.

use std::error::Error;
use std::time::Duration;

use orchard::keys::FullViewingKey;
use seer_sync::sync::chain::LwdClient;
use seer_sync::sync::scan::WalletTx;
use seer_sync::{Account, Cursor as SeerCursor, Resume, UnifiedFullViewingKey};
use tokio::sync::watch;
use zcash_protocol::consensus::{BlockHeight, Network};

use crate::registry::{core, Db};

use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum SyncError {
    #[error("seer-sync error: {0}")]
    SeerSync(#[from] seer_sync::SyncError),

    #[error("invalid registry UFVK: {0}")]
    InvalidUfvk(String),

    #[error("registry UFVK has no orchard component")]
    MissingOrchard,

    #[error("registry error: {0}")]
    Registry(#[from] rusqlite::Error),
}

/// The network path: observes the chain head live and publishes it to status
/// readers. Separate from the sync loop — the tip is an observation, never
/// correctness state, so it is never persisted.
pub(crate) async fn run_tip_publisher(network: Network, tip_tx: watch::Sender<Option<u32>>) {
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

pub(crate) async fn run_sync_loop(
    db: Db,
    network: Network,
    ufvk: &str,
    birthday: u32,
) -> Result<(), SyncError> {
    let ufvk_decoded = UnifiedFullViewingKey::decode(&network, ufvk)
        .map_err(|e| SyncError::InvalidUfvk(e.to_string()))?;
    let fvk = ufvk_decoded
        .orchard()
        .ok_or(SyncError::MissingOrchard)?
        .clone();

    let network_name = if network == Network::MainNetwork {
        "main"
    } else {
        "test"
    };

    tracing::info!(network = network_name, birthday, "starting sync");
    let account = ZnsAccount { db, fvk };

    // seer-sync's run loops internally until an error; any return is a
    // restart, never a hot loop.
    loop {
        if let Err(error) = seer_sync::run(ufvk, network, &account).await {
            tracing::warn!(%error, "sync error; reconnecting");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

struct ZnsAccount {
    db: Db,
    fvk: FullViewingKey,
}

impl Account for ZnsAccount {
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
