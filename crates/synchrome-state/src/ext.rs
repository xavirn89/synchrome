//! Extensions inventory + enabled state (M1 slice 10, `docs/03` §3.4).
//!
//! Same machinery as bookmarks: observations from `chrome.management` become ordinary ops
//! (`col: "extensions"`) in this device's journal (same seq/HLC/chunks/store_events), remote
//! ops merge by per-field LWW, remote changes reach the browser through explicit apply plans,
//! and a remote op meeting an unpublished local change is held in a conflict record (same
//! table, lifecycle and resolution API as bookmarks).
//!
//! What is never automatic: installing (the user opens the store page from the app) and
//! uninstalling (the user confirms "Remove here"). Enable/disable is applied with
//! `chrome.management.setEnabled` when the extension may be disabled; a refusal (e.g. a
//! user gesture is required) becomes "user action required" for the popup.

use crate::capture::{claim, emit_ops, pending_of, set_cursor, BaseRec, OpSpec};
use crate::conflict::{
    self, ConflictKind, ConflictStatus, ConflictSubtype, Group, Resolution, ResolveError,
    ResolveOutcome, Row, Version,
};
use crate::{CaptureError, CaptureOutcome, Source, StateDb, StateError};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use synchrome_sync::extensions::{
    self as ex, Availability, Class, ExtEvent, ExtInfo, Source as ExtSource, COLLECTION,
};
use synchrome_sync::{Fields, Op, OpKind};
use uuid::Uuid;

// ------------------------------------------------------------------ rows

#[derive(Debug, Clone, Default)]
pub(crate) struct ExtRow {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: String,
    pub enabled: bool,
    pub deleted: bool,
    pub field_hlc: BTreeMap<String, String>,
    pub deleted_hlc: Option<String>,
    pub hlc: String,
    pub local_seq: i64,
    pub base: Option<String>,
    pub scope: String,
    pub present: bool,
    pub seen: bool,
    pub local_enabled: Option<bool>,
    pub may_disable: Option<bool>,
    pub install_type: Option<String>,
    pub disabled_reason: Option<String>,
    pub pending_apply: bool,
    pub pending_fields: BTreeSet<String>,
    pub conflict: bool,
    pub want: Option<String>,
    pub local_state: Option<String>,
    pub local_error: Option<String>,
    /// (enabled, its HLC, deleted, its HLC) as loaded — the pre-edit values for BASE, since
    /// a row is changed before its ops are written.
    pub loaded: Option<(bool, Option<String>, bool, Option<String>)>,
}

const COLS: &str = "ext_id, name, version, source, enabled, deleted, field_hlc, deleted_hlc, hlc,
    local_seq, base, scope, present, seen, local_enabled, may_disable, install_type,
    disabled_reason, pending_apply, pending_fields, conflict, want, local_state, local_error";

fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ExtRow> {
    let fh: String = r.get(6)?;
    let pf: String = r.get(19)?;
    let b = |i: usize| -> rusqlite::Result<bool> { Ok(r.get::<_, i64>(i)? != 0) };
    let ob = |i: usize| -> rusqlite::Result<Option<bool>> {
        Ok(r.get::<_, Option<i64>>(i)?.map(|v| v != 0))
    };
    let mut row = ExtRow {
        id: r.get(0)?,
        name: r.get(1)?,
        version: r.get(2)?,
        source: r.get(3)?,
        enabled: b(4)?,
        deleted: b(5)?,
        field_hlc: serde_json::from_str(&fh).unwrap_or_default(),
        deleted_hlc: r.get(7)?,
        hlc: r.get(8)?,
        local_seq: r.get(9)?,
        base: r.get(10)?,
        scope: r.get(11)?,
        present: b(12)?,
        seen: b(13)?,
        local_enabled: ob(14)?,
        may_disable: ob(15)?,
        install_type: r.get(16)?,
        disabled_reason: r.get(17)?,
        pending_apply: b(18)?,
        pending_fields: serde_json::from_str(&pf).unwrap_or_default(),
        conflict: b(20)?,
        want: r.get(21)?,
        local_state: r.get(22)?,
        local_error: r.get(23)?,
        loaded: None,
    };
    row.loaded = Some((
        row.enabled,
        row.field_hlc.get("enabled").cloned(),
        row.deleted,
        row.deleted_hlc.clone(),
    ));
    Ok(row)
}

pub(crate) fn load(conn: &Connection, pid: &str, id: &str) -> Result<Option<ExtRow>, StateError> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM ext_entities WHERE profile_id = ?1 AND ext_id = ?2"),
            params![pid, id],
            from_row,
        )
        .optional()?)
}

fn load_all(conn: &Connection, pid: &str) -> Result<Vec<ExtRow>, StateError> {
    let mut st = conn.prepare(&format!(
        "SELECT {COLS} FROM ext_entities WHERE profile_id = ?1 ORDER BY name, ext_id"
    ))?;
    let rows = st.query_map([pid], from_row)?.collect::<Result<_, _>>()?;
    Ok(rows)
}

