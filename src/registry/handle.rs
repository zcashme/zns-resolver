//! Registry handle: writer thread + reader pool.
//!
//! Concurrency model: single-writer / multiple-reader.
//! - One dedicated OS thread owns the single writer `Connection` (via
//!   [`WriterConn`]) and processes write ops from an mpsc channel. It is the
//!   only mutator, which guarantees per-name chain integrity (no TOCTOU on the
//!   tip read used for binding verification).
//! - A pool of N reader `Connection`s (default 8) lives behind an mpsc checkout
//!   channel. Each RPC read borrows a connection, runs one query (or one read
//!   transaction) under a WAL snapshot, and returns it. WAL permits all N
//!   readers to run concurrently with each other and with an in-progress writer
//!   transaction.
//! - Writes are async: the caller sends an [`Op`] then `.await`s a
//!   `tokio::sync::oneshot` reply, yielding the tokio worker while the writer
//!   thread does the work.

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use rusqlite::Connection;
use tokio::sync::oneshot;
use zcash_protocol::consensus::Network;

use super::core::{self, WriterConn};
use super::storage;
use super::{Checkpoint, Cursor, Event, NameNote, Registration, RegistryError, StatusSnapshot};
use crate::orchard::DecryptedNote;
use crate::sync::SyncStatus;
use zns_verify::Action;

const DEFAULT_READER_POOL_SIZE: usize = 8;

/// Cloneable handle to the registry index.
#[derive(Clone)]
pub(crate) struct Registry {
    inner: Arc<Inner>,
}

struct Inner {
    writer_tx: Mutex<Option<Sender<Op>>>,
    reader_pool: ReaderPool,
    sync_status: Mutex<SyncStatus>,
    shutting_down: AtomicBool,
    writer_join: Mutex<Option<JoinHandle<()>>>,
}

// ── write ops (sent to the writer thread) ────────────────────────────────────

enum Op {
    InstallRegistryConfig {
        uivk: String,
        network: String,
        birthday: u32,
        reply: oneshot::Sender<Result<(), rusqlite::Error>>,
    },
    ApplyBatch {
        scanned: Cursor,
        live: Cursor,
        decrypted: Vec<DecryptedNote>,
        reply: oneshot::Sender<Result<Vec<NameNote>, rusqlite::Error>>,
    },
    Rewind {
        fork_height: u32,
        scanned_height: u32,
        reply: oneshot::Sender<Result<(), rusqlite::Error>>,
    },
}

impl Registry {
    pub(crate) fn start(path: PathBuf) -> Result<Self, rusqlite::Error> {
        Self::start_with(path, DEFAULT_READER_POOL_SIZE)
    }

    pub(crate) fn start_with(path: PathBuf, pool_size: usize) -> Result<Self, rusqlite::Error> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (op_tx, op_rx) = mpsc::channel();

        let path_for_pool = path.clone();
        let join = std::thread::Builder::new()
            .name("registry-writer".into())
            .spawn(move || match WriterConn::open(&path) {
                Ok(conn) => {
                    let _ = ready_tx.send(Ok(()));
                    run_writer_thread(conn, op_rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .expect("spawn registry-writer");

        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
                    Some("registry writer thread exited before ready".into()),
                ))
            }
        }

