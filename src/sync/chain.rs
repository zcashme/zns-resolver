//! Thin wrapper around the lightwalletd client.

use std::time::Duration;

use seer_sync::chain::{self, ChainError, LwdClient};

use crate::registry::Cursor;

const RETRY_DELAY: Duration = Duration::from_secs(5);

pub(crate) struct Lwd {
    client: LwdClient,
    url: &'static str,
}

impl Lwd {
    pub(crate) async fn connect(url: &'static str) -> Self {
        loop {
            match chain::connect(url).await {
                Ok(client) => return Self { client, url },
                Err(e) => {
                    tracing::warn!(error = %e, "lightwalletd connect failed");
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
    }

    pub(crate) async fn reconnect(&mut self) {
        loop {
            match chain::connect(self.url).await {
                Ok(client) => {
                    self.client = client;
                    return;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "lightwalletd reconnect failed");
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
    }

    pub(crate) fn fork(&self) -> LwdClient {
        self.client.clone()
    }

    pub(crate) async fn tip(&mut self) -> Result<Cursor, ChainError> {
        chain::tip(&mut self.client).await
    }
}
