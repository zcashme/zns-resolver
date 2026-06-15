//! ZNS resolver — see `AGENTS.md`.

use std::path::Path;
use std::time::Duration;

use futures::StreamExt;
use rusqlite::Connection;
use seer_sync::chain::{self, ChainError, DEFAULT_CHUNK_OUTPUTS};
use seer_sync::BlockHash;
use thiserror::Error;

/// Default on-disk index path.
const DEFAULT_DB_PATH: &str = "zns-resolver.sqlite";

/// Reorgs deeper than this trigger a full rescan; shallower ones delete + re-upsert.
const REORG_SHALLOW_MAX: u32 = 30;

/// Orchard pool exists from NU5 — scan from here until network selection exists.
const SCAN_BIRTHDAY: u32 = 1_687_104;

const RETRY_DELAY: Duration = Duration::from_secs(5);
const TIP_POLL: Duration = Duration::from_secs(10);

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS scan_state (
    id     INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
    height INTEGER NOT NULL,
    hash   BLOB
);

CREATE TABLE IF NOT EXISTS name_events (
    name         TEXT    NOT NULL,
    height       INTEGER NOT NULL,
    action       TEXT    NOT NULL CHECK (action IN ('claim', 'update', 'release')),
    ua           TEXT    NOT NULL,
    prev_rcm     BLOB    NOT NULL,
    rcm          BLOB    NOT NULL,
    psi          BLOB    NOT NULL,
    cmx          BLOB    NOT NULL,
    txid         BLOB    NOT NULL,
    action_index INTEGER NOT NULL,
    raw_tx       BLOB    NOT NULL,
    PRIMARY KEY (name, height)
);
CREATE INDEX IF NOT EXISTS idx_name_events_height ON name_events (height);
CREATE INDEX IF NOT EXISTS idx_name_events_txid ON name_events (txid);

CREATE TABLE IF NOT EXISTS names (
    name         TEXT    NOT NULL PRIMARY KEY,
    height       INTEGER NOT NULL,
    action       TEXT    NOT NULL CHECK (action IN ('claim', 'update', 'release')),
    ua           TEXT    NOT NULL,
    prev_rcm     BLOB    NOT NULL,
    rcm          BLOB    NOT NULL,
    psi          BLOB    NOT NULL,
    cmx          BLOB    NOT NULL,
    txid         BLOB    NOT NULL,
    action_index INTEGER NOT NULL,
    raw_tx       BLOB    NOT NULL
);

CREATE TABLE IF NOT EXISTS proof_material (
    txid          BLOB    NOT NULL PRIMARY KEY,
    height        INTEGER NOT NULL,
    raw_tx        BLOB    NOT NULL,
    header        BLOB    NOT NULL,
    merkle_branch BLOB    NOT NULL,
    merkle_index  INTEGER NOT NULL
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS pending_events (
    txid         BLOB    NOT NULL,
    action_index INTEGER NOT NULL,
    name         TEXT    NOT NULL,
    action       TEXT    NOT NULL CHECK (action IN ('claim', 'update', 'release')),
    ua           TEXT    NOT NULL,
    prev_rcm     BLOB    NOT NULL,
    rcm          BLOB    NOT NULL,
    psi          BLOB    NOT NULL,
    cmx          BLOB    NOT NULL,
    raw_tx       BLOB    NOT NULL,
    PRIMARY KEY (txid, action_index)
);
"#;

#[derive(Debug, Error)]
enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

struct Db {
    conn: Connection,
}

impl Db {
    fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self { conn })
    }
}

/// Last applied block — the seam for the next `chain::blocks` pass.
#[derive(Clone, Copy)]
struct Cursor {
    height: u32,
    hash: Option<[u8; 32]>,
}

#[tokio::main]
async fn main() {
    let mut checkpoint: Option<Cursor> = None;
    let mut rewind_by = 1u32;

    loop {
        let mut client = match chain::connect_auto().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("lightwalletd: {e}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let tip = match chain::tip_height(&mut client).await {
            Ok(h) => h,
            Err(e) => {
                eprintln!("tip: {e}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let (start, seam) = match checkpoint {
            Some(c) => (c.height.saturating_add(1), c.hash.map(BlockHash)),
            None => (SCAN_BIRTHDAY, None),
        };

        if start > tip {
            tokio::time::sleep(TIP_POLL).await;
            continue;
        }

        let mut stream = chain::blocks(client, start, tip, DEFAULT_CHUNK_OUTPUTS, seam);

        loop {
            match stream.next().await {
                None => break,
                Some(Ok(batch)) => {
                    let Some(last) = batch.last() else {
                        continue;
                    };
                    checkpoint = Some(Cursor {
                        height: last.height as u32,
                        hash: last.hash[..].try_into().ok(),
                    });
                    rewind_by = 1;
                    eprintln!(
                        "batch {}..={} ({} blocks)",
                        batch.first().map(|b| b.height).unwrap_or(0),
                        last.height,
                        batch.len()
                    );
                }
                Some(Err(ChainError::Reorg(at))) => {
                    let rewind_to = at.saturating_sub(rewind_by);
                    eprintln!("reorg at {at}, rewind to {rewind_to}");
                    checkpoint = Some(Cursor {
                        height: rewind_to,
                        hash: None,
                    });
                    rewind_by = rewind_by.saturating_mul(2);
                    break;
                }
                Some(Err(e)) => {
                    eprintln!("scan: {e}");
                    break;
                }
            }
        }
    }
}