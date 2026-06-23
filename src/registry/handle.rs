//! ZNS name index using a single tokio-rusqlite connection.
//!

use std::path::PathBuf;

use tokio_rusqlite::rusqlite::{self, Connection};
use tokio_rusqlite::Connection as AsyncConnection;

use super::core::{self};
use super::storage;
use super::{ChainPosition, Checkpoint, Event, Registration, RegistryError, ResumeInfo};
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

        conn.call(|c| c.execute_batch(storage::SCHEMA_SQL)).await?;

        let ufvk = UFVK.to_owned();
        let net_str = if NETWORK == zcash_protocol::consensus::Network::MainNetwork {
            "main"
        } else {
            "test"
        };

        conn.call(move |c| core::install_registry_config(c, &ufvk, net_str, SCAN_BIRTHDAY))
            .await?;

        Ok(Self { conn })
    }

    /// Run a closure against the underlying rusqlite connection.
    /// Everything is serialized by tokio-rusqlite.
    async fn call<F, T>(&self, f: F) -> Result<T, RegistryError>
    where
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        Ok(self.conn.call(f).await?)
    }

    // ── writes ─────────────────────────────────────────────────────────────

    pub(crate) async fn apply_batch(
        &self,
        decrypted: Vec<DecryptedNote>,
        scanned: ChainPosition,
        tip: ChainPosition,
    ) -> Result<(), RegistryError> {
        self.call(move |c| core::apply_batch(c, scanned, tip, &decrypted))
            .await?;
        Ok(())
    }

    pub(crate) async fn rewind(&self, fork_height: u32) -> Result<(), RegistryError> {
        self.call(move |c| core::rewind(c, fork_height)).await
    }

    // ── reads (all async; everything serialised on the same connection) ────

    pub(crate) async fn get_resume_info(&self, birthday: u32) -> Result<ResumeInfo, RegistryError> {
        let cp = self.checkpoint().await?;
        let start_height = cp
            .as_ref()
            .map(|c| c.scanned_height.saturating_add(1))
            .unwrap_or(birthday);
        let seam_hash = cp.and_then(|c| c.scanned_hash);
        Ok(ResumeInfo {
            start_height,
            seam_hash,
        })
    }

    pub(crate) async fn checkpoint(&self) -> Result<Option<Checkpoint>, RegistryError> {
        self.call(|c| core::checkpoint(c)).await
    }

    pub(crate) async fn registry_ufvk(&self) -> Result<Option<String>, RegistryError> {
        self.call(|c| core::registry_ufvk(c)).await
    }

    pub(crate) async fn name_count(&self) -> Result<u64, RegistryError> {
        self.call(|c| core::name_count(c)).await
    }

    pub(crate) async fn resolve_by_name(
        &self,
        name: &str,
    ) -> Result<Option<Registration>, RegistryError> {
        let name = name.to_owned();
        self.call(move |c| core::resolve_by_name(c, &name)).await
    }

    pub(crate) async fn registrations_by_ua(
        &self,
        ua: &str,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Registration>, u64), RegistryError> {
        let ua = ua.to_owned();
        self.call(move |c| core::registrations_by_ua(c, &ua, limit, offset))
            .await
    }

    pub(crate) async fn list_registrations(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Registration>, u64), RegistryError> {
        self.call(move |c| core::list_registrations(c, limit, offset))
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
        self.call(move |c| core::events(c, name.as_deref(), action, since_height, limit, offset))
            .await
    }
}
