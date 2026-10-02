//! Capture: extension observations → normalised changes → ops in the outbox, atomically.

use crate::{load_profile, StateDb, StateError, CHUNK_MAX_BYTES};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use synchrome_sync::bookmarks::{
    self, BookmarkEvent, BookmarkState, Change, ChromiumNode, Entity, Kind, NormalizeError, Root,
};
use synchrome_sync::{HlcClock, HlcError, Op, OpKind};
use thiserror::Error;
use uuid::Uuid;

/// The confirmed mapping a capture comes from (established by the handshake).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub mapping_id: Uuid,
    pub account_id: String,
    pub device_id: Uuid,
    pub profile_id: Uuid,
    /// `browser|user_data_dir|profile_dir` of the local profile.
    pub identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// Ops created and durable.
    Applied { ops: usize, pending: u64 },
    /// Every event of the batch was already applied (redelivery).
    Duplicate { pending: u64 },
    /// An event referenced an unknown node; nothing was written; send a snapshot.
    ResyncRequired(String),
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("another local profile on this device already publishes bookmarks for this Synchrome profile")]
    SourceConflict,
    #[error("local state of this profile belongs to another account or device")]
    OtherAccount,
    #[error(transparent)]
    Clock(#[from] HlcError),
    #[error(transparent)]
    State(#[from] StateError),
}

impl From<rusqlite::Error> for CaptureError {
    fn from(e: rusqlite::Error) -> Self {
        CaptureError::State(StateError::Db(e))
    }
}

/// UTC `yyyy/mm` for an epoch-ms timestamp (civil-from-days).
pub(crate) fn year_month(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if mo <= 2 { 1 } else { 0 };
    format!("{y:04}/{mo:02}")
}

pub(crate) fn claim(tx: &Transaction<'_>, src: &Source) -> Result<(), CaptureError> {
    let pid = src.profile_id.to_string();
    let mid = src.mapping_id.to_string();
    match load_profile(tx, &pid)? {
        None => {
            tx.execute(
                "INSERT INTO profiles (profile_id, account_id, device_id, source_mapping_id, source_identity)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![pid, src.account_id, src.device_id.to_string(), mid, src.identity],
            )?;
        }
        Some(p) => {
            if p.account_id != src.account_id || p.device_id != src.device_id.to_string() {
                return Err(CaptureError::OtherAccount);
            }
            match p.source_mapping_id.as_deref() {
                Some(m) if m == mid => {}
                Some(_) => return Err(CaptureError::SourceConflict),
                None => {
                    if p.source_identity.as_deref() != Some(src.identity.as_str()) {
                        // Different local profile: its Chromium ids mean nothing here. The
                        // snapshot reconcile pairs its nodes with the existing entities.
                        tx.execute("DELETE FROM chromium_index WHERE profile_id = ?1", [&pid])?;
                    }
                    tx.execute(
                        "UPDATE profiles SET source_mapping_id = ?2, source_identity = ?3,
                                last_error = NULL, last_error_kind = NULL
                         WHERE profile_id = ?1",
                        params![pid, mid, src.identity],
                    )?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn load_state(tx: &Transaction<'_>, pid: &str) -> Result<BookmarkState, StateError> {
    let mut s = BookmarkState::default();
    let mut st = tx.prepare(
        "SELECT sid, kind, root, parent, pos, title, url, created_at, deleted, pending_apply
         FROM entities WHERE profile_id = ?1",
    )?;
    let rows = st.query_map([pid], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<i64>>(7)?,
            r.get::<_, i64>(8)?,
            r.get::<_, i64>(9)?,
        ))
    })?;
    for r in rows {
        let (sid, kind, root, parent, pos, title, url, created_at, deleted, pending) = r?;
        if pending != 0 {
            s.frozen.insert(sid.clone());
        }
        let e = Entity {
            sid: sid.clone(),
            kind: Kind::parse(&kind).ok_or_else(|| StateError::Corrupt("kind".into()))?,
            root: Root::parse(&root).ok_or_else(|| StateError::Corrupt("root".into()))?,
            parent,
            pos,
            title,
            url,
            created_at,
            deleted: deleted != 0,
        };
        s.entities.insert(sid, e);
    }
    let mut st = tx.prepare("SELECT chromium_id, sid FROM chromium_index WHERE profile_id = ?1")?;
    let rows = st.query_map([pid], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    for r in rows {
        let (c, sid) = r?;
        s.index.insert(c, sid);
    }
    Ok(s)
}

pub(crate) fn clock(tx: &Transaction<'_>, device: &str) -> Result<HlcClock, StateError> {
    let last: Option<String> = tx
        .query_row("SELECT value FROM meta WHERE key = 'hlc_last'", [], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(HlcClock::new(device, last.and_then(|s| s.parse().ok())))
}

fn append_chunk(
    tx: &Transaction<'_>,
    pid: &str,
    did: &str,
    seq: u64,
    line_len: usize,
    wall_ms: u64,
) -> Result<(), StateError> {
    let month = year_month(wall_ms);
    let cur: Option<(i64, i64, String)> = tx
        .query_row(
            "SELECT start_seq, bytes, month FROM chunks WHERE profile_id = ?1 AND closed = 0",
            [pid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let need_new = match &cur {
        None => true,
        Some((_, bytes, m)) => {
            m != &month || (*bytes > 0 && *bytes as usize + line_len + 1 > CHUNK_MAX_BYTES)
        }
    };
    let start = if need_new {
        if let Some((s, _, _)) = cur {
            tx.execute(
                "UPDATE chunks SET closed = 1 WHERE profile_id = ?1 AND start_seq = ?2",
                params![pid, s],
            )?;
        }
        let path = format!("profiles/{pid}/journal/{did}/{month}/{seq:08}.jsonl");
        tx.execute(
            "INSERT INTO chunks (profile_id, start_seq, path, month, bytes, closed, end_seq)
             VALUES (?1, ?2, ?3, ?4, 0, 0, ?2)",
            params![pid, seq as i64, path, month],
        )?;
        seq as i64
    } else {
        cur.unwrap().0
    };
    tx.execute(
        "UPDATE chunks SET bytes = bytes + ?3, end_seq = ?4 WHERE profile_id = ?1 AND start_seq = ?2",
        params![pid, start, (line_len + 1) as i64, seq as i64],
    )?;
    Ok(())
}

/// One op to append to this device's journal.
pub(crate) struct OpSpec {
    pub col: &'static str,
    pub id: String,
    pub kind: OpKind,
    pub fields: Option<synchrome_sync::Fields>,
}

/// Append ops to this device's journal (outbox): per-device `seq`, a fresh HLC each, the
/// canonical line, chunk accounting. Shared by every collection (one journal per device).
pub(crate) fn emit_ops(
    tx: &Transaction<'_>,
    pid: &str,
    did: &str,
    specs: &[OpSpec],
    now_ms: u64,
) -> Result<Vec<Op>, CaptureError> {
    let mut next_seq: i64 = tx.query_row(
        "SELECT next_seq FROM profiles WHERE profile_id = ?1",
        [pid],
        |r| r.get(0),
    )?;
    let mut clk = clock(tx, did)?;
    let mut out = Vec::with_capacity(specs.len());
    for sp in specs {
        let hlc = clk.tick(now_ms)?;
        let op = Op {
            seq: next_seq as u64,
            hlc: hlc.to_string(),
            col: sp.col.into(),
            id: sp.id.clone(),
            op: sp.kind,
            fields: sp.fields.clone(),
            by: did.to_string(),
        };
        let line = op.to_line();
        tx.execute(
            "INSERT INTO ops (profile_id, seq, line) VALUES (?1, ?2, ?3)",
            params![pid, next_seq, line],
        )?;
        append_chunk(tx, pid, did, next_seq as u64, line.len(), hlc.wall_ms)?;
        out.push(op);
        next_seq += 1;
    }
    if let Some(last) = &clk.last {
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('hlc_last', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [last.to_string()],
        )?;
    }
    tx.execute(
        "UPDATE profiles SET next_seq = ?2 WHERE profile_id = ?1",
        params![pid, next_seq],
    )?;
    Ok(out)
}

/// Value of an entity column as a journal field value (for BASE snapshots).
fn prior_value(
    field: &str,
    prior: &(i64, String, Option<String>, String, String, String),
) -> Option<serde_json::Value> {
    let (deleted, title, url, parent, pos, root) = prior;
    Some(match field {
        "title" => title.clone().into(),
        "url" => url
            .clone()
            .map(Into::into)
            .unwrap_or(serde_json::Value::Null),
        "parentId" => parent.clone().into(),
        "pos" => pos.clone().into(),
        "root" => root.clone().into(),
        "deleted" => (*deleted != 0).into(),
        _ => return None,
    })
}

/// Pre-edit values of an entity's unpublished local changes: `seq` is the first local op of
/// the current unpublished run; a record whose `seq` is already published is stale.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct BaseRec {
    pub seq: i64,
    #[serde(default)]
    pub f: std::collections::BTreeMap<String, serde_json::Value>,
    /// HLCs of those pre-edit values (`deleted` = the previous tombstone HLC, if any).
    #[serde(default)]
    pub h: std::collections::BTreeMap<String, String>,
}

/// Stamp changes with seq/HLC, write ops + chunk accounting + dirty state. Returns #ops.
/// Also used by the conflict resolver: a resolution is a set of ordinary local ops.
pub(crate) fn persist(
    tx: &Transaction<'_>,
    pid: &str,
    did: &str,
    state: &BookmarkState,
    changes: &[Change],
    now_ms: u64,
) -> Result<usize, CaptureError> {
    // per entity: (HLC per field set by these changes, deleted HLC, last local seq)
    #[derive(Default)]
    struct Touch {
        fields: Vec<(String, String)>,
        deleted: Option<String>,
        seq: i64,
        first_seq: i64,
    }
    let specs: Vec<OpSpec> = changes
        .iter()
        .map(|ch| match ch {
            Change::Set { sid, fields } => OpSpec {
                col: bookmarks::COLLECTION,
                id: sid.clone(),
                kind: OpKind::Set,
                fields: Some(fields.clone()),
            },
            Change::Delete { sid } => OpSpec {
                col: bookmarks::COLLECTION,
                id: sid.clone(),
                kind: OpKind::Delete,
                fields: None,
            },
        })
        .collect();
    let ops = emit_ops(tx, pid, did, &specs, now_ms)?;
    let mut touched: std::collections::HashMap<&str, Touch> = Default::default();
    let mut hlc_of: std::collections::HashMap<&str, String> = Default::default();
    for (ch, op) in changes.iter().zip(&ops) {
        let t = touched.entry(ch.sid()).or_default();
        if t.first_seq == 0 {
            t.first_seq = op.seq as i64;
        }
        match ch {
            Change::Set { fields, .. } => {
                for k in fields.keys() {
                    t.fields.push((k.clone(), op.hlc.clone()));
                }
            }
            Change::Delete { .. } => t.deleted = Some(op.hlc.clone()),
        }
        t.seq = op.seq as i64;
        hlc_of.insert(ch.sid(), op.hlc.clone());
    }
    let published: i64 = tx.query_row(
        "SELECT published_through FROM profiles WHERE profile_id = ?1",
        [pid],
        |r| r.get(0),
    )?;
    for sid in &state.dirty_entities {
        let e = &state.entities[sid];
        let hlc = hlc_of.get(sid.as_str()).cloned().unwrap_or_default();
        type Prior = (i64, String, Option<String>, String, String, String);
        let prior: Option<(Prior, Option<String>)> = tx
            .query_row(
                "SELECT deleted, title, url, parent, pos, root, base FROM entities
                 WHERE profile_id = ?1 AND sid = ?2",
                params![pid, sid],
                |r| {
                    Ok((
                        (
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ),
                        r.get(6)?,
                    ))
                },
            )
            .optional()?;
        tx.execute(
            "INSERT INTO entities (profile_id, sid, kind, root, parent, pos, title, url, created_at, deleted, hlc)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(profile_id, sid) DO UPDATE SET kind = excluded.kind, root = excluded.root,
               parent = excluded.parent, pos = excluded.pos, title = excluded.title, url = excluded.url,
               created_at = COALESCE(entities.created_at, excluded.created_at),
               deleted = excluded.deleted,
               hlc = CASE WHEN excluded.hlc = '' THEN entities.hlc ELSE excluded.hlc END",
            params![
                pid,
                sid,
                e.kind.as_str(),
                e.root.as_str(),
                e.parent,
                e.pos,
                e.title,
                e.url,
                e.created_at,
                e.deleted as i64,
                hlc
            ],
        )?;
        if let Some(t) = touched.get(sid.as_str()) {
            let cur: String = tx.query_row(
                "SELECT field_hlc FROM entities WHERE profile_id = ?1 AND sid = ?2",
                params![pid, sid],
                |r| r.get(0),
            )?;
            let mut fh: std::collections::BTreeMap<String, String> =
                serde_json::from_str(&cur).unwrap_or_default();
            let prior_hlc = fh.clone();
            let prior_deleted_hlc: Option<String> = tx.query_row(
                "SELECT deleted_hlc FROM entities WHERE profile_id = ?1 AND sid = ?2",
                params![pid, sid],
                |r| r.get(0),
            )?;
            for (k, h) in &t.fields {
                fh.insert(k.clone(), h.clone());
            }
            // BASE: the values before the first unpublished local change of each field
            let base = prior.as_ref().map(|(p, b)| {
                let mut rec = b
                    .as_deref()
                    .and_then(|b| serde_json::from_str::<BaseRec>(b).ok())
                    .filter(|b| b.seq > published)
                    .unwrap_or(BaseRec {
                        seq: t.first_seq,
                        f: Default::default(),
                        h: Default::default(),
                    });
                let names = t
                    .fields
                    .iter()
                    .map(|(k, _)| k.as_str())
                    .chain(t.deleted.as_ref().map(|_| "deleted"));
                for k in names {
                    if !rec.f.contains_key(k) {
                        if let Some(v) = prior_value(k, p) {
                            rec.f.insert(k.to_string(), v);
                            let h = if k == "deleted" {
                                prior_deleted_hlc.clone()
                            } else {
                                prior_hlc.get(k).cloned()
                            };
                            if let Some(h) = h {
                                rec.h.insert(k.to_string(), h);
                            }
                        }
                    }
                }
                serde_json::to_string(&rec).unwrap_or_default()
            });
            tx.execute(
                "UPDATE entities SET field_hlc = ?3, local_seq = ?4,
                        deleted_hlc = COALESCE(?5, deleted_hlc), base = ?6
                 WHERE profile_id = ?1 AND sid = ?2",
                params![
                    pid,
                    sid,
                    serde_json::to_string(&fh).unwrap_or_else(|_| "{}".into()),
                    t.seq,
                    t.deleted,
                    base
                ],
            )?;
        }
    }
    for cid in &state.dirty_index {
        match state.index.get(cid) {
            Some(sid) => tx.execute(
                "INSERT INTO chromium_index (profile_id, chromium_id, sid) VALUES (?1, ?2, ?3)
                 ON CONFLICT(profile_id, chromium_id) DO UPDATE SET sid = excluded.sid",
                params![pid, cid, sid],
            )?,
            None => tx.execute(
                "DELETE FROM chromium_index WHERE profile_id = ?1 AND chromium_id = ?2",
                params![pid, cid],
            )?,
        };
    }
    Ok(changes.len())
}

pub(crate) fn set_cursor(
    tx: &Transaction<'_>,
    pid: &str,
    epoch: &str,
    n: u64,
    now_ms: u64,
) -> Result<(), StateError> {
    tx.execute(
        "INSERT INTO ext_cursor (profile_id, epoch, last_n, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(profile_id, epoch) DO UPDATE SET last_n = MAX(last_n, excluded.last_n),
                                                      updated_at = excluded.updated_at",
        params![pid, epoch, n as i64, now_ms as i64],
    )?;
    // forget epochs idle for a week
    tx.execute(
        "DELETE FROM ext_cursor WHERE profile_id = ?1 AND updated_at < ?2",
        params![pid, now_ms as i64 - 7 * 86_400_000],
    )?;
    Ok(())
}

pub(crate) fn pending_of(tx: &Transaction<'_>, pid: &str) -> Result<u64, StateError> {
    Ok(tx.query_row(
        "SELECT next_seq - 1 - published_through FROM profiles WHERE profile_id = ?1",
        [pid],
        |r| r.get::<_, i64>(0),
    )? as u64)
}

impl StateDb {
    /// Claim the profile for this source without capturing anything (so a joining device can
    /// pull the remote state BEFORE its first snapshot and pair instead of duplicating).
    pub fn ensure_claimed(&mut self, src: &Source) -> Result<(), CaptureError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        claim(&tx, src)?;
        tx.commit()?;
        Ok(())
    }

    /// Whether this profile has completed at least one pull.
    pub fn has_pulled(&self, profile_id: Uuid) -> Result<bool, StateError> {
        Ok(load_profile(&self.conn, &profile_id.to_string())?.is_some_and(|p| p.remote_checked))
    }

    /// Every profile this device keeps state for.
    pub fn profile_ids(&self) -> Result<Vec<Uuid>, StateError> {
        let mut st = self.conn.prepare("SELECT profile_id FROM profiles")?;
        let ids = st
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok().and_then(|s| Uuid::parse_str(&s).ok()))
            .collect();
        Ok(ids)
    }

    /// Reconcile a full tree (initial capture, extension start, resync). Idempotent.
    pub fn capture_snapshot(
        &mut self,
        src: &Source,
        epoch: &str,
        covers_through: u64,
        nodes: &[ChromiumNode],
        now_ms: u64,
    ) -> Result<CaptureOutcome, CaptureError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        claim(&tx, src)?;
        let pid = src.profile_id.to_string();
        let did = src.device_id.to_string();
        let mut state = load_state(&tx, &pid)?;
        let changes = bookmarks::reconcile(&mut state, nodes);
        let n = persist(&tx, &pid, &did, &state, &changes, now_ms)?;
        crate::conflict::after_local_changes(&tx, &pid, &changes, (now_ms / 1000) as i64)?;
        tx.execute(
            "UPDATE profiles SET initial_through = COALESCE(initial_through, next_seq - 1)
             WHERE profile_id = ?1",
            [&pid],
        )?;
        set_cursor(&tx, &pid, epoch, covers_through, now_ms)?;
        let pending = pending_of(&tx, &pid)?;
        tx.commit()?;
        Ok(CaptureOutcome::Applied { ops: n, pending })
    }

    /// Apply a batch of numbered events from one extension epoch. Events at or below the
    /// epoch's cursor are skipped (redelivery); an unknown reference rolls the whole batch
    /// back and asks for a snapshot.
    pub fn capture_events(
        &mut self,
        src: &Source,
        epoch: &str,
        events: &[(u64, BookmarkEvent)],
        now_ms: u64,
    ) -> Result<CaptureOutcome, CaptureError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        claim(&tx, src)?;
        let pid = src.profile_id.to_string();
        let did = src.device_id.to_string();
        let last: i64 = tx
            .query_row(
                "SELECT last_n FROM ext_cursor WHERE profile_id = ?1 AND epoch = ?2",
                params![pid, epoch],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let fresh: Vec<&(u64, BookmarkEvent)> =
            events.iter().filter(|(n, _)| *n as i64 > last).collect();
        if fresh.is_empty() {
            let pending = pending_of(&tx, &pid)?;
            return Ok(CaptureOutcome::Duplicate { pending });
        }
        let mut state = load_state(&tx, &pid)?;
        let mut changes = Vec::new();
        for (_, ev) in &fresh {
            match bookmarks::apply_event(&mut state, ev) {
                Ok(c) => changes.extend(c),
                Err(NormalizeError::ResyncRequired(why)) => {
                    return Ok(CaptureOutcome::ResyncRequired(why)); // tx dropped = rollback
                }
            }
        }
        let n = persist(&tx, &pid, &did, &state, &changes, now_ms)?;
        crate::conflict::after_local_changes(&tx, &pid, &changes, (now_ms / 1000) as i64)?;
        let max_n = fresh.iter().map(|(n, _)| *n).max().unwrap_or(0);
        set_cursor(&tx, &pid, epoch, max_n, now_ms)?;
        let pending = pending_of(&tx, &pid)?;
        tx.commit()?;
        Ok(CaptureOutcome::Applied { ops: n, pending })
    }

    /// The materialised tree of a profile (diagnostics, validation, tests).
    pub fn tree(&mut self, profile_id: Uuid) -> Result<BookmarkState, StateError> {
        let tx = self.conn.transaction()?;
        let s = load_state(&tx, &profile_id.to_string())?;
        tx.commit()?;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_month_is_utc_civil() {
        assert_eq!(year_month(0), "1970/01");
        assert_eq!(year_month(1_790_893_270_000), "2026/10");
        assert_eq!(year_month(951_782_400_000), "2000/02"); // 2000-02-29
    }
}
