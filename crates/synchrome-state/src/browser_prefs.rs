//! Browser preferences (M1 slice 12, `docs/03` §3.6): an allowlist of Chromium `Preferences`
//! paths, opt-in per profile and per device, per-path LWW.
//!
//! The app reads and writes `Preferences` itself, only while the browser of that profile is
//! closed. This module is the state side: capture (allowlisted logical values → ops), remote
//! merge (LWW per path), the write plan (compatibility, schema and version gated) and its
//! recording, views, conflicts. Same journal, HLC, chunks, store_events, lease and conflict
//! records as everything else (`col: "browser_preferences"`, id = the preference path).

use crate::capture::{claim, emit_ops, BaseRec, OpSpec};
use crate::conflict::{
    self, ConflictKind, ConflictStatus, ConflictSubtype, Group, Resolution, ResolveError,
    ResolveOutcome, Row, Version,
};
use crate::{CaptureError, Source, StateDb, StateError};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use synchrome_sync::browser_prefs::{self as bp, Mode, Skip, COLLECTION};
use synchrome_sync::{Fields, Op, OpKind};
use uuid::Uuid;

/// (value, its HLC, deleted, its HLC) of a row as loaded.
type Loaded = (Option<String>, Option<String>, bool, Option<String>);

#[derive(Debug, Clone, Default)]
struct PRow {
    path: String,
    value: Option<String>,
    deleted: bool,
    writer_kind: Option<String>,
    writer_major: Option<u32>,
    field_hlc: BTreeMap<String, String>,
    deleted_hlc: Option<String>,
    hlc: String,
    local_seq: i64,
    base: Option<String>,
    local_value: Option<String>,
    observed_at: Option<i64>,
    pending_apply: bool,
    compat: Option<String>,
    conflict: bool,
    loaded: Option<Loaded>,
}

const COLS: &str = "path, value, deleted, writer_kind, writer_major, field_hlc, deleted_hlc, hlc,
    local_seq, base, local_value, observed_at, pending_apply, compat, conflict";

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<PRow> {
    let fh: String = r.get(5)?;
    let mut s = PRow {
        path: r.get(0)?,
        value: r.get(1)?,
        deleted: r.get::<_, i64>(2)? != 0,
        writer_kind: r.get(3)?,
        writer_major: r.get::<_, Option<i64>>(4)?.map(|m| m as u32),
        field_hlc: serde_json::from_str(&fh).unwrap_or_default(),
        deleted_hlc: r.get(6)?,
        hlc: r.get(7)?,
        local_seq: r.get(8)?,
        base: r.get(9)?,
        local_value: r.get(10)?,
        observed_at: r.get(11)?,
        pending_apply: r.get::<_, i64>(12)? != 0,
        compat: r.get(13)?,
        conflict: r.get::<_, i64>(14)? != 0,
        loaded: None,
    };
    s.loaded = Some((
        s.value.clone(),
        s.field_hlc.get("value").cloned(),
        s.deleted,
        s.deleted_hlc.clone(),
    ));
    Ok(s)
}

fn load(conn: &Connection, pid: &str, path: &str) -> Result<Option<PRow>, StateError> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM browser_preferences_entities WHERE profile_id = ?1 AND path = ?2"),
            params![pid, path],
            from_row,
        )
        .optional()?)
}

fn load_all(conn: &Connection, pid: &str) -> Result<Vec<PRow>, StateError> {
    let mut st = conn.prepare(&format!(
        "SELECT {COLS} FROM browser_preferences_entities WHERE profile_id = ?1 ORDER BY path"
    ))?;
    let v = st.query_map([pid], from_row)?.collect::<Result<_, _>>()?;
    Ok(v)
}

fn save(conn: &Connection, pid: &str, e: &PRow) -> Result<(), StateError> {
    conn.execute(
        "INSERT INTO browser_preferences_entities (profile_id, path, value, deleted, writer_kind,
             writer_major, field_hlc, deleted_hlc, hlc, local_seq, base, local_value, observed_at,
             pending_apply, compat, conflict)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
         ON CONFLICT(profile_id, path) DO UPDATE SET value = excluded.value,
             deleted = excluded.deleted, writer_kind = excluded.writer_kind,
             writer_major = excluded.writer_major, field_hlc = excluded.field_hlc,
             deleted_hlc = excluded.deleted_hlc, hlc = excluded.hlc, local_seq = excluded.local_seq,
             base = excluded.base, local_value = excluded.local_value,
             observed_at = excluded.observed_at, pending_apply = excluded.pending_apply,
             compat = excluded.compat, conflict = excluded.conflict",
        params![
            pid,
            e.path,
            e.value,
            e.deleted as i64,
            e.writer_kind,
            e.writer_major.map(|m| m as i64),
            serde_json::to_string(&e.field_hlc).unwrap_or_else(|_| "{}".into()),
            e.deleted_hlc,
            e.hlc,
            e.local_seq,
            e.base,
            e.local_value,
            e.observed_at,
            e.pending_apply as i64,
            e.compat,
            e.conflict as i64
        ],
    )?;
    Ok(())
}

