//! # ZNS resolver
//!
//! Zcash Names binds human-readable names to shielded (unified) addresses via note
//! commitments on chain.
//!
//! The ZNS Resolver watches the registry inbox, verifies bindings,
//! indexes name tips, and serves names using a JSON-RPC HTTP API.
//

mod jsonrpc; // API implementation
mod registry; // Name index Database
mod sync; // Sync Loop

use seer_sync::UnifiedFullViewingKey;
use sync::{live_tip, Registry};
use tracing::level_filters::LevelFilter;
use zcash_protocol::consensus::Network;

use jsonrpc::serve_rpc;
use registry::Db;

// ── compile-time network selection ───────────────────────────────────────────

#[cfg(all(feature = "mainnet", feature = "testnet"))]
compile_error!("mainnet and testnet are mutually exclusive");
#[cfg(not(any(feature = "mainnet", feature = "testnet")))]
compile_error!("enable either mainnet (default) or testnet feature");

#[cfg(feature = "mainnet")]
pub(crate) const NETWORK: Network = Network::MainNetwork;
#[cfg(feature = "testnet")]
pub(crate) const NETWORK: Network = Network::TestNetwork;

/// Registry unified full viewing key for the active network.
#[cfg(feature = "mainnet")]
const UFVK: &str = "uview1209ld9y4fr2sel9q87kfjg7xm2sy53e3qf7dvcw3w8jwhhjpldl8z0vk0a6hveg4jk7xcn0rc9mr8yg77djfpc0uxrm5pqzt8q7ul4w0nvs3x5q9vmk9nm2crywtqaealek4mp47ur7ywu7g4g4xcdlwv6gnqzvu5panv5je2ps6rxmx4856unlt5wde8fpz8uhrqchmwhn2j252h3cyx5phad877xezerm6ex8l5kcfjw02ll7m0nspfufamh3ae9ea48395elptspnv6vvc70j2gcjrdze9syt8kpr2kc67flvg8687thdkpha7lpt9r4p9wjty90q2v2yjanqsr2zrecrqs7lfx9cpkmshf7jyh2vvwfqnyzwtpuy7p72hm2mjhperdafvm9u5rwa6s6urcy0uw30f687e3jnmas45qfqtwyz39kf6kxklfrdn7t4hkz56jegmlr8s8hwmt9wa8vz8zlmm76lrmzyj82nzpeqfu572njq";
#[cfg(feature = "testnet")]
const UFVK: &str = "uviewtest1gkq78nsemsvs4qekadrv6qlqdmvcv2cct8k3c8cwycedpk90p8rtkx24qe8q73gg2ark8crqmmaavrerf7cxu0sp50an54crmeaalfxl72aetxnr89ksad4lvxzmnkyejhhwn65euzamt9wsukgfemeayhcesreasarmq600lfeju3qenh05e5e5cz6nhhsw3cfdcd3d08d0nxswxmkkxl2fq0hzv6t5dm3ftnz4y76236a49v3us4uq2tlsewf86tjtfwm6kue348me04rm7dw6uawv8myw7t5tyx2t453clhjz5f55lvh7hnvr8g8zt8lf4sy5rzq23ntp7g5ly6kyz2nkwfek06llhjtuz2fft9rg6cuuwg4n7299vajnmq6swg3sf563r0jjzuwuvfpjhmfmydar9dcu3fkuawt6u8qu08vhg5mthrcggrjuhht6f0perh0tqmdfrtgzvhjymay7h9ymvfxgd4r56uz6zrsf5gnvw5jy";

/// Persisted name index filename. Distinct per network so mainnet and testnet
/// builds do not share on-disk state.
#[cfg(feature = "mainnet")]
const DB_PATH: &str = "zns.sqlite";
#[cfg(feature = "testnet")]
const DB_PATH: &str = "zns-testnet.sqlite";

/// The mint's ceremony height — the registry's first notes are the
/// ceremony anchors, so scanning starts there and never before: the
/// height of the ceremony's anchor transaction. A stored
/// registry_account row keeps its own birthday; this constant only seeds
/// a first sync.
#[cfg(feature = "mainnet")]
const MINT_BIRTHDAY: u32 = 3_518_764;
#[cfg(feature = "testnet")]
const MINT_BIRTHDAY: u32 = 4_504_792;

/// Where clients send JSON-RPC name queries. Distinct per network so both
/// resolvers can run on one host.
#[cfg(feature = "mainnet")]
const RPC_ADDR: &str = "127.0.0.1:3031";
#[cfg(feature = "testnet")]
const RPC_ADDR: &str = "127.0.0.1:8080";

#[tokio::main]
async fn main() {
    // --- Logging ---
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .init();

    // --- The registry key is decoded before anything persists it: a bad key
    // --- panics here and never poisons the registry_account row. ---
    let registry_fvk = UnifiedFullViewingKey::decode(&NETWORK, UFVK)
        .expect("registry UFVK decodes")
        .orchard()
        .expect("registry UFVK has an orchard component")
        .clone();

    // --- Network path: the live chain head, observed and published ---
    let (tip_tx, tip_rx) = tokio::sync::watch::channel(None);
    tokio::spawn(live_tip(tip_tx, NETWORK));

    // --- Persistent layer bootstrap. Without it there is nothing to serve. ---
    let db = match Db::open(UFVK, MINT_BIRTHDAY, DB_PATH) {
        Ok(db) => db,
        Err(error) => {
            tracing::error!(error = %error, "fatal: registry database failed to open");
            std::process::exit(1);
        }
    };

    // --- RPC server: serves whatever the registry has ---
    let _rpc_handle = match serve_rpc(RPC_ADDR, db.clone(), tip_rx).await {
        Ok(handle) => handle,
        Err(error) => {
            tracing::error!(error = %error, "fatal: rpc server failed to start");
            std::process::exit(1);
        }
    };

    // --- The indexer: run forever, reconnecting when sync returns ---
    tracing::info!(network = ?NETWORK, birthday = MINT_BIRTHDAY, "starting indexer");
    let account = Registry {
        db,
        fvk: registry_fvk,
    };

    loop {
        if let Err(error) = seer_sync::run(UFVK, &NETWORK, &account).await {
            tracing::warn!(%error, "sync error; reconnecting");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
