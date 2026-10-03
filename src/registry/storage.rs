//! Durable representation of the ZNS name index.

use rusqlite::Connection;

/// The SQL to create the name index tables (and supporting state).
/// Run once by the writer connection at startup.
pub(crate) const SCHEMA_SQL: &str = r#"
PRAGMA user_version = 1;
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
    memo         BLOB    NOT NULL
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

/// Installs the single supported schema. Older database files are not
/// migrated; remove them before running a build with a changed schema.
pub(crate) fn install_schema(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != 0 && version != 1 {
        tracing::error!(
            version,
            "unsupported registry database; delete the old SQLite file and rescan"
        );
        return Err(rusqlite::Error::InvalidQuery);
    }
    conn.execute_batch(SCHEMA_SQL)?;
    Ok(())
}
