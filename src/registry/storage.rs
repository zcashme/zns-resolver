//! Durable representation of the ZNS name index.

use rusqlite::Connection;

/// The SQL to create the name index tables (and supporting state).
/// Run once by the writer connection at startup.
pub(crate) const SCHEMA_SQL: &str = r#"
PRAGMA user_version = 6;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA wal_autocheckpoint = 5000;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;

CREATE TABLE IF NOT EXISTS registry_account (
    id          INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
    ufvk        TEXT    NOT NULL,  -- full viewing key (UFVK) for the name-note account
    network     TEXT    NOT NULL,
    birthday    INTEGER NOT NULL,
    -- The sync position: where indexing stopped. NULL = scan from birthday.
    sync_height INTEGER,
    sync_hash   BLOB
);

CREATE TABLE IF NOT EXISTS name_events (
    name         TEXT    NOT NULL,
    height       INTEGER NOT NULL,
    action       TEXT    NOT NULL CHECK (action IN ('claim', 'update', 'release')),
    ua           TEXT    NOT NULL,
    expires_at   TEXT    NOT NULL,
    prev_rcm     BLOB    NOT NULL,
    rcm          BLOB    NOT NULL,
    psi          BLOB    NOT NULL,
    cmx          BLOB    NOT NULL,
    nullifier    BLOB    NOT NULL,
    txid         BLOB    NOT NULL,
    tx_index     INTEGER NOT NULL,
    action_index INTEGER NOT NULL,
    confirmed_mtp INTEGER,
    memo         BLOB    NOT NULL,
    PRIMARY KEY (name, height, txid, action_index)
);
CREATE INDEX IF NOT EXISTS idx_name_events_height ON name_events (height);
CREATE INDEX IF NOT EXISTS idx_name_events_txid  ON name_events (txid);

CREATE TABLE IF NOT EXISTS names (
    name         TEXT    NOT NULL PRIMARY KEY,
    height       INTEGER NOT NULL,
    action       TEXT    NOT NULL CHECK (action IN ('claim', 'update', 'release')),
    ua           TEXT    NOT NULL,
    expires_at   TEXT    NOT NULL,
    prev_rcm     BLOB    NOT NULL,
    rcm          BLOB    NOT NULL,
    psi          BLOB    NOT NULL,
    cmx          BLOB    NOT NULL,
    nullifier    BLOB    NOT NULL,
    txid         BLOB    NOT NULL,
    tx_index     INTEGER NOT NULL,
    action_index INTEGER NOT NULL,
    confirmed_mtp INTEGER,
    memo         BLOB    NOT NULL
);

CREATE TABLE IF NOT EXISTS block_times (
    height INTEGER NOT NULL PRIMARY KEY,
    time   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS implicit_releases (
    name         TEXT    NOT NULL,
    height       INTEGER NOT NULL,
    txid         BLOB    NOT NULL,
    tx_index     INTEGER NOT NULL,
    action_index INTEGER NOT NULL,
    nullifier    BLOB    NOT NULL,
    PRIMARY KEY (name, height, txid, action_index)
);

CREATE TABLE IF NOT EXISTS anchor_facts (
    nullifier          BLOB    NOT NULL PRIMARY KEY,
    value              INTEGER NOT NULL,
    height             INTEGER NOT NULL,
    tx_index           INTEGER NOT NULL,
    action_index       INTEGER NOT NULL,
    name_note_candidates INTEGER NOT NULL,
    -- 1 when this note is a claim's only registry output and it is zero-value.
    claim_successor    INTEGER NOT NULL DEFAULT 0,
    spent_height       INTEGER,
    spent_tx_index     INTEGER,
    spent_action_index INTEGER
);
CREATE INDEX IF NOT EXISTS idx_anchor_facts_value ON anchor_facts (value);

"#;

/// Installs the schema, wiping pre-lineage databases. A database written
/// before the anchor-fact tables cannot be upgraded in place: the old
/// watch table lacks value and canonical-position data, and its names
/// rows lack tx_index. The version gate drops everything and reinstalls,
/// so the next open rescans from the configured birthday. A version 1
/// database has tips and no confirmation times. Keeping those rows would
/// admit a clock-due update as a renewal. A version 2 database stored
/// every one-candidate zero-value note as a successor, so an update or a
/// claim with a second registry output would seat an anchor the mint
/// rejects. A version 3 database stored every zero-value note, so the
/// pool can contain anchors the keygen transaction did not create, and a
/// name may have been bound to one of them. A version 4 database stored
/// a successor for every claim-shaped note, including a claim that was
/// not admitted. A later claim may already have spent that successor and
/// been recorded. A replay from the birthday would not admit that later
/// claim, because the successor it spent would never have entered the
/// pool. A version 5 database did not store transaction expiry. A claim
/// built before the latest release may have been admitted, and a later
/// claim may have spent its successor. Those scans cannot be replayed
/// into the mint's pool. In each case the chain tables and the checkpoint
/// are dropped and the next open replays from the birthday.
pub(crate) fn install_schema(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(
            "DROP TABLE IF EXISTS registry_account;
             DROP TABLE IF EXISTS name_events;
             DROP TABLE IF EXISTS names;
             DROP TABLE IF EXISTS watched_ironwood_notes;
             DROP TABLE IF EXISTS anchor_facts;
             DROP TABLE IF EXISTS implicit_releases;
             DROP TABLE IF EXISTS block_times;",
        )?;
    } else if version < 6 {
        if version == 1 {
            tracing::warn!(
                "schema version 1 has no confirmation times; dropping the scan so it replays from the birthday"
            );
        } else if version == 2 {
            tracing::warn!(
                "schema version 2 cannot tell a claim successor from any other zero-value note; dropping the scan so it replays from the birthday"
            );
        } else if version == 3 {
            tracing::warn!(
                "schema version 3 stored every zero-value note; the ceremony is only the keygen transaction, so the scan replays from the birthday"
            );
        } else if version == 4 {
            tracing::warn!(
                "schema version 4 kept a successor for a claim that was not admitted; a later claim may have spent it, so the scan replays from the birthday"
            );
        } else {
            tracing::warn!(
                "schema version 5 admitted a claim built before the latest release; the scan replays from the birthday"
            );
        }
        conn.execute_batch(
            "DROP TABLE IF EXISTS name_events;
             DROP TABLE IF EXISTS names;
             DROP TABLE IF EXISTS anchor_facts;
             DROP TABLE IF EXISTS implicit_releases;
             DROP TABLE IF EXISTS block_times;
             UPDATE registry_account SET sync_height = NULL, sync_hash = NULL WHERE id = 0;",
        )?;
    }
    conn.execute_batch(SCHEMA_SQL)?;
    Ok(())
}
