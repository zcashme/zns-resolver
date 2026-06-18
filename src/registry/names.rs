//! Durable representation of the ZNS name index.
//
// The structure captures:
//   name_events  — append-only per-name chain of binding-verified transitions
//   names        — materialized current tip per name (for resolution + reorg rebuild)
//
// scan_state provides resumability ("checkpoint after commit").
// registry_account holds the inbox viewing key.

/// The SQL to create the name index tables (and supporting state).
pub(super) const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS registry_account (
    id   INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
    uivk TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS scan_state (
    id               INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
    height           INTEGER NOT NULL,
    hash             BLOB,
    chain_tip_height INTEGER,
    chain_tip_hash   BLOB
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
"#;
