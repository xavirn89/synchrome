//! Extension settings (M1 slice 11, `docs/03` §3.5): per-key `chrome.storage.local` /
//! `chrome.storage.sync` values of other extensions, opt-in per extension and per device.
//!
//! Chromium gives no extension access to another extension's storage, so the app reads and
//! writes the LevelDB stores itself, and only while the browser of that profile is closed (a
//! copy of a store in use can miss its newest records, and a missing key would look like a
//! delete). This module is the state side: capture (store snapshot → ops), remote merge
//! (LWW per key), the write plan (version-gated) and its recording, views, conflicts.
//!
//! Same journal, HLC, chunks, store_events, lease and conflict records as everything else
//! (`col: "extension_settings"`, id `<ext>/<area>/<key>`).

use crate::capture::{claim, emit_ops, BaseRec, OpSpec};
use crate::conflict::{
    self, ConflictKind, ConflictStatus, ConflictSubtype, Group, Resolution, ResolveError,
    ResolveOutcome, Row, Version,
};
use crate::{CaptureError, Source, StateDb, StateError};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use synchrome_sync::extension_settings::{self as es, Area, Excluded, Mode, COLLECTION};
use synchrome_sync::{Fields, Op, OpKind};
use uuid::Uuid;

/// (value, its HLC, deleted, its HLC) of a row as loaded.
type Loaded = (Option<String>, Option<String>, bool, Option<String>);

#[derive(Debug, Clone, Default)]
struct SRow {
    ext_id: String,
    area: String,
    key: String,
    value: Option<String>,
    deleted: bool,
    writer_version: Option<String>,
    field_hlc: BTreeMap<String, String>,
    deleted_hlc: Option<String>,
    hlc: String,
    local_seq: i64,
    base: Option<String>,
    local_value: Option<String>,
    pending_apply: bool,
    conflict: bool,
    /// (value, its HLC, deleted, its HLC) as loaded: the pre-edit values for BASE.
    loaded: Option<Loaded>,
}

const COLS: &str = "ext_id, area, key, value, deleted, writer_version, field_hlc, deleted_hlc, hlc,
    local_seq, base, local_value, pending_apply, conflict";

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<SRow> {
    let fh: String = r.get(6)?;
    let mut s = SRow {
        ext_id: r.get(0)?,
        area: r.get(1)?,
        key: r.get(2)?,
        value: r.get(3)?,
        deleted: r.get::<_, i64>(4)? != 0,
        writer_version: r.get(5)?,
        field_hlc: serde_json::from_str(&fh).unwrap_or_default(),
        deleted_hlc: r.get(7)?,
        hlc: r.get(8)?,
        local_seq: r.get(9)?,
        base: r.get(10)?,
        local_value: r.get(11)?,
        pending_apply: r.get::<_, i64>(12)? != 0,
        conflict: r.get::<_, i64>(13)? != 0,
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

fn load(
    conn: &Connection,
    pid: &str,
    ext: &str,
    area: &str,
    key: &str,
) -> Result<Option<SRow>, StateError> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLS} FROM ext_settings_entities
                 WHERE profile_id = ?1 AND ext_id = ?2 AND area = ?3 AND key = ?4"
            ),
            params![pid, ext, area, key],
            from_row,
        )
        .optional()?)
}

fn load_area(conn: &Connection, pid: &str, ext: &str, area: &str) -> Result<Vec<SRow>, StateError> {
    let mut st = conn.prepare(&format!(
        "SELECT {COLS} FROM ext_settings_entities WHERE profile_id = ?1 AND ext_id = ?2 AND area = ?3"
    ))?;
    let v = st
        .query_map(params![pid, ext, area], from_row)?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

fn save(conn: &Connection, pid: &str, e: &SRow) -> Result<(), StateError> {
    conn.execute(
        "INSERT INTO ext_settings_entities (profile_id, ext_id, area, key, value, deleted,
             writer_version, field_hlc, deleted_hlc, hlc, local_seq, base, local_value,
             pending_apply, conflict)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(profile_id, ext_id, area, key) DO UPDATE SET value = excluded.value,
             deleted = excluded.deleted, writer_version = excluded.writer_version,
             field_hlc = excluded.field_hlc, deleted_hlc = excluded.deleted_hlc,
             hlc = excluded.hlc, local_seq = excluded.local_seq, base = excluded.base,
             local_value = excluded.local_value, pending_apply = excluded.pending_apply,
             conflict = excluded.conflict",
        params![
            pid,
            e.ext_id,
            e.area,
            e.key,
            e.value,
            e.deleted as i64,
            e.writer_version,
            serde_json::to_string(&e.field_hlc).unwrap_or_else(|_| "{}".into()),
            e.deleted_hlc,
            e.hlc,
            e.local_seq,
            e.base,
            e.local_value,
            e.pending_apply as i64,
            e.conflict as i64
        ],
    )?;
    Ok(())
}

