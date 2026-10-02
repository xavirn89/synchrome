//! Download: other devices' journal chunks → the local materialised state (`docs/03` §5
//! "pull" + "merge", reduced to what the remote-apply slice needs).
//!
//! - `store_events` say which chunks exist; only chunks past the per-remote-device cursor
//!   are downloaded, and each is verified (contiguous `seq`, author = the journal's device,
//!   range = the event's range) before anything is applied.
//! - A chunk's ops and the cursor move in ONE transaction: a crash re-applies nothing and
//!   loses nothing. A missing chunk or a hole in the event sequence stops that device at the
//!   gap (recorded) until it is filled.
//! - Merge per field by HLC (last writer wins); a `set` newer than a tombstone resurrects.
//!   If the entity has LOCAL ops not yet published, the remote op is NOT applied: it is held
//!   in a conflict record and remote apply stops for that entity (explicit conflict instead
//!   of a silent overwrite).
//! - Every entity whose browser-visible state changed gets `pending_apply` (+ which fields),
//!   which the apply planner turns into explicit browser operations.

use crate::conflict::{ConflictKind, ConflictStatus};
use crate::publish::{JournalSink, MappingCheck, SinkError};
use crate::{load_profile, StateDb, StateError};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::{BTreeMap, BTreeSet};
use synchrome_sync::{HlcClock, Op, OpKind};
use uuid::Uuid;

/// One `store_events` row as the puller needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEvent {
    pub device_id: String,
    pub path: String,
    pub seq_from: u64,
    pub seq_to: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullStats {
    pub chunks: usize,
    pub ops: usize,
    pub conflicts: usize,
    /// Remote devices stopped at a gap: (device, first missing seq).
    pub gaps: Vec<(String, u64)>,
    /// Entities waiting to be applied to the browser after this pull.
    pub remote_pending: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullOutcome {
    NothingToDo,
    Pulled(PullStats),
    Blocked { kind: String },
    Failed { kind: String },
}

fn record_download(
    conn: &rusqlite::Connection,
    pid: &str,
    kind: Option<&str>,
    msg: Option<&str>,
    now: i64,
) -> Result<(), StateError> {
    if kind.is_none() {
        conn.execute(
            "UPDATE profiles SET last_download_at = ?2, download_error = NULL,
                    download_error_kind = NULL, remote_checked = 1
             WHERE profile_id = ?1",
            params![pid, now],
        )?;
    } else {
        conn.execute(
            "UPDATE profiles SET download_error = ?3, download_error_kind = ?2 WHERE profile_id = ?1",
            params![pid, kind, msg],
        )?;
    }
    Ok(())
}

fn fail_kind(e: &SinkError) -> PullOutcome {
    match e {
        SinkError::Offline(_) | SinkError::Other(_) => PullOutcome::Failed {
            kind: e.kind().into(),
        },
        _ => PullOutcome::Blocked {
            kind: e.kind().into(),
        },
    }
}

/// Parse and verify one chunk against its event.
pub(crate) fn verify_chunk(bytes: &[u8], ev: &RemoteEvent) -> Result<Vec<Op>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "chunk is not UTF-8")?;
    let mut ops = Vec::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        ops.push(Op::from_line(line).map_err(|e| format!("bad op line: {e}"))?);
    }
    let (Some(first), Some(last)) = (ops.first(), ops.last()) else {
        return Err("empty chunk".into());
    };
    if first.seq != ev.seq_from || last.seq != ev.seq_to {
        return Err(format!(
            "chunk range {}..{} does not match event {}..{}",
            first.seq, last.seq, ev.seq_from, ev.seq_to
        ));
    }
    if !ops.windows(2).all(|w| w[1].seq == w[0].seq + 1) {
        return Err("non-contiguous seq inside chunk".into());
    }
    if ops.iter().any(|o| o.by != ev.device_id) {
        return Err("op author differs from the journal's device".into());
    }
    if !ev.path.contains(&format!("/journal/{}/", ev.device_id)) {
        return Err("event path does not belong to its device".into());
    }
    Ok(ops)
}

struct Row {
    exists: bool,
    deleted: bool,
    field_hlc: BTreeMap<String, String>,
    deleted_hlc: Option<String>,
    local_seq: i64,
    pending_fields: BTreeSet<String>,
}