fn save(conn: &Connection, pid: &str, e: &ExtRow) -> Result<(), StateError> {
    conn.execute(
        "INSERT INTO ext_entities (profile_id, ext_id, name, version, source, enabled, deleted,
             field_hlc, deleted_hlc, hlc, local_seq, base, scope, present, seen, local_enabled,
             may_disable, install_type, disabled_reason, pending_apply, pending_fields, conflict,
             want, local_state, local_error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 ?19, ?20, ?21, ?22, ?23, ?24, ?25)
         ON CONFLICT(profile_id, ext_id) DO UPDATE SET name = excluded.name,
             version = excluded.version, source = excluded.source, enabled = excluded.enabled,
             deleted = excluded.deleted, field_hlc = excluded.field_hlc,
             deleted_hlc = excluded.deleted_hlc, hlc = excluded.hlc, local_seq = excluded.local_seq,
             base = excluded.base, scope = excluded.scope, present = excluded.present,
             seen = excluded.seen, local_enabled = excluded.local_enabled,
             may_disable = excluded.may_disable, install_type = excluded.install_type,
             disabled_reason = excluded.disabled_reason, pending_apply = excluded.pending_apply,
             pending_fields = excluded.pending_fields, conflict = excluded.conflict,
             want = excluded.want, local_state = excluded.local_state,
             local_error = excluded.local_error",
        params![
            pid,
            e.id,
            e.name,
            e.version,
            e.source,
            e.enabled as i64,
            e.deleted as i64,
            serde_json::to_string(&e.field_hlc).unwrap_or_else(|_| "{}".into()),
            e.deleted_hlc,
            e.hlc,
            e.local_seq,
            e.base,
            e.scope,
            e.present as i64,
            e.seen as i64,
            e.local_enabled.map(|v| v as i64),
            e.may_disable.map(|v| v as i64),
            e.install_type,
            e.disabled_reason,
            e.pending_apply as i64,
            serde_json::to_string(&e.pending_fields).unwrap_or_else(|_| "[]".into()),
            e.conflict as i64,
            e.want,
            e.local_state,
            e.local_error
        ],
    )?;
    Ok(())
}

fn full_fields(e: &ExtRow) -> Fields {
    let mut f = Fields::new();
    f.insert("name".into(), e.name.clone().into());
    f.insert("version".into(), e.version.clone().into());
    f.insert("source".into(), e.source.clone().into());
    f.insert("enabled".into(), e.enabled.into());
    f.insert("type".into(), "extension".into());
    f
}

/// A local change to publish for `id`.
struct Pending {
    id: String,
    kind: OpKind,
    fields: Option<Fields>,
}

/// Write the ops, then stamp HLCs / local seq / BASE on the rows they touched.
fn publish_local(
    tx: &Transaction<'_>,
    pid: &str,
    did: &str,
    rows: &mut HashMap<String, ExtRow>,
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
        // BASE: pre-edit enabled/deleted (+HLC) of the current unpublished run
        let mut rec = e
            .base
            .as_deref()
            .and_then(|b| serde_json::from_str::<BaseRec>(b).ok())
            .filter(|b| b.seq > published)
            .unwrap_or(BaseRec {
                seq: op.seq as i64,
                ..Default::default()
            });
        let prior = |e: &ExtRow, k: &str| -> (serde_json::Value, Option<String>) {
            let (en, enh, del, delh) = e.loaded.clone().unwrap_or((true, None, false, None));
            match k {
                "deleted" => (del.into(), delh),
                _ => (en.into(), enh),
            }
        };
        let names: Vec<&str> = match c.kind {
            OpKind::Delete => vec!["deleted"],
            OpKind::Set => vec!["enabled", "deleted"],
        };
        for k in names {
            if !rec.f.contains_key(k) {
                let (v, h) = prior(e, k);
                rec.f.insert(k.to_string(), v);
                if let Some(h) = h {
                    rec.h.insert(k.to_string(), h);
                }
            }
        }
        e.base = serde_json::to_string(&rec).ok();
        match c.kind {
            OpKind::Set => {
                for k in c.fields.iter().flatten().map(|(k, _)| k) {
                    e.field_hlc.insert(k.clone(), op.hlc.clone());
                }
            }
            OpKind::Delete => e.deleted_hlc = Some(op.hlc.clone()),
        }
        e.hlc = op.hlc.clone();
        e.local_seq = op.seq as i64;
    }
    Ok(ops.len())
}

// ------------------------------------------------------------------ capture

/// The Synchrome extension's own id (never synchronised as an ordinary extension).
fn synchrome_id() -> &'static str {
    synchrome_types::SYNCHROME_EXTENSION_ID
}

/// One observed extension of this device.
fn observe(e: &mut ExtRow, info: &ExtInfo, out: &mut Vec<Pending>) {
    let was_present = e.present;
    let fresh = e.name.is_empty() && e.hlc.is_empty() && !e.deleted;
    e.present = true;
    e.seen = true;
    e.local_enabled = Some(info.enabled);
    e.may_disable = Some(info.may_disable);
    e.install_type = Some(info.install_type.clone());
    e.disabled_reason = info.disabled_reason.clone();
    if e.local_state.as_deref() == Some("install_opened") {
        e.local_state = None;
    }
    if e.want.as_deref() == Some("install") {
        e.want = None;
    }
    // the user's own decision, if this observation is one (Chromium disabling an extension
    // after a permission increase is a local condition, not a state to publish)
    let user_value = if info.enabled {
        Some(true)
    } else if ex::user_disabled(info) {
        Some(false)
    } else {
        e.local_state = Some("user_action_required".into());
        None
    };
    if e.local_state.as_deref() == Some("protected") {
        if !info.may_disable {
            return; // still not changeable here: nothing to apply, nothing to publish
        }
        // Chromium lets it be changed again: bring it to the synchronised state
        e.local_state = None;
        if !e.deleted && user_value.is_some() && user_value != Some(e.enabled) {
            e.pending_apply = true;
            e.pending_fields.insert("enabled".into());
        }
    }
    if fresh {
        e.name = ex::clean_name(&info.name);
        e.version = ex::clean_version(&info.version);
        e.source = ex::source_of(info).as_str().into();
        e.enabled = user_value.unwrap_or(true);
        out.push(Pending {
            id: e.id.clone(),
            kind: OpKind::Set,
            fields: Some(full_fields(e)),
        });
        return;
    }
    if e.pending_apply {
        // a remote change is on its way to the browser: what the browser reports is not a
        // local decision; once it shows the materialised value the apply is complete
        if !e.deleted && e.local_enabled == Some(e.enabled) {
            e.pending_apply = false;
            e.pending_fields.clear();
            if e.local_state.as_deref() == Some("user_action_required") {
                e.local_state = None;
            }
        }
        return;
    }
    if e.deleted {
        if was_present {
            return; // removed on another device: kept here until the user decides
        }
        // installed here (again) after it was removed: a new installation
        e.deleted = false;
        e.name = ex::clean_name(&info.name);
        e.version = ex::clean_version(&info.version);
        e.source = ex::source_of(info).as_str().into();
        e.enabled = user_value.unwrap_or(true);
        out.push(Pending {
            id: e.id.clone(),
            kind: OpKind::Set,
            fields: Some(full_fields(e)),
        });
        return;
    }
    if !was_present {
        // it was missing here and is installed now: bring it to the synchronised state
        if e.may_disable != Some(false) && user_value.is_some() && user_value != Some(e.enabled) {
            e.pending_apply = true;
            e.pending_fields.insert("enabled".into());
        }
        return;
    }
    let mut f = Fields::new();
    if let Some(v) = user_value.filter(|v| *v != e.enabled) {
        e.enabled = v;
        f.insert("enabled".into(), v.into());
    }
    let v = ex::clean_version(&info.version);
    if ex::version_newer(&v, &e.version) {
        e.version = v;
        e.name = ex::clean_name(&info.name);
        f.insert("version".into(), e.version.clone().into());
        f.insert("name".into(), e.name.clone().into());
    }
    if !f.is_empty() {
        out.push(Pending {
            id: e.id.clone(),
            kind: OpKind::Set,
            fields: Some(f),
        });
    }
}