fn new_row(path: &str) -> PRow {
    PRow {
        path: path.into(),
        ..Default::default()
    }
}

// ------------------------------------------------------------------ config

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PrefsConfig {
    pub mode: Mode,
    pub status: Option<String>,
    pub error: Option<String>,
    pub browser_kind: Option<String>,
    pub browser_major: Option<u32>,
    pub invalid: u64,
    pub last_capture_at: Option<i64>,
    pub last_apply_at: Option<i64>,
}

fn config(conn: &Connection, pid: &str) -> Result<PrefsConfig, StateError> {
    Ok(conn
        .query_row(
            "SELECT mode, status, error, browser_kind, browser_major, invalid, last_capture_at,
                    last_apply_at FROM browser_preferences_config WHERE profile_id = ?1",
            [pid],
            |r| {
                Ok(PrefsConfig {
                    mode: Mode::parse(&r.get::<_, String>(0)?),
                    status: r.get(1)?,
                    error: r.get(2)?,
                    browser_kind: r.get(3)?,
                    browser_major: r.get::<_, Option<i64>>(4)?.map(|m| m as u32),
                    invalid: r.get::<_, i64>(5)? as u64,
                    last_capture_at: r.get(6)?,
                    last_apply_at: r.get(7)?,
                })
            },
        )
        .optional()?
        .unwrap_or(PrefsConfig {
            mode: Mode::Off,
            status: None,
            error: None,
            browser_kind: None,
            browser_major: None,
            invalid: 0,
            last_capture_at: None,
            last_apply_at: None,
        }))
}

