//! ZNS-specific persistence on top of seer-sync's generic scan pipeline.

use std::error::Error;
use std::sync::Mutex;
use std::time::Duration;

use group::{Group, GroupEncoding};
use orchard::keys::FullViewingKey;
use pasta_curves::arithmetic::CurveExt;
use pasta_curves::pallas;
use seer_sync::sync::scan::WalletTx;
use seer_sync::{Account, Cursor as SeerCursor, Resume, UnifiedFullViewingKey};
use zcash_primitives::transaction::{Transaction, TxId};
use zcash_protocol::consensus::{BlockHeight, Network};
use zns_verify::decrypt as zns_decrypt;

use crate::registry::Registry;

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
    Registry(#[from] crate::registry::RegistryError),
}

pub(crate) struct DecryptedNote {
    pub(crate) g_d: [u8; 32],
    pub(crate) pk_d: [u8; 32],
    pub(crate) rho: [u8; 32],
    pub(crate) value: u64,
    pub(crate) cmx: [u8; 32],
    pub(crate) memo: [u8; 512],
    pub(crate) txid: [u8; 32],
    pub(crate) height: u32,
    pub(crate) action_index: usize,
    pub(crate) nullifier: [u8; 32],
}

/// Ephemeral context joining seer-sync's transaction and raw-block callbacks.
struct PendingBatch {
    at: SeerCursor,
    received: Vec<([u8; 32], [u8; 32], u32)>,
    spent: Vec<([u8; 32], u32)>,
    name_actions: Vec<([u8; 32], usize)>,
}

pub(crate) async fn run_sync_loop(
    registry: Registry,
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

    tracing::info!(
        network = network_name,
        birthday,
        "starting sync"
    );
    let account = ZnsAccount {
        registry,
        fvk,
        pending: Mutex::new(None),
    };

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
    registry: Registry,
    fvk: FullViewingKey,
    pending: Mutex<Option<PendingBatch>>,
}

impl Account for ZnsAccount {
    fn resume(&self) -> Result<Resume, Box<dyn Error + Send + Sync>> {
        let registry = self.registry.clone();
        Ok(block_on(async move { registry.resume().await })?)
    }

    fn rewind(&self, to: BlockHeight) -> Result<(), Box<dyn Error + Send + Sync>> {
        let registry = self.registry.clone();
        block_on(async move { registry.rewind(u32::from(to)).await })?;
        Ok(())
    }

    fn apply_transactions(
        &self,
        at: SeerCursor,
        transactions: &[WalletTx],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let mut received = Vec::new();
        let mut spent = Vec::new();
        let mut name_actions = Vec::new();

        for transaction in transactions {
            let txid = *transaction.txid.as_ref();
            let height = u32::from(transaction.height);
            for output in &transaction.ironwood_outputs {
                if !output.is_sent {
                    if let Some(nullifier) = output.nf {
                        received.push((nullifier.to_bytes(), txid, height));
                    }
                }
                if output.is_sent
                    && output
                        .memo
                        .as_ref()
                        .is_some_and(|memo| memo.starts_with(b"ZNS:"))
                {
                    name_actions.push((txid, output.index as usize));
                }
            }
            spent.extend(
                transaction
                    .ironwood_spends
                    .iter()
                    .map(|spend| (spend.nf.to_bytes(), height)),
            );
        }

        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "pending batch mutex poisoned")?;
        *pending = Some(PendingBatch {
            at,
            received,
            spent,
            name_actions,
        });
        Ok(())
    }

    fn apply_blocks(
        &self,
        at: SeerCursor,
        _blocks: &[seer_sync::proto::CompactBlock],
        full_txs: &[(TxId, BlockHeight, Transaction)],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let pending = self
            .pending
            .lock()
            .map_err(|_| "pending batch mutex poisoned")?
            .take()
            .ok_or("missing pending transaction batch")?;
        if pending.at != at {
            return Err("mismatched seer-sync callbacks".into());
        }

        let mut decrypted = Vec::new();
        for (txid_bytes, action_index) in &pending.name_actions {
            let txid = TxId::from_bytes(*txid_bytes);
            let Some((_, height, transaction)) = full_txs.iter().find(|(id, _, _)| *id == txid)
            else {
                return Err("selected transaction missing full data".into());
            };
            let Some(action) = transaction
                .ironwood_bundle()
                .and_then(|bundle| bundle.actions().get(*action_index))
            else {
                return Err("selected Ironwood action missing from full transaction".into());
            };

            // Full outgoing recovery gives the memo and proves Registry authorship.
            let Some((note, recipient, memo)) =
                zns_decrypt::try_decrypt_ironwood_sent(action, &self.fvk)
            else {
                continue;
            };
            let mut memo_bytes = [0u8; 512];
            memo_bytes.copy_from_slice(memo.as_slice());
            if !memo_bytes.starts_with(b"ZNS:") {
                continue;
            }

            let raw = recipient.to_raw_address_bytes();
            let diversifier: [u8; 11] = raw[..11].try_into().expect("raw address is 43 bytes");
            let pk_d: [u8; 32] = raw[11..].try_into().expect("raw address is 43 bytes");
            let Some(nullifier) = Option::from(note.nullifier(&self.fvk)) else {
                continue;
            };
            decrypted.push(DecryptedNote {
                g_d: diversify_hash(&diversifier),
                pk_d,
                rho: note.rho().to_bytes(),
                value: note.value().inner(),
                cmx: action.cmx().to_bytes(),
                memo: memo_bytes,
                txid: *txid_bytes,
                height: u32::from(*height),
                action_index: *action_index,
                nullifier: nullifier.to_bytes(),
            });
        }

        let registry = self.registry.clone();
        block_on(async move {
            registry
                .apply_batch(decrypted, pending.received, pending.spent, at, at)
                .await
        })?;
        Ok(())
    }
}

fn diversify_hash(diversifier: &[u8; 11]) -> [u8; 32] {
    let hash = pallas::Point::hash_to_curve("z.cash:Orchard-gd");
    let point = hash(diversifier);
    if bool::from(point.is_identity()) {
        hash(&[]).to_bytes()
    } else {
        point.to_bytes()
    }
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future))
}