/// An extension of this device is gone (uninstalled here, or removed as requested).
fn absent(e: &mut ExtRow, out: &mut Vec<Pending>) {
    if !e.present {
        return;
    }
    e.present = false;
    e.local_enabled = None;
    e.pending_apply = false;
    e.pending_fields.clear();
    let requested = e.want.as_deref() == Some("remove");
    e.want = None;
    e.local_state = None;
    if !e.deleted && e.scope == "synced" && !requested {
        e.deleted = true;
        out.push(Pending {
            id: e.id.clone(),
            kind: OpKind::Delete,
            fields: None,
        });
    }
}

fn new_row(id: &str) -> ExtRow {
    ExtRow {
        id: id.to_string(),
        scope: "synced".into(),
        enabled: true,
        ..Default::default()
    }
}

fn process(
    tx: &Transaction<'_>,
    src: &Source,
    observed: &[ExtInfo],
    gone: &[String],
    full: bool,
    now_ms: u64,
) -> Result<usize, CaptureError> {
    let pid = src.profile_id.to_string();
    let mut rows: HashMap<String, ExtRow> = load_all(tx, &pid)?
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();
    let mut out = Vec::new();
    let mut seen_now: HashSet<String> = HashSet::new();
    for info in observed {
        match ex::classify(info, synchrome_id()) {
            Class::Ignored => continue,
            Class::Protected => {
                let e = rows
                    .entry(info.id.clone())
                    .or_insert_with(|| new_row(&info.id));
                if e.hlc.is_empty() {
                    e.scope = "protected".into();
                    e.name = ex::clean_name(&info.name);
                    e.version = ex::clean_version(&info.version);
                }
                e.present = true;
                e.seen = true;
                e.local_enabled = Some(info.enabled);
                e.may_disable = Some(info.may_disable);
                e.install_type = Some(info.install_type.clone());
                seen_now.insert(info.id.clone());
            }
            Class::Synced => {
                let e = rows
                    .entry(info.id.clone())
                    .or_insert_with(|| new_row(&info.id));
                if e.scope != "synced" {
                    continue;
                }
                observe(e, info, &mut out);
                seen_now.insert(info.id.clone());
            }
        }
    }
    let mut missing: Vec<String> = gone.to_vec();
    if full {
        missing.extend(
            rows.values()
                .filter(|r| r.present && !seen_now.contains(&r.id))
                .map(|r| r.id.clone()),
        );
    }
    for id in missing {
        if let Some(e) = rows.get_mut(&id) {
            if e.scope == "protected" {
                e.present = false;
                continue;
            }
            absent(e, &mut out);
        }
    }
    let n = publish_local(tx, &pid, &src.device_id.to_string(), &mut rows, out, now_ms)?;
    for r in rows.values() {
        save(tx, &pid, r)?;
    }
    refresh_conflicts(tx, &pid, (now_ms / 1000) as i64)?;
    Ok(n)
}

impl StateDb {
    /// Reconcile the full `chrome.management.getAll()` inventory (idempotent).
    pub fn capture_ext_snapshot(
        &mut self,
        src: &Source,
        epoch: &str,
        covers_through: u64,
        items: &[ExtInfo],
        now_ms: u64,
    ) -> Result<CaptureOutcome, CaptureError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        claim(&tx, src)?;
        let pid = src.profile_id.to_string();
        let n = process(&tx, src, items, &[], true, now_ms)?;
        set_cursor(&tx, &pid, &format!("ext:{epoch}"), covers_through, now_ms)?;
        let pending = pending_of(&tx, &pid)?;
        tx.commit()?;
        Ok(CaptureOutcome::Applied { ops: n, pending })
    }

    /// Apply numbered `chrome.management` events of one extension epoch (redelivery-safe).
    pub fn capture_ext_events(
        &mut self,
        src: &Source,
        epoch: &str,
        events: &[(u64, ExtEvent)],
        now_ms: u64,
    ) -> Result<CaptureOutcome, CaptureError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        claim(&tx, src)?;
        let pid = src.profile_id.to_string();
        let ep = format!("ext:{epoch}");
        let last: i64 = tx
            .query_row(
                "SELECT last_n FROM ext_cursor WHERE profile_id = ?1 AND epoch = ?2",
                params![pid, ep],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let fresh: Vec<&(u64, ExtEvent)> =
            events.iter().filter(|(n, _)| *n as i64 > last).collect();
        if fresh.is_empty() {
            let pending = pending_of(&tx, &pid)?;
            return Ok(CaptureOutcome::Duplicate { pending });
        }
        let mut n = 0;
        for (_, ev) in &fresh {
            n += match ev {
                ExtEvent::Installed { info }
                | ExtEvent::Enabled { info }
                | ExtEvent::Disabled { info } => {
                    process(&tx, src, std::slice::from_ref(info), &[], false, now_ms)?
                }
                ExtEvent::Uninstalled { id } => {
                    process(&tx, src, &[], std::slice::from_ref(id), false, now_ms)?
                }
            };
        }
        let max_n = fresh.iter().map(|(n, _)| *n).max().unwrap_or(0);
        set_cursor(&tx, &pid, &ep, max_n, now_ms)?;
        let pending = pending_of(&tx, &pid)?;
        tx.commit()?;
        Ok(CaptureOutcome::Applied { ops: n, pending })
    }
}

