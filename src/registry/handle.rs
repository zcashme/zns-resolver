//! ZNS name index using a single tokio-rusqlite connection.
//!

use std::path::PathBuf;

use orchard::note::Nullifier;
use rusqlite::{self, Connection};
use seer_sync::{Cursor, Nullifiers, Resume};
use tokio_rusqlite::Connection as AsyncConnection;
use zcash_primitives::block::BlockHash;
use zcash_protocol::consensus::BlockHeight;

use super::core::{self};
use super::storage;
use super::{Checkpoint, Event, Registration, RegistryError};
use crate::network::{DB_PATH, NETWORK, SCAN_BIRTHDAY, UFVK};
use crate::sync::DecryptedNote;
use zns_verify::Action;

#[derive(Clone)]
pub(crate) struct Registry {
    conn: AsyncConnection,
}

impl Registry {
    /// Open the registry using the compile-time DB path and stamp the
    /// compile-time registry identity (UFVK + network + birthday).
    pub(crate) async fn start() -> Result<Self, RegistryError> {
        let conn = AsyncConnection::open(PathBuf::from(DB_PATH)).await?;

        conn.call(|c| Ok(c.execute_batch(storage::SCHEMA_SQL)?))
            .await?;

        let ufvk = UFVK.to_owned();
        let net_str = if NETWORK == zcash_protocol::consensus::Network::MainNetwork {
            "main"
        } else {
            "test"
        };

        conn.call(move |c| {
            Ok(core::install_registry_config(
                c,
                &ufvk,
                net_str,
                SCAN_BIRTHDAY,
            )?)
        })
        .await?;

        Ok(Self { conn })
    }

    /// Run a closure against the underlying rusqlite connection.
    /// Everything is serialized by tokio-rusqlite.
    async fn call<F, T>(&self, f: F) -> Result<T, RegistryError>
    where
        F: FnOnce(&mut Connection) -> tokio_rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        Ok(self.conn.call(f).await?)
    }

    // ── writes ─────────────────────────────────────────────────────────────

    pub(crate) async fn apply_batch(
        &self,
        decrypted: Vec<DecryptedNote>,
        received_nullifiers: Vec<([u8; 32], [u8; 32], u32)>,
        spent_nullifiers: Vec<([u8; 32], u32)>,
        scanned: Cursor,
        tip: Cursor,
    ) -> Result<(), RegistryError> {
        self.call(move |c| {
            Ok(core::apply_batch(
                c,
                scanned,
                tip,
                &decrypted,
                &received_nullifiers,
                &spent_nullifiers,
            )?)
        })
        .await?;
        Ok(())
    }

    pub(crate) async fn rewind(&self, fork_height: u32) -> Result<(), RegistryError> {
        self.call(move |c| Ok(core::rewind(c, fork_height)?)).await
    }

    // ── reads (all async; everything serialised on the same connection) ────

    pub(crate) async fn resume(&self) -> Result<Resume, RegistryError> {
        let checkpoint = cursor_from_checkpoint(self.checkpoint().await?);
        let ironwood = self
            .call(|c| Ok(core::ironwood_nullifiers(c)?))
            .await?
            .into_iter()
            .filter_map(|bytes| Option::from(Nullifier::from_bytes(&bytes)))
            .collect();

        Ok(Resume {
            birthday: BlockHeight::from_u32(SCAN_BIRTHDAY),
            checkpoint,
            nullifiers: Nullifiers {
                sapling: vec![],
                orchard: vec![],
                ironwood,
            },
        })
    }

    pub(crate) async fn checkpoint(&self) -> Result<Option<Checkpoint>, RegistryError> {
        self.call(|c| Ok(core::checkpoint(c)?)).await
    }

    pub(crate) async fn registry_ufvk(&self) -> Result<Option<String>, RegistryError> {
        self.call(|c| Ok(core::registry_ufvk(c)?)).await
    }

    pub(crate) async fn name_count(&self) -> Result<u64, RegistryError> {
        self.call(|c| Ok(core::name_count(c)?)).await
    }

    pub(crate) async fn resolve_by_name(
        &self,
        name: &str,
    ) -> Result<Option<Registration>, RegistryError> {
        let name = name.to_owned();
        self.call(move |c| Ok(core::resolve_by_name(c, &name)?))
            .await
    }

    pub(crate) async fn registrations_by_ua(
        &self,
        ua: &str,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Registration>, u64), RegistryError> {
        let ua = ua.to_owned();
        self.call(move |c| Ok(core::registrations_by_ua(c, &ua, limit, offset)?))
            .await
    }

    pub(crate) async fn list_registrations(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Registration>, u64), RegistryError> {
        self.call(move |c| Ok(core::list_registrations(c, limit, offset)?))
            .await
    }

    pub(crate) async fn events(
        &self,
        name: Option<&str>,
        action: Option<Action>,
        since_height: Option<u32>,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Event>, u64), RegistryError> {
        let name = name.map(|s| s.to_owned());
        self.call(move |c| {
            Ok(core::events(
                c,
                name.as_deref(),
                action,
                since_height,
                limit,
                offset,
            )?)
        })
        .await
    }
}

fn cursor_from_checkpoint(checkpoint: Option<Checkpoint>) -> Option<Cursor> {
    checkpoint.and_then(|checkpoint| {
        checkpoint.scanned_hash.map(|hash| Cursor {
            height: BlockHeight::from_u32(checkpoint.scanned_height),
            hash: BlockHash(hash),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashless_checkpoint_is_not_resumable() {
        let checkpoint = Checkpoint {
            scanned_height: 42,
            scanned_hash: None,
            chain_tip_height: None,
            chain_tip_hash: None,
        };

        assert!(cursor_from_checkpoint(Some(checkpoint)).is_none());
    }
}