fn set_status(
    conn: &Connection,
    pid: &str,
    status: &str,
    error: Option<&str>,
) -> Result<(), StateError> {
    conn.execute(
        "INSERT INTO browser_preferences_config (profile_id, status, error) VALUES (?1, ?2, ?3)
         ON CONFLICT(profile_id) DO UPDATE SET status = excluded.status, error = excluded.error",
        params![pid, status, error],
    )?;
    Ok(())
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PrefsError {
    #[error("profile not found for this account")]
    NotFound,
    #[error("local state unavailable")]
    State,
}

impl StateDb {
    /// Turn browser preferences sync on (the recommended allowlist) or off, on this device.
    pub fn set_prefs_mode(
        &mut self,
        account_id: &str,
        profile_id: Uuid,
        mode: Mode,
    ) -> Result<(), PrefsError> {
        let pid = profile_id.to_string();
        crate::load_profile(&self.conn, &pid)
            .map_err(|_| PrefsError::State)?
            .filter(|p| p.account_id == account_id)
            .ok_or(PrefsError::NotFound)?;
        self.conn
            .execute(
                "INSERT INTO browser_preferences_config (profile_id, mode, status) VALUES (?1, ?2, NULL)
                 ON CONFLICT(profile_id) DO UPDATE SET mode = excluded.mode, status = NULL, error = NULL",
                params![pid, mode.as_str()],
            )
            .map_err(|_| PrefsError::State)?;
        Ok(())
    }

    pub fn prefs_config(&self, profile_id: Uuid) -> Result<PrefsConfig, StateError> {
        config(&self.conn, &profile_id.to_string())
    }

    /// (browser, user-data dir, profile dir) of this device's local profile.
    pub fn prefs_target(
        &self,
        profile_id: Uuid,
    ) -> Result<Option<(String, String, String)>, StateError> {
        crate::ext_settings::target(&self.conn, &profile_id.to_string())
    }

    /// Record a cycle that could not read/write (browser open, file locked, error).
    pub fn prefs_status(
        &mut self,
        profile_id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StateError> {
        set_status(&self.conn, &profile_id.to_string(), status, error)
    }
}

// ------------------------------------------------------------------ capture

struct Pending {
    id: String,
    kind: OpKind,
    fields: Option<Fields>,
}

fn publish_local(
    tx: &Transaction<'_>,
    pid: &str,
    did: &str,
    rows: &mut HashMap<String, PRow>,
    changes: Vec<Pending>,
    now_ms: u64,
) -> Result<usize, CaptureError> {
    if changes.is_empty() {
        return Ok(0);
    }
    let published: i64 = tx.query_row(
        "SELECT published_through FROM profiles WHERE profile_id = ?1",
        [pid],
        |r| r.get(0),
    )?;
    let specs: Vec<OpSpec> = changes
        .iter()
        .map(|c| OpSpec {
            col: COLLECTION,
            id: c.id.clone(),
            kind: c.kind,
            fields: c.fields.clone(),
        })
        .collect();
    let ops = emit_ops(tx, pid, did, &specs, now_ms)?;
    for (c, op) in changes.iter().zip(&ops) {
        let Some(e) = rows.get_mut(&c.id) else {
            continue;
        };
        let mut rec = e
            .base
            .as_deref()
            .and_then(|b| serde_json::from_str::<BaseRec>(b).ok())
            .filter(|b| b.seq > published)
            .unwrap_or(BaseRec {
                seq: op.seq as i64,
                ..Default::default()
            });
        let (pv, pvh, pd, pdh) = e.loaded.clone().unwrap_or((None, None, true, None));
        if !rec.f.contains_key("value") {
            rec.f
                .insert("value".into(), pv.map(Value::String).unwrap_or(Value::Null));
            if let Some(h) = pvh {
                rec.h.insert("value".into(), h);
            }
        }
        if !rec.f.contains_key("deleted") {
            rec.f.insert("deleted".into(), pd.into());
            if let Some(h) = pdh {
                rec.h.insert("deleted".into(), h);
            }
        }
        e.base = serde_json::to_string(&rec).ok();
        match c.kind {
            OpKind::Set => {
                e.field_hlc.insert("value".into(), op.hlc.clone());
            }
            OpKind::Delete => e.deleted_hlc = Some(op.hlc.clone()),
        }
        e.hlc = op.hlc.clone();
        e.local_seq = op.seq as i64;
    }
    Ok(ops.len())
}

fn set_fields(value: &str, kind: Option<&str>, major: Option<u32>) -> Fields {
    let mut f = Fields::new();
    f.insert(
        "value".into(),
        serde_json::from_str(value).unwrap_or(Value::Null),
    );
    if let Some(k) = kind {
        f.insert("bk".into(), k.into());
    }
    if let Some(m) = major {
        f.insert("maj".into(), m.into());
    }
    f
}

/// Outcome of one capture (counts only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrefsCapture {
    pub ops: usize,
    /// Allowlisted paths present with a valid value.
    pub paths: usize,
    pub unsupported: usize,
    pub invalid: usize,
}

impl StateDb {
    /// Reconcile one snapshot of the allowlist (logical values extracted from `Preferences`,
    /// read with the browser closed): changed/new paths become `set` ops, paths seen here and
    /// gone now become tombstones; paths with a remote value still to be written are frozen;
    /// unsupported / invalid paths are skipped (never published, never deleted). Idempotent.
    pub fn capture_prefs(
        &mut self,
        src: &Source,
        kind: &str,
        major: Option<u32>,
        values: &BTreeMap<&'static str, Value>,
        skipped: &BTreeMap<&'static str, Skip>,
        now_ms: u64,
    ) -> Result<PrefsCapture, CaptureError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        claim(&tx, src)?;
        let pid = src.profile_id.to_string();
        let now = (now_ms / 1000) as i64;
        let mut rows: HashMap<String, PRow> = load_all(&tx, &pid)?
            .into_iter()
            .map(|r| (r.path.clone(), r))
            .collect();
        let mut cap = PrefsCapture {
            unsupported: skipped
                .values()
                .filter(|s| **s == Skip::Unsupported)
                .count(),
            invalid: skipped.values().filter(|s| **s == Skip::Invalid).count(),
            ..Default::default()
        };
        let mut out = Vec::new();
        for (path, v) in values {
            let c = bp::canonical(v);
            cap.paths += 1;
            let e = rows
                .entry(path.to_string())
                .or_insert_with(|| new_row(path));
            e.observed_at = Some(now);
            let fresh = e.hlc.is_empty() && e.value.is_none() && !e.deleted;
            if e.pending_apply {
                // a remote value waits to be written: the file's value is stale, not a decision
                if e.value.as_deref() == Some(c.as_str()) && !e.deleted {
                    e.pending_apply = false;
                }
                e.local_value = Some(c);
                continue;
            }
            if fresh || e.deleted || e.value.as_deref() != Some(c.as_str()) {
                e.value = Some(c.clone());
                e.deleted = false;
                e.writer_kind = Some(kind.to_string());
                e.writer_major = major;
                out.push(Pending {
                    id: path.to_string(),
                    kind: OpKind::Set,
                    fields: Some(set_fields(&c, Some(kind), major)),
                });
            }
            e.local_value = Some(c);
        }
        // allowlisted paths seen here and absent now: reset to the browser default
        for (path, e) in rows.iter_mut() {
            if values.contains_key(path.as_str()) || e.local_value.is_none() {
                continue;
            }
            if skipped.contains_key(path.as_str()) || bp::spec(path).is_none() {
                continue; // unsupported here or invalid: not gone, never a delete
            }
            e.local_value = None;
            e.observed_at = Some(now);
            if e.pending_apply {
                continue;
            }
            if !e.deleted {
                e.deleted = true;
                e.value = None;
                out.push(Pending {
                    id: path.clone(),
                    kind: OpKind::Delete,
                    fields: None,
                });
            }
        }
        cap.ops = publish_local(
            &tx,
            &pid,
            &src.device_id.to_string(),
            &mut rows,
            out,
            now_ms,
        )?;
        for r in rows.values() {
            save(&tx, &pid, r)?;
        }
        tx.execute(
            "INSERT INTO browser_preferences_config (profile_id, browser_kind, browser_major, invalid, last_capture_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(profile_id) DO UPDATE SET browser_kind = excluded.browser_kind,
                 browser_major = excluded.browser_major, invalid = excluded.invalid,
                 last_capture_at = excluded.last_capture_at",
            params![pid, kind, major.map(|m| m as i64), cap.invalid as i64, now],
        )?;
        refresh_conflicts(&tx, &pid, now)?;
        tx.commit()?;
        Ok(cap)
    }
}