        let mut reader_conns = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let c = Connection::open(&path_for_pool)?;
            c.execute_batch(storage::READER_PRAGMAS)?;
            reader_conns.push(c);
        }

        Ok(Self {
            inner: Arc::new(Inner {
                writer_tx: Mutex::new(Some(op_tx)),
                reader_pool: ReaderPool::new(reader_conns),
                sync_status: Mutex::new(SyncStatus::default()),
                shutting_down: AtomicBool::new(false),
                writer_join: Mutex::new(Some(join)),
            }),
        })
    }

    /// Graceful shutdown. Sets the shutting-down flag (new reads return
    /// `ShuttingDown`), drops the writer op sender so the writer thread drains
    /// all queued ops then exits, and joins the writer thread.
    ///
    /// MUST be called after the RPC server has been stopped so no new reads
    /// arrive while the pool drains.
    pub(crate) async fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::Release);

        let join = {
            let mut tx_guard = self.inner.writer_tx.lock().unwrap();
            *tx_guard = None;
            self.inner.writer_join.lock().unwrap().take()
        };

        if let Some(handle) = join {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = handle.join();
            })
            .await;
        }
    }

    // ── in-memory sync status (bypasses DB) ──

    pub(crate) fn set_sync_status(&self, status: SyncStatus) {
        if let Ok(mut guard) = self.inner.sync_status.lock() {
            *guard = status;
        }
    }

    pub(crate) fn sync_status(&self) -> SyncStatus {
        self.inner
            .sync_status
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Returns false if the dedicated writer thread has exited (e.g. panicked).
    /// This is a fatal condition for the resolver.
    pub(crate) fn writer_is_alive(&self) -> bool {
        let guard = self.inner.writer_join.lock().unwrap();
        guard.as_ref().map_or(false, |h| !h.is_finished())
    }

    // ── writes (async; serialized on the writer thread) ──

    pub(crate) async fn install_registry_config(
        &self,
        uivk: &str,
        network: Network,
        birthday: u32,
    ) -> Result<(), RegistryError> {
        let net_str = network_to_str(network);
        let (reply, rx) = oneshot::channel();
        self.send_op(Op::InstallRegistryConfig {
            uivk: uivk.to_string(),
            network: net_str.to_string(),
            birthday,
            reply,
        })?;
        await_reply(rx).await
    }

    pub(crate) async fn apply_batch(
        &self,
        scanned: Cursor,
        live: Cursor,
        decrypted: Vec<DecryptedNote>,
    ) -> Result<Vec<NameNote>, RegistryError> {
        let (reply, rx) = oneshot::channel();
        self.send_op(Op::ApplyBatch {
            scanned,
            live,
            decrypted,
            reply,
        })?;
        await_reply(rx).await
    }

    pub(crate) async fn rewind(
        &self,
        fork_height: u32,
        scanned_height: u32,
    ) -> Result<(), RegistryError> {
        let (reply, rx) = oneshot::channel();
        self.send_op(Op::Rewind {
            fork_height,
            scanned_height,
            reply,
        })?;
        await_reply(rx).await
    }

    // ── reads (sync; concurrent via the pool; WAL snapshot per call) ──

    pub(crate) fn checkpoint(&self) -> Result<Option<Checkpoint>, RegistryError> {
        self.read(core::checkpoint)
    }

    #[allow(dead_code)]
    pub(crate) fn registry_uivk(&self) -> Result<Option<String>, RegistryError> {
        self.read(core::registry_uivk)
    }

    #[allow(dead_code)]
    pub(crate) fn name_count(&self) -> Result<u64, RegistryError> {
        self.read(core::name_count)
    }

    pub(crate) fn status_snapshot(&self) -> Result<StatusSnapshot, RegistryError> {
        self.read(core::status_snapshot)
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

    fn send_op(&self, op: Op) -> Result<(), RegistryError> {
        if !self.writer_is_alive() {
            return Err(RegistryError::WriterDead);
        }
        let guard = self.inner.writer_tx.lock().unwrap();
        match &*guard {
            Some(tx) => tx.send(op).map_err(|_| RegistryError::Disconnected),
            None => Err(RegistryError::Disconnected),
        }
    }

    fn read<T, F>(&self, f: F) -> Result<T, RegistryError>
    where
        F: FnOnce(&Connection) -> rusqlite::Result<T>,
    {
        if self.inner.shutting_down.load(Ordering::Acquire) {
            return Err(RegistryError::ShuttingDown);
        }
        let conn = self.inner.reader_pool.get()?;
        f(&conn).map_err(RegistryError::Db)
    }
}

// ── writer thread ────────────────────────────────────────────────────────────

fn run_writer_thread(conn: WriterConn, rx: Receiver<Op>) {
    // rx.recv() blocks this OS thread (not a tokio worker). It returns Err only
    // after all Senders are dropped AND the queue is empty — so the writer
    // drains every queued op before exiting.
    while let Ok(op) = rx.recv() {
        match op {
            Op::InstallRegistryConfig {
                uivk,
                network,
                birthday,
                reply,
            } => {
                let _ = reply.send(conn.install_registry_config(&uivk, &network, birthday));
            }
            Op::ApplyBatch {
                scanned,
                live,
                decrypted,
                reply,
            } => {
                let _ = reply.send(conn.apply_batch(scanned, live, &decrypted));
            }
            Op::Rewind {
                fork_height,
                scanned_height,
                reply,
            } => {
                let _ = reply.send(conn.rewind(fork_height, scanned_height));
            }
        }
    }
    // Channel closed; all queued ops drained. conn drops, closing the writer
    // connection. Any oneshot senders whose replies we didn't get to would have
    // been dropped above, but the queue-is-empty guarantee means there are none.
}

async fn await_reply<T>(
    rx: oneshot::Receiver<Result<T, rusqlite::Error>>,
) -> Result<T, RegistryError> {
    rx.await
        .map_err(|_| RegistryError::Disconnected)?
        .map_err(RegistryError::Db)
}

// ── reader pool ──────────────────────────────────────────────────────────────
//
// Hand-rolled fixed-size pool. N connections are pre-opened and sent into an
// mpsc channel. `get()` recv()s one (blocking if all are checked out — the
// channel itself is the semaphore). `PooledConn` returns the connection to the
// channel on Drop, so a panic or early return never leaks a connection.

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
            .map_err(|_| RegistryError::Disconnected)?
            .recv()
            .map_err(|_| RegistryError::Disconnected)?;
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
