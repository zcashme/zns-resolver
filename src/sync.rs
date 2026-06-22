//! ZNS chain sync module.

pub mod status;

mod chain;
mod engine;

pub(crate) use engine::run_sync_loop;
pub use status::SyncStatus;

#[derive(thiserror::Error, Debug)]
pub enum SyncError {
    #[error("registry operation failed: {0}")]
    Registry(#[from] crate::registry::RegistryError),

    #[error("writer thread has exited; no further writes possible")]
    WriterDead,
}