// ------------------------------------------------------------------ remote merge

fn relevant(e: &PRow, op: &Op, published: i64) -> bool {
    let Some(rec) = e
        .base
        .as_deref()
        .and_then(|b| serde_json::from_str::<BaseRec>(b).ok())
        .filter(|b| b.seq > published)
    else {
        return true;
    };
    let before = |k: &str| -> Option<String> {
        if rec.f.contains_key(k) {
            rec.h.get(k).cloned()
        } else if k == "deleted" {
            e.deleted_hlc.clone()
        } else {
            e.field_hlc.get(k).cloned()
        }
    };
    let newest = [before("value"), before("deleted")]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or_default();
    newest.as_str() < op.hlc.as_str()
}

pub(crate) fn apply_remote_op(
    tx: &Transaction<'_>,
    pid: &str,
    published: i64,
    op: &Op,
    now: i64,
) -> Result<bool, StateError> {
    if op.id.is_empty() {
        return Ok(false);
    }
    let row = load(tx, pid, &op.id)?;
    let open = open_conflict(tx, pid, &op.id)?;
    let hold = match &open {
        Some(c) => c.status == ConflictStatus::Unresolved,
        None => row
            .as_ref()
            .is_some_and(|r| r.local_seq > published && relevant(r, op, published)),
    };
    if hold {
        hold_op(tx, pid, open, row.as_ref(), op, published, now)?;
        return Ok(true);
    }
    lww(tx, pid, op)?;
    Ok(false)
}

pub(crate) fn lww(tx: &Transaction<'_>, pid: &str, op: &Op) -> Result<(), StateError> {
    if op.id.is_empty() {
        return Ok(());
    }
    let mut e = load(tx, pid, &op.id)?.unwrap_or_else(|| new_row(&op.id));
    match op.op {
        OpKind::Set => {
            let f = op.fields.clone().unwrap_or_default();
            let Some(v) = f.get("value") else {
                return Ok(());
            };
            let newer = e
                .field_hlc
                .get("value")
                .map_or(true, |h| h.as_str() < op.hlc.as_str());
            if newer {
                e.value = Some(bp::canonical(v));
                e.writer_kind = f.get("bk").and_then(|v| v.as_str()).map(str::to_string);
                e.writer_major = f.get("maj").and_then(|v| v.as_u64()).map(|m| m as u32);
                e.field_hlc.insert("value".into(), op.hlc.clone());
            }
            if e.deleted
                && e.deleted_hlc
                    .as_deref()
                    .map_or(true, |d| d < op.hlc.as_str())
            {
                e.deleted = false;
            }
            if e.deleted {
                // a deleted entity carries no value, whatever the delivery order (the field
                // HLC is still recorded, so an older set can never come back)
                e.value = None;
            }
            if !e.deleted && e.local_value != e.value {
                e.pending_apply = true;
            }
            e.hlc = e.hlc.clone().max(op.hlc.clone());
        }
        OpKind::Delete => {
            let newest = e.field_hlc.values().max().cloned().unwrap_or_default();
            if e.deleted {
                // keep the NEWEST tombstone: a set between two deletes must not depend on
                // which delete arrived first (convergence)
                if e.deleted_hlc
                    .as_deref()
                    .map_or(true, |d| d < op.hlc.as_str())
                {
                    e.deleted_hlc = Some(op.hlc.clone());
                }
                return save(tx, pid, &e);
            }
            if newest.as_str() >= op.hlc.as_str() {
                return save(tx, pid, &e);
            }
            e.deleted = true;
            e.value = None;
            e.deleted_hlc = Some(op.hlc.clone());
            e.pending_apply = e.local_value.is_some();
        }
    }
    save(tx, pid, &e)
}

// ------------------------------------------------------------------ write plan

/// What to write into `Preferences` now (browser closed): logical values per path (`None` =
/// remove → browser default), plus what was held back and why.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrefsPlan {
    pub edits: Vec<(String, Option<Value>)>,
    pub unsupported: usize,
    pub version_mismatch: usize,
    pub invalid: usize,
}

