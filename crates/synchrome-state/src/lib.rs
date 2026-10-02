//! Local durable state of the desktop app (`<appData>/Synchrome/state.sqlite`, `docs/05` §1.1).
//!
//! Holds, per Synchrome profile on this device: the materialised bookmarks state, the local
//! Chromium-id index (never published), the journal outbox (ops with their per-device `seq`,
//! grouped into chunks) and the publication checkpoint. SQLite in WAL mode is shared by the
//! native-host processes (capture) and the app (publication); every capture batch is one
//! `BEGIN IMMEDIATE` transaction, so an ack sent after commit is never lost.

mod capture;
mod publish;
mod schema;

pub use capture::{CaptureError, CaptureOutcome, Source};
pub use publish::{
    publish, JournalSink, MappingCheck, PublishOutcome, PublishedEvent, SinkError, LEASE_SECONDS,
};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;
use thiserror::Error;
use uuid::Uuid;

/// Journal chunks rotate at this size (`docs/03` §4).
pub const CHUNK_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("local state database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("corrupt local state: {0}")]
    Corrupt(String),
}

pub struct StateDb {
    pub(crate) conn: Connection,
}

impl StateDb {
    pub fn open(path: &Path) -> Result<Self, StateError> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, StateError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StateError> {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }
}

/// What the UI shows for a mapping's bookmarks (`Bookmarks: …`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BookmarksSyncState {
    NotSynced,
    InitialUpload,
    UpToDate,
    PendingChanges,
    Offline,
    Error,
}

/// Token-free, URL-free status for the webview and the extension popup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BookmarkSyncStatus {
    pub state: BookmarksSyncState,
    pub bookmarks: u64,
    pub folders: u64,
    pub pending: u64,
    pub last_upload_at: Option<i64>,
    pub error: Option<String>,
}

impl BookmarkSyncStatus {
    fn not_synced() -> Self {
        Self {
            state: BookmarksSyncState::NotSynced,
            bookmarks: 0,
            folders: 0,
            pending: 0,
            last_upload_at: None,
            error: None,
        }
    }
}

pub(crate) struct ProfileRow {
    pub account_id: String,
    pub device_id: String,
    pub next_seq: u64,
    pub published_through: u64,
    pub initial_through: Option<u64>,
    pub source_mapping_id: Option<String>,
    pub last_source_mapping_id: Option<String>,
    pub source_identity: Option<String>,
    pub last_upload_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_error_kind: Option<String>,
    pub remote_checked: bool,
}

pub(crate) fn load_profile(
    conn: &Connection,
    profile_id: &str,
) -> Result<Option<ProfileRow>, StateError> {
    Ok(conn
        .query_row(
            "SELECT account_id, device_id, next_seq, published_through, initial_through,
                    source_mapping_id, last_source_mapping_id, source_identity, last_upload_at,
                    last_error, last_error_kind, remote_checked
             FROM profiles WHERE profile_id = ?1",
            [profile_id],
            |r| {
                Ok(ProfileRow {
                    account_id: r.get(0)?,
                    device_id: r.get(1)?,
                    next_seq: r.get::<_, i64>(2)? as u64,
                    published_through: r.get::<_, i64>(3)? as u64,
                    initial_through: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                    source_mapping_id: r.get(5)?,
                    last_source_mapping_id: r.get(6)?,
                    source_identity: r.get(7)?,
                    last_upload_at: r.get(8)?,
                    last_error: r.get(9)?,
                    last_error_kind: r.get(10)?,
                    remote_checked: r.get::<_, i64>(11)? != 0,
                })
            },
        )
        .optional()?)
}

impl StateDb {
    /// Ops captured but not yet published for a profile.
    pub fn pending(&self, profile_id: Uuid) -> Result<u64, StateError> {
        Ok(load_profile(&self.conn, &profile_id.to_string())?
            .map(|p| (p.next_seq - 1).saturating_sub(p.published_through))
            .unwrap_or(0))
    }