fn new_row(ext: &str, area: &str, key: &str) -> SRow {
    SRow {
        ext_id: ext.into(),
        area: area.into(),
        key: key.into(),
        ..Default::default()
    }
}

fn id_of(e: &SRow) -> String {
    format!("{}/{}/{}", e.ext_id, e.area, e.key)
}

// ------------------------------------------------------------------ config

/// Per-extension settings configuration on this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingsConfig {
    pub ext_id: String,
    pub mode: Mode,
    pub exclude: Vec<String>,
    pub status: Option<String>,
    pub error: Option<String>,
    pub excluded_sensitive: u64,
    pub excluded_other: u64,
    pub last_capture_at: Option<i64>,
    pub last_apply_at: Option<i64>,
}

fn config(conn: &Connection, pid: &str, ext: &str) -> Result<SettingsConfig, StateError> {
    let r = conn
        .query_row(
            "SELECT mode, exclude, status, error, excluded, last_capture_at, last_apply_at
             FROM ext_settings_config WHERE profile_id = ?1 AND ext_id = ?2",
            params![pid, ext],
            |r| {
                let mode: String = r.get(0)?;
                let ex: String = r.get(1)?;
                let counts: String = r.get(4)?;
                let counts: BTreeMap<String, (u64, u64)> =
                    serde_json::from_str(&counts).unwrap_or_default();
                let mode = Mode::parse(&mode);
                let (mut sens, mut other) = (0, 0);
                for a in mode.areas() {
                    if let Some((s, o)) = counts.get(a.as_str()) {
                        sens += s;
                        other += o;
                    }
                }
                Ok(SettingsConfig {
                    ext_id: ext.to_string(),
                    mode,
                    exclude: serde_json::from_str(&ex).unwrap_or_default(),
                    status: r.get(2)?,
                    error: r.get(3)?,
                    excluded_sensitive: sens,
                    excluded_other: other,
                    last_capture_at: r.get(5)?,
                    last_apply_at: r.get(6)?,
                })
            },
        )
        .optional()?;
    Ok(r.unwrap_or(SettingsConfig {
        ext_id: ext.to_string(),
        mode: Mode::Off,
        exclude: vec![],
        status: None,
        error: None,
        excluded_sensitive: 0,
        excluded_other: 0,
        last_capture_at: None,
        last_apply_at: None,
    }))
}

fn set_status(
    conn: &Connection,
    pid: &str,
    ext: &str,
    status: &str,
    error: Option<&str>,
) -> Result<(), StateError> {
    conn.execute(
        "INSERT INTO ext_settings_config (profile_id, ext_id, status, error) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(profile_id, ext_id) DO UPDATE SET status = excluded.status, error = excluded.error",
        params![pid, ext, status, error],
    )?;
    Ok(())
}