impl StateDb {
    /// The gated write plan for `profile_id` on this browser (`kind`, `major`).
    pub fn plan_prefs(
        &mut self,
        profile_id: Uuid,
        kind: &str,
        major: Option<u32>,
    ) -> Result<PrefsPlan, StateError> {
        let pid = profile_id.to_string();
        let mut plan = PrefsPlan::default();
        if config(&self.conn, &pid)?.mode == Mode::Off {
            return Ok(plan);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for mut r in load_all(&tx, &pid)? {
            if !r.pending_apply || r.conflict {
                continue;
            }
            let verdict = match bp::spec(&r.path) {
                None => "unsupported",
                Some(s) if !bp::supported(s, kind, major) => "unsupported",
                Some(_) if r.deleted => "ok",
                Some(s) => match r
                    .value
                    .as_deref()
                    .and_then(|v| serde_json::from_str::<Value>(v).ok())
                {
                    Some(v) if !bp::valid(s, &v) => "invalid",
                    None => "invalid",
                    Some(_) if !bp::same_semantics(r.writer_major, major) => "version_mismatch",
                    Some(_) => "ok",
                },
            };
            match verdict {
                "ok" => plan.edits.push((
                    r.path.clone(),
                    if r.deleted {
                        None
                    } else {
                        r.value
                            .as_deref()
                            .and_then(|v| serde_json::from_str(v).ok())
                    },
                )),
                "unsupported" => plan.unsupported += 1,
                "invalid" => plan.invalid += 1,
                _ => plan.version_mismatch += 1,
            }
            if r.compat.as_deref() != Some(verdict) {
                r.compat = Some(verdict.into());
                save(&tx, &pid, &r)?;
            }
        }
        // the browser is closed (that is when plans are made)
        set_status(
            &tx,
            &pid,
            if plan.version_mismatch > 0 {
                "version_mismatch"
            } else {
                "synced"
            },
            None,
        )?;
        tx.commit()?;
        Ok(plan)
    }

    /// `Preferences` now holds the planned `edits` (logical values).
    pub fn prefs_written(
        &mut self,
        profile_id: Uuid,
        edits: &[(String, Option<Value>)],
        now: i64,
    ) -> Result<(), StateError> {
        let pid = profile_id.to_string();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (path, v) in edits {
            if let Some(mut e) = load(&tx, &pid, path)? {
                e.local_value = v.as_ref().map(bp::canonical);
                e.observed_at = Some(now);
                if e.local_value == e.value || (e.deleted && e.local_value.is_none()) {
                    e.pending_apply = false;
                }
                save(&tx, &pid, &e)?;
            }
            finish_resolving(&tx, &pid, path, now)?;
        }
        tx.execute(
            "UPDATE browser_preferences_config SET last_apply_at = ?2 WHERE profile_id = ?1",
            params![pid, now],
        )?;
        tx.commit()?;
        Ok(())
    }
}

// ------------------------------------------------------------------ views

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefsState {
    Off,
    WaitingForBrowserClose,
    Syncing,
    Synced,
    Unsupported,
    VersionMismatch,
    Conflict,
    Error,
}

/// Browser preferences of one profile on this device (counts only; never values).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrefsView {
    pub mode: Mode,
    pub state: PrefsState,
    pub synced: u64,
    pub unsupported: u64,
    pub pending: u64,
    pub conflicts: u64,
    pub invalid: u64,
    pub browser_kind: Option<String>,
    pub browser_major: Option<u32>,
    pub error: Option<String>,
    pub last_capture_at: Option<i64>,
    pub last_apply_at: Option<i64>,
}

