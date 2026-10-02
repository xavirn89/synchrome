//! `state.sqlite` schema, versioned with `PRAGMA user_version`.

use rusqlite::Connection;

pub(crate) const V1: &str = "
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

/// v2 (M1 slice 8, remote apply): per-field HLCs and local/remote bookkeeping per entity,
/// per-remote-device download cursors, conflict records, download status per profile.
pub(crate) const V2: &str = "
ALTER TABLE entities ADD COLUMN field_hlc TEXT NOT NULL DEFAULT '{}';   -- field -> HLC of its value
ALTER TABLE entities ADD COLUMN deleted_hlc TEXT;
ALTER TABLE entities ADD COLUMN local_seq INTEGER NOT NULL DEFAULT 0;  -- seq of the last LOCAL op
ALTER TABLE entities ADD COLUMN pending_apply INTEGER NOT NULL DEFAULT 0; -- remote change not yet in the browser
ALTER TABLE entities ADD COLUMN pending_fields TEXT NOT NULL DEFAULT '[]';
ALTER TABLE entities ADD COLUMN conflict INTEGER NOT NULL DEFAULT 0;   -- remote apply stopped for it
ALTER TABLE profiles ADD COLUMN last_download_at INTEGER;
ALTER TABLE profiles ADD COLUMN download_error TEXT;
ALTER TABLE profiles ADD COLUMN download_error_kind TEXT;
ALTER TABLE profiles ADD COLUMN applying_since INTEGER;
CREATE TABLE IF NOT EXISTS remote_cursors (
    profile_id TEXT NOT NULL,
    device_id TEXT NOT NULL,                 -- the remote (publishing) device
    applied_through INTEGER NOT NULL DEFAULT 0, -- last remote seq materialised
    gap_at INTEGER,                          -- seq we are waiting for (missing chunk/event)
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (profile_id, device_id)
);
CREATE TABLE IF NOT EXISTS conflicts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    profile_id TEXT NOT NULL,
    sid TEXT NOT NULL,
    kind TEXT NOT NULL,                      -- local_pending | apply_failed
    remote_device TEXT,
    remote_seq INTEGER,
    remote_line TEXT,                        -- the held remote op (not applied)
    detail TEXT,
    created_at INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'open'
);
CREATE INDEX IF NOT EXISTS conflicts_open_idx ON conflicts (profile_id, status);
";

/// v3 (M1 slice 9, conflict resolution): one stable, persisted record per conflicting entity
/// (UUID `conflict_id`, status lifecycle, LOCAL / REMOTE / BASE snapshots, the chosen
/// resolution) plus the remote ops it holds, in order; and the pre-edit values of an entity's
/// unpublished local changes (`entities.base`) so a conflict can show and restore the BASE.
/// The v2 `conflicts` rows (one per held op, status `open`) are folded into the new table.
const V3: &str = "
ALTER TABLE entities ADD COLUMN base TEXT;  -- {seq, f:{field: value}} before the first unpublished local change
CREATE TABLE conflicts_v3 (
    conflict_id TEXT PRIMARY KEY,            -- UUID v4, stable for the conflict's life
    account_id TEXT NOT NULL,
    profile_id TEXT NOT NULL,
    sid TEXT NOT NULL,                       -- entity id
    kind TEXT NOT NULL,                      -- local_pending | apply_failed | orphan
    subtype TEXT NOT NULL,                   -- edit_edit | edit_delete | delete_edit | delete_delete
                                             -- | delete_not_empty | apply_error | orphan
    status TEXT NOT NULL,                    -- unresolved | resolving | resolved | superseded
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    local_json TEXT NOT NULL DEFAULT '{}',   -- LOCAL version (frozen once closed)
    remote_json TEXT NOT NULL DEFAULT '{}',  -- REMOTE version (fold of the held ops)
    base_json TEXT,                          -- BASE (partial) when known
    detail TEXT,                             -- short reason / browser error code (no content)
    attempts INTEGER NOT NULL DEFAULT 0,     -- browser apply attempts that failed
    resolution TEXT,                         -- keep_local | keep_remote | merge | retry | relocate
    resolution_json TEXT,                    -- the chosen version
    resolution_hlc TEXT,                     -- HLC of the op that carries the decision
    resolution_ops INTEGER NOT NULL DEFAULT 0,
    resolved_at INTEGER
);
CREATE UNIQUE INDEX conflicts_one_open ON conflicts_v3 (profile_id, sid)
    WHERE status IN ('unresolved', 'resolving');
CREATE INDEX conflicts_status_idx ON conflicts_v3 (profile_id, status);
CREATE TABLE conflict_remote (
    conflict_id TEXT NOT NULL,
    hlc TEXT NOT NULL,                       -- unique per op (device-suffixed, monotonic)
    remote_device TEXT NOT NULL,
    remote_seq INTEGER,                      -- NULL for a delete reconstructed from a tombstone
    line TEXT NOT NULL,                      -- the held op, canonical journal line
    PRIMARY KEY (conflict_id, hlc)
);
";

pub fn migrate(conn: &Connection) -> Result<(), rusqlite::Error> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v < 1 {
        conn.execute_batch(V1)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    if v < 2 {
        conn.execute_batch(&format!("BEGIN; {V2} COMMIT;"))?;
        conn.pragma_update(None, "user_version", 2)?;
    }
    if v < 3 {
        conn.execute_batch("BEGIN;")?;
        let r = (|| {
            conn.execute_batch(V3)?;
            crate::conflict::migrate_v2_rows(conn)?;
            conn.execute_batch(
                "DROP TABLE conflicts; ALTER TABLE conflicts_v3 RENAME TO conflicts;",
            )
        })();
        match r {
            Ok(()) => conn.execute_batch("COMMIT;")?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(e);
            }
        }
        conn.pragma_update(None, "user_version", 3)?;
    }
    Ok(())
}