fn load_row(tx: &Transaction<'_>, pid: &str, sid: &str) -> Result<Row, StateError> {
    let r: Option<(i64, String, Option<String>, i64, String)> = tx
        .query_row(
            "SELECT deleted, field_hlc, deleted_hlc, local_seq, pending_fields
             FROM entities WHERE profile_id = ?1 AND sid = ?2",
            params![pid, sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    Ok(match r {
        None => Row {
            exists: false,
            deleted: false,
            field_hlc: BTreeMap::new(),
            deleted_hlc: None,
            local_seq: 0,
            pending_fields: BTreeSet::new(),
        },
        Some((deleted, fh, dh, ls, pf)) => Row {
            exists: true,
            deleted: deleted != 0,
            field_hlc: serde_json::from_str(&fh).unwrap_or_default(),
            deleted_hlc: dh,
            local_seq: ls,
            pending_fields: serde_json::from_str(&pf).unwrap_or_default(),
        },
    })
}

const COLUMN: &[(&str, &str)] = &[
    ("parentId", "parent"),
    ("pos", "pos"),
    ("title", "title"),
    ("kind", "kind"),
    ("root", "root"),
    ("url", "url"),
    ("createdAt", "created_at"),
];

/// Apply one remote op. Returns whether it was held in a conflict instead.
///
/// Held: the entity has unpublished LOCAL ops, or an open conflict that is not just an
/// orphan (whose remote ops keep flowing so the remote side can fix it) and not already being
/// resolved (the resolution's op is newer than everything held; later ops merge by LWW).
fn apply_remote_op(
    tx: &Transaction<'_>,
    pid: &str,
    published_through: i64,
    _device: &str,
    op: &Op,
    now: i64,
) -> Result<bool, StateError> {
    if op.col == synchrome_sync::extensions::COLLECTION {
        return crate::ext::apply_remote_op(tx, pid, published_through, op, now);
    }
    if op.col == synchrome_sync::extension_settings::COLLECTION {
        return crate::ext_settings::apply_remote_op(tx, pid, published_through, op, now);
    }
    if op.col == synchrome_sync::browser_prefs::COLLECTION {
        return crate::browser_prefs::apply_remote_op(tx, pid, published_through, op, now);
    }
    if op.col != synchrome_sync::bookmarks::COLLECTION {
        return Ok(false); // a collection this version does not know: skipped
    }
    let row = load_row(tx, pid, &op.id)?;
    let local_pending = row.exists && row.local_seq > published_through;
    let open = crate::conflict::open_for(tx, pid, &op.id)?;
    let hold = match &open {
        Some(c) => c.status == ConflictStatus::Unresolved && c.kind != ConflictKind::Orphan,
        None => local_pending && relevant(tx, pid, published_through, &row, op)?,
    };
    if hold {
        crate::conflict::hold(tx, pid, open, op, published_through, now)?;
        return Ok(true);
    }
    apply_lww(tx, pid, op)?;
    Ok(false)
}

/// Whether a remote op would change anything against this device's state BEFORE its
/// unpublished edits (the BASE values and their HLCs). A stale op — one that per-field LWW
/// discards even without the local edits (e.g. another device's old edit, published late
/// while that device sorted out its own conflict) — is not concurrent with them: it merges
/// normally (and loses) instead of opening a conflict. Without a BASE record, it is held.
fn relevant(
    tx: &Transaction<'_>,
    pid: &str,
    published: i64,
    row: &Row,
    op: &Op,
) -> Result<bool, StateError> {
    let base: Option<String> = tx
        .query_row(
            "SELECT base FROM entities WHERE profile_id = ?1 AND sid = ?2",
            params![pid, op.id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(rec) = base
        .as_deref()
        .and_then(|b| serde_json::from_str::<crate::capture::BaseRec>(b).ok())
        .filter(|b| b.seq > published)
    else {
        return Ok(true);
    };
    // the HLC each field had before the local edits
    let before = |k: &str| -> Option<&str> {
        if rec.f.contains_key(k) {
            rec.h.get(k).map(String::as_str)
        } else {
            row.field_hlc.get(k).map(String::as_str)
        }
    };
    Ok(match op.op {
        OpKind::Set => op.fields.iter().flatten().any(|(k, _)| {
            COLUMN.iter().any(|(f, _)| f == k) && before(k).map_or(true, |h| h < op.hlc.as_str())
        }),
        OpKind::Delete => {
            let newest = row
                .field_hlc
                .keys()
                .filter_map(|k| before(k))
                .max()
                .unwrap_or("");
            newest < op.hlc.as_str()
        }
    })
}

/// Merge one remote op into the materialised state by per-field LWW (no conflict check).
pub(crate) fn apply_lww(tx: &Transaction<'_>, pid: &str, op: &Op) -> Result<(), StateError> {
    if op.col == synchrome_sync::extensions::COLLECTION {
        return crate::ext::lww(tx, pid, op);
    }
    if op.col == synchrome_sync::extension_settings::COLLECTION {
        return crate::ext_settings::lww(tx, pid, op);
    }
    if op.col == synchrome_sync::browser_prefs::COLLECTION {
        return crate::browser_prefs::lww(tx, pid, op);
    }
    if op.col != synchrome_sync::bookmarks::COLLECTION {
        return Ok(());
    }
    let row = load_row(tx, pid, &op.id)?;
    let mut pending = row.pending_fields.clone();
    match op.op {
        OpKind::Set => {
            let fields = op.fields.clone().unwrap_or_default();
            if !row.exists {
                tx.execute(
                    "INSERT INTO entities (profile_id, sid, kind, root, parent, pos, title, url, created_at, deleted, hlc)
                     VALUES (?1, ?2, 'folder', 'other', 'root:other', '', '', NULL, NULL, 0, ?3)",
                    params![pid, op.id, op.hlc],
                )?;
                pending.insert("created".into());
            }
            let mut fh = row.field_hlc.clone();
            let mut changed = false;
            for (k, v) in &fields {
                let Some((_, col)) = COLUMN.iter().find(|(f, _)| f == k) else {
                    continue; // unknown field from a newer schema: ignored
                };
                if fh.get(k).is_some_and(|h| h.as_str() >= op.hlc.as_str()) {
                    continue; // we already hold a newer value for this field
                }
                let sql =
                    format!("UPDATE entities SET {col} = ?3 WHERE profile_id = ?1 AND sid = ?2");
                let val: rusqlite::types::Value = match v {
                    serde_json::Value::Null => rusqlite::types::Value::Null,
                    serde_json::Value::String(s) => rusqlite::types::Value::Text(s.clone()),
                    serde_json::Value::Number(n) => n
                        .as_i64()
                        .map(rusqlite::types::Value::Integer)
                        .unwrap_or(rusqlite::types::Value::Null),
                    other => rusqlite::types::Value::Text(other.to_string()),
                };
                tx.execute(&sql, params![pid, op.id, val])?;
                fh.insert(k.clone(), op.hlc.clone());
                pending.insert(k.clone());
                changed = true;
            }
            let resurrect = row.deleted
                && row
                    .deleted_hlc
                    .as_deref()
                    .map_or(true, |d| d < op.hlc.as_str());
            if resurrect {
                pending.insert("created".into());
                changed = true;
            }
            if changed || !row.exists {
                tx.execute(
                    "UPDATE entities SET field_hlc = ?3, pending_apply = 1, pending_fields = ?4,
                            deleted = CASE WHEN ?5 THEN 0 ELSE deleted END, hlc = ?6
                     WHERE profile_id = ?1 AND sid = ?2",
                    params![
                        pid,
                        op.id,
                        serde_json::to_string(&fh).unwrap_or_default(),
                        serde_json::to_string(&pending).unwrap_or_default(),
                        resurrect,
                        op.hlc
                    ],
                )?;
            }
        }
        OpKind::Delete => {
            if !row.exists {
                // deleted before we ever saw it: keep the tombstone so stale sets stay dead
                tx.execute(
                    "INSERT INTO entities (profile_id, sid, kind, root, parent, pos, title, url, created_at, deleted, hlc, deleted_hlc)
                     VALUES (?1, ?2, 'url', 'other', 'root:other', '', '', NULL, NULL, 1, ?3, ?3)",
                    params![pid, op.id, op.hlc],
                )?;
                return Ok(());
            }
            let newest = row.field_hlc.values().max().cloned().unwrap_or_default();
            if row.deleted || newest.as_str() >= op.hlc.as_str() {
                return Ok(()); // already deleted (idempotent) or a newer set wins
            }
            pending.insert("deleted".into());
            tx.execute(
                "UPDATE entities SET deleted = 1, deleted_hlc = ?3, pending_apply = 1, pending_fields = ?4, hlc = ?3
                 WHERE profile_id = ?1 AND sid = ?2",
                params![pid, op.id, op.hlc, serde_json::to_string(&pending).unwrap_or_default()],
            )?;
        }
    }
    Ok(())
}

pub(crate) fn remote_pending(conn: &rusqlite::Connection, pid: &str) -> Result<u64, StateError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM entities WHERE profile_id = ?1 AND pending_apply = 1 AND conflict = 0",
        [pid],
        |r| r.get::<_, i64>(0),
    )? as u64)
}

/// Pull with the lease already held (or from tests).
pub(crate) async fn pull_locked<S: JournalSink>(
    db: &mut StateDb,
    sink: &S,
    profile_id: Uuid,
    now: i64,
) -> Result<PullOutcome, StateError> {
    let pid = profile_id.to_string();
    let Some(row) = load_profile(&db.conn, &pid)? else {
        return Ok(PullOutcome::NothingToDo);
    };
    // same guards as publication: same account, the source mapping still confirmed
    match sink.account_id().await {
        Ok(a) if a == row.account_id => {}
        Ok(_) => {
            record_download(
                &db.conn,
                &pid,
                Some("other_account"),
                Some("signed in to another account"),
                now,
            )?;
            return Ok(PullOutcome::Blocked {
                kind: "other_account".into(),
            });
        }
        Err(e) => {
            record_download(&db.conn, &pid, Some(e.kind()), Some(&e.to_string()), now)?;
            return Ok(fail_kind(&e));
        }
    }
    let Some(mapping) = row
        .source_mapping_id
        .as_deref()
        .or(row.last_source_mapping_id.as_deref())
        .and_then(|m| Uuid::parse_str(m).ok())
    else {
        return Ok(PullOutcome::NothingToDo);
    };
    match sink.mapping(mapping).await {
        Ok(MappingCheck::Active) => {}
        Ok(MappingCheck::Missing) => {
            record_download(
                &db.conn,
                &pid,
                Some("mapping_deleted"),
                Some("the mapping of this profile was removed"),
                now,
            )?;
            return Ok(PullOutcome::Blocked {
                kind: "mapping_deleted".into(),
            });
        }
        Err(e) => {
            record_download(&db.conn, &pid, Some(e.kind()), Some(&e.to_string()), now)?;
            return Ok(fail_kind(&e));
        }
    }
    let events = match sink.list_events(profile_id).await {
        Ok(e) => e,
        Err(e) => {
            record_download(&db.conn, &pid, Some(e.kind()), Some(&e.to_string()), now)?;
            return Ok(fail_kind(&e));
        }
    };
    let mut by_device: BTreeMap<String, Vec<RemoteEvent>> = BTreeMap::new();
    for ev in events.into_iter().filter(|e| e.device_id != row.device_id) {
        by_device.entry(ev.device_id.clone()).or_default().push(ev);
    }
    let mut stats = PullStats::default();
    for (device, mut evs) in by_device {
        evs.sort_by_key(|e| (e.seq_from, e.seq_to));
        evs.dedup_by_key(|e| (e.seq_from, e.seq_to));
        for ev in evs {
            let cursor: i64 = db
                .conn
                .query_row(
                    "SELECT applied_through FROM remote_cursors WHERE profile_id = ?1 AND device_id = ?2",
                    params![pid, device],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            if ev.seq_to as i64 <= cursor {
                continue; // already applied (or a duplicate event)
            }
            if ev.seq_from as i64 > cursor + 1 {
                stats.gaps.push((device.clone(), cursor as u64 + 1));
                set_gap(&db.conn, &pid, &device, Some(cursor + 1), now)?;
                break;
            }
            let bytes = match sink.get_chunk(&ev.path).await {
                Ok(Some(b)) => b,
                Ok(None) => {
                    stats.gaps.push((device.clone(), cursor as u64 + 1));
                    set_gap(&db.conn, &pid, &device, Some(cursor + 1), now)?;
                    break;
                }
                Err(e) => {
                    record_download(&db.conn, &pid, Some(e.kind()), Some(&e.to_string()), now)?;
                    return Ok(fail_kind(&e));
                }
            };
            let ops = match verify_chunk(&bytes, &ev) {
                Ok(o) => o,
                Err(why) => {
                    record_download(
                        &db.conn,
                        &pid,
                        Some("corrupt"),
                        Some(&format!("remote journal rejected: {why}")),
                        now,
                    )?;
                    return Ok(PullOutcome::Blocked {
                        kind: "corrupt".into(),
                    });
                }
            };
            let tx = db
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            // re-read inside the transaction: a concurrent puller may have advanced it
            let cursor: i64 = tx
                .query_row(
                    "SELECT applied_through FROM remote_cursors WHERE profile_id = ?1 AND device_id = ?2",
                    params![pid, device],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            let published: i64 = tx.query_row(
                "SELECT published_through FROM profiles WHERE profile_id = ?1",
                [&pid],
                |r| r.get(0),
            )?;
            let last: Option<String> = tx
                .query_row("SELECT value FROM meta WHERE key = 'hlc_last'", [], |r| {
                    r.get(0)
                })
                .optional()?;
            let mut clock = HlcClock::new(row.device_id.clone(), last.and_then(|s| s.parse().ok()));
            let mut through = cursor;
            for op in ops.iter().filter(|o| o.seq as i64 > cursor) {
                if apply_remote_op(&tx, &pid, published, &device, op, now)? {
                    stats.conflicts += 1;
                }
                if let Ok(h) = op.hlc.parse() {
                    clock.observe(&h);
                }
                stats.ops += 1;
                through = op.seq as i64;
            }
            if let Some(l) = &clock.last {
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES ('hlc_last', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = MAX(value, excluded.value)",
                    [l.to_string()],
                )?;
            }
            tx.execute(
                "INSERT INTO remote_cursors (profile_id, device_id, applied_through, gap_at, updated_at)
                 VALUES (?1, ?2, ?3, NULL, ?4)
                 ON CONFLICT(profile_id, device_id) DO UPDATE SET applied_through = MAX(applied_through, excluded.applied_through),
                        gap_at = NULL, updated_at = excluded.updated_at",
                params![pid, device, through, now],
            )?;
            tx.commit()?;
            stats.chunks += 1;
        }
    }
    record_download(&db.conn, &pid, None, None, now)?;
    stats.remote_pending = remote_pending(&db.conn, &pid)?;
    Ok(PullOutcome::Pulled(stats))
}

fn set_gap(
    conn: &rusqlite::Connection,
    pid: &str,
    device: &str,
    at: Option<i64>,
    now: i64,
) -> Result<(), StateError> {
    conn.execute(
        "INSERT INTO remote_cursors (profile_id, device_id, applied_through, gap_at, updated_at)
         VALUES (?1, ?2, 0, ?3, ?4)
         ON CONFLICT(profile_id, device_id) DO UPDATE SET gap_at = excluded.gap_at, updated_at = excluded.updated_at",
        params![pid, device, at, now],
    )?;
    Ok(())
}

/// (entity id, kind, remote device, remote seq) of an open conflict.
pub type OpenConflict = (String, String, Option<String>, Option<i64>);

impl StateDb {
    /// Remote cursor of one remote device (tests / diagnostics).
    pub fn remote_cursor(&self, profile_id: Uuid, device: &str) -> Result<u64, StateError> {
        Ok(self
            .conn
            .query_row(
                "SELECT applied_through FROM remote_cursors WHERE profile_id = ?1 AND device_id = ?2",
                params![profile_id.to_string(), device],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0) as u64)
    }

    /// Unresolved conflicts of a profile: (entity id, kind, last held remote device, its seq).
    pub fn open_conflicts(&self, profile_id: Uuid) -> Result<Vec<OpenConflict>, StateError> {
        let mut st = self.conn.prepare(
            "SELECT c.sid, c.kind,
                    (SELECT r.remote_device FROM conflict_remote r WHERE r.conflict_id = c.conflict_id
                     ORDER BY r.hlc DESC LIMIT 1),
                    (SELECT r.remote_seq FROM conflict_remote r WHERE r.conflict_id = c.conflict_id
                     ORDER BY r.hlc DESC LIMIT 1)
             FROM conflicts c
             WHERE c.profile_id = ?1 AND c.status = 'unresolved' ORDER BY c.created_at, c.rowid",
        )?;
        let rows = st.query_map([profile_id.to_string()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