impl StateDb {
    pub fn prefs_view(&self, profile_id: Uuid) -> Result<PrefsView, StateError> {
        let pid = profile_id.to_string();
        let c = config(&self.conn, &pid)?;
        let rows = load_all(&self.conn, &pid)?;
        let conflicts: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM conflicts WHERE profile_id = ?1 AND col = 'browser_preferences' AND status = 'unresolved'",
            [&pid],
            |r| r.get(0),
        )?;
        let conflicts = conflicts as u64;
        let held = |r: &PRow| matches!(r.compat.as_deref(), Some("unsupported") | Some("invalid"));
        let pending = rows
            .iter()
            .filter(|r| r.pending_apply && !r.conflict && !held(r))
            .count() as u64;
        let synced = rows
            .iter()
            .filter(|r| {
                !r.deleted
                    && !r.pending_apply
                    && !r.conflict
                    && r.value.is_some()
                    && r.local_value == r.value
            })
            .count() as u64;
        let (unsupported, all_unsupported) = match &c.browser_kind {
            Some(k) => {
                let n = bp::ALLOWLIST
                    .iter()
                    .filter(|s| !bp::supported(s, k, c.browser_major))
                    .count();
                let foreign = rows
                    .iter()
                    .filter(|r| r.pending_apply && bp::spec(&r.path).is_none())
                    .count();
                ((n + foreign) as u64, n == bp::ALLOWLIST.len())
            }
            None => (0, false),
        };
        let state = if c.mode == Mode::Off {
            PrefsState::Off
        } else if conflicts > 0 {
            PrefsState::Conflict
        } else {
            match c.status.as_deref() {
                Some("error") => PrefsState::Error,
                Some("waiting") => PrefsState::WaitingForBrowserClose,
                _ if all_unsupported => PrefsState::Unsupported,
                Some("version_mismatch") if pending > 0 => PrefsState::VersionMismatch,
                _ if pending > 0 => PrefsState::Syncing,
                None => PrefsState::WaitingForBrowserClose, // never captured yet
                _ => PrefsState::Synced,
            }
        };
        Ok(PrefsView {
            mode: c.mode,
            state,
            synced,
            unsupported,
            pending,
            conflicts,
            // local values outside the schema + remote values held for the same reason
            invalid: c.invalid
                + rows
                    .iter()
                    .filter(|r| r.pending_apply && r.compat.as_deref() == Some("invalid"))
                    .count() as u64,
            browser_kind: c.browser_kind,
            browser_major: c.browser_major,
            error: c.error,
            last_capture_at: c.last_capture_at,
            last_apply_at: c.last_apply_at,
        })
    }
}

// ------------------------------------------------------------------ conflicts

fn open_conflict(conn: &Connection, pid: &str, id: &str) -> Result<Option<Row>, StateError> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {} FROM conflicts WHERE profile_id = ?1 AND sid = ?2 AND col = 'browser_preferences'
                 AND status IN ('unresolved', 'resolving')",
                conflict::ROW_COLS
            ),
            params![pid, id],
            conflict::row_from,
        )
        .optional()?)
}

fn version_of(e: &PRow) -> Version {
    let mut hlc = BTreeMap::new();
    if let Some(h) = e.field_hlc.get("value") {
        hlc.insert("value".into(), h.clone());
    }
    if let Some(h) = &e.deleted_hlc {
        hlc.insert("deleted".into(), h.clone());
    }
    Version {
        exists: !e.deleted,
        title: Some(e.path.clone()),
        value: e.value.clone(),
        hlc,
        ..Default::default()
    }
}

fn fold(ops: &[Op]) -> Version {
    let mut v = Version {
        exists: true,
        ..Default::default()
    };
    let mut del: Option<&str> = None;
    let mut max_set: Option<&str> = None;
    for o in ops {
        match o.op {
            OpKind::Set => {
                if let Some(val) = o.fields.as_ref().and_then(|f| f.get("value")) {
                    v.value = Some(bp::canonical(val));
                    v.changed.insert(Group::Value);
                    v.hlc.insert("value".into(), o.hlc.clone());
                }
                max_set = max_set.max(Some(o.hlc.as_str()));
            }
            OpKind::Delete => {
                del = del.max(Some(o.hlc.as_str()));
                v.hlc.insert("deleted".into(), o.hlc.clone());
            }
        }
        v.device = Some(o.by.clone());
        v.seq = (o.seq > 0).then_some(o.seq);
    }
    if let Some(d) = del {
        v.exists = max_set.is_some_and(|s| s > d);
    }
    if !v.exists {
        v.value = None;
        v.changed.insert(Group::Existence);
    }
    v
}

/// Browser kind and major of the newest held `set` (the writer of the remote value).
fn remote_writer(ops: &[Op]) -> (Option<String>, Option<u32>) {
    ops.iter()
        .filter(|o| o.op == OpKind::Set)
        .max_by(|a, b| a.hlc.cmp(&b.hlc))
        .and_then(|o| o.fields.as_ref())
        .map(|f| {
            (
                f.get("bk").and_then(|v| v.as_str()).map(str::to_string),
                f.get("maj").and_then(|v| v.as_u64()).map(|m| m as u32),
            )
        })
        .unwrap_or((None, None))
}

fn base_of(e: &PRow, published: i64) -> Option<Version> {
    let rec: BaseRec = serde_json::from_str(e.base.as_deref()?).ok()?;
    if rec.seq <= published {
        return None;
    }
    let deleted = rec
        .f
        .get("deleted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Some(Version {
        exists: !deleted,
        title: Some(e.path.clone()),
        value: rec
            .f
            .get("value")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        changed: [Group::Value, Group::Existence].into_iter().collect(),
        ..Default::default()
    })
}