// ------------------------------------------------------------------ remote merge

/// Whether a remote op would change the pre-edit state (see bookmarks' `relevant`).
fn relevant(e: &ExtRow, op: &Op, published: i64) -> bool {
    let touches = match op.op {
        OpKind::Delete => true,
        OpKind::Set => op
            .fields
            .as_ref()
            .is_some_and(|f| f.contains_key("enabled")),
    };
    if !touches {
        return false; // informational fields never conflict
    }
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
    match op.op {
        OpKind::Delete => {
            let newest = e
                .field_hlc
                .keys()
                .filter_map(|k| before(k))
                .max()
                .unwrap_or_default();
            newest.as_str() < op.hlc.as_str()
        }
        OpKind::Set => before("enabled").map_or(true, |h| h.as_str() < op.hlc.as_str()),
    }
}

/// Merge one remote extensions op. Returns whether it was held in a conflict.
pub(crate) fn apply_remote_op(
    tx: &Transaction<'_>,
    pid: &str,
    published: i64,
    op: &Op,
    now: i64,
) -> Result<bool, StateError> {
    if !ex::valid_id(&op.id) || op.id == synchrome_id() {
        return Ok(false);
    }
    let row = load(tx, pid, &op.id)?;
    if row.as_ref().is_some_and(|r| r.scope == "protected") {
        return Ok(false); // managed by policy here: never touched
    }
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
    let mut e = load(tx, pid, &op.id)?.unwrap_or_else(|| new_row(&op.id));
    match op.op {
        OpKind::Set => {
            let newer = |e: &ExtRow, k: &str| {
                e.field_hlc
                    .get(k)
                    .map_or(true, |h| h.as_str() < op.hlc.as_str())
            };
            let mut enabled_changed = false;
            for (k, v) in op.fields.iter().flatten() {
                if !newer(&e, k) {
                    continue;
                }
                match k.as_str() {
                    "name" => e.name = ex::clean_name(v.as_str().unwrap_or_default()),
                    "version" => e.version = ex::clean_version(v.as_str().unwrap_or_default()),
                    "source" => {
                        e.source = ExtSource::parse(v.as_str().unwrap_or_default())
                            .as_str()
                            .into()
                    }
                    "enabled" => {
                        let b = v.as_bool().unwrap_or(true);
                        enabled_changed |= b != e.enabled;
                        e.enabled = b;
                    }
                    _ => continue,
                }
                e.field_hlc.insert(k.clone(), op.hlc.clone());
            }
            if e.deleted
                && e.deleted_hlc
                    .as_deref()
                    .map_or(true, |d| d < op.hlc.as_str())
            {
                e.deleted = false; // installed again somewhere: the desired state is "installed"
                enabled_changed = true;
            }
            if enabled_changed && e.present && !e.deleted && e.local_enabled != Some(e.enabled) {
                e.pending_apply = true;
                e.pending_fields.insert("enabled".into());
            }
            e.hlc = e.hlc.clone().max(op.hlc.clone());
        }
        OpKind::Delete => {
            let newest = e.field_hlc.values().max().cloned().unwrap_or_default();
            if e.deleted || newest.as_str() >= op.hlc.as_str() {
                return save(tx, pid, &e); // idempotent / a newer set wins
            }
            // removed on another device: never uninstalled automatically here
            e.deleted = true;
            e.deleted_hlc = Some(op.hlc.clone());
            e.pending_apply = false;
            e.pending_fields.clear();
            if e.want.as_deref() == Some("install") {
                e.want = None;
            }
        }
    }
    save(tx, pid, &e)
}

