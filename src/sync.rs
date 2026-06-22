//! ZNS chain sync module.

mod chain;
mod engine;

pub(crate) use engine::run_sync_loop;

#[derive(thiserror::Error, Debug)]
pub enum SyncError {
    #[error("registry operation failed: {0}")]
    Registry(#[from] crate::registry::RegistryError),

    #[error("chain I/O failed: {0}")]
    Chain(#[from] seer_sync::chain::ChainError),
}
