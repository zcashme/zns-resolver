//! ZNS-specific persistence on top of seer-sync's generic scan pipeline.

use std::error::Error;
use std::time::Duration;

use orchard::keys::FullViewingKey;
use seer_sync::sync::scan::WalletTx;
use seer_sync::{Account, Cursor as SeerCursor, Resume, UnifiedFullViewingKey};
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

    loop {
        match seer_sync::run(ufvk, network, &account).await {
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(%error, "sync error; reconnecting");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
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
        core::apply_batch(&conn, at, at, transactions, &self.fvk)?;
        Ok(())
    }
}
