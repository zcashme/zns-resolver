//! ZNS chain sync module.

use std::time::Duration;

use seer_sync::chain as seer_chain;
use seer_sync::{
    run, Account as SeerAccount, AccountError, Batch, BlockHash, BlockHeight, Cursor as SeerCursor,
    Resume, ShieldedNote, ViewKey,
};
use zcash_protocol::memo::MemoBytes;

use crate::network::{NETWORK, SCAN_BIRTHDAY, UFVK};
use crate::registry::{ChainPosition, Registry};

#[derive(thiserror::Error, Debug)]
pub enum SyncError {
    #[error("registry operation failed: {0}")]
    Registry(#[from] crate::registry::RegistryError),

    #[error("chain I/O failed: {0}")]
    Chain(#[from] seer_sync::SyncError),
}

/// A decrypted ZNS name note candidate.
pub(crate) struct DecryptedNote {
    /// Raw inputs required by `zns_verify` to recompute the note commitment.
    pub(crate) g_d: [u8; 32],
    pub(crate) pk_d: [u8; 32],
    pub(crate) rho: [u8; 32],
    pub(crate) value: u64,

    /// Note commitment (cmx).
    pub(crate) cmx: [u8; 32],
    pub(crate) memo: MemoBytes,
    pub(crate) txid: [u8; 32],
    pub(crate) height: u32,
    /// Index of this Orchard action within the transaction's action list.
    pub(crate) action_index: usize,
}

const RETRY_DELAY: Duration = Duration::from_secs(2);

async fn connect_client() -> Option<seer_chain::LwdClient> {
    // Delegate to seer-sync's built-in connection manager (with its own
    // failover to multiple lightwalletd servers).
    match seer_chain::connect_auto().await {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(error = %e, "connect_auto failed, will retry");
            None
        }
    }
}

// --- sync loop using seer-sync engine ---

pub(crate) async fn run_sync_loop(registry: Registry) -> Result<(), SyncError> {
    let view_key = ViewKey::decode(&NETWORK, UFVK).expect("registry UFVK must be valid at startup");

    let account = Account(registry.clone());

    loop {
        // Obtain a fresh lightwalletd client (connection + failover is handled
        // by seer-sync).
        let client = match connect_client().await {
            Some(c) => c,
            None => {
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        match run(client, &view_key, NETWORK, &account).await {
            Ok(Some(cursor)) => {
                tracing::debug!(height = u32::from(cursor.height), "sync pass reached tip");
                // One pass completed. Loop to stay live for future blocks.
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Ok(None) => {
                // Birthday beyond tip; nothing to do yet.
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "seer-sync run returned error; will retry");
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
}

struct Account(Registry);

impl SeerAccount for Account {
    fn resume(&self) -> Result<Resume, AccountError> {
        let reg = self.0.clone();
        let info = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async move { reg.get_resume_info(SCAN_BIRTHDAY).await })
        })
        .map_err(|e| Box::new(e) as AccountError)?;

        // Map our ResumeInfo to seer's Resume.
        // Our info.start_height is already "where to start scanning".
        // The seam (previous scanned hash) belongs to the *previous* height.
        let checkpoint = if let Some(seam) = info.seam_hash {
            // We have a previous scanned position: the height we last fully applied
            // is start_height - 1.
            let prev_height = info.start_height.saturating_sub(1);
            Some(SeerCursor {
                height: BlockHeight::from_u32(prev_height),
                hash: Some(BlockHash(seam)),
            })
        } else {
            None
        };

        Ok(Resume {
            birthday: BlockHeight::from_u32(SCAN_BIRTHDAY),
            checkpoint,
            nullifiers: vec![],
            outpoints: vec![],
        })
    }

    fn rewind(&self, to: BlockHeight) -> Result<(), AccountError> {
        let reg = self.0.clone();
        let height = u32::from(to);
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move { reg.rewind(height).await })
        })
        .map_err(|e| Box::new(e) as AccountError)
    }

    fn apply(&self, at: SeerCursor, batch: &Batch) -> Result<(), AccountError> {
        let decrypted = notes_from_batch(batch);

        let scanned: ChainPosition = (u32::from(at.height), at.hash.map(|h| h.0)).into();

        let reg = self.0.clone();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async move { reg.apply_batch(decrypted, scanned, scanned).await })
        })
        .map_err(|e| Box::new(e) as AccountError)?;

        Ok(())
    }
}

fn notes_from_batch(batch: &Batch) -> Vec<DecryptedNote> {
    let mut decrypted = Vec::new();

    for note in &batch.notes {
        // We only care about Orchard notes for ZNS.
        let ShieldedNote::Orchard(orch_note) = &note.note else {
            continue;
        };

        // Rely on the engine: any note delivered for our UFVK that carries
        // a ZNS: memo is a candidate. (No extra sent-proof check per decision.)
        let memo_slice = note.memo.as_ref().map(|m| m.as_slice()).unwrap_or(&[]);

        if !memo_slice.starts_with(b"ZNS:") {
            continue;
        }

        let (g_d, pk_d) = orch_note.recipient().zns_commitment_keys();
        let rho = orch_note.rho().to_bytes();
        let value = orch_note.value().inner();

        // The commitment (cmx) is derived from the decrypted note.
        // This matches the on-chain value the note was created with.
        let commitment = orch_note.commitment();
        let extracted = orchard::note::ExtractedNoteCommitment::from(commitment);
        let cmx: [u8; 32] = (&extracted).into();

        let memo = note.memo.clone().unwrap_or_else(|| {
            // Should not happen for ZNS memos, but be defensive
            MemoBytes::from(zcash_protocol::memo::Memo::Arbitrary(Box::new([0u8; 511])))
        });

        let txid_bytes: [u8; 32] = *note.txid.as_ref();

        decrypted.push(DecryptedNote {
            g_d,
            pk_d,
            rho,
            value,
            cmx,
            memo,
            txid: txid_bytes,
            height: u32::from(note.height),
            action_index: note.output_index as usize,
        });
    }

    decrypted
}