    /// Profiles with unpublished ops (for the background publisher).
    pub fn profiles_with_pending(&self) -> Result<Vec<Uuid>, StateError> {
        let mut st = self
            .conn
            .prepare("SELECT profile_id FROM profiles WHERE next_seq - 1 > published_through")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(id) = Uuid::parse_str(&r?) {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// The profile whose capture source is `mapping_id`, if any.
    pub fn profile_for_mapping(&self, mapping_id: Uuid) -> Result<Option<Uuid>, StateError> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT profile_id FROM profiles
                 WHERE source_mapping_id = ?1 OR last_source_mapping_id = ?1
                 ORDER BY source_mapping_id = ?1 DESC LIMIT 1",
                [mapping_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.and_then(|s| Uuid::parse_str(&s).ok()))
    }

    /// Bookmarks status as seen from one mapping of `profile_id`.
    pub fn status(
        &self,
        profile_id: Uuid,
        mapping_id: Uuid,
    ) -> Result<BookmarkSyncStatus, StateError> {
        let pid = profile_id.to_string();
        let Some(p) = load_profile(&self.conn, &pid)? else {
            return Ok(BookmarkSyncStatus::not_synced());
        };
        let mid = mapping_id.to_string();
        let mine = p.source_mapping_id.as_deref() == Some(mid.as_str())
            || (p.source_mapping_id.is_none()
                && p.last_source_mapping_id.as_deref() == Some(mid.as_str()));
        if !mine {
            if p.source_mapping_id.is_some() {
                let mut s = BookmarkSyncStatus::not_synced();
                s.state = BookmarksSyncState::Error;
                s.error = Some(
                    "another local profile on this device already publishes bookmarks for this Synchrome profile"
                        .into(),
                );
                return Ok(s);
            }
            return Ok(BookmarkSyncStatus::not_synced());
        }
        let (bookmarks, folders): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(kind = 'url'), 0), COALESCE(SUM(kind = 'folder'), 0)
             FROM entities WHERE profile_id = ?1 AND deleted = 0",
            [&pid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let pending = (p.next_seq - 1).saturating_sub(p.published_through);
        let blocking = matches!(
            p.last_error_kind.as_deref(),
            Some("mapping_deleted")
                | Some("remote_data")
                | Some("other_account")
                | Some("not_signed_in")
                | Some("forbidden")
                | Some("schema_missing")
                | Some("other")
        );
        let state = if p.initial_through.is_none() {
            BookmarksSyncState::NotSynced
        } else if pending > 0 && blocking {
            BookmarksSyncState::Error
        } else if pending > 0 && p.last_error_kind.as_deref() == Some("offline") {
            BookmarksSyncState::Offline
        } else if p.initial_through.is_some_and(|t| p.published_through < t) {
            BookmarksSyncState::InitialUpload
        } else if pending > 0 {
            BookmarksSyncState::PendingChanges
        } else {
            BookmarksSyncState::UpToDate
        };
        Ok(BookmarkSyncStatus {
            state,
            bookmarks: bookmarks as u64,
            folders: folders as u64,
            pending,
            last_upload_at: p.last_upload_at,
            error: if pending > 0 { p.last_error } else { None },
        })
    }

    /// The mapping stopped being the capture source (unmapped in the app). The captured but
    /// unpublished ops stay; another mapping may claim the profile.
    pub fn release_source(&mut self, mapping_id: Uuid) -> Result<(), StateError> {
        self.conn.execute(
            "UPDATE profiles SET last_source_mapping_id = source_mapping_id,
                                 source_mapping_id = NULL
             WHERE source_mapping_id = ?1",
            params![mapping_id.to_string()],
        )?;
        Ok(())
    }

    /// Drop everything known about a profile (cleanup, deleted profile).
    pub fn forget_profile(&mut self, profile_id: Uuid) -> Result<(), StateError> {
        let pid = profile_id.to_string();
        let tx = self.conn.transaction()?;
        for t in [
            "entities",
            "chromium_index",
            "ops",
            "chunks",
            "ext_cursor",
            "lease",
            "profiles",
        ] {
            tx.execute(&format!("DELETE FROM {t} WHERE profile_id = ?1"), [&pid])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Journal chunks of a profile as `(relative path, body)` — what the cloud should hold
    /// for the published part. Diagnostics/tests only.
    pub fn chunk_bodies(&self, profile_id: Uuid) -> Result<Vec<(String, String)>, StateError> {
        publish::chunk_bodies(&self.conn, &profile_id.to_string(), u64::MAX)
            .map(|v| v.into_iter().map(|c| (c.path, c.body)).collect())
    }
}
