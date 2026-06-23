//! Concrete `Registry` implementation using tokio-rusqlite.
//!
//! - Writes (`install`, `apply_batch`, `rewind`) are serialized through a single
//!   `tokio_rusqlite::Connection` (the single-writer guarantee for per-name
//!   chain integrity and atomic checkpoints lives here).
//! - Reads are synchronous and go through a small fixed-size pool of plain
//!   `rusqlite::Connection`s (WAL snapshot isolation).
//! - The handle is cheap to clone (Arc<Inner>).
//! - Startup opens the writer, runs schema, then opens the reader pool.

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use zcash_protocol::consensus::Network;

use tokio_rusqlite::rusqlite::{self, Connection};
use tokio_rusqlite::Connection as AsyncConnection;

use super::core::{self};
use super::storage;
use super::{
    ChainPosition, Checkpoint, Event, Registration, RegistryError, ResumeInfo,
};
use crate::network::{DB_PATH, NETWORK, SCAN_BIRTHDAY, UFVK};
use crate::sync::DecryptedNote;
use zns_verify::Action;

/// Turn a non-application error from the tokio-rusqlite layer (connection
/// closed, executor panic, etc.) into a RegistryError. Real rusqlite errors
/// from inside closures are propagated directly.
fn map_call_err(ctx: &str, e: impl std::fmt::Display) -> RegistryError {
    RegistryError::from(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
        Some(format!("{}: {}", ctx, e)),
    ))
}

const DEFAULT_READER_POOL_SIZE: usize = 8;

#[derive(Clone)]
pub(crate) struct Registry {
    inner: Arc<Inner>,
}

struct Inner {
    writer: AsyncConnection,
    reader_pool: ReaderPool,
}

impl Registry {
    /// Open the registry (name index) using the baked-in DB path for this build.
    ///
    /// This is the normal production entry point. The DB path, UFVK, network,
    /// and scan birthday are chosen at compile time via Cargo features and
    /// installed (idempotently) into the `registry_account` table.
    pub(crate) async fn start() -> Result<Self, RegistryError> {
        Self::start_with(PathBuf::from(DB_PATH), DEFAULT_READER_POOL_SIZE).await
    }

    /// Open the registry at an explicit path with a custom reader pool size.
    /// Intended for tests and special tooling.
    pub(crate) async fn start_with(path: PathBuf, pool_size: usize) -> Result<Self, RegistryError> {
        // Open writer first (this is the serialized execution path).
        let writer = AsyncConnection::open(&path).await?;

        // Run schema (CREATEs are IF NOT EXISTS; also sets WAL mode etc.).
        writer
            .call(|conn| conn.execute_batch(storage::SCHEMA_SQL))
            .await
            .map_err(|e| match e {
                tokio_rusqlite::Error::Error(inner) => RegistryError::from(inner),
                other => map_call_err("schema initialization", other),
            })?;

        // Stamp (or verify) the immutable registry identity using the
        // compile-time constants. This records the UFVK + network + birthday
        // that this binary was built for. Idempotent: existing values are
        // left alone (with a warning on mismatch).
        let ufvk = UFVK.to_owned();
        let net_str = network_to_str(NETWORK);
        writer
            .call(move |conn| {
                core::install_registry_config(conn, &ufvk, &net_str, SCAN_BIRTHDAY)
            })
            .await
            .map_err(|e| match e {
                tokio_rusqlite::Error::Error(inner) => RegistryError::from(inner),
                other => map_call_err("install registry config", other),
            })?;

        // Open the reader pool *after* the writer so WAL is configured and
        // the DB file exists.
        let mut reader_conns = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let c = Connection::open(&path)?;
            c.execute_batch(storage::READER_PRAGMAS)?;
            reader_conns.push(c);
        }