fn unpublished(
    conn: &Connection,
    pid: &str,
    id: &str,
    published: i64,
) -> Result<BTreeSet<Group>, StateError> {
    let needle = format!("%\"id\":{}%", serde_json::to_string(id).unwrap_or_default());
    let mut st =
        conn.prepare("SELECT line FROM ops WHERE profile_id = ?1 AND seq > ?2 AND line LIKE ?3")?;
    let mut out = BTreeSet::new();
    for l in st.query_map(params![pid, published, needle], |r| r.get::<_, String>(0))? {
        let Ok(op) = Op::from_line(&l?) else { continue };
        if op.id != id || op.col != COLLECTION {
            continue;
        }
        out.insert(if op.op == OpKind::Delete {
            Group::Existence
        } else {
            Group::Value
        });
    }
    Ok(out)
}

fn hold_op(
    tx: &Transaction<'_>,
    pid: &str,
    open: Option<Row>,
    row: Option<&PRow>,
    op: &Op,
    published: i64,
    now: i64,
) -> Result<(), StateError> {
    let c = match open {
        Some(c) => c,
        None => {
            let Some(e) = row else { return Ok(()) };
            let mut local = version_of(e);
            local.changed = unpublished(tx, pid, &op.id, published)?;
            let base = base_of(e, published);
            let id = conflict::insert_row_col(
                tx,
                COLLECTION,
                pid,
                &op.id,
                ConflictKind::LocalPending,
                ConflictSubtype::EditEdit,
                &local,
                &Version::default(),
                base.as_ref(),
                None,
                now,
            )?;
            conflict::by_id(tx, &id)?.ok_or_else(|| StateError::Corrupt("conflict".into()))?
        }
    };
    conflict::add_held(tx, &c.id, op)?;
    if recompute(tx, &c, published, now)? {
        tx.execute(
            "UPDATE browser_preferences_entities SET conflict = 1 WHERE profile_id = ?1 AND path = ?2",
            params![pid, op.id],
        )?;
    }
    Ok(())
}

fn subtype(local: &Version, remote: &Version) -> ConflictSubtype {
    match (local.exists, remote.exists) {
        (true, true) => ConflictSubtype::EditEdit,
        (true, false) => ConflictSubtype::EditDelete,
        (false, true) => ConflictSubtype::DeleteEdit,
        (false, false) => ConflictSubtype::DeleteDelete,
    }
}

fn change_of(e: &PRow, id: &str) -> Pending {
    match (&e.deleted, &e.value) {
        (false, Some(v)) => Pending {
            id: id.to_string(),
            kind: OpKind::Set,
            fields: Some(set_fields(v, e.writer_kind.as_deref(), e.writer_major)),
        },
        _ => Pending {
            id: id.to_string(),
            kind: OpKind::Delete,
            fields: None,
        },
    }
}

fn recompute(tx: &Transaction<'_>, c: &Row, published: i64, now: i64) -> Result<bool, StateError> {
    let held = conflict::held_ops(tx, &c.id)?;
    let remote = fold(&held);
    let Some(e) = load(tx, &c.pid, &c.sid)? else {
        return Ok(true);
    };
    let mut local = version_of(&e);
    local.changed = unpublished(tx, &c.pid, &c.sid, published)?;
    local.changed.extend(c.local.changed.iter().copied());
    let st = subtype(&local, &remote);
    tx.execute(
        "UPDATE conflicts SET local_json = ?2, remote_json = ?3, subtype = ?4, updated_at = ?5 WHERE conflict_id = ?1",
        params![
            c.id,
            serde_json::to_string(&local).unwrap_or_default(),
            serde_json::to_string(&remote).unwrap_or_default(),
            st.as_str(),
            now
        ],
    )?;
    let same = local.exists == remote.exists && local.value == remote.value;
    let base_only = c
        .base
        .as_ref()
        .is_some_and(|b| b.exists == remote.exists && (!remote.exists || b.value == remote.value));
    let why = if st == ConflictSubtype::DeleteDelete {
        Some("deleted on both sides")
    } else if same {
        Some("both sides made the same change")
    } else if base_only {
        Some("the other device only re-asserted this device's previous value")
    } else {
        None
    };
    let Some(why) = why else { return Ok(true) };
    if base_only && !same {
        let lh = local.hlc.values().max().cloned().unwrap_or_default();
        let rh = remote.hlc.values().max().cloned().unwrap_or_default();
        if lh < rh {
            let did: String = tx.query_row(
                "SELECT device_id FROM profiles WHERE profile_id = ?1",
                [&c.pid],
                |r| r.get(0),
            )?;
            let mut rows = HashMap::from([(c.sid.clone(), e.clone())]);
            publish_local(
                tx,
                &c.pid,
                &did,
                &mut rows,
                vec![change_of(&e, &c.sid)],
                synchrome_sync::hlc::now_ms(),
            )
            .map_err(|e| StateError::Corrupt(e.to_string()))?;
            for r in rows.values() {
                save(tx, &c.pid, r)?;
            }
        }
    }
    tx.execute(
        "UPDATE conflicts SET status = 'superseded', detail = ?2, resolved_at = ?3, updated_at = ?3 WHERE conflict_id = ?1",
        params![c.id, why, now],
    )?;
    for op in held {
        lww(tx, &c.pid, &op)?;
    }
    tx.execute(
        "UPDATE browser_preferences_entities SET conflict = 0 WHERE profile_id = ?1 AND path = ?2",
        params![c.pid, c.sid],
    )?;
    Ok(false)
}

