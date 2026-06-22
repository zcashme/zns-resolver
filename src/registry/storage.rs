//! Durable representation of the ZNS name index.
//
// WAL is set by the writer's first open and persisted on the DB file; reader
// connections inherit it. `busy_timeout` is set on every connection so the
// writer's `commit` retries with backoff if a reader holds a read lock during
// a WAL checkpoint, instead of returning `SQLITE_BUSY`.

/// The SQL to create the name index tables (and supporting state).
/// Run once by the writer connection at startup.
pub(super) const SCHEMA_SQL: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;

CREATE TABLE IF NOT EXISTS registry_account (
    id       INTEGER NOT NULL PRIMARY KEY CHECK (id = 0),
    ufvk     TEXT    NOT NULL,  -- full viewing key (UFVK) for the name-note account
    network  TEXT    NOT NULL,
    birthday INTEGER NOT NULL
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
    PRIMARY KEY (name, height, txid, action_index)
);
CREATE INDEX IF NOT EXISTS idx_name_events_height ON name_events (height);
CREATE INDEX IF NOT EXISTS idx_name_events_txid  ON name_events (txid);

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

/// Pragmas applied on every reader connection at open time. `journal_mode`
/// is omitted because it is persisted on the DB file by the writer's first
/// open and is a no-op to re-set from a reader.
pub(super) const READER_PRAGMAS: &str = r#"
PRAGMA busy_timeout = 5000;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
"#;
