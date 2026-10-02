//! `state.sqlite` schema, versioned with `PRAGMA user_version`.

use rusqlite::Connection;

const V1: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
-- one row per Synchrome profile this device captures for (journal owner = this device)
CREATE TABLE IF NOT EXISTS profiles (
    profile_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    next_seq INTEGER NOT NULL DEFAULT 1,
    published_through INTEGER NOT NULL DEFAULT 0,
    initial_through INTEGER,                 -- last seq of the first snapshot (NULL = none yet)
    source_mapping_id TEXT,                  -- mapping currently allowed to capture
    last_source_mapping_id TEXT,
    source_identity TEXT,                    -- browser|user_data_dir|profile_dir of the source
    remote_checked INTEGER NOT NULL DEFAULT 0, -- first-upload check for other devices done
    last_upload_at INTEGER,
    last_attempt_at INTEGER,
    last_error TEXT,
    last_error_kind TEXT
);
CREATE TABLE IF NOT EXISTS entities (
    profile_id TEXT NOT NULL,
    sid TEXT NOT NULL,
    kind TEXT NOT NULL,
    root TEXT NOT NULL,
    parent TEXT NOT NULL,
    pos TEXT NOT NULL,
    title TEXT NOT NULL,
    url TEXT,
    created_at INTEGER,
    deleted INTEGER NOT NULL DEFAULT 0,
    hlc TEXT NOT NULL,                       -- HLC of the last op that touched the entity
    PRIMARY KEY (profile_id, sid)
);
CREATE TABLE IF NOT EXISTS chromium_index (
    profile_id TEXT NOT NULL,
    chromium_id TEXT NOT NULL,
    sid TEXT NOT NULL,
    PRIMARY KEY (profile_id, chromium_id)
);
CREATE TABLE IF NOT EXISTS ops (
    profile_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    line TEXT NOT NULL,                      -- canonical journal line (bytes never change)
    PRIMARY KEY (profile_id, seq)
);
CREATE TABLE IF NOT EXISTS chunks (
    profile_id TEXT NOT NULL,
    start_seq INTEGER NOT NULL,
    path TEXT NOT NULL,                      -- relative to the account's store root
    month TEXT NOT NULL,                     -- yyyy/mm of the first op
    bytes INTEGER NOT NULL DEFAULT 0,
    closed INTEGER NOT NULL DEFAULT 0,
    end_seq INTEGER NOT NULL,                -- last seq appended so far
    published_through INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (profile_id, start_seq)
);
-- extension delivery dedupe: highest event number applied per service-worker epoch
CREATE TABLE IF NOT EXISTS ext_cursor (
    profile_id TEXT NOT NULL,
    epoch TEXT NOT NULL,
    last_n INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (profile_id, epoch)
);
-- one publisher at a time per profile (app and host processes share this database)
CREATE TABLE IF NOT EXISTS lease (
    profile_id TEXT PRIMARY KEY,
    holder TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);
";

pub fn migrate(conn: &Connection) -> Result<(), rusqlite::Error> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v < 1 {
        conn.execute_batch(V1)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    Ok(())
}