// ------------------------------------------------------------------ apply plans

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ExtAction {
    SetEnabled {
        id: String,
        enabled: bool,
    },
    /// Only after the user confirmed "Remove here" in the app.
    Uninstall {
        id: String,
    },
    /// Only after the user clicked "Install": opens the official store page in a tab.
    OpenInstall {
        id: String,
        url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtApplyOp {
    pub op_id: u32,
    #[serde(flatten)]
    pub action: ExtAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtApplyResult {
    pub op_id: u32,
    pub ok: bool,
    /// `gesture_required`, `not_found`, `protected`, or a short reason.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ExtPlan {
    pub apply_id: String,
    pub ops: Vec<ExtApplyOp>,
}

pub const MAX_EXT_PLAN_OPS: usize = 500;

fn browser_of(conn: &Connection, pid: &str) -> Result<String, StateError> {
    let id: Option<String> = conn
        .query_row(
            "SELECT source_identity FROM profiles WHERE profile_id = ?1",
            [pid],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(id
        .and_then(|i| i.split('|').next().map(str::to_string))
        .unwrap_or_default())
}

fn availability(e: &ExtRow, browser: &str) -> (Availability, Option<String>) {
    ex::install_url(&e.id, ExtSource::parse(&e.source), browser)
}

impl StateDb {
    /// The next extensions apply plan (possibly empty).
    pub fn plan_ext_apply(&mut self, profile_id: Uuid) -> Result<ExtPlan, StateError> {
        let pid = profile_id.to_string();
        let browser = browser_of(&self.conn, &pid)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut plan = ExtPlan {
            apply_id: Uuid::new_v4().to_string(),
            ops: vec![],
        };
        let mut next = 1;
        for mut e in load_all(&tx, &pid)? {
            if e.scope != "synced" || plan.ops.len() >= MAX_EXT_PLAN_OPS {
                continue;
            }
            let mut changed = false;
            let blocked = matches!(
                e.local_state.as_deref(),
                Some("user_action_required" | "apply_failed")
            );
            if e.present && !e.deleted && e.pending_apply && !e.conflict && !blocked {
                if e.local_enabled == Some(e.enabled) {
                    e.pending_apply = false;
                    e.pending_fields.clear();
                    changed = true;
                } else if e.may_disable == Some(false) {
                    // Chromium does not let it be changed here: never try
                    e.pending_apply = false;
                    e.pending_fields.clear();
                    e.local_state = Some("protected".into());
                    changed = true;
                } else {
                    plan.ops.push(ExtApplyOp {
                        op_id: next,
                        action: ExtAction::SetEnabled {
                            id: e.id.clone(),
                            enabled: e.enabled,
                        },
                    });
                    next += 1;
                }
            }
            if e.present && e.want.as_deref() == Some("remove") && !blocked {
                plan.ops.push(ExtApplyOp {
                    op_id: next,
                    action: ExtAction::Uninstall { id: e.id.clone() },
                });
                next += 1;
            }
            if !e.present && e.want.as_deref() == Some("install") {
                match availability(&e, &browser) {
                    (Availability::Available, Some(url)) => {
                        plan.ops.push(ExtApplyOp {
                            op_id: next,
                            action: ExtAction::OpenInstall {
                                id: e.id.clone(),
                                url,
                            },
                        });
                        next += 1;
                    }
                    _ => {
                        e.want = None;
                        e.local_state = Some("unavailable".into());
                        changed = true;
                    }
                }
            }
            if changed {
                save(&tx, &pid, &e)?;
            }
        }
        tx.commit()?;
        Ok(plan)
    }

    /// Record what the extension did with an extensions plan.
    pub fn record_ext_apply(
        &mut self,
        profile_id: Uuid,
        plan: &ExtPlan,
        results: &[ExtApplyResult],
        now: i64,
    ) -> Result<usize, StateError> {
        let pid = profile_id.to_string();
        let res: HashMap<u32, &ExtApplyResult> = results.iter().map(|r| (r.op_id, r)).collect();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut applied = 0;
        for op in &plan.ops {
            let Some(r) = res.get(&op.op_id) else {
                continue; // no result (worker stopped): planned again
            };
            let id = match &op.action {
                ExtAction::SetEnabled { id, .. }
                | ExtAction::Uninstall { id }
                | ExtAction::OpenInstall { id, .. } => id,
            };
            let Some(mut e) = load(&tx, &pid, id)? else {
                continue;
            };
            let err = r.error.clone().unwrap_or_else(|| "failed".into());
            match (&op.action, r.ok) {
                (ExtAction::SetEnabled { enabled, .. }, true) => {
                    e.local_enabled = Some(*enabled);
                    e.pending_apply = false;
                    e.pending_fields.clear();
                    e.local_state = None;
                    e.local_error = None;
                    finish_resolving(&tx, &pid, id, now)?;
                    applied += 1;
                }
                (ExtAction::Uninstall { .. }, true) => {
                    e.present = false;
                    e.local_enabled = None;
                    e.want = None;
                    e.local_state = None;
                    applied += 1;
                }
                (ExtAction::OpenInstall { .. }, true) => {
                    e.want = None;
                    e.local_state = Some("install_opened".into());
                    applied += 1;
                }
                (_, false) if err == "not_found" => {
                    e.present = false;
                    e.local_enabled = None;
                    e.pending_apply = false;
                    e.pending_fields.clear();
                    e.want = None;
                }
                (_, false) if err == "gesture_required" => {
                    e.local_state = Some("user_action_required".into());
                    e.local_error = Some(err);
                }
                (_, false) if err == "protected" => {
                    e.pending_apply = false;
                    e.pending_fields.clear();
                    e.want = None;
                    e.local_state = Some("protected".into());
                }
                (_, false) => {
                    e.local_state = Some("apply_failed".into());
                    e.local_error = Some(err);
                }
            }
            save(&tx, &pid, &e)?;
        }
        tx.commit()?;
        Ok(applied)
    }
}

// ------------------------------------------------------------------ user actions (app)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserExtAction {
    /// Open the store page for a missing extension (the user installs there).
    Install,
    /// Set the synchronised state to enabled / disabled (published, applied here).
    Enable,
    Disable,
    /// Confirmed in the app: uninstall here an extension removed on another device.
    Remove,
    /// Keep an extension removed on another device (it is installed again everywhere's state).
    Keep,
    /// Retry after "user action required" / "apply failed".
    Retry,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtActionError {
    #[error("extension not found in this profile")]
    NotFound,
    #[error("action not available for this extension now")]
    NotAllowed,
    #[error("this extension cannot be installed from a store on this browser")]
    NotInstallable,
    #[error("local state unavailable")]
    State,
}

impl StateDb {
    pub fn ext_user_action(
        &mut self,
        account_id: &str,
        profile_id: Uuid,
        ext_id: &str,
        action: UserExtAction,
        now_ms: u64,
    ) -> Result<(), ExtActionError> {
        let pid = profile_id.to_string();
        let browser = browser_of(&self.conn, &pid).map_err(|_| ExtActionError::State)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| ExtActionError::State)?;
        let prof = crate::load_profile(&tx, &pid)
            .map_err(|_| ExtActionError::State)?
            .filter(|p| p.account_id == account_id)
            .ok_or(ExtActionError::NotFound)?;
        let mut e = load(&tx, &pid, ext_id)
            .map_err(|_| ExtActionError::State)?
            .filter(|e| e.scope == "synced")
            .ok_or(ExtActionError::NotFound)?;
        if e.conflict {
            return Err(ExtActionError::NotAllowed); // resolve the conflict instead
        }
        let mut out = Vec::new();
        match action {
            UserExtAction::Install => {
                if e.present || e.deleted {
                    return Err(ExtActionError::NotAllowed);
                }
                if availability(&e, &browser).0 != Availability::Available {
                    return Err(ExtActionError::NotInstallable);
                }
                e.want = Some("install".into());
                e.local_state = None;
            }
            UserExtAction::Enable | UserExtAction::Disable => {
                let on = action == UserExtAction::Enable;
                if e.deleted || e.may_disable == Some(false) && e.present {
                    return Err(ExtActionError::NotAllowed);
                }
                e.enabled = on;
                e.local_state = None;
                e.local_error = None;
                if e.present && e.local_enabled != Some(on) {
                    e.pending_apply = true;
                    e.pending_fields.insert("enabled".into());
                }
                let mut f = Fields::new();
                f.insert("enabled".into(), on.into());
                out.push(Pending {
                    id: e.id.clone(),
                    kind: OpKind::Set,
                    fields: Some(f),
                });
            }
            UserExtAction::Remove => {
                if !e.present || !e.deleted {
                    return Err(ExtActionError::NotAllowed);
                }
                e.want = Some("remove".into());
                e.local_state = None;
            }
            UserExtAction::Keep => {
                if !e.present || !e.deleted {
                    return Err(ExtActionError::NotAllowed);
                }
                e.deleted = false;
                e.want = None;
                if let Some(le) = e.local_enabled {
                    e.enabled = le;
                }
                out.push(Pending {
                    id: e.id.clone(),
                    kind: OpKind::Set,
                    fields: Some(full_fields(&e)),
                });
            }
            UserExtAction::Retry => {
                if !matches!(
                    e.local_state.as_deref(),
                    Some("user_action_required" | "apply_failed")
                ) {
                    return Err(ExtActionError::NotAllowed);
                }
                e.local_state = None;
                e.local_error = None;
                if e.present && e.local_enabled != Some(e.enabled) {
                    e.pending_apply = true;
                    e.pending_fields.insert("enabled".into());
                }
            }
        }
        let mut rows = HashMap::from([(e.id.clone(), e)]);
        publish_local(&tx, &pid, &prof.device_id, &mut rows, out, now_ms)
            .map_err(|_| ExtActionError::State)?;
        for r in rows.values() {
            save(&tx, &pid, r).map_err(|_| ExtActionError::State)?;
        }
        tx.commit().map_err(|_| ExtActionError::State)?;
        Ok(())
    }
}

// ------------------------------------------------------------------ views (UI)

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtState {
    /// Installed here and in the synchronised state, same enabled flag.
    Synced,
    /// In the synchronised state, not installed here.
    Missing,
    /// Uninstalled on another device, still installed here.
    RemovedElsewhere,
    /// A remote enable/disable is being applied.
    Applying,
    /// Chromium needs the user (gesture, re-enable after a permission increase…).
    UserActionRequired,
    /// Policy-installed, or Chromium does not let Synchrome change it.
    Protected,
    Conflict,
    ApplyFailed,
    /// The store page was opened; finish the installation there.
    InstallOpened,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExtView {
    /// The Chromium extension id (public identity; the handle for actions).
    pub ext_id: String,
    pub name: String,
    pub version: String,
    pub source: String,
    pub state: ExtState,
    pub installed_here: bool,
    pub enabled_here: Option<bool>,
    /// The synchronised (desired) state.
    pub installed_synced: bool,
    pub enabled_synced: bool,
    pub availability: Availability,
    pub actions: Vec<UserExtAction>,
    pub error: Option<String>,
}

/// Counts for the profile header.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExtStatus {
    pub synced: u64,
    pub installed: u64,
    pub missing: u64,
    pub differences: u64,
    pub conflicts: u64,
    pub user_action: u64,
}

fn view_of(e: &ExtRow, browser: &str) -> ExtView {
    let (avail, _) = availability(e, browser);
    let state = if e.scope == "protected" {
        ExtState::Protected
    } else if e.conflict {
        ExtState::Conflict
    } else if e.present && e.local_state.as_deref() == Some("user_action_required") {
        ExtState::UserActionRequired
    } else if e.present && e.deleted {
        ExtState::RemovedElsewhere
    } else if !e.present && !e.deleted {
        if e.local_state.as_deref() == Some("install_opened") {
            ExtState::InstallOpened
        } else {
            ExtState::Missing
        }
    } else {
        match e.local_state.as_deref() {
            Some("user_action_required") => ExtState::UserActionRequired,
            Some("protected") => ExtState::Protected,
            Some("apply_failed") => ExtState::ApplyFailed,
            _ if e.pending_apply || e.want.is_some() => ExtState::Applying,
            _ => ExtState::Synced,
        }
    };
    use UserExtAction::*;
    let actions = match state {
        ExtState::Missing if avail == Availability::Available => vec![Install],
        ExtState::RemovedElsewhere => vec![Remove, Keep],
        ExtState::Synced if e.may_disable != Some(false) => {
            if e.enabled {
                vec![Disable]
            } else {
                vec![Enable]
            }
        }
        ExtState::UserActionRequired | ExtState::ApplyFailed => vec![Retry],
        _ => vec![],
    };
    ExtView {
        ext_id: e.id.clone(),
        name: e.name.clone(),
        version: e.version.clone(),
        source: e.source.clone(),
        state,
        installed_here: e.present,
        enabled_here: e.local_enabled,
        installed_synced: !e.deleted,
        enabled_synced: e.enabled,
        availability: avail,
        actions,
        error: e.local_error.clone(),
    }
}

impl StateDb {
    pub fn ext_views(&self, profile_id: Uuid) -> Result<Vec<ExtView>, StateError> {
        let pid = profile_id.to_string();
        let browser = browser_of(&self.conn, &pid)?;
        Ok(load_all(&self.conn, &pid)?
            .iter()
            .filter(|e| e.present || !e.deleted)
            .map(|e| view_of(e, &browser))
            .collect())
    }

    pub fn ext_status(&self, profile_id: Uuid) -> Result<ExtStatus, StateError> {
        let mut s = ExtStatus::default();
        for v in self.ext_views(profile_id)? {
            if v.installed_here {
                s.installed += 1;
            }
            match v.state {
                ExtState::Synced => s.synced += 1,
                ExtState::Missing | ExtState::InstallOpened => s.missing += 1,
                ExtState::Conflict => s.conflicts += 1,
                ExtState::UserActionRequired => s.user_action += 1,
                ExtState::RemovedElsewhere | ExtState::Applying | ExtState::ApplyFailed => {
                    s.differences += 1
                }
                ExtState::Protected => {}
            }
        }
        Ok(s)
    }

    /// Extensions whose apply needs the user in the browser (for the popup): (id, enable?).
    pub fn ext_user_actions(&self, profile_id: Uuid) -> Result<Vec<(String, String)>, StateError> {
        Ok(load_all(&self.conn, &profile_id.to_string())?
            .into_iter()
            .filter(|e| e.local_state.as_deref() == Some("user_action_required") && e.present)
            .map(|e| {
                let what = if e.want.as_deref() == Some("remove") {
                    "remove"
                } else if e.enabled {
                    "enable"
                } else {
                    "disable"
                };
                (e.id, what.to_string())
            })
            .collect())
    }
}

// ------------------------------------------------------------------ conflicts

fn open_conflict(conn: &Connection, pid: &str, id: &str) -> Result<Option<Row>, StateError> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {} FROM conflicts WHERE profile_id = ?1 AND sid = ?2 AND col = 'extensions'
                 AND status IN ('unresolved', 'resolving')",
                conflict::ROW_COLS
            ),
            params![pid, id],
            conflict::row_from,
        )
        .optional()?)
}

