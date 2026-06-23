//! Lightwalletd connection management owned by the sync module.

use std::time::Duration;

use seer_sync::chain::{self, ChainError};

/// The plain LwdClient from seer-sync. This is the "just the client" type.
/// Code that only needs to send RPCs or start a blocks() stream should use this.
pub(crate) type LwdClient = seer_sync::chain::LwdClient;

/// Known lightwalletd endpoints. Connection management responsibility lives here.
#[cfg(feature = "mainnet")]
pub(crate) const LIGHTWALLETD_ENDPOINTS: &[&str] = &[
    "https://zec.rocks:443",
    // Add other reliable mainnet lightwalletd servers here.
];
#[cfg(feature = "testnet")]
pub(crate) const LIGHTWALLETD_ENDPOINTS: &[&str] = &[
    "https://testnet.zec.rocks:443",
    // Add other reliable testnet lightwalletd servers here.
];

/// Just a live connection to lightwalletd.
pub(crate) struct Connection {
    client: LwdClient,
}

impl Connection {
    pub(crate) async fn connect() -> Self {
        loop {
            for &url in LIGHTWALLETD_ENDPOINTS {
                match chain::connect(url).await {
                    Ok(client) => return Self { client },
                    Err(e) => {
                        tracing::warn!(error = %e, "lightwalletd connect failed");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }

    pub(crate) async fn reconnect(&mut self) {
        // Use the list of URLs to re-establish a connection.
        // No stored URL inside the struct. The list is used to always maintain
        // a connection.
        loop {
            for &url in LIGHTWALLETD_ENDPOINTS {
                match chain::connect(url).await {
                    Ok(client) => {
                        self.client = client;
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "lightwalletd reconnect failed");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }

    /// Produce a clone of the current client.
    /// Needed because seer_sync::chain::blocks takes ownership of a client.
    pub(crate) fn fork(&self) -> LwdClient {
        self.client.clone()
    }

    pub(crate) async fn tip(&mut self) -> Result<(u32, Option<[u8; 32]>), ChainError> {
        chain::tip(&mut self.client).await
    }
}
