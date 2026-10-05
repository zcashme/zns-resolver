//! Durable representation of the ZNS name index.

use rusqlite::Connection;

/// Connection-local settings. WAL mode persists in the database file, but
/// these apply only to the current connection, so they must be reapplied on
/// every open — not just when a fresh database is initialized.
pub(crate) const CONNECTION_SQL: &str = r#"
PRAGMA synchronous = NORMAL;
PRAGMA wal_autocheckpoint = 5000;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;
"#;

/// The SQL to create the name index tables (and supporting state).
/// Run once, when an empty database file is initialized.
pub(crate) const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;

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

-- Older databases used user_version = 1 but never this application ID.
-- Mark the schema only after all tables have been created.
PRAGMA application_id = 1515082545; -- ZNS1
PRAGMA user_version = 1;
"#;

/// Opens only this schema. An older database must be removed by the operator
/// and rebuilt by scanning from the birthday; no old checkpoint is reused.
pub(crate) fn install_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(CONNECTION_SQL)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let application_id: i64 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let objects: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if objects == 0 && version == 0 && application_id == 0 {
        return conn.execute_batch(SCHEMA_SQL);
    }
    if version == 1 && application_id == 1515082545 {
        return Ok(());
    }
    tracing::error!(
        version,
        application_id,
        "unsupported registry database; remove the old SQLite file and rescan"
    );
    Err(rusqlite::Error::InvalidQuery)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The connection-local pragmas survive into later opens: the fast path
    /// for an already-marked database must not skip them.
    #[test]
    fn connection_settings_apply_on_every_open() {
        // A process-unique directory: parallel test runs must not share it.
        let dir =
            std::env::temp_dir().join(format!("zns-install-schema-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("connection-settings.sqlite");
        let _ = std::fs::remove_file(&path);

        let conn = Connection::open(&path).unwrap();
        install_schema(&conn).unwrap();
        drop(conn);

        // A fresh connection on an existing database: the early-return path.
        let conn = Connection::open(&path).unwrap();
        install_schema(&conn).unwrap();
        let synchronous: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        let checkpoint: i64 = conn
            .query_row("PRAGMA wal_autocheckpoint", [], |r| r.get(0))
            .unwrap();
        assert_eq!(synchronous, 1, "synchronous = NORMAL on reopen");
        assert_eq!(checkpoint, 5000, "checkpoint threshold survives reopen");
    }
}
