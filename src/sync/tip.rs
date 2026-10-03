use std::time::Duration;

use seer_sync::sync::chain::LwdClient;
use tokio::sync::watch;
use zcash_protocol::consensus::Network;

/// Observes the chain head and publishes it to status readers. The tip is
/// never persisted and does not determine registry correctness.
pub(crate) async fn live_tip(tip_tx: watch::Sender<Option<u32>>, network: Network) {
    let mut client = LwdClient::connect_auto(network).await.ok();

    loop {
        if client.is_none() {
            client = LwdClient::connect_auto(network).await.ok();
            if client.is_none() {
                tracing::warn!("no lightwalletd server for the tip publisher; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        }
        let client_ref = client.as_mut().expect("checked above");

        match client_ref.latest_block().await {
            Ok((height, _)) => {
                let _ = tip_tx.send(Some(u32::from(height)));
            }
            Err(error) => {
                tracing::warn!(%error, "tip poll failed; reconnecting");
                client = LwdClient::connect_auto(network).await.ok();
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}
