//! Internal implementation of the registry boundary (writer thread + reader pool).
//!
//! This module contains the concurrency machinery that enforces the core
//! invariant (single writer for safe per-name chain verification + atomic
//! commits). Callers should go through the high-level API and types in
//! `registry.rs` instead of reaching into here.

use std::ops::Deref;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tokio::sync::oneshot;
use zcash_protocol::consensus::Network;

use super::core::{self, WriterConn};
use super::storage;
use super::{
    BatchOutcome, ChainPosition, Checkpoint, Cursor, Event, NameNote, Registration, RegistryError,
    ResumeInfo,
};
use crate::orchard::DecryptedNote;
use zns_verify::Action;

const DEFAULT_READER_POOL_SIZE: usize = 8;

/// Cloneable handle to the registry index.
#[derive(Clone)]
pub(crate) struct Registry {
    inner: Arc<Inner>,
}

struct Inner {
    writer_tx: Sender<Op>,
    reader_pool: ReaderPool,
}

// ── write ops (sent to the writer thread) ────────────────────────────────────

enum Op {
    InstallRegistryConfig {
        ufvk: String,
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
    pub(crate) fn start(path: PathBuf) -> Result<Self, RegistryError> {
        Self::start_with(path, DEFAULT_READER_POOL_SIZE)
    }

    pub(crate) fn start_with(path: PathBuf, pool_size: usize) -> Result<Self, RegistryError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (op_tx, op_rx) = mpsc::channel();

        let path_for_pool = path.clone();
        // The writer thread `JoinHandle` is intentionally dropped here: the
        // thread runs for the process lifetime. If it panics, the `Op` channel
        // disconnects and callers receive a RegistryError (wrapping a synthesized
        // rusqlite error); the sync loop treats registry failures as fatal.
        std::thread::Builder::new()
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
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
                    Some("registry writer thread exited before ready".into()),
                ).into())
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
                writer_tx: op_tx,
                reader_pool: ReaderPool::new(reader_conns),
            }),
        })
    }

    // ── writes (async; serialized on the writer thread) ──

    pub(crate) async fn install_registry_config(
        &self,
        ufvk: &str,
        network: Network,
        birthday: u32,
    ) -> Result<(), RegistryError> {
        let net_str = network_to_str(network);
        let (reply, rx) = oneshot::channel();
        self.send_op(Op::InstallRegistryConfig {
            ufvk: ufvk.to_string(),
            network: net_str.to_string(),
            birthday,
            reply,
        })?;
        await_reply(rx).await
    }

    /// High-level boundary API for the sync loop.
    /// Performs verification + admission inside the safe single-writer context
    /// and advances the atomic checkpoint.
    pub(crate) async fn apply_batch(
        &self,
        decrypted: Vec<DecryptedNote>,
        scanned: ChainPosition,
        tip: ChainPosition,
    ) -> Result<BatchOutcome, RegistryError> {
        let scanned_cur: Cursor = scanned.into();
        let live_cur: Cursor = tip.into();

        let (reply, rx) = oneshot::channel();
        self.send_op(Op::ApplyBatch {
            scanned: scanned_cur,
            live: live_cur,
            decrypted,
            reply,
        })?;
        let indexed_notes = await_reply(rx).await?;
        Ok(BatchOutcome {
            indexed: indexed_notes.len(),
        })
    }

    /// High-level boundary API for reorg handling.
    /// Rewinds the name index to the given fork height.
    pub(crate) async fn rewind(&self, fork_height: u32) -> Result<(), RegistryError> {
        let (reply, rx) = oneshot::channel();
        self.send_op(Op::Rewind {
            fork_height,
            scanned_height: 0, // no longer used by the implementation
            reply,
        })?;
        await_reply(rx).await
    }

    // ── reads (sync; concurrent via the pool; WAL snapshot per call) ──

    /// Returns the information the sync loop needs to decide where to resume.
    /// `birthday` is used only when there is no persisted checkpoint yet.
    pub(crate) fn get_resume_info(
        &self,
        birthday: u32,
    ) -> Result<ResumeInfo, RegistryError> {
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

    fn send_op(&self, op: Op) -> Result<(), RegistryError> {
        // send() returns Err only if the writer thread has disconnected its
        // receiver (panic or process exit). We synthesize a rusqlite error
        // (which then becomes a RegistryError) so the whole surface is uniform.
        self.inner
            .writer_tx
            .send(op)
            .map_err(|_| internal_failure("registry writer thread channel closed"))?;
        Ok(())
    }

    fn read<T, F>(&self, f: F) -> Result<T, RegistryError>
    where
        F: FnOnce(&Connection) -> rusqlite::Result<T>,
    {
        let conn = self.inner.reader_pool.get()?;
        Ok(f(&conn)?)
    }
}

// ── writer thread ────────────────────────────────────────────────────────────

fn run_writer_thread(conn: WriterConn, rx: Receiver<Op>) {
    // This thread runs for the process lifetime. The `Sender` lives in
    // `Arc<Inner>` for as long as any `Registry` handle exists, so under normal
    // operation `rx.recv()` never returns Err — the loop only exits if the
    // thread panics (in which case senders get an error) or the process is killed.
    while let Ok(op) = rx.recv() {
        match op {
            Op::InstallRegistryConfig {
                ufvk,
                network,
                birthday,
                reply,
            } => {
                let _ = reply.send(conn.install_registry_config(&ufvk, &network, birthday));
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
    // Only reachable via panic in the writer thread. conn drops, closing the
    // writer connection. Any in-flight oneshot replies are dropped; callers will
    // receive a RegistryError from await_reply.
}

async fn await_reply<T>(
    rx: oneshot::Receiver<Result<T, rusqlite::Error>>,
) -> Result<T, RegistryError> {
    rx.await
        .map_err(|_| internal_failure("registry writer thread reply channel closed"))?
        .map_err(Into::into)
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

/// Turn an internal communication failure (mpsc channel, oneshot, pool) into
/// a rusqlite::Error. It then gets wrapped into RegistryError at the API boundary
/// so callers only ever see the tiny wrapper (which surfaces the rusqlite error).
fn internal_failure(msg: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
        Some(msg.to_owned()),
    )
}
