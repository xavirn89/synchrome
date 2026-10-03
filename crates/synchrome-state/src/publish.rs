//! Publication: outbox chunks → the account's store, idempotently.
//!
//! The open chunk is sealed when a publication starts, so each publication uploads only the
//! new ops and a sealed chunk's bytes never change: a retry after a timeout, a crash between
//! upload and checkpoint, or two publishers racing past an expired lease all write the same
//! object (`x-upsert`). The checkpoint (`published_through`) only advances after the upload **and** the
//! `store_events` row (unique per `seq_to`, ignore-duplicates) succeeded.

use crate::{load_profile, StateDb, StateError};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::future::Future;
use synchrome_sync::Op;
use thiserror::Error;
use uuid::Uuid;

/// A publisher holds the per-profile lease at most this long.
pub const LEASE_SECONDS: i64 = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingCheck {
    /// The mapping exists in the account and is confirmed for this device.
    Active,
    /// The mapping is gone (unmapped, or its profile/device deleted).
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SinkError {
    #[error("Synchrome Cloud unreachable")]
    Offline(String),
    #[error("not signed in")]
    NotSignedIn,
    #[error("forbidden by server policy")]
    Forbidden(String),
    #[error("cloud schema not available")]
    SchemaMissing(String),
    #[error("cloud error")]
    Other(String),
    /// The server refused a write on purpose (a database rule / check), e.g. a watermark
    /// the database does not accept or a lost compaction claim.
    #[error("refused by the server")]
    Rejected(String),
}

impl SinkError {
    pub fn kind(&self) -> &'static str {
        match self {
            SinkError::Offline(_) => "offline",
            SinkError::NotSignedIn => "not_signed_in",
            SinkError::Forbidden(_) => "forbidden",
            SinkError::SchemaMissing(_) => "schema_missing",
            SinkError::Other(_) => "other",
            SinkError::Rejected(_) => "rejected",
        }
    }
}

/// One `store_events` row (metadata of a journal publication).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedEvent {
    pub profile_id: Uuid,
    pub device_id: Uuid,
    pub path: String,
    pub seq_from: u64,
    pub seq_to: u64,
    pub hlc: String,
}