        Ok(Self {
            inner: Arc::new(Inner {
                writer,
                reader_pool: ReaderPool::new(reader_conns),
            }),
        })
    }

    /// Helper for serialized calls on the writer connection.
    /// Turns background execution / connection errors into RegistryError.
    async fn with_writer<F, T>(&self, f: F) -> Result<T, RegistryError>
    where
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        self.inner.writer.call(f).await.map_err(|e| match e {
            // Real error returned by the closure (e.g. a failed INSERT,
            // constraint violation, etc.) — preserve it.
            tokio_rusqlite::Error::Error(inner) => RegistryError::from(inner),
            // Connection/executor problems — turn into synthetic error.
            other => map_call_err("writer call", other),
        })
    }

    // ── writes (async; serialized via tokio-rusqlite writer connection) ──

    /// High-level boundary API for the sync loop.
    /// Performs verification and note construction inside the safe single-writer context
    /// and advances the atomic checkpoint.
    pub(crate) async fn apply_batch(
        &self,
        decrypted: Vec<DecryptedNote>,
        scanned: ChainPosition,
        tip: ChainPosition,
    ) -> Result<(), RegistryError> {
        self.with_writer(move |conn| core::apply_batch(conn, scanned, tip, &decrypted))
            .await?;
        Ok(())
    }

    /// High-level boundary API for reorg handling.
    /// Rewinds the name index to the given fork height.
    pub(crate) async fn rewind(&self, fork_height: u32) -> Result<(), RegistryError> {
        self.with_writer(move |conn| core::rewind(conn, fork_height))
            .await?;
        Ok(())
    }

    // ── reads (sync via reader pool; WAL snapshot isolation per call) ──
    // Readers run on a small pool of plain rusqlite connections (concurrent
    // with the single writer thanks to WAL). The pool uses mpsc checkout so
    // only a limited number of readers are active at once.
    // Writes are fully serialized on the tokio-rusqlite writer connection.

    /// Returns the information the sync loop needs to decide where to resume.
    /// `birthday` is used only when there is no persisted checkpoint yet.
    pub(crate) fn get_resume_info(&self, birthday: u32) -> Result<ResumeInfo, RegistryError> {
        let cp = self.checkpoint()?;
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

    pub(crate) fn checkpoint(&self) -> Result<Option<Checkpoint>, RegistryError> {
        self.read(core::checkpoint)
    }

    pub(crate) fn registry_ufvk(&self) -> Result<Option<String>, RegistryError> {
        self.read(core::registry_ufvk)
    }

    pub(crate) fn name_count(&self) -> Result<u64, RegistryError> {
        self.read(core::name_count)
    }

    pub(crate) fn resolve_by_name(
        &self,
        name: &str,
    ) -> Result<Option<Registration>, RegistryError> {
        self.read(|conn| core::resolve_by_name(conn, name))
    }

    pub(crate) fn registrations_by_ua(
        &self,
        ua: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Registration>, RegistryError> {
        self.read(|conn| core::registrations_by_ua(conn, ua, limit, offset))
    }

    pub(crate) fn list_registrations(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Registration>, RegistryError> {
        self.read(|conn| core::list_registrations(conn, limit, offset))
    }

    pub(crate) fn events(
        &self,
        name: Option<&str>,
        action: Option<Action>,
        since_height: Option<u32>,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Event>, u64), RegistryError> {
        self.read(|conn| core::events(conn, name, action, since_height, limit, offset))
    }

    // ── private helpers ──

    fn read<T, F>(&self, f: F) -> Result<T, RegistryError>
    where
        F: FnOnce(&Connection) -> rusqlite::Result<T>,
    {
        // Use the reader pool for synchronous reads. Each checkout gets a
        // dedicated rusqlite connection (WAL gives snapshot isolation).
        let conn = self.inner.reader_pool.get()?;
        Ok(f(&conn)?)
    }
}

// ── reader pool ──────────────────────────────────────────────────────────────
//
// Fixed-size pool of plain rusqlite connections for synchronous reads.
// Connections are pre-opened and parked in an mpsc channel. `get()` takes
// one (blocking if the pool is exhausted; the channel acts as a semaphore).
// `PooledConn` returns the connection to the pool on Drop.

struct ReaderPool {
    return_tx: Sender<Connection>,
    checkout: Mutex<Receiver<Connection>>,
}

impl ReaderPool {
    fn new(conns: Vec<Connection>) -> Self {
        let (tx, rx) = mpsc::channel();
        for c in conns {
            let _ = tx.send(c);
        }
        Self {
            return_tx: tx,
            checkout: Mutex::new(rx),
        }
    }

    fn get(&self) -> Result<PooledConn, RegistryError> {
        let conn = self
            .checkout
            .lock()
            .map_err(|_| internal_failure("reader pool closed"))?
            .recv()
            .map_err(|_| internal_failure("reader pool closed"))?;
        Ok(PooledConn {
            conn: Some(conn),
            return_tx: self.return_tx.clone(),
        })
    }
}

struct PooledConn {
    conn: Option<Connection>,
    return_tx: Sender<Connection>,
}

impl Deref for PooledConn {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        // INVARIANT: conn is Some until Drop takes it.
        self.conn.as_ref().expect("pooled conn used after drop")
    }
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        if let Some(c) = self.conn.take() {
            let _ = self.return_tx.send(c);
        }
    }
}

fn network_to_str(network: Network) -> &'static str {
    if network == Network::MainNetwork {
        "main"
    } else {
        "test"
    }
}

/// Turn an internal pool/communication failure into a rusqlite::Error so it
/// becomes a RegistryError at the call site.
fn internal_failure(msg: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
        Some(msg.to_owned()),
    )
}