fn unpublished_groups(
    conn: &Connection,
    pid: &str,
    id: &str,
    published: i64,
) -> Result<BTreeSet<Group>, StateError> {
    let needle = format!("%\"id\":\"{id}\"%");
    let mut st =
        conn.prepare("SELECT line FROM ops WHERE profile_id = ?1 AND seq > ?2 AND line LIKE ?3")?;
    let mut out = BTreeSet::new();
    for l in st.query_map(params![pid, published, needle], |r| r.get::<_, String>(0))? {
        let Ok(op) = Op::from_line(&l?) else { continue };
        if op.id != id || op.col != COLLECTION {
            continue;
        }
        match op.op {
            OpKind::Delete => {
                out.insert(Group::Existence);
            }
            OpKind::Set => {
                if op
                    .fields
                    .as_ref()
                    .is_some_and(|f| f.contains_key("enabled"))
                {
                    out.insert(Group::Enabled);
                }
            }
        }
    }
    Ok(out)
}

fn version_of(e: &ExtRow) -> Version {
    let mut hlc = BTreeMap::new();
    if let Some(h) = e.field_hlc.get("enabled") {
        hlc.insert("enabled".into(), h.clone());
    }
    if let Some(h) = &e.deleted_hlc {
        hlc.insert("deleted".into(), h.clone());
    }
    Version {
        exists: !e.deleted,
        enabled: Some(e.enabled),
        title: Some(e.name.clone()),
        hlc,
        ..Default::default()
    }
}

