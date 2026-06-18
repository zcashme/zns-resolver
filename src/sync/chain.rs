//! Thin wrapper around the lightwalletd client.

use seer_sync::chain::{self, ChainError, LwdClient};

use crate::registry::Cursor;

async fn connect_with_retry(url: &'static str, what: &str) -> LwdClient {
    loop {
        match chain::connect(url).await {
            Ok(client) => return client,
            Err(e) => {
                tracing::warn!(error = %e, "lightwalletd {what} failed");
            }
        }
    }
}

pub(crate) struct Lwd {
    client: LwdClient,
    url: &'static str,
}

impl Lwd {
    pub(crate) async fn connect(url: &'static str) -> Self {
        let client = connect_with_retry(url, "connect").await;
        Self { client, url }
    }

    pub(crate) async fn reconnect(&mut self) {
        self.client = connect_with_retry(self.url, "reconnect").await;
    }

    pub(crate) fn fork(&self) -> LwdClient {
        self.client.clone()
    }

    pub(crate) async fn tip(&mut self) -> Result<Cursor, ChainError> {
        chain::tip(&mut self.client).await
    }
}
