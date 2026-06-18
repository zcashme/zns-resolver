//! Live sync status for debugging and observability.
//!
//! This is intentionally not a state machine. It is just a snapshot of what the
//! sync loop is currently doing.

#[derive(Debug, Clone, Default)]
pub struct SyncStatus {
    /// Whether the sync loop is actively scanning blocks right now.
    pub scanning: bool,
    /// Highest block height we have scanned and committed.
    pub scanned_height: u32,
    /// Best chain tip height reported by lightwalletd.
    pub tip_height: u32,
    /// Last error message from the sync loop, if any.
    pub last_error: Option<String>,
}

impl SyncStatus {
    pub fn catching_up(scanned_height: u32, tip_height: u32) -> Self {
        Self {
            scanning: true,
            scanned_height,
            tip_height,
            last_error: None,
        }
    }

    pub fn caught_up(height: u32) -> Self {
        Self {
            scanning: false,
            scanned_height: height,
            tip_height: height,
            last_error: None,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            scanning: false,
            scanned_height: 0,
            tip_height: 0,
            last_error: Some(message.into()),
        }
    }
}
