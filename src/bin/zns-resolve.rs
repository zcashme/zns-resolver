//! Reference resolver binary: drives the chain-observer loop (`observe`),
//! verifying ZNS bindings and maintaining a name → UA index.
//!
//! Subcommands:
//!   sync     scan to the chain tip with addr_reg's UIVK, applying ZNS actions
//!   serve    follow the tip and serve the JSON-RPC API
//!   lookup   query the local index by name
//!   status   print current index state

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use orchard::keys::PreparedIncomingViewingKey;
use seer_sync::sync::chain;
use seer_sync::Network;
use zns_resolver::http::{serve, RpcContext};
use zns_resolver::observe::{orchard_ivk, sync_to_tip};
use zns_resolver::{index::SqliteIndex, MAINNET_NU5_HEIGHT, TESTNET_NU5_HEIGHT};

const DEFAULT_LIGHTWALLETD: &str = "https://testnet.zec.rocks:443";
const DEFAULT_DB_PATH: &str = "zns-resolver.sqlite";
const DEFAULT_RPC_ADDR: &str = "127.0.0.1:8080";
const SYNC_INTERVAL_SECS: u64 = 30;

#[derive(Parser, Debug)]
#[command(name = "zns-resolve", about = "ZNS resolver — view-key chain observer + name index")]
struct Cli {
    #[arg(long, default_value = DEFAULT_LIGHTWALLETD, global = true)]
    lightwalletd: String,
    #[arg(long, default_value = DEFAULT_DB_PATH, global = true)]
    db: PathBuf,
    /// Use mainnet instead of testnet (affects key decoding + default birthday).
    #[arg(long, global = true)]
    mainnet: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Scan to the chain tip and apply any ZNS actions found.
    Sync {
        /// addr_reg's unified incoming viewing key (`uivk1…`).
        #[arg(long)]
        uivk: String,
        /// First block to scan when the index is empty (default: NU5 activation).
        #[arg(long)]
        birthday: Option<u32>,
    },
    /// Follow the chain tip and serve the JSON-RPC API.
    Serve {
        /// addr_reg's unified incoming viewing key (`uivk1…`).
        #[arg(long)]
        uivk: String,
        /// First block to scan when the index is empty (default: NU5 activation).
        #[arg(long)]
        birthday: Option<u32>,
        /// Address to bind the JSON-RPC server to.
        #[arg(long, default_value = DEFAULT_RPC_ADDR)]
        addr: String,
    },
    /// Look up a name in the local index.
    Lookup { name: String },
    /// Print current state of the local index.
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Required for rustls 0.23 — picks the ring provider as default.
    rustls::crypto::ring::default_provider().install_default().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let network = if cli.mainnet { Network::MainNetwork } else { Network::TestNetwork };
    match &cli.cmd {
        Cmd::Sync { uivk, birthday } => cmd_sync(&cli, &network, uivk, *birthday).await,
        Cmd::Serve { uivk, birthday, addr } => {
            cmd_serve(&cli, &network, uivk, *birthday, addr).await
        }
        Cmd::Lookup { name } => cmd_lookup(&cli.db, name),
        Cmd::Status => cmd_status(&cli.db),
    }
}

async fn cmd_serve(
    cli: &Cli,
    network: &Network,
    uivk: &str,
    birthday: Option<u32>,
    addr: &str,
) -> Result<()> {
    let ivk = orchard_ivk(network, uivk)?;
    let birthday = birthday.unwrap_or(match network {
        Network::MainNetwork => MAINNET_NU5_HEIGHT,
        _ => TESTNET_NU5_HEIGHT,
    });

    // Create the DB + schema before serving: RPC handlers open it read-only,
    // so the file must exist even if the first sync pass hasn't run yet.
    drop(SqliteIndex::open(&cli.db)?);

    println!("[serve] following tip via {} every {SYNC_INTERVAL_SECS}s", cli.lightwalletd);

    // The sync loop borrows a `!Sync` SQLite connection across awaits, so it
    // can't be `tokio::spawn`ed (that needs `Send`). Run it concurrently with
    // the RPC server on this task via select! instead. jsonrpsee serves
    // requests on its own background tasks, so HTTP stays responsive while a
    // sync pass runs.
    let sync = async {
        loop {
            if let Err(e) = sync_pass(&cli.db, &cli.lightwalletd, network, &ivk, birthday).await {
                tracing::warn!("sync pass failed: {e}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(SYNC_INTERVAL_SECS)).await;
        }
    };

    let ctx = RpcContext { db: cli.db.clone(), uivk: uivk.to_string() };
    tokio::select! {
        r = serve(addr, ctx) => r,
        _ = sync => Ok(()),
    }
}

async fn sync_pass(
    db: &Path,
    lwd: &str,
    network: &Network,
    ivk: &PreparedIncomingViewingKey,
    birthday: u32,
) -> Result<()> {
    let index = SqliteIndex::open(db)?;
    let client = chain::connect(lwd).await?;
    sync_to_tip(&index, client, network, ivk, birthday).await?;
    Ok(())
}

async fn cmd_sync(cli: &Cli, network: &Network, uivk: &str, birthday: Option<u32>) -> Result<()> {
    let index = SqliteIndex::open(&cli.db)?;
    let ivk = orchard_ivk(network, uivk)?;
    let birthday = birthday.unwrap_or(match network {
        Network::MainNetwork => MAINNET_NU5_HEIGHT,
        _ => TESTNET_NU5_HEIGHT,
    });
    println!("[sync] scanning to tip via {} (birthday {birthday})", cli.lightwalletd);

    let client = chain::connect(&cli.lightwalletd).await?;
    sync_to_tip(&index, client, network, &ivk, birthday).await?;

    println!(
        "[sync] done. height={} names={}",
        index.last_scanned_height()?.map_or_else(|| "(none)".to_string(), |h| h.to_string()),
        index.name_count()?,
    );
    Ok(())
}

fn cmd_lookup(db: &Path, name: &str) -> Result<()> {
    let index = SqliteIndex::open(db)?;
    match index.lookup(name)? {
        None => println!("(not found)"),
        Some(e) => {
            println!("name        = {}", e.name);
            println!("ua          = {}", e.ua);
            println!("rcm         = {}", hex::encode(e.rcm));
            println!("last_action = {:?}", e.last_action);
        }
    }
    Ok(())
}

fn cmd_status(db: &Path) -> Result<()> {
    let index = SqliteIndex::open(db)?;
    println!("db                  = {}", db.display());
    println!(
        "last_scanned_height = {}",
        index
            .last_scanned_height()?
            .map_or_else(|| "(never synced)".to_string(), |h| h.to_string())
    );
    println!("name_count          = {}", index.name_count()?);
    Ok(())
}
