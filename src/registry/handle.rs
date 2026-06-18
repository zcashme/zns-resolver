//! Actor handle for the registry.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::JoinHandle;

use rusqlite;
use zcash_protocol::consensus::Network;

use super::core;
use super::{Checkpoint, Cursor, Event, NameNote, Registration, RegistryError};
use crate::orchard::DecryptedNote;
use zns_verify::Action;

const QUEUE_CAP: usize = 256;

/// Cloneable handle to the registry index — enqueues work on the dedicated DB thread.
#[derive(Clone)]
pub(crate) struct Registry {
    tx: SyncSender<Op>,
}

enum Op {
    InstallRegistryConfig {
        uivk: String,
        network: String,
        birthday: u32,
        reply: Sender<Result<(), rusqlite::Error>>,
    },
    ApplyBatch {
        network: Network,
        scanned: Cursor,
        live: Cursor,
        decrypted: Vec<DecryptedNote>,
        reply: Sender<Result<Vec<NameNote>, rusqlite::Error>>,
    },
    Rewind {
        fork_height: u32,
        scanned_height: u32,
        reply: Sender<Result<(), rusqlite::Error>>,
    },
    Checkpoint {
        reply: Sender<Result<Option<Checkpoint>, rusqlite::Error>>,
    },
    RegistryUivk {
        reply: Sender<Result<Option<String>, rusqlite::Error>>,
    },
    ResolveByName {
        name: String,
        reply: Sender<Result<Option<Registration>, rusqlite::Error>>,
    },
    RegistrationsByUa {
        ua: String,
        limit: u32,
        offset: u32,
        reply: Sender<Result<Vec<Registration>, rusqlite::Error>>,
    },
    ListRegistrations {
        limit: u32,
        offset: u32,
        reply: Sender<Result<Vec<Registration>, rusqlite::Error>>,
    },
    NameCount {
        reply: Sender<Result<u64, rusqlite::Error>>,
    },
    Events {
        name: Option<String>,
        action: Option<Action>,
        since_height: Option<u32>,
        limit: u32,
        offset: u32,
        reply: Sender<Result<(Vec<Event>, u64), rusqlite::Error>>,
    },
    Shutdown,
}

fn recv_db_reply<T>(rx: mpsc::Receiver<Result<T, rusqlite::Error>>) -> Result<T, RegistryError> {
    rx.recv()
        .map_err(|_| RegistryError::Disconnected)?
        .map_err(RegistryError::from)
}

impl Registry {
    pub(crate) fn start(path: PathBuf) -> Result<(Self, JoinHandle<()>), rusqlite::Error> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (op_tx, op_rx) = mpsc::sync_channel(QUEUE_CAP);

        let join = std::thread::spawn(move || match core::DbConn::open(&path) {
            Ok(mut db) => {
                let _ = ready_tx.send(Ok(()));
                run_db_thread(&mut db, op_rx);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
            }
        });

        match ready_rx.recv() {
            Ok(Ok(())) => Ok((Registry { tx: op_tx }, join)),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
                Some("registry db thread exited before ready".into()),
            )),
        }
    }

    pub(crate) fn shutdown(&self) {
        let _ = self.tx.send(Op::Shutdown);
    }

    pub(crate) fn install_registry_config(
        &self,
        uivk: &str,
        network: Network,
        birthday: u32,
    ) -> Result<(), RegistryError> {
        let net_str = network_to_str(network);
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::InstallRegistryConfig {
                uivk: uivk.to_string(),
                network: net_str.to_string(),
                birthday,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn apply_batch(
        &self,
        network: Network,
        scanned: Cursor,
        live: Cursor,
        decrypted: Vec<DecryptedNote>,
    ) -> Result<Vec<NameNote>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::ApplyBatch {
                network,
                scanned,
                live,
                decrypted,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn rewind(
        &self,
        fork_height: u32,
        scanned_height: u32,
    ) -> Result<(), RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::Rewind {
                fork_height,
                scanned_height,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn checkpoint(&self) -> Result<Option<Checkpoint>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::Checkpoint { reply })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn registry_uivk(&self) -> Result<Option<String>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::RegistryUivk { reply })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn resolve_by_name(
        &self,
        name: &str,
    ) -> Result<Option<Registration>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::ResolveByName {
                name: name.to_string(),
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn registrations_by_ua(
        &self,
        ua: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Registration>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::RegistrationsByUa {
                ua: ua.to_string(),
                limit,
                offset,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn list_registrations(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Registration>, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::ListRegistrations {
                limit,
                offset,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn name_count(&self) -> Result<u64, RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::NameCount { reply })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }

    pub(crate) fn events(
        &self,
        name: Option<&str>,
        action: Option<Action>,
        since_height: Option<u32>,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Event>, u64), RegistryError> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(Op::Events {
                name: name.map(str::to_string),
                action,
                since_height,
                limit,
                offset,
                reply,
            })
            .map_err(|_| RegistryError::Disconnected)?;
        recv_db_reply(rx)
    }
}

fn run_db_thread(db: &mut core::DbConn, rx: Receiver<Op>) {
    while let Ok(op) = rx.recv() {
        match op {
            Op::Shutdown => break,
            Op::InstallRegistryConfig {
                uivk,
                network,
                birthday,
                reply,
            } => {
                let _ = reply.send(db.install_registry_config(&uivk, &network, birthday));
            }
            Op::ApplyBatch {
                network,
                scanned,
                live,
                decrypted,
                reply,
            } => {
                let _ = reply.send(db.apply_batch(&network, scanned, live, &decrypted));
            }
            Op::Rewind {
                fork_height,
                scanned_height,
                reply,
            } => {
                let _ = reply.send(db.rewind(fork_height, scanned_height));
            }
            Op::Checkpoint { reply } => {
                let _ = reply.send(db.checkpoint());
            }
            Op::RegistryUivk { reply } => {
                let _ = reply.send(db.registry_uivk());
            }
            Op::ResolveByName { name, reply } => {
                let _ = reply.send(db.resolve_by_name(&name));
            }
            Op::RegistrationsByUa {
                ua,
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(db.registrations_by_ua(&ua, limit, offset));
            }
            Op::ListRegistrations {
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(db.list_registrations(limit, offset));
            }
            Op::NameCount { reply } => {
                let _ = reply.send(db.name_count());
            }
            Op::Events {
                name,
                action,
                since_height,
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(db.events(name.as_deref(), action, since_height, limit, offset));
            }
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