fn refresh_conflicts(tx: &Transaction<'_>, pid: &str, now: i64) -> Result<(), StateError> {
    let published = conflict::published_of(tx, pid)?;
    let mut st = tx.prepare(&format!(
        "SELECT {} FROM conflicts WHERE profile_id = ?1 AND col = 'browser_preferences' AND status = 'unresolved'",
        conflict::ROW_COLS
    ))?;
    let rows: Vec<Row> = st
        .query_map([pid], conflict::row_from)?
        .collect::<Result<_, _>>()?;
    drop(st);
    for c in rows {
        recompute(tx, &c, published, now)?;
    }
    Ok(())
}

fn finish_resolving(tx: &Transaction<'_>, pid: &str, id: &str, now: i64) -> Result<(), StateError> {
    tx.execute(
        "UPDATE conflicts SET status = 'resolved', resolved_at = ?3, updated_at = ?3
         WHERE profile_id = ?1 AND sid = ?2 AND col = 'browser_preferences' AND status = 'resolving'",
        params![pid, id, now],
    )?;
    Ok(())
}

pub(crate) fn options(c: &Row) -> Vec<Resolution> {
    match c.subtype {
        ConflictSubtype::DeleteDelete => vec![],
        _ => vec![Resolution::KeepLocal, Resolution::KeepRemote],
    }
}

/// Resolve a preference conflict (called by `StateDb::resolve_conflict`).
pub(crate) fn resolve(
    tx: Transaction<'_>,
    c: &Row,
    res: Resolution,
    device_id: &str,
    published: i64,
    now_ms: u64,
) -> Result<ResolveOutcome, ResolveError> {
    let now = (now_ms / 1000) as i64;
    if !recompute(&tx, c, published, now)? {
        tx.commit()?;
        return Ok(ResolveOutcome::Closed {
            status: ConflictStatus::Superseded,
        });
    }
    let c = conflict::by_id(&tx, &c.id)?.ok_or(ResolveError::NotFound)?;
    if !options(&c).contains(&res) {
        return Err(ResolveError::NotAllowed);
    }
    let mut e = load(&tx, &c.pid, &c.sid)?.ok_or(ResolveError::NotFound)?;
    match res {
        Resolution::KeepLocal => {}
        Resolution::KeepRemote => {
            if c.remote.exists {
                let v = c.remote.value.clone().unwrap_or_else(|| "null".into());
                let (k, m) = remote_writer(&conflict::held_ops(&tx, &c.id)?);
                e.value = Some(v.clone());
                e.deleted = false;
                e.writer_kind = k;
                e.writer_major = m;
                e.pending_apply = e.local_value.as_deref() != Some(v.as_str());
            } else {
                e.deleted = true;
                e.value = None;
                e.pending_apply = e.local_value.is_some();
            }
        }
        _ => return Err(ResolveError::NotAllowed),
    }
    let change = change_of(&e, &c.sid);
    e.conflict = false;
    let pending = e.pending_apply;
    let mut rows = HashMap::from([(c.sid.clone(), e)]);
    let n = publish_local(&tx, &c.pid, device_id, &mut rows, vec![change], now_ms).map_err(
        |e| match e {
            CaptureError::Clock(c) => ResolveError::Clock(c),
            CaptureError::State(s) => ResolveError::State(s),
            other => ResolveError::State(StateError::Corrupt(other.to_string())),
        },
    )?;
    for r in rows.values() {
        save(&tx, &c.pid, r)?;
    }
    let chosen = version_of(&rows[&c.sid]);
    let status = if pending { "resolving" } else { "resolved" };
    tx.execute(
        "UPDATE conflicts SET status = ?2, resolution = ?3, resolution_json = ?4, resolution_hlc = ?5,
                resolution_ops = ?6, updated_at = ?7,
                resolved_at = CASE WHEN ?2 = 'resolved' THEN ?7 ELSE NULL END
         WHERE conflict_id = ?1",
        params![
            c.id,
            status,
            res.as_str(),
            serde_json::to_string(&chosen).unwrap_or_default(),
            rows[&c.sid].hlc,
            n as i64,
            now
        ],
    )?;
    tx.commit()?;
    Ok(if pending {
        ResolveOutcome::Resolving { ops: n }
    } else {
        ResolveOutcome::Resolved { ops: n }
    })
}