/// REMOTE: held extension ops folded in HLC order.
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
                for (k, val) in o.fields.iter().flatten() {
                    match k.as_str() {
                        "enabled" => {
                            v.enabled = val.as_bool();
                            v.changed.insert(Group::Enabled);
                            v.hlc.insert("enabled".into(), o.hlc.clone());
                        }
                        "name" => v.title = val.as_str().map(str::to_string),
                        _ => {}
                    }
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
        v.changed.insert(Group::Existence);
    }
    v
}

fn base_of(e: &ExtRow, published: i64) -> Option<Version> {
    let rec: BaseRec = serde_json::from_str(e.base.as_deref()?).ok()?;
    if rec.seq <= published {
        return None;
    }
    let mut b = version_of(e);
    b.changed = [Group::Enabled, Group::Existence].into_iter().collect();
    if let Some(v) = rec.f.get("enabled").and_then(|v| v.as_bool()) {
        b.enabled = Some(v);
    }
    if let Some(d) = rec.f.get("deleted").and_then(|v| v.as_bool()) {
        b.exists = !d;
    }
    Some(b)
}

fn subtype(local: &Version, remote: &Version) -> ConflictSubtype {
    match (local.exists, remote.exists) {
        (true, true) => ConflictSubtype::EditEdit,
        (true, false) => ConflictSubtype::EditDelete,
        (false, true) => ConflictSubtype::DeleteEdit,
        (false, false) => ConflictSubtype::DeleteDelete,
    }
}

fn hold_op(
    tx: &Transaction<'_>,
    pid: &str,
    open: Option<Row>,
    row: Option<&ExtRow>,
    op: &Op,
    published: i64,
    now: i64,
) -> Result<(), StateError> {
    let c = match open {
        Some(c) => c,
        None => {
            let e = row.cloned().unwrap_or_else(|| new_row(&op.id));
            let mut local = version_of(&e);
            local.changed = unpublished_groups(tx, pid, &op.id, published)?;
            let base = base_of(&e, published);
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
            "UPDATE ext_entities SET conflict = 1 WHERE profile_id = ?1 AND ext_id = ?2",
            params![pid, op.id],
        )?;
    }
    Ok(())
}