/// Where this device's profile lives (from the confirmed mapping's source identity):
/// (browser, user-data dir, profile dir).
pub(crate) fn target(
    conn: &Connection,
    pid: &str,
) -> Result<Option<(String, String, String)>, StateError> {
    let id: Option<String> = conn
        .query_row(
            "SELECT source_identity FROM profiles WHERE profile_id = ?1",
            [pid],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(id.and_then(|i| {
        let mut p = i.splitn(3, '|');
        Some((
            p.next()?.to_string(),
            p.next()?.to_string(),
            p.next()?.to_string(),
        ))
    }))
}

/// Version of an extension installed on this device, if installed and synchronised here.
fn local_version(conn: &Connection, pid: &str, ext: &str) -> Result<Option<String>, StateError> {
    Ok(conn
        .query_row(
            "SELECT local_version FROM ext_entities WHERE profile_id = ?1 AND ext_id = ?2
             AND present = 1 AND scope = 'synced'",
            params![pid, ext],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// What the app's settings cycle needs for one profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsJob {
    pub ext_id: String,
    pub areas: Vec<Area>,
    pub exclude: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("extension not found in this profile")]
    NotFound,
    #[error("settings of this extension cannot be synchronised (protected, removed, or not installed here)")]
    NotAllowed,
    #[error("local state unavailable")]
    State,
}

impl StateDb {
    /// Turn settings sync on (local / local+sync) or off for one extension, on this device.
    /// Only for synchronised, installed, non-protected extensions of the current account.
    pub fn set_settings_mode(
        &mut self,
        account_id: &str,
        profile_id: Uuid,
        ext_id: &str,
        mode: Mode,
        exclude: Option<Vec<String>>,
    ) -> Result<(), SettingsError> {
        let pid = profile_id.to_string();
        let prof = crate::load_profile(&self.conn, &pid)
            .map_err(|_| SettingsError::State)?
            .filter(|p| p.account_id == account_id)
            .ok_or(SettingsError::NotFound)?;
        let _ = prof;
        let ok: Option<(String, i64, i64)> = self
            .conn
            .query_row(
                "SELECT scope, present, deleted FROM ext_entities WHERE profile_id = ?1 AND ext_id = ?2",
                params![pid, ext_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|_| SettingsError::State)?;
        let Some((scope, present, deleted)) = ok else {
            return Err(SettingsError::NotFound);
        };
        if mode != Mode::Off && (scope != "synced" || present == 0 || deleted != 0) {
            return Err(SettingsError::NotAllowed);
        }
        let ex = exclude.map(|e| serde_json::to_string(&e).unwrap_or_else(|_| "[]".into()));
        self.conn
            .execute(
                "INSERT INTO ext_settings_config (profile_id, ext_id, mode, exclude, status)
                 VALUES (?1, ?2, ?3, COALESCE(?4, '[]'), NULL)
                 ON CONFLICT(profile_id, ext_id) DO UPDATE SET mode = excluded.mode,
                     exclude = COALESCE(?4, ext_settings_config.exclude), status = NULL, error = NULL",
                params![pid, ext_id, mode.as_str(), ex],
            )
            .map_err(|_| SettingsError::State)?;
        Ok(())
    }

    /// Extensions whose settings this device synchronises, with their areas.
    pub fn settings_jobs(&self, profile_id: Uuid) -> Result<Vec<SettingsJob>, StateError> {
        let pid = profile_id.to_string();
        let mut st = self.conn.prepare(
            "SELECT c.ext_id, c.mode, c.exclude FROM ext_settings_config c
             JOIN ext_entities e ON e.profile_id = c.profile_id AND e.ext_id = c.ext_id
             WHERE c.profile_id = ?1 AND c.mode != 'off' AND e.scope = 'synced' AND e.present = 1
               AND e.deleted = 0",
        )?;
        let rows = st.query_map([&pid], |r| {
            let mode: String = r.get(1)?;
            let ex: String = r.get(2)?;
            Ok(SettingsJob {
                ext_id: r.get(0)?,
                areas: Mode::parse(&mode).areas().to_vec(),
                exclude: serde_json::from_str(&ex).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// (browser, user-data dir, profile dir) of this device's local profile.
    pub fn settings_target(
        &self,
        profile_id: Uuid,
    ) -> Result<Option<(String, String, String)>, StateError> {
        target(&self.conn, &profile_id.to_string())
    }

    /// Record that a cycle could not read/write because the browser is open (or the store is
    /// locked / an error happened).
    pub fn settings_status(
        &mut self,
        profile_id: Uuid,
        ext_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StateError> {
        set_status(&self.conn, &profile_id.to_string(), ext_id, status, error)
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
    rows: &mut HashMap<String, SRow>,
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
            rec.f.insert(
                "value".into(),
                pv.map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            );
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

fn set_fields(value: &str, ver: &str) -> Fields {
    let mut f = Fields::new();
    f.insert(
        "value".into(),
        serde_json::from_str(value).unwrap_or(serde_json::Value::Null),
    );
    f.insert("ver".into(), ver.into());
    f
}

/// Outcome of one capture (counts only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsCapture {
    pub ops: usize,
    pub keys: usize,
    pub excluded_sensitive: usize,
    pub excluded_other: usize,
}

impl StateDb {
    /// Reconcile one area store snapshot (key → JSON text, read with the browser closed) of
    /// one extension: changed/new keys become `set` ops, keys gone since the last
    /// capture/apply become tombstones; keys with a remote value still to be written are
    /// frozen (their store value is stale, not a local decision). Idempotent.
    pub fn capture_settings(
        &mut self,
        src: &Source,
        ext_id: &str,
        area: Area,
        store: &BTreeMap<String, String>,
        exclude: &[String],
        now_ms: u64,
    ) -> Result<SettingsCapture, CaptureError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        claim(&tx, src)?;
        let pid = src.profile_id.to_string();
        let Some(ver) = local_version(&tx, &pid, ext_id)? else {
            return Ok(SettingsCapture::default());
        };
        let a = area.as_str();
        let mut rows: HashMap<String, SRow> = load_area(&tx, &pid, ext_id, a)?
            .into_iter()
            .map(|r| (id_of(&r), r))
            .collect();
        let mut out = Vec::new();
        let mut cap = SettingsCapture::default();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut budget = 0usize;
        for (k, raw) in store {
            let v = match es::check(k, raw, exclude) {
                Ok(v) => v,
                Err(Excluded::Sensitive) => {
                    cap.excluded_sensitive += 1;
                    continue;
                }
                Err(_) => {
                    cap.excluded_other += 1;
                    continue;
                }
            };
            let c = es::canonical(&v);
            // over the area budget: not synchronised (and not charged, so smaller keys after
            // it still fit); deterministic by key order
            if budget + k.len() + c.len() > es::MAX_AREA_BYTES {
                cap.excluded_other += 1;
                continue;
            }
            budget += k.len() + c.len();
            cap.keys += 1;
            let id = es::entity_id(ext_id, area, k);
            seen.insert(id.clone());
            let e = rows
                .entry(id.clone())
                .or_insert_with(|| new_row(ext_id, a, k));
            let fresh = e.hlc.is_empty() && e.value.is_none() && !e.deleted;
            if e.pending_apply {
                // a remote value is waiting to be written: the store's value is not a decision
                if e.value.as_deref() == Some(c.as_str()) && !e.deleted {
                    e.pending_apply = false; // already there (written outside, or equal)
                }
                e.local_value = Some(c);
                continue;
            }
            if fresh || e.deleted || e.value.as_deref() != Some(c.as_str()) {
                e.value = Some(c.clone());
                e.deleted = false;
                e.writer_version = Some(ver.clone());
                out.push(Pending {
                    id: id.clone(),
                    kind: OpKind::Set,
                    fields: Some(set_fields(&c, &ver)),
                });
            }
            e.local_value = Some(c);
        }
        // keys of this store known here and gone now: deleted locally. A key still in the
        // store but excluded this time (oversized, invalid, over the area budget) is not gone:
        // its last synchronised value stays, never a tombstone.
        for (id, e) in rows.iter_mut() {
            if seen.contains(id) || e.local_value.is_none() || store.contains_key(&e.key) {
                continue;
            }
            if es::is_sensitive(&e.key, exclude) {
                continue; // newly excluded: never turned into a delete
            }
            e.local_value = None;
            if e.pending_apply {
                continue;
            }
            if !e.deleted {
                e.deleted = true;
                e.value = None;
                out.push(Pending {
                    id: id.clone(),
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
        let prev: Option<String> = tx
            .query_row(
                "SELECT excluded FROM ext_settings_config WHERE profile_id = ?1 AND ext_id = ?2",
                params![pid, ext_id],
                |r| r.get(0),
            )
            .optional()?;
        let mut counts: BTreeMap<String, (u64, u64)> = prev
            .and_then(|p| serde_json::from_str(&p).ok())
            .unwrap_or_default();
        counts.insert(
            a.to_string(),
            (cap.excluded_sensitive as u64, cap.excluded_other as u64),
        );
        tx.execute(
            "INSERT INTO ext_settings_config (profile_id, ext_id, excluded, last_capture_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(profile_id, ext_id) DO UPDATE SET excluded = excluded.excluded,
                 last_capture_at = excluded.last_capture_at",
            params![
                pid,
                ext_id,
                serde_json::to_string(&counts).unwrap_or_else(|_| "{}".into()),
                (now_ms / 1000) as i64
            ],
        )?;
        refresh_conflicts(&tx, &pid, (now_ms / 1000) as i64)?;
        tx.commit()?;
        Ok(cap)
    }
}

// ------------------------------------------------------------------ remote merge

fn relevant(e: &SRow, op: &Op, published: i64) -> bool {
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
    let Some((ext, area, key)) = es::parse_id(&op.id) else {
        return Ok(false);
    };
    let row = load(tx, pid, ext, area.as_str(), key)?;
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
    let Some((ext, area, key)) = es::parse_id(&op.id) else {
        return Ok(());
    };
    let mut e =
        load(tx, pid, ext, area.as_str(), key)?.unwrap_or_else(|| new_row(ext, area.as_str(), key));
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
                let c = es::canonical(v);
                if c.len() > es::MAX_VALUE_BYTES {
                    return Ok(()); // never accepted from a remote either
                }
                e.value = Some(c);
                e.writer_version = f.get("ver").and_then(|v| v.as_str()).map(str::to_string);
                e.field_hlc.insert("value".into(), op.hlc.clone());
            }
            if e.deleted
                && e.deleted_hlc
                    .as_deref()
                    .map_or(true, |d| d < op.hlc.as_str())
            {
                e.deleted = false;
            }
            if !e.deleted && e.local_value != e.value {
                e.pending_apply = true;
            }
            e.hlc = e.hlc.clone().max(op.hlc.clone());
        }
        OpKind::Delete => {
            let newest = e.field_hlc.values().max().cloned().unwrap_or_default();
            if e.deleted || newest.as_str() >= op.hlc.as_str() {
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

/// Writes for one extension area (browser closed), all-or-nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsWrite {
    pub ext_id: String,
    pub area: Area,
    pub puts: Vec<(String, String)>,
    pub deletes: Vec<String>,
}

impl StateDb {
    /// What to write into the stores of `profile_id` now. A dataset whose pending values come
    /// from another extension version is not written (Version mismatch).
    pub fn plan_settings(&mut self, profile_id: Uuid) -> Result<Vec<SettingsWrite>, StateError> {
        let pid = profile_id.to_string();
        let mut out = Vec::new();
        for job in self.settings_jobs(profile_id)? {
            let ver = local_version(&self.conn, &pid, &job.ext_id)?;
            let mut mismatch = false;
            for area in &job.areas {
                let rows: Vec<SRow> = load_area(&self.conn, &pid, &job.ext_id, area.as_str())?
                    .into_iter()
                    .filter(|r| {
                        r.pending_apply && !r.conflict && !es::is_sensitive(&r.key, &job.exclude)
                    })
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                if rows.iter().any(|r| !r.deleted && r.writer_version != ver) {
                    mismatch = true;
                    continue; // never write values of another extension version
                }
                let mut w = SettingsWrite {
                    ext_id: job.ext_id.clone(),
                    area: *area,
                    puts: vec![],
                    deletes: vec![],
                };
                for r in rows {
                    match (r.deleted, r.value) {
                        (false, Some(v)) => w.puts.push((r.key, v)),
                        _ => w.deletes.push(r.key),
                    }
                }
                out.push(w);
            }
            // the browser is closed (that is when plans are made): the dataset is either
            // synchronised or waiting for a matching extension version
            set_status(
                &self.conn,
                &pid,
                &job.ext_id,
                if mismatch {
                    "version_mismatch"
                } else {
                    "synced"
                },
                None,
            )?;
        }
        Ok(out)
    }

    /// The store now holds what `w` wrote.
    pub fn settings_written(
        &mut self,
        profile_id: Uuid,
        w: &SettingsWrite,
        now: i64,
    ) -> Result<(), StateError> {
        let pid = profile_id.to_string();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let a = w.area.as_str();
        for (k, v) in &w.puts {
            if let Some(mut e) = load(&tx, &pid, &w.ext_id, a, k)? {
                e.local_value = Some(v.clone());
                if e.value.as_deref() == Some(v.as_str()) {
                    e.pending_apply = false;
                }
                save(&tx, &pid, &e)?;
            }
            finish_resolving(&tx, &pid, &es::entity_id(&w.ext_id, w.area, k), now)?;
        }
        for k in &w.deletes {
            if let Some(mut e) = load(&tx, &pid, &w.ext_id, a, k)? {
                e.local_value = None;
                if e.deleted {
                    e.pending_apply = false;
                }
                save(&tx, &pid, &e)?;
            }
            finish_resolving(&tx, &pid, &es::entity_id(&w.ext_id, w.area, k), now)?;
        }
        tx.execute(
            "UPDATE ext_settings_config SET last_apply_at = ?3 WHERE profile_id = ?1 AND ext_id = ?2",
            params![pid, w.ext_id, now],
        )?;
        tx.commit()?;
        Ok(())
    }
}

// ------------------------------------------------------------------ views

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsState {
    Off,
    WaitingForBrowserClose,
    Syncing,
    Synced,
    VersionMismatch,
    Conflict,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingsView {
    pub ext_id: String,
    pub mode: Mode,
    pub state: SettingsState,
    pub keys_local: u64,
    pub keys_sync: u64,
    pub pending: u64,
    pub conflicts: u64,
    pub excluded_sensitive: u64,
    pub excluded_other: u64,
    pub error: Option<String>,
    pub last_capture_at: Option<i64>,
    pub last_apply_at: Option<i64>,
}

impl StateDb {
    /// Settings state of one extension on this device (counts only; never values).
    pub fn settings_view(
        &self,
        profile_id: Uuid,
        ext_id: &str,
    ) -> Result<SettingsView, StateError> {
        let pid = profile_id.to_string();
        let c = config(&self.conn, &pid, ext_id)?;
        let count = |sql: &str| -> Result<u64, StateError> {
            Ok(self
                .conn
                .query_row(sql, params![pid, ext_id], |r| r.get::<_, i64>(0))?
                as u64)
        };
        let keys_local = count("SELECT COUNT(*) FROM ext_settings_entities WHERE profile_id = ?1 AND ext_id = ?2 AND area = 'local' AND deleted = 0")?;
        let keys_sync = count("SELECT COUNT(*) FROM ext_settings_entities WHERE profile_id = ?1 AND ext_id = ?2 AND area = 'sync' AND deleted = 0")?;
        let pending = count("SELECT COUNT(*) FROM ext_settings_entities WHERE profile_id = ?1 AND ext_id = ?2 AND pending_apply = 1")?;
        let conflicts = count("SELECT COUNT(*) FROM conflicts WHERE profile_id = ?1 AND col = 'extension_settings' AND sid LIKE ?2 || '/%' AND status = 'unresolved'")?;
        let state = if c.mode == Mode::Off {
            SettingsState::Off
        } else if conflicts > 0 {
            SettingsState::Conflict
        } else {
            match c.status.as_deref() {
                Some("error") => SettingsState::Error,
                Some("version_mismatch") if pending > 0 => SettingsState::VersionMismatch,
                Some("waiting") => SettingsState::WaitingForBrowserClose,
                _ if pending > 0 => SettingsState::Syncing,
                None => SettingsState::WaitingForBrowserClose, // never captured yet
                _ => SettingsState::Synced,
            }
        };
        Ok(SettingsView {
            ext_id: ext_id.to_string(),
            mode: c.mode,
            state,
            keys_local,
            keys_sync,
            pending,
            conflicts,
            excluded_sensitive: c.excluded_sensitive,
            excluded_other: c.excluded_other,
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
                "SELECT {} FROM conflicts WHERE profile_id = ?1 AND sid = ?2 AND col = 'extension_settings'
                 AND status IN ('unresolved', 'resolving')",
                conflict::ROW_COLS
            ),
            params![pid, id],
            conflict::row_from,
        )
        .optional()?)
}

fn version_of(e: &SRow) -> Version {
    let mut hlc = BTreeMap::new();
    if let Some(h) = e.field_hlc.get("value") {
        hlc.insert("value".into(), h.clone());
    }
    if let Some(h) = &e.deleted_hlc {
        hlc.insert("deleted".into(), h.clone());
    }
    Version {
        exists: !e.deleted,
        title: Some(e.key.clone()),
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
                    v.value = Some(es::canonical(val));
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

fn base_of(e: &SRow, published: i64) -> Option<Version> {
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
        title: Some(e.key.clone()),
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
    row: Option<&SRow>,
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
            "UPDATE ext_settings_entities SET conflict = 1 WHERE profile_id = ?1 AND ext_id || '/' || area || '/' || key = ?2",
            params![pid, op.id],
        )?;
    }
    Ok(())
}

fn load_id(conn: &Connection, pid: &str, id: &str) -> Result<Option<SRow>, StateError> {
    match es::parse_id(id) {
        Some((ext, area, key)) => load(conn, pid, ext, area.as_str(), key),
        None => Ok(None),
    }
}

fn subtype(local: &Version, remote: &Version) -> ConflictSubtype {
    match (local.exists, remote.exists) {
        (true, true) => ConflictSubtype::EditEdit,
        (true, false) => ConflictSubtype::EditDelete,
        (false, true) => ConflictSubtype::DeleteEdit,
        (false, false) => ConflictSubtype::DeleteDelete,
    }
}

fn recompute(tx: &Transaction<'_>, c: &Row, published: i64, now: i64) -> Result<bool, StateError> {
    let held = conflict::held_ops(tx, &c.id)?;
    let remote = fold(&held);
    let Some(e) = load_id(tx, &c.pid, &c.sid)? else {
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
            let ver = e.writer_version.clone().unwrap_or_default();
            let change = match (&e.deleted, &e.value) {
                (false, Some(v)) => Pending {
                    id: c.sid.clone(),
                    kind: OpKind::Set,
                    fields: Some(set_fields(v, &ver)),
                },
                _ => Pending {
                    id: c.sid.clone(),
                    kind: OpKind::Delete,
                    fields: None,
                },
            };
            let mut rows = HashMap::from([(c.sid.clone(), e.clone())]);
            publish_local(
                tx,
                &c.pid,
                &did,
                &mut rows,
                vec![change],
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
        "UPDATE ext_settings_entities SET conflict = 0 WHERE profile_id = ?1 AND ext_id || '/' || area || '/' || key = ?2",
        params![c.pid, c.sid],
    )?;
    Ok(false)
}

fn refresh_conflicts(tx: &Transaction<'_>, pid: &str, now: i64) -> Result<(), StateError> {
    let published = conflict::published_of(tx, pid)?;
    let mut st = tx.prepare(&format!(
        "SELECT {} FROM conflicts WHERE profile_id = ?1 AND col = 'extension_settings' AND status = 'unresolved'",
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
         WHERE profile_id = ?1 AND sid = ?2 AND col = 'extension_settings' AND status = 'resolving'",
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

/// Resolve a settings conflict (called by `StateDb::resolve_conflict`).
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
    let mut e = load_id(&tx, &c.pid, &c.sid)?.ok_or(ResolveError::NotFound)?;
    let ver = e.writer_version.clone().unwrap_or_default();
    let change = match res {
        Resolution::KeepLocal => match (e.deleted, e.value.clone()) {
            (false, Some(v)) => Pending {
                id: c.sid.clone(),
                kind: OpKind::Set,
                fields: Some(set_fields(&v, &ver)),
            },
            _ => Pending {
                id: c.sid.clone(),
                kind: OpKind::Delete,
                fields: None,
            },
        },
        Resolution::KeepRemote => {
            if c.remote.exists {
                let v = c.remote.value.clone().unwrap_or_else(|| "null".into());
                e.value = Some(v.clone());
                e.deleted = false;
                e.pending_apply = e.local_value.as_deref() != Some(v.as_str());
                Pending {
                    id: c.sid.clone(),
                    kind: OpKind::Set,
                    fields: Some(set_fields(&v, &ver)),
                }
            } else {
                e.deleted = true;
                e.value = None;
                e.pending_apply = e.local_value.is_some();
                Pending {
                    id: c.sid.clone(),
                    kind: OpKind::Delete,
                    fields: None,
                }
            }
        }
        _ => return Err(ResolveError::NotAllowed),
    };
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
