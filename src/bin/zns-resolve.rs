//! Reference resolver binary: wires `seer-sync` + `zns-verify` +
//! `zns-resolver` into a single daemon-style CLI.
//!
//! Subcommands:
//!   sync     scan blocks [--from, --to], persist hits + scan state
//!   lookup   query the local index by name
//!   status   print current index state

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use seer_sync::{
    chain::{open, LwdFullTxFetcher},
    scan::{scan_range, ReorgBuffer, ScanEvent},
};
use zcash_protocol::consensus::TEST_NETWORK;
use zns_resolver::{callback::ZnsCallback, index::SqliteIndex, TESTNET_NU5_HEIGHT};

const DEFAULT_LIGHTWALLETD: &str = "https://testnet.zec.rocks:443";
const DEFAULT_DB_PATH: &str = "zns-resolver.sqlite";
const REORG_BUFFER_DEPTH: usize = 100;

#[derive(Parser, Debug)]
#[command(name = "zns-resolve", about = "ZNS resolver — view-key-only chain observer + name index")]
struct Cli {
    #[arg(long, default_value = DEFAULT_LIGHTWALLETD, global = true)]
    lightwalletd: String,
    #[arg(long, default_value = DEFAULT_DB_PATH, global = true)]
    db: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Scan blocks in [--from, --to] (inclusive) and persist any ZNS hits.
    /// If --from is omitted, resumes from the last-scanned height in the DB
    /// (or NU5 activation if the DB is empty).
    Sync {
        #[arg(long)]
        from: Option<u32>,
        #[arg(long)]
        to: u32,
        /// Hex-encoded addr_reg orchard IVK (64 raw bytes). Required: the
        /// resolver must know which IVK to scan with.
        #[arg(long)]
        ivk_hex: String,
    },
    /// Look up a name in the local index.
    Lookup {
        name: String,
    },
    /// Print current state of the local index.
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Required for rustls 0.23 — picks the ring provider as default.
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Sync { from, to, ivk_hex } => cmd_sync(&cli.lightwalletd, &cli.db, from, to, &ivk_hex).await,
        Cmd::Lookup { name } => cmd_lookup(&cli.db, &name),
        Cmd::Status => cmd_status(&cli.db),
    }
}

async fn cmd_sync(
    lwd_url: &str,
    db_path: &PathBuf,
    from: Option<u32>,
    to: u32,
    ivk_hex: &str,
) -> Result<()> {
    let mut index = SqliteIndex::open(db_path)?;
    let from = from.unwrap_or_else(|| {
        index
            .last_scanned_height()
            .ok()
            .flatten()
            .map(|h| h + 1)
            .unwrap_or(TESTNET_NU5_HEIGHT)
    });
    if to < from {
        return Err(anyhow!("--to ({to}) must be >= effective from ({from})"));
    }
    println!("[sync] scanning {from}..={to} against {lwd_url}");

    // Parse IVK.
    let ivk_bytes = hex::decode(ivk_hex).context("decoding --ivk-hex")?;
    let ivk_bytes: [u8; 64] = ivk_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("ivk must be exactly 64 bytes when hex-decoded"))?;
    let raw_ivk = orchard::keys::IncomingViewingKey::from_bytes(&ivk_bytes)
        .into_option()
        .ok_or_else(|| anyhow!("ivk bytes did not parse to a valid IncomingViewingKey"))?;
    let ivk = orchard::keys::PreparedIncomingViewingKey::new(&raw_ivk);

    let mut client = open(lwd_url).await?;
    let mut fetcher_client = open(lwd_url).await?;
    let mut fetcher = LwdFullTxFetcher {
        client: &mut fetcher_client,
    };
    let mut buffer = index.load_reorg_buffer(REORG_BUFFER_DEPTH)?;
    let mut callback = ZnsCallback;

    let events = scan_range(
        &mut client,
        &mut fetcher,
        &TEST_NETWORK,
        from,
        to,
        &ivk,
        &mut buffer,
        &mut callback,
    )
    .await?;

    let mut total_hits = 0;
    let mut total_rejected = 0;
    for event in events {
        match event {
            ScanEvent::Block {
                height,
                hash,
                hits,
            } => {
                let hits_with_prev: Vec<_> = hits
                    .iter()
                    .map(|h| (h.valid.clone(), h.prev_rcm))
                    .collect();
                let outcome = index.commit_block(height, hash, &hits_with_prev, to)?;
                total_hits += outcome.applied;
                total_rejected += outcome.rejected.len();
                for reason in &outcome.rejected {
                    eprintln!("[sync] block {height} REJECTED: {reason}");
                }
                if outcome.applied > 0 {
                    println!("[sync] block {height} applied {} hit(s)", outcome.applied);
                }
            }
            ScanEvent::Reorg { fork_height } => {
                eprintln!("[sync] REORG at fork_height={fork_height} — rewinding");
                index.rewind_to(fork_height)?;
            }
        }
    }
    println!(
        "[sync] done. applied={total_hits} rejected={total_rejected} names={}",
        index.name_count()?
    );
    Ok(())
}

fn cmd_lookup(db_path: &PathBuf, name: &str) -> Result<()> {
    let index = SqliteIndex::open(db_path)?;
    match index.lookup(name)? {
        None => {
            println!("(not found)");
        }
        Some(e) => {
            println!("name        = {}", e.name);
            println!("ua          = {}", e.ua);
            println!("rcm         = {}", hex::encode(e.rcm));
            println!("last_action = {:?}", e.last_action);
        }
    }
    Ok(())
}

fn cmd_status(db_path: &PathBuf) -> Result<()> {
    let index = SqliteIndex::open(db_path)?;
    println!(
        "db                  = {}",
        db_path.display()
    );
    println!(
        "last_scanned_height = {}",
        index
            .last_scanned_height()?
            .map(|h| h.to_string())
            .unwrap_or_else(|| "(never scanned)".to_string())
    );
    println!("name_count          = {}", index.name_count()?);
    Ok(())
}