/// Refresh snapshots; supersede when moot (same state on both sides, or the remote only
/// re-asserted this device's previous value). Returns whether it is still open.
fn recompute(tx: &Transaction<'_>, c: &Row, published: i64, now: i64) -> Result<bool, StateError> {
    let held = conflict::held_ops(tx, &c.id)?;
    let remote = fold(&held);
    let Some(e) = load(tx, &c.pid, &c.sid)? else {
        return Ok(true);
    };
    let mut local = version_of(&e);
    local.changed = unpublished_groups(tx, &c.pid, &c.sid, published)?;
    local.changed.extend(c.local.changed.iter().copied());
    let st = subtype(&local, &remote);
    tx.execute(
        "UPDATE conflicts SET local_json = ?2, remote_json = ?3, subtype = ?4, updated_at = ?5
         WHERE conflict_id = ?1",
        params![
            c.id,
            serde_json::to_string(&local).unwrap_or_default(),
            serde_json::to_string(&remote).unwrap_or_default(),
            st.as_str(),
            now
        ],
    )?;
    let same =
        local.exists == remote.exists && remote.changed.iter().all(|g| local.same(&remote, *g));
    // three-way: the remote side only re-asserts what this device had before its own
    // unpublished change (its BASE): nothing new arrived, the local change wins
    let base_only = c.base.as_ref().is_some_and(|b| {
        b.exists == remote.exists
            && (remote.enabled.is_none() || !remote.exists || b.enabled == remote.enabled)
    });
    let why = if st == ConflictSubtype::DeleteDelete {
        Some("removed on both sides")
    } else if same {
        Some("both sides made the same change")
    } else if base_only {
        Some("the other device only re-asserted this device's previous state")
    } else {
        None
    };
    let Some(why) = why else {
        return Ok(true);
    };
    if base_only && !same {
        // local wins; re-emit it with a fresh HLC when LWW alone would pick the remote op
        let (lh, rh) = if local.exists {
            (
                local.hlc.get("enabled").cloned().unwrap_or_default(),
                remote.hlc.get("enabled").cloned().unwrap_or_default(),
            )
        } else {
            (
                local.hlc.get("deleted").cloned().unwrap_or_default(),
                remote.hlc.values().max().cloned().unwrap_or_default(),
            )
        };
        if lh < rh {
            let did: String = tx.query_row(
                "SELECT device_id FROM profiles WHERE profile_id = ?1",
                [&c.pid],
                |r| r.get(0),
            )?;
            let change = if local.exists {
                let mut f = Fields::new();
                f.insert("enabled".into(), e.enabled.into());
                Pending {
                    id: e.id.clone(),
                    kind: OpKind::Set,
                    fields: Some(f),
                }
            } else {
                Pending {
                    id: e.id.clone(),
                    kind: OpKind::Delete,
                    fields: None,
                }
            };
            let mut rows = HashMap::from([(e.id.clone(), e.clone())]);
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
        "UPDATE conflicts SET status = 'superseded', detail = ?2, resolved_at = ?3, updated_at = ?3
         WHERE conflict_id = ?1",
        params![c.id, why, now],
    )?;
    for op in held {
        lww(tx, &c.pid, &op)?;
    }
    tx.execute(
        "UPDATE ext_entities SET conflict = 0 WHERE profile_id = ?1 AND ext_id = ?2",
        params![c.pid, c.sid],
    )?;
    Ok(false)
}

/// After local capture: refresh LOCAL of open extension conflicts (may supersede them).
fn refresh_conflicts(tx: &Transaction<'_>, pid: &str, now: i64) -> Result<(), StateError> {
    let published = conflict::published_of(tx, pid)?;
    let mut st = tx.prepare(&format!(
        "SELECT {} FROM conflicts WHERE profile_id = ?1 AND col = 'extensions' AND status = 'unresolved'",
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
         WHERE profile_id = ?1 AND sid = ?2 AND col = 'extensions' AND status = 'resolving'",
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

/// Resolve an extensions conflict (called by `StateDb::resolve_conflict`).
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
    let (l, r) = (&c.local, &c.remote);
    let mut out = Vec::new();
    match res {
        Resolution::KeepLocal => {
            if l.exists {
                let mut f = if !r.exists {
                    full_fields(&e)
                } else {
                    Fields::new()
                };
                f.insert("enabled".into(), e.enabled.into());
                out.push(Pending {
                    id: e.id.clone(),
                    kind: OpKind::Set,
                    fields: Some(f),
                });
            } else {
                out.push(Pending {
                    id: e.id.clone(),
                    kind: OpKind::Delete,
                    fields: None,
                });
            }
        }
        Resolution::KeepRemote => {
            if r.exists {
                e.deleted = false;
                if let Some(en) = r.enabled {
                    e.enabled = en;
                }
                if e.present && e.local_enabled != Some(e.enabled) {
                    e.pending_apply = true;
                    e.pending_fields.insert("enabled".into());
                }
                let mut f = if !l.exists {
                    full_fields(&e)
                } else {
                    Fields::new()
                };
                f.insert("enabled".into(), e.enabled.into());
                out.push(Pending {
                    id: e.id.clone(),
                    kind: OpKind::Set,
                    fields: Some(f),
                });
            } else {
                // removed on the other device: the desired state becomes "removed"; this
                // device keeps it installed until the user confirms "Remove here"
                e.deleted = true;
                e.pending_apply = false;
                e.pending_fields.clear();
                out.push(Pending {
                    id: e.id.clone(),
                    kind: OpKind::Delete,
                    fields: None,
                });
            }
        }
        _ => return Err(ResolveError::NotAllowed),
    }
    e.conflict = false;
    let pending = e.pending_apply;
    let mut rows = HashMap::from([(e.id.clone(), e)]);
    let n = publish_local(&tx, &c.pid, device_id, &mut rows, out, now_ms).map_err(|e| match e {
        CaptureError::Clock(c) => ResolveError::Clock(c),
        CaptureError::State(s) => ResolveError::State(s),
        other => ResolveError::State(StateError::Corrupt(other.to_string())),
    })?;
    for row in rows.values() {
        save(&tx, &c.pid, row)?;
    }
    let chosen = version_of(&rows[&c.sid]);
    let status = if pending { "resolving" } else { "resolved" };
    tx.execute(
        "UPDATE conflicts SET status = ?2, resolution = ?3, resolution_json = ?4,
                resolution_hlc = ?5, resolution_ops = ?6, updated_at = ?7,
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