/// The store, as the publisher sees it. Paths are relative to the account's store root.
/// Implementations obtain (and refresh) the session themselves; nothing here sees a token.
pub trait JournalSink: Sync {
    fn account_id(&self) -> impl Future<Output = Result<String, SinkError>> + Send;
    fn mapping(
        &self,
        mapping_id: Uuid,
    ) -> impl Future<Output = Result<MappingCheck, SinkError>> + Send;
    /// Device ids that already have journal objects for this profile.
    fn journal_devices(
        &self,
        profile_id: Uuid,
    ) -> impl Future<Output = Result<Vec<String>, SinkError>> + Send;
    fn put_chunk(
        &self,
        path: &str,
        body: &[u8],
    ) -> impl Future<Output = Result<(), SinkError>> + Send;
    fn record_event(
        &self,
        ev: &PublishedEvent,
    ) -> impl Future<Output = Result<(), SinkError>> + Send;
    /// Journal publications of a profile (all devices; the puller filters its own).
    fn list_events(
        &self,
        profile_id: Uuid,
    ) -> impl Future<Output = Result<Vec<crate::pull::RemoteEvent>, SinkError>> + Send;
    /// One journal chunk; `None` if it does not exist (yet). Also used for snapshot objects.
    fn get_chunk(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, SinkError>> + Send;
    /// Ready snapshot generations of a profile, newest first (at most current + previous).
    /// Stores without compaction have none.
    fn ready_snapshots(
        &self,
        _profile_id: Uuid,
    ) -> impl Future<Output = Result<Vec<crate::compaction::SnapshotRow>, SinkError>> + Send {
        async { Ok(Vec::new()) }
    }
    /// Debug harness only: false forces a full replay even when a snapshot exists.
    fn prefer_snapshot_bootstrap(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    NothingToDo,
    /// Another process holds the lease.
    Busy,
    Published {
        chunks: usize,
        through: u64,
    },
    /// Publication refused until the user acts (kind: mapping_deleted, remote_data, …).
    Blocked {
        kind: String,
    },
    /// Transient or auth failure; the outbox is intact and will be retried.
    Failed {
        kind: String,
        chunks: usize,
    },
}

pub(crate) struct ChunkBody {
    pub start: u64,
    pub path: String,
    pub body: String,
    pub last_seq: u64,
    pub last_hlc: String,
}

pub(crate) fn chunk_bodies(
    conn: &Connection,
    pid: &str,
    only_unpublished_through: u64,
) -> Result<Vec<ChunkBody>, StateError> {
    let mut st = conn.prepare(
        "SELECT start_seq, path, end_seq, published_through FROM chunks
         WHERE profile_id = ?1 ORDER BY start_seq",
    )?;
    let chunks: Vec<(i64, String, i64, i64)> = st
        .query_map([pid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let mut out = Vec::new();
    for (start, path, end, published) in chunks {
        if only_unpublished_through != u64::MAX && end <= published {
            continue;
        }
        let mut q = conn.prepare(
            "SELECT seq, line FROM ops WHERE profile_id = ?1 AND seq BETWEEN ?2 AND ?3 ORDER BY seq",
        )?;
        let lines: Vec<(i64, String)> = q
            .query_map(params![pid, start, end], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let Some((last_seq, last_line)) = lines.last().cloned() else {
            continue; // pruned (fully published, closed)
        };
        if lines.first().map(|l| l.0) != Some(start) {
            return Err(StateError::Corrupt(format!("chunk {path} lost its head")));
        }
        let mut body = String::new();
        for (_, l) in &lines {
            body.push_str(l);
            body.push('\n');
        }
        let last_hlc = Op::from_line(&last_line)
            .map(|o| o.hlc)
            .map_err(|e| StateError::Corrupt(e.to_string()))?;
        out.push(ChunkBody {
            start: start as u64,
            path,
            body,
            last_seq: last_seq as u64,
            last_hlc,
        });
    }
    Ok(out)
}

/// Whether the process that holds a lease is gone. Holders are processes of THIS machine
/// sharing this state.sqlite and are named `<pid>-<uuid>`; a holder killed mid-sync (app quit,
/// host closed by the browser) must not block the profile until the lease expires. Unknown
/// holder formats, or a PID that is alive (or reused), fall back to the expiry.
fn holder_is_dead(holder: &str) -> bool {
    let Some(pid) = holder
        .split_once('-')
        .and_then(|(p, _)| p.parse::<i32>().ok())
        .filter(|p| *p > 0)
    else {
        return false;
    };
    if pid as u32 == std::process::id() {
        return false;
    }
    #[cfg(unix)]
    {
        // SAFETY: kill with signal 0 only checks for existence/permission.
        let r = unsafe { libc::kill(pid, 0) };
        r == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn acquire(conn: &mut Connection, pid: &str, holder: &str, now: i64) -> Result<bool, StateError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let cur: Option<(String, i64)> = tx
        .query_row(
            "SELECT holder, expires_at FROM lease WHERE profile_id = ?1",
            [pid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((h, exp)) = cur {
        if h != holder && exp > now && !holder_is_dead(&h) {
            return Ok(false);
        }
    }
    tx.execute(
        "INSERT INTO lease (profile_id, holder, expires_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(profile_id) DO UPDATE SET holder = excluded.holder, expires_at = excluded.expires_at",
        params![pid, holder, now + LEASE_SECONDS],
    )?;
    tx.commit()?;
    Ok(true)
}

fn release(conn: &Connection, pid: &str, holder: &str) -> Result<(), StateError> {
    conn.execute(
        "DELETE FROM lease WHERE profile_id = ?1 AND holder = ?2",
        params![pid, holder],
    )?;
    Ok(())
}

fn record_error(
    conn: &Connection,
    pid: &str,
    kind: &str,
    msg: &str,
    now: i64,
) -> Result<(), StateError> {
    conn.execute(
        "UPDATE profiles SET last_error = ?3, last_error_kind = ?2, last_attempt_at = ?4
         WHERE profile_id = ?1",
        params![pid, kind, msg, now],
    )?;
    Ok(())
}

fn user_message(e: &SinkError) -> String {
    match e {
        SinkError::Offline(_) => {
            "Synchrome Cloud is unreachable; changes are kept and will be uploaded".into()
        }
        SinkError::NotSignedIn => "sign in to upload pending changes".into(),
        SinkError::Forbidden(_) => "the cloud refused the upload (policy)".into(),
        SinkError::SchemaMissing(_) => {
            "the cloud is not ready for bookmarks (migration 0004 missing)".into()
        }
        SinkError::Other(_) | SinkError::Rejected(_) => "upload failed; will retry".into(),
    }
}

/// Publish every unpublished chunk of `profile_id`, in order, stopping at the first failure.
pub async fn publish<S: JournalSink>(
    db: &mut StateDb,
    sink: &S,
    profile_id: Uuid,
    holder: &str,
    now: i64,
) -> Result<PublishOutcome, StateError> {
    let pid = profile_id.to_string();
    let Some(row) = load_profile(&db.conn, &pid)? else {
        return Ok(PublishOutcome::NothingToDo);
    };
    if row.next_seq - 1 <= row.published_through {
        return Ok(PublishOutcome::NothingToDo);
    }
    if !acquire(&mut db.conn, &pid, holder, now)? {
        return Ok(PublishOutcome::Busy);
    }
    let out = publish_locked(db, sink, profile_id, &pid, now).await;
    release(&db.conn, &pid, holder)?;
    out
}

/// Pull (download + materialise) then publish, under one lease. The usual cycle: the pull
/// before the push is what lets local and remote changes meet in the materialised state.
pub async fn sync<S: JournalSink>(
    db: &mut StateDb,
    sink: &S,
    profile_id: Uuid,
    holder: &str,
    now: i64,
) -> Result<(crate::pull::PullOutcome, PublishOutcome), StateError> {
    let pid = profile_id.to_string();
    if load_profile(&db.conn, &pid)?.is_none() {
        return Ok((
            crate::pull::PullOutcome::NothingToDo,
            PublishOutcome::NothingToDo,
        ));
    }
    if !acquire(&mut db.conn, &pid, holder, now)? {
        return Ok((crate::pull::PullOutcome::NothingToDo, PublishOutcome::Busy));
    }
    let pulled = crate::pull::pull_locked(db, sink, profile_id, now).await;
    let published = match &pulled {
        Ok(_) => {
            let row = load_profile(&db.conn, &pid)?;
            if row.is_some_and(|r| r.next_seq - 1 > r.published_through) {
                publish_locked(db, sink, profile_id, &pid, now).await
            } else {
                Ok(PublishOutcome::NothingToDo)
            }
        }
        Err(_) => Ok(PublishOutcome::NothingToDo),
    };
    release(&db.conn, &pid, holder)?;
    Ok((pulled?, published?))
}

/// Pull only, under the lease.
pub async fn pull<S: JournalSink>(
    db: &mut StateDb,
    sink: &S,
    profile_id: Uuid,
    holder: &str,
    now: i64,
) -> Result<crate::pull::PullOutcome, StateError> {
    let pid = profile_id.to_string();
    if load_profile(&db.conn, &pid)?.is_none() {
        return Ok(crate::pull::PullOutcome::NothingToDo);
    }
    if !acquire(&mut db.conn, &pid, holder, now)? {
        return Ok(crate::pull::PullOutcome::NothingToDo);
    }
    let r = crate::pull::pull_locked(db, sink, profile_id, now).await;
    release(&db.conn, &pid, holder)?;
    r
}

async fn publish_locked<S: JournalSink>(
    db: &mut StateDb,
    sink: &S,
    profile_id: Uuid,
    pid: &str,
    now: i64,
) -> Result<PublishOutcome, StateError> {
    let row = load_profile(&db.conn, pid)?.ok_or_else(|| StateError::Corrupt("profile".into()))?;
    let fail = |db: &StateDb, e: SinkError, chunks: usize| -> Result<PublishOutcome, StateError> {
        record_error(&db.conn, pid, e.kind(), &user_message(&e), now)?;
        Ok(match e {
            SinkError::Offline(_) | SinkError::Other(_) => PublishOutcome::Failed {
                kind: e.kind().into(),
                chunks,
            },
            _ => PublishOutcome::Blocked {
                kind: e.kind().into(),
            },
        })
    };
    let block = |db: &StateDb, kind: &str, msg: &str| -> Result<PublishOutcome, StateError> {
        record_error(&db.conn, pid, kind, msg, now)?;
        Ok(PublishOutcome::Blocked { kind: kind.into() })
    };

    // 1. same account as the one that captured
    match sink.account_id().await {
        Ok(a) if a == row.account_id => {}
        Ok(_) => {
            return block(
                db,
                "other_account",
                "signed in to another account; changes stay local",
            )
        }
        Err(e) => return fail(db, e, 0),
    }
    // 2. the capture source must still be a confirmed mapping of this device
    let Some(mapping) = row
        .source_mapping_id
        .as_deref()
        .or(row.last_source_mapping_id.as_deref())
        .and_then(|m| Uuid::parse_str(m).ok())
    else {
        return Ok(PublishOutcome::NothingToDo);
    };
    match sink.mapping(mapping).await {
        Ok(MappingCheck::Active) => {}
        Ok(MappingCheck::Missing) => {
            return block(
                db,
                "mapping_deleted",
                "the mapping of this profile was removed; changes are kept locally",
            )
        }
        Err(e) => return fail(db, e, 0),
    }
    // 3. never publish before having pulled once: the remote state must be materialised
    //    first so that a joining device pairs its local nodes with existing entities
    //    instead of duplicating them (bootstrap, docs/03 §5)
    if !row.remote_checked {
        match crate::pull::pull_locked(db, sink, profile_id, now).await? {
            crate::pull::PullOutcome::Pulled(_) | crate::pull::PullOutcome::NothingToDo => {}
            crate::pull::PullOutcome::Blocked { kind } => {
                return Ok(PublishOutcome::Blocked { kind })
            }
            crate::pull::PullOutcome::Failed { kind } => {
                return Ok(PublishOutcome::Failed { kind, chunks: 0 })
            }
        }
    }
    // 4. seal the open chunk: from now on its bytes never change (a retry re-sends exactly
    //    the same object) and ops captured meanwhile go to the next chunk, so every
    //    publication uploads only the delta.
    db.conn.execute(
        "UPDATE chunks SET closed = 1 WHERE profile_id = ?1 AND closed = 0",
        [pid],
    )?;
    // 5. chunks in order
    let device_id = Uuid::parse_str(&row.device_id).unwrap_or(Uuid::nil());
    let bodies = chunk_bodies(&db.conn, pid, row.published_through)?;
    let mut done = 0;
    let mut through = row.published_through;
    for c in bodies {
        if let Err(e) = sink.put_chunk(&c.path, c.body.as_bytes()).await {
            return fail(db, e, done);
        }
        let ev = PublishedEvent {
            profile_id,
            device_id,
            path: c.path.clone(),
            seq_from: c.start,
            seq_to: c.last_seq,
            hlc: c.last_hlc.clone(),
        };
        if let Err(e) = sink.record_event(&ev).await {
            return fail(db, e, done);
        }
        db.conn.execute(
            "UPDATE chunks SET published_through = ?3 WHERE profile_id = ?1 AND start_seq = ?2",
            params![pid, c.start as i64, c.last_seq as i64],
        )?;
        through = through.max(c.last_seq);
        db.conn.execute(
            "UPDATE profiles SET published_through = ?2, last_upload_at = ?3, last_attempt_at = ?3,
                    last_error = NULL, last_error_kind = NULL
             WHERE profile_id = ?1",
            params![pid, through as i64, now],
        )?;
        done += 1;
    }
    // 6. prune ops of sealed, fully published chunks
    db.conn.execute(
        "DELETE FROM ops WHERE profile_id = ?1 AND seq IN (
             SELECT o.seq FROM ops o JOIN chunks c
               ON c.profile_id = o.profile_id AND o.seq BETWEEN c.start_seq AND c.end_seq
             WHERE o.profile_id = ?1 AND c.closed = 1 AND c.published_through >= c.end_seq)",
        [pid],
    )?;
    Ok(PublishOutcome::Published {
        chunks: done,
        through,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lease_of_a_dead_local_process_is_taken_over_at_once() {
        let mut db = StateDb::open_in_memory().unwrap();
        // a child process that has exited: its PID no longer exists
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let mine = format!("{}-me", std::process::id());
        assert!(acquire(&mut db.conn, "p", &format!("{dead}-gone"), 100).unwrap());
        assert!(
            acquire(&mut db.conn, "p", &mine, 101).unwrap(),
            "dead holder"
        );
        // a live holder (this process, another holder name) still blocks until expiry
        let live = format!("{}-other", std::process::id());
        assert!(
            !acquire(&mut db.conn, "p", &live, 102).unwrap(),
            "live holder"
        );
        // unknown formats keep the expiry semantics
        assert!(acquire(&mut db.conn, "q", "h", 100).unwrap());
        assert!(!acquire(&mut db.conn, "q", "other", 101).unwrap());
        assert!(acquire(&mut db.conn, "q", "other", 100 + LEASE_SECONDS + 1).unwrap());
    }
}
