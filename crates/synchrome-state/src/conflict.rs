//! Conflict records and their resolution (M1 slice 9, `docs/03` §3.3).
//!
//! A conflict is LOCAL reconciliation state of this device (`state.sqlite`): it exists because
//! this device's own unpublished changes, its browser, or its local deletions met a remote op.
//! Other devices converge by the ordinary per-field LWW; only this device holds a divergence,
//! so only it can decide. A decision is published as ordinary journal ops with a fresh HLC
//! (later than every version involved), so every device converges on it through the normal
//! cycle; nothing about conflicts is stored in the cloud.
//!
//! Lifecycle: `unresolved` → (`resolving` while the browser is being brought to the decision)
//! → `resolved`; or `superseded` when later changes made it moot (both sides now agree, both
//! deleted, the orphan's folder exists again…). Records are kept as history.

use crate::capture::{load_state, persist, BaseRec};
use crate::{load_profile, StateDb, StateError};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use synchrome_sync::bookmarks::{BookmarkState, Change, Entity, Kind, Root};
use synchrome_sync::fractional::{key_between, keys_between};
use synchrome_sync::{Fields, Hlc, HlcError, Op, OpKind};
use thiserror::Error;
use uuid::Uuid;

// ------------------------------------------------------------------ model

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// A remote op met unpublished local changes of the same entity (held, not applied).
    LocalPending,
    /// The browser refused an operation of an apply plan.
    ApplyFailed,
    /// A remote create/move targets a folder deleted on this device.
    Orphan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictSubtype {
    /// Both sides changed the entity (fields may or may not overlap).
    EditEdit,
    /// Local edit, remote delete.
    EditDelete,
    /// Local delete, remote edit.
    DeleteEdit,
    /// Both deleted: superseded automatically.
    DeleteDelete,
    /// A remote folder delete refused because the folder holds items Synchrome did not delete.
    DeleteNotEmpty,
    /// Any other browser failure (transient or not): retried on request.
    ApplyError,
    /// See [`ConflictKind::Orphan`].
    Orphan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictStatus {
    Unresolved,
    Resolving,
    Resolved,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    KeepLocal,
    KeepRemote,
    Merge,
    Retry,
    Relocate,
}

/// Fields that are decided together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Title,
    Url,
    /// `parentId` + `pos` (+ derived `root`): a move is never split.
    Location,
    Existence,
    /// Extensions: the enabled flag.
    Enabled,
    /// Extension settings: the key's value.
    Value,
}

macro_rules! str_enum {
    ($t:ty { $($v:ident => $s:literal),* $(,)? }) => {
        impl $t {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$v => $s),* }
            }
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some(Self::$v),)* _ => None }
            }
        }
    };
}
str_enum!(ConflictKind { LocalPending => "local_pending", ApplyFailed => "apply_failed", Orphan => "orphan" });
str_enum!(ConflictSubtype {
    EditEdit => "edit_edit", EditDelete => "edit_delete", DeleteEdit => "delete_edit",
    DeleteDelete => "delete_delete", DeleteNotEmpty => "delete_not_empty",
    ApplyError => "apply_error", Orphan => "orphan",
});
str_enum!(ConflictStatus {
    Unresolved => "unresolved", Resolving => "resolving", Resolved => "resolved",
    Superseded => "superseded",
});
str_enum!(Resolution {
    KeepLocal => "keep_local", KeepRemote => "keep_remote", Merge => "merge",
    Retry => "retry", Relocate => "relocate",
});

pub(crate) fn group_of(field: &str) -> Option<Group> {
    match field {
        "title" => Some(Group::Title),
        "url" => Some(Group::Url),
        "parentId" | "pos" | "root" => Some(Group::Location),
        _ => None,
    }
}

/// One side of a conflict as persisted (`local_json` / `remote_json` / `base_json`).
///
/// LOCAL carries every value; REMOTE carries values only for the groups in `changed` (the
/// others are "unchanged by the remote device"); for BASE, `changed` lists the groups whose
/// pre-edit value is known.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub exists: bool,
    /// Extensions only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Extension settings only: canonical JSON of the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<String>,
    /// HLC per field (`title`, `url`, `parentId`, `pos`, `deleted`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hlc: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub changed: BTreeSet<Group>,
    /// Remote: device that wrote the newest held op, and its `seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

impl Version {
    /// Whether `self` already holds `other`'s value for `g` (a field `other` does not carry —
    /// e.g. a move that kept its `pos` key — is not compared).
    pub(crate) fn same(&self, other: &Version, g: Group) -> bool {
        let eq = |a: &Option<String>, b: &Option<String>| b.is_none() || a == b;
        match g {
            Group::Title => self.title == other.title,
            Group::Url => self.url == other.url,
            Group::Location => eq(&self.parent, &other.parent) && eq(&self.pos, &other.pos),
            Group::Existence => self.exists == other.exists,
            Group::Enabled => other.enabled.is_none() || self.enabled == other.enabled,
            Group::Value => self.value == other.value,
        }
    }
}

/// A conflict row.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub id: String,
    pub account_id: String,
    pub pid: String,
    pub sid: String,
    pub kind: ConflictKind,
    pub subtype: ConflictSubtype,
    pub status: ConflictStatus,
    pub created_at: i64,
    pub updated_at: i64,
    pub local: Version,
    pub remote: Version,
    pub base: Option<Version>,
    pub detail: Option<String>,
    pub attempts: i64,
    pub resolution: Option<Resolution>,
    pub resolution_ops: i64,
    pub resolved_at: Option<i64>,
    /// Collection of the entity (`bookmarks`, `extensions`).
    pub col: String,
}

pub(crate) const ROW_COLS: &str =
    "conflict_id, account_id, profile_id, sid, kind, subtype, status, created_at,
     updated_at, local_json, remote_json, base_json, detail, attempts, resolution,
     resolution_ops, resolved_at, col";

pub(crate) fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let js = |i: usize| -> rusqlite::Result<Version> {
        let s: String = r.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or_default())
    };
    let base: Option<String> = r.get(11)?;
    let kind: String = r.get(4)?;
    let subtype: String = r.get(5)?;
    let status: String = r.get(6)?;
    let resolution: Option<String> = r.get(14)?;
    Ok(Row {
        id: r.get(0)?,
        account_id: r.get(1)?,
        pid: r.get(2)?,
        sid: r.get(3)?,
        kind: ConflictKind::parse(&kind).unwrap_or(ConflictKind::LocalPending),
        subtype: ConflictSubtype::parse(&subtype).unwrap_or(ConflictSubtype::EditEdit),
        status: ConflictStatus::parse(&status).unwrap_or(ConflictStatus::Unresolved),
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        local: js(9)?,
        remote: js(10)?,
        base: base.and_then(|b| serde_json::from_str(&b).ok()),
        detail: r.get(12)?,
        attempts: r.get(13)?,
        resolution: resolution.as_deref().and_then(Resolution::parse),
        resolution_ops: r.get(15)?,
        resolved_at: r.get(16)?,
        col: r.get(17)?,
    })
}

/// The open (unresolved or resolving) conflict of an entity, if any.
pub(crate) fn open_for(conn: &Connection, pid: &str, sid: &str) -> Result<Option<Row>, StateError> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {ROW_COLS} FROM conflicts WHERE profile_id = ?1 AND sid = ?2
                 AND col = 'bookmarks' AND status IN ('unresolved', 'resolving')"
            ),
            params![pid, sid],
            row_from,
        )
        .optional()?)
}

pub(crate) fn by_id(conn: &Connection, id: &str) -> Result<Option<Row>, StateError> {
    Ok(conn
        .query_row(
            &format!("SELECT {ROW_COLS} FROM conflicts WHERE conflict_id = ?1"),
            [id],
            row_from,
        )
        .optional()?)
}

/// Held remote ops of a conflict, in merge order (HLC, then seq).
pub(crate) fn held_ops(conn: &Connection, id: &str) -> Result<Vec<Op>, StateError> {
    let mut st = conn.prepare("SELECT line FROM conflict_remote WHERE conflict_id = ?1")?;
    let mut ops: Vec<Op> = st
        .query_map([id], |r| r.get::<_, String>(0))?
        .filter_map(|l| l.ok().and_then(|l| Op::from_line(&l).ok()))
        .collect();
    ops.sort_by(|a, b| (&a.hlc, a.seq).cmp(&(&b.hlc, b.seq)));
    Ok(ops)
}

pub(crate) fn add_held(conn: &Connection, id: &str, op: &Op) -> Result<(), StateError> {
    conn.execute(
        "INSERT OR IGNORE INTO conflict_remote (conflict_id, hlc, remote_device, remote_seq, line)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id,
            op.hlc,
            op.by,
            (op.seq > 0).then_some(op.seq as i64),
            op.to_line()
        ],
    )?;
    Ok(())
}

pub(crate) fn device_of(hlc: &str) -> Option<String> {
    hlc.parse::<Hlc>().ok().map(|h| h.device_id)
}

/// The REMOTE version: the held ops folded in HLC order (per-field LWW, a newer set after a
/// delete resurrects).
pub(crate) fn fold_remote(ops: &[Op]) -> Version {
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
                    let s = val.as_str().map(str::to_string);
                    match k.as_str() {
                        "title" => v.title = s,
                        "url" => v.url = s,
                        "parentId" => v.parent = s,
                        "pos" => v.pos = s,
                        _ => {}
                    }
                    if let Some(g) = group_of(k) {
                        v.changed.insert(g);
                        if k != "root" {
                            v.hlc.insert(k.clone(), o.hlc.clone());
                        }
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

struct Ent {
    deleted: bool,
    title: String,
    url: Option<String>,
    parent: String,
    pos: String,
    field_hlc: BTreeMap<String, String>,
    deleted_hlc: Option<String>,
    base: Option<String>,
    pending_fields: BTreeSet<String>,
}

fn load_ent(conn: &Connection, pid: &str, sid: &str) -> Result<Option<Ent>, StateError> {
    Ok(conn
        .query_row(
            "SELECT deleted, title, url, parent, pos, field_hlc, deleted_hlc, base, pending_fields
             FROM entities WHERE profile_id = ?1 AND sid = ?2",
            params![pid, sid],
            |r| {
                let fh: String = r.get(5)?;
                let pf: String = r.get(8)?;
                Ok(Ent {
                    deleted: r.get::<_, i64>(0)? != 0,
                    title: r.get(1)?,
                    url: r.get(2)?,
                    parent: r.get(3)?,
                    pos: r.get(4)?,
                    field_hlc: serde_json::from_str(&fh).unwrap_or_default(),
                    deleted_hlc: r.get(6)?,
                    base: r.get(7)?,
                    pending_fields: serde_json::from_str(&pf).unwrap_or_default(),
                })
            },
        )
        .optional()?)
}

/// Groups changed by this device's unpublished ops of `sid`.
fn unpublished_groups(
    conn: &Connection,
    pid: &str,
    sid: &str,
    published: i64,
) -> Result<BTreeSet<Group>, StateError> {
    let needle = format!("%\"id\":\"{sid}\"%");
    let mut st =
        conn.prepare("SELECT line FROM ops WHERE profile_id = ?1 AND seq > ?2 AND line LIKE ?3")?;
    let mut out = BTreeSet::new();
    for l in st.query_map(params![pid, published, needle], |r| r.get::<_, String>(0))? {
        let Ok(op) = Op::from_line(&l?) else { continue };
        if op.id != sid {
            continue;
        }
        match op.op {
            OpKind::Delete => {
                out.insert(Group::Existence);
            }
            OpKind::Set => {
                for k in op.fields.iter().flatten().map(|(k, _)| k) {
                    if let Some(g) = group_of(k) {
                        out.insert(g);
                    }
                }
            }
        }
    }
    Ok(out)
}

fn version_of(e: &Ent) -> Version {
    let mut hlc: BTreeMap<String, String> = e
        .field_hlc
        .iter()
        .filter(|(k, _)| matches!(k.as_str(), "title" | "url" | "parentId" | "pos"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(d) = &e.deleted_hlc {
        hlc.insert("deleted".into(), d.clone());
    }
    Version {
        exists: !e.deleted,
        title: Some(e.title.clone()),
        url: e.url.clone(),
        parent: Some(e.parent.clone()),
        pos: Some(e.pos.clone()),
        hlc,
        ..Default::default()
    }
}

/// LOCAL version of an entity as it is now (= the browser, unless frozen).
fn local_version(
    conn: &Connection,
    pid: &str,
    sid: &str,
    published: i64,
) -> Result<Version, StateError> {
    let Some(e) = load_ent(conn, pid, sid)? else {
        return Ok(Version::default());
    };
    let mut v = version_of(&e);
    v.changed = unpublished_groups(conn, pid, sid, published)?;
    Ok(v)
}

/// BASE: LOCAL with the pre-edit values of its unpublished changes (when recorded).
fn base_version(e: &Ent, local: &Version, published: i64) -> Option<Version> {
    let rec: BaseRec = serde_json::from_str(e.base.as_deref()?).ok()?;
    if rec.seq <= published {
        return None; // stale: describes an already published run
    }
    let mut b = Version {
        exists: local.exists,
        title: local.title.clone(),
        url: local.url.clone(),
        parent: local.parent.clone(),
        pos: local.pos.clone(),
        changed: [Group::Title, Group::Url, Group::Location, Group::Existence]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let s = |k: &str| rec.f.get(k).and_then(|v| v.as_str()).map(str::to_string);
    for g in &local.changed {
        let known = match g {
            Group::Title => rec.f.contains_key("title"),
            Group::Url => rec.f.contains_key("url"),
            Group::Location => rec.f.contains_key("parentId") || rec.f.contains_key("pos"),
            Group::Existence => rec.f.contains_key("deleted"),
            Group::Enabled => rec.f.contains_key("enabled"),
            Group::Value => rec.f.contains_key("value"),
        };
        if !known {
            b.changed.remove(g);
        }
    }
    if rec.f.contains_key("title") {
        b.title = s("title");
    }
    if rec.f.contains_key("url") {
        b.url = s("url");
    }
    if rec.f.contains_key("parentId") {
        b.parent = s("parentId");
    }
    if rec.f.contains_key("pos") {
        b.pos = s("pos");
    }
    if let Some(d) = rec.f.get("deleted").and_then(|v| v.as_bool()) {
        b.exists = !d;
    }
    Some(b)
}

fn subtype_for_pending(local: &Version, remote: &Version) -> ConflictSubtype {
    match (local.exists, remote.exists) {
        (true, true) => ConflictSubtype::EditEdit,
        (true, false) => ConflictSubtype::EditDelete,
        (false, true) => ConflictSubtype::DeleteEdit,
        (false, false) => ConflictSubtype::DeleteDelete,
    }
}

/// Nothing left to decide.
fn moot(c: &Row, local: &Version, remote: &Version) -> Option<&'static str> {
    match (c.kind, c.subtype) {
        (ConflictKind::LocalPending, ConflictSubtype::DeleteDelete) => {
            Some("deleted on both sides")
        }
        (ConflictKind::LocalPending, _)
            if local.exists == remote.exists
                && remote.changed.iter().all(|g| local.same(remote, *g)) =>
        {
            Some("both sides made the same change")
        }
        // three-way: every remote change only brings back what this device had before its
        // own unpublished edit (its BASE) — e.g. another device's Keep-remote resolution
        // re-asserting this device's previous value: nothing new arrived, the local edit wins
        (ConflictKind::LocalPending, ConflictSubtype::EditEdit)
            if c.base.as_ref().is_some_and(|b| {
                remote.changed.iter().all(|g| {
                    local.same(remote, *g) || (b.changed.contains(g) && b.same(remote, *g))
                })
            }) =>
        {
            Some(BASE_ONLY)
        }
        (ConflictKind::ApplyFailed, ConflictSubtype::DeleteNotEmpty) if remote.exists => {
            Some("the remote side restored the folder")
        }
        _ => None,
    }
}

const BASE_ONLY: &str = "the other device only re-asserted this device's previous value";

fn group_hlc(v: &Version, g: Group) -> Option<&str> {
    let f: &[&str] = match g {
        Group::Title => &["title"],
        Group::Url => &["url"],
        Group::Location => &["parentId", "pos"],
        Group::Existence => &["deleted"],
        Group::Enabled => &["enabled"],
        Group::Value => &["value"],
    };
    f.iter()
        .filter_map(|k| v.hlc.get(*k))
        .map(String::as_str)
        .max()
}

/// The local edit wins a [`BASE_ONLY`] record: when LWW alone would pick the (older-valued)
/// remote op — its HLC is newer — the local values are re-emitted with a fresh HLC so every
/// device converges on them.
fn reassert_local(
    tx: &Transaction<'_>,
    c: &Row,
    local: &Version,
    remote: &Version,
) -> Result<(), StateError> {
    let groups: BTreeSet<Group> = remote
        .changed
        .iter()
        .copied()
        .filter(|g| *g != Group::Existence)
        .filter(|g| group_hlc(local, *g) < group_hlc(remote, *g))
        .collect();
    if groups.is_empty() {
        return Ok(());
    }
    let mut state = load_state(tx, &c.pid)?;
    let Some(e) = state.entities.get(&c.sid).cloned() else {
        return Ok(());
    };
    let did: String = tx.query_row(
        "SELECT device_id FROM profiles WHERE profile_id = ?1",
        [&c.pid],
        |r| r.get(0),
    )?;
    let ed = set(&e, &groups, false);
    state.dirty_entities.insert(c.sid.clone());
    persist(
        tx,
        &c.pid,
        &did,
        &state,
        &[ed.change],
        synchrome_sync::hlc::now_ms(),
    )
    .map_err(|e| match e {
        crate::CaptureError::State(s) => s,
        other => StateError::Corrupt(other.to_string()),
    })?;
    Ok(())
}

fn mergeable(c: &Row) -> bool {
    c.kind == ConflictKind::LocalPending
        && c.subtype == ConflictSubtype::EditEdit
        && !c.local.changed.is_empty()
        && !c.remote.changed.is_empty()
        && c.local.changed.is_disjoint(&c.remote.changed)
}

/// The resolutions a conflict offers, best first.
pub(crate) fn options(c: &Row) -> Vec<Resolution> {
    use Resolution::*;
    if c.col == synchrome_sync::extensions::COLLECTION {
        return crate::ext::options(c);
    }
    if c.col == synchrome_sync::extension_settings::COLLECTION {
        return crate::ext_settings::options(c);
    }
    if c.col == synchrome_sync::browser_prefs::COLLECTION {
        return crate::browser_prefs::options(c);
    }
    match (c.kind, c.subtype) {
        (ConflictKind::LocalPending, ConflictSubtype::EditEdit) => {
            let mut v = vec![KeepLocal, KeepRemote];
            if mergeable(c) {
                v.insert(0, Merge);
            }
            v
        }
        (ConflictKind::LocalPending, ConflictSubtype::DeleteDelete) => vec![],
        (ConflictKind::LocalPending, _) => vec![KeepLocal, KeepRemote],
        (ConflictKind::ApplyFailed, ConflictSubtype::DeleteNotEmpty) => {
            vec![KeepLocal, Relocate, KeepRemote, Retry]
        }
        (ConflictKind::ApplyFailed, _) => vec![Retry],
        (ConflictKind::Orphan, _) => vec![Relocate, KeepRemote, KeepLocal],
    }
}

/// Whether a resolution deletes something that still exists on one side.
pub(crate) fn destructive(c: &Row, r: Resolution) -> bool {
    matches!(
        (c.subtype, r),
        (ConflictSubtype::EditDelete, Resolution::KeepRemote)
            | (ConflictSubtype::DeleteEdit, Resolution::KeepLocal)
            | (ConflictSubtype::DeleteNotEmpty, Resolution::KeepRemote)
            | (ConflictSubtype::Orphan, Resolution::KeepLocal)
    )
}

fn save_snapshots(
    conn: &Connection,
    c: &Row,
    local: &Version,
    remote: &Version,
    subtype: ConflictSubtype,
    now: i64,
) -> Result<(), StateError> {
    conn.execute(
        "UPDATE conflicts SET local_json = ?2, remote_json = ?3, subtype = ?4, updated_at = ?5
         WHERE conflict_id = ?1",
        params![
            c.id,
            serde_json::to_string(local).unwrap_or_default(),
            serde_json::to_string(remote).unwrap_or_default(),
            subtype.as_str(),
            now
        ],
    )?;
    Ok(())
}

/// Close as superseded: the held ops are merged normally (LWW) and remote apply resumes.
fn supersede(tx: &Transaction<'_>, c: &Row, why: &str, now: i64) -> Result<(), StateError> {
    tx.execute(
        "UPDATE conflicts SET status = 'superseded', detail = ?2, resolved_at = ?3, updated_at = ?3
         WHERE conflict_id = ?1",
        params![c.id, why, now],
    )?;
    // (deleted, title, url, parent, pos, pending_apply)
    type Visible = (bool, String, Option<String>, String, String, bool);
    let visible = |tx: &Transaction<'_>| -> Result<Option<Visible>, StateError> {
        Ok(tx
            .query_row(
                "SELECT deleted, title, url, parent, pos, pending_apply FROM entities
                 WHERE profile_id = ?1 AND sid = ?2",
                params![c.pid, c.sid],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)? != 0,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get::<_, i64>(5)? != 0,
                    ))
                },
            )
            .optional()?)
    };
    let before = visible(tx)?;
    for op in held_ops(tx, &c.id)? {
        crate::pull::apply_lww(tx, &c.pid, &op)?;
    }
    let after = visible(tx)?;
    // merged values identical to what the browser already shows: nothing to apply (and the
    // entity must not stay frozen for a no-op)
    let unchanged = match (&before, &after) {
        (Some(b), Some(a)) => {
            !b.5 && (b.0, &b.1, &b.2, &b.3, &b.4) == (a.0, &a.1, &a.2, &a.3, &a.4)
        }
        _ => false,
    };
    tx.execute(
        "UPDATE entities SET conflict = 0,
                pending_apply = CASE WHEN ?3 THEN 0 ELSE pending_apply END,
                pending_fields = CASE WHEN ?3 THEN '[]' ELSE pending_fields END
         WHERE profile_id = ?1 AND sid = ?2",
        params![c.pid, c.sid, unchanged],
    )?;
    Ok(())
}

/// Refresh LOCAL / REMOTE / subtype of an unresolved conflict; supersede it when moot.
/// Returns whether it is still open.
fn recompute(tx: &Transaction<'_>, c: &Row, published: i64, now: i64) -> Result<bool, StateError> {
    let held = held_ops(tx, &c.id)?;
    let remote = if held.is_empty() {
        c.remote.clone()
    } else {
        fold_remote(&held)
    };
    let mut local = local_version(tx, &c.pid, &c.sid, published)?;
    if c.kind == ConflictKind::Orphan {
        return Ok(true);
    }
    local.changed.extend(c.local.changed.iter().copied());
    let subtype = match c.kind {
        ConflictKind::LocalPending => subtype_for_pending(&local, &remote),
        _ => c.subtype,
    };
    save_snapshots(tx, c, &local, &remote, subtype, now)?;
    let fresh = Row {
        subtype,
        local: local.clone(),
        remote: remote.clone(),
        ..c.clone()
    };
    if let Some(why) = moot(&fresh, &local, &remote) {
        if why == BASE_ONLY {
            reassert_local(tx, &fresh, &local, &remote)?;
        }
        supersede(tx, &fresh, why, now)?;
        return Ok(false);
    }
    Ok(true)
}

pub(crate) fn account_of(conn: &Connection, pid: &str) -> Result<String, StateError> {
    Ok(conn
        .query_row(
            "SELECT account_id FROM profiles WHERE profile_id = ?1",
            [pid],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default())
}

#[allow(clippy::too_many_arguments)]
fn insert_row(
    conn: &Connection,
    pid: &str,
    sid: &str,
    kind: ConflictKind,
    subtype: ConflictSubtype,
    local: &Version,
    remote: &Version,
    base: Option<&Version>,
    detail: Option<&str>,
    now: i64,
) -> Result<String, StateError> {
    insert_row_col(
        conn,
        "bookmarks",
        pid,
        sid,
        kind,
        subtype,
        local,
        remote,
        base,
        detail,
        now,
    )
}

/// [`insert_row`] for any collection.
#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_row_col(
    conn: &Connection,
    col: &str,
    pid: &str,
    sid: &str,
    kind: ConflictKind,
    subtype: ConflictSubtype,
    local: &Version,
    remote: &Version,
    base: Option<&Version>,
    detail: Option<&str>,
    now: i64,
) -> Result<String, StateError> {
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO conflicts (conflict_id, account_id, profile_id, sid, kind, subtype, status,
                                created_at, updated_at, local_json, remote_json, base_json, detail, col)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'unresolved', ?7, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id,
            account_of(conn, pid)?,
            pid,
            sid,
            kind.as_str(),
            subtype.as_str(),
            now,
            serde_json::to_string(local).unwrap_or_default(),
            serde_json::to_string(remote).unwrap_or_default(),
            base.map(|b| serde_json::to_string(b).unwrap_or_default()),
            detail,
            col
        ],
    )?;
    Ok(id)
}

/// Hold a remote op in the entity's conflict (created on first hold). Snapshots are taken at
/// creation (LOCAL + BASE) and refreshed on every later hold (REMOTE = all held ops folded).
pub(crate) fn hold(
    tx: &Transaction<'_>,
    pid: &str,
    open: Option<Row>,
    op: &Op,
    published: i64,
    now: i64,
) -> Result<(), StateError> {
    let c = match open {
        Some(c) => c,
        None => {
            let local = local_version(tx, pid, &op.id, published)?;
            let base = match load_ent(tx, pid, &op.id)? {
                Some(e) => base_version(&e, &local, published),
                None => None,
            };
            let id = insert_row(
                tx,
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
            by_id(tx, &id)?.ok_or_else(|| StateError::Corrupt("conflict".into()))?
        }
    };
    add_held(tx, &c.id, op)?;
    if recompute(tx, &c, published, now)? {
        tx.execute(
            "UPDATE entities SET conflict = 1 WHERE profile_id = ?1 AND sid = ?2",
            params![pid, op.id],
        )?;
    }
    Ok(())
}

pub(crate) fn published_of(conn: &Connection, pid: &str) -> Result<i64, StateError> {
    Ok(conn
        .query_row(
            "SELECT published_through FROM profiles WHERE profile_id = ?1",
            [pid],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// After local capture: refresh the LOCAL side of affected unresolved conflicts (a local
/// change can also make a conflict moot, e.g. the user typed the remote title).
pub(crate) fn after_local_changes(
    tx: &Transaction<'_>,
    pid: &str,
    changes: &[Change],
    now: i64,
) -> Result<(), StateError> {
    if changes.is_empty() {
        return Ok(());
    }
    let published = published_of(tx, pid)?;
    let mut seen = HashSet::new();
    for ch in changes {
        if !seen.insert(ch.sid()) {
            continue;
        }
        if let Some(c) = open_for(tx, pid, ch.sid())? {
            if c.status == ConflictStatus::Unresolved && c.kind != ConflictKind::Orphan {
                let mut c2 = c.clone();
                c2.local
                    .changed
                    .extend(unpublished_groups(tx, pid, &c.sid, published)?);
                recompute(tx, &c2, published, now)?;
            }
        }
    }
    Ok(())
}

/// An orphan: `sid` (alive, created/moved by a remote op) sits in a folder deleted here.
pub(crate) fn create_orphan(
    tx: &Transaction<'_>,
    pid: &str,
    sid: &str,
    now: i64,
) -> Result<(), StateError> {
    if open_for(tx, pid, sid)?.is_some() {
        return Ok(());
    }
    let Some(e) = load_ent(tx, pid, sid)? else {
        return Ok(());
    };
    let mut remote = version_of(&e);
    remote.changed = [Group::Location].into_iter().collect();
    remote.device = e.field_hlc.get("parentId").and_then(|h| device_of(h));
    let parent_del = load_ent(tx, pid, &e.parent)?.and_then(|p| p.deleted_hlc);
    let mut local = Version {
        exists: false,
        parent: Some(e.parent.clone()),
        changed: [Group::Existence].into_iter().collect(),
        ..Default::default()
    };
    if let Some(h) = parent_del {
        local.hlc.insert("deleted".into(), h);
    }
    insert_row(
        tx,
        pid,
        sid,
        ConflictKind::Orphan,
        ConflictSubtype::Orphan,
        &local,
        &remote,
        None,
        Some("its folder was deleted on this device"),
        now,
    )?;
    tx.execute(
        "UPDATE entities SET conflict = 1 WHERE profile_id = ?1 AND sid = ?2",
        params![pid, sid],
    )?;
    Ok(())
}

/// Records whose situation changed are superseded: orphans whose folder exists again (or that
/// were deleted), folder deletes that no longer hold anything.
pub(crate) fn reevaluate(tx: &Transaction<'_>, pid: &str, now: i64) -> Result<(), StateError> {
    let mut st = tx.prepare(&format!(
        "SELECT {ROW_COLS} FROM conflicts WHERE profile_id = ?1 AND kind = 'orphan' AND status = 'unresolved'"
    ))?;
    let rows: Vec<Row> = st.query_map([pid], row_from)?.collect::<Result<_, _>>()?;
    drop(st);
    for c in rows {
        let Some(e) = load_ent(tx, pid, &c.sid)? else {
            continue;
        };
        let parent_alive = Root::from_sentinel(&e.parent).is_some()
            || load_ent(tx, pid, &e.parent)?.is_some_and(|p| !p.deleted);
        if e.deleted {
            supersede(tx, &c, "the item was deleted", now)?;
        } else if parent_alive {
            supersede(tx, &c, "its folder exists again", now)?;
        }
    }
    // A folder whose remote delete was refused because it held local items: once nothing is
    // left in it (the items were moved elsewhere or deleted), the remote delete is no longer
    // ambiguous and is applied.
    let mut st = tx.prepare(&format!(
        "SELECT {ROW_COLS} FROM conflicts WHERE profile_id = ?1 AND kind = 'apply_failed'
         AND subtype = 'delete_not_empty' AND status = 'unresolved'"
    ))?;
    let rows: Vec<Row> = st.query_map([pid], row_from)?.collect::<Result<_, _>>()?;
    drop(st);
    for c in rows {
        let kids: i64 = tx.query_row(
            "SELECT COUNT(*) FROM entities WHERE profile_id = ?1 AND parent = ?2 AND deleted = 0",
            params![pid, c.sid],
            |r| r.get(0),
        )?;
        if kids == 0 && !c.remote.exists {
            supersede(
                tx,
                &c,
                "the folder is empty now; the remote delete is applied",
                now,
            )?;
        }
    }
    Ok(())
}

impl StateDb {
    /// Held remote ops of a conflict in merge order: (device, seq, HLC). Diagnostics.
    pub fn conflict_held(
        &self,
        conflict_id: &str,
    ) -> Result<Vec<(String, Option<u64>, String)>, StateError> {
        let mut st = self.conn.prepare(
            "SELECT remote_device, remote_seq, hlc FROM conflict_remote WHERE conflict_id = ?1
             ORDER BY hlc",
        )?;
        let rows = st.query_map([conflict_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?.map(|s| s as u64),
                r.get::<_, String>(2)?,
            ))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// A browser op of an apply plan failed for `sid`. A conflict being resolved goes back to
/// unresolved (nothing is lost: its LOCAL / REMOTE snapshots stay); otherwise a new
/// `apply_failed` conflict is created. For a failed delete the remote delete is held (the
/// caller restores the entity, which is still in the browser).
pub(crate) fn on_apply_failed(
    tx: &Transaction<'_>,
    pid: &str,
    sid: &str,
    error: &str,
    was_delete: bool,
    now: i64,
) -> Result<(), StateError> {
    let subtype = if was_delete && error == "not_empty" {
        ConflictSubtype::DeleteNotEmpty
    } else {
        ConflictSubtype::ApplyError
    };
    let Some(e) = load_ent(tx, pid, sid)? else {
        return Ok(());
    };
    let tomb = || -> Option<Op> {
        let h = e.deleted_hlc.clone()?;
        Some(Op {
            seq: 0,
            by: device_of(&h)?,
            hlc: h,
            col: synchrome_sync::bookmarks::COLLECTION.into(),
            id: sid.to_string(),
            op: OpKind::Delete,
            fields: None,
        })
    };
    match open_for(tx, pid, sid)? {
        Some(c) => {
            tx.execute(
                "UPDATE conflicts SET kind = 'apply_failed', subtype = ?2, status = 'unresolved',
                        detail = ?3, attempts = attempts + 1, resolution = NULL,
                        resolved_at = NULL, updated_at = ?4
                 WHERE conflict_id = ?1",
                params![c.id, subtype.as_str(), error, now],
            )?;
            if was_delete && held_ops(tx, &c.id)?.is_empty() {
                if let Some(op) = tomb() {
                    add_held(tx, &c.id, &op)?;
                }
            }
        }
        None => {
            let (local, remote, held) = if was_delete {
                let mut local = version_of(&e);
                local.exists = true;
                let held = tomb();
                let remote = held
                    .as_ref()
                    .map(|o| fold_remote(std::slice::from_ref(o)))
                    .unwrap_or_default();
                (local, remote, held)
            } else {
                // what Synchrome wanted the browser to show (the fields of the failed ops)
                let mut want = version_of(&e);
                want.changed = e
                    .pending_fields
                    .iter()
                    .filter_map(|f| group_of(f))
                    .collect();
                if e.pending_fields.contains("created") {
                    want.changed.insert(Group::Existence);
                }
                want.device = e.field_hlc.values().max().and_then(|h| device_of(h));
                let local = Version {
                    exists: true,
                    ..Default::default()
                };
                (local, want, None)
            };
            let id = insert_row(
                tx,
                pid,
                sid,
                ConflictKind::ApplyFailed,
                subtype,
                &local,
                &remote,
                None,
                Some(error),
                now,
            )?;
            tx.execute(
                "UPDATE conflicts SET attempts = 1 WHERE conflict_id = ?1",
                [&id],
            )?;
            if let Some(op) = held {
                add_held(tx, &id, &op)?;
            }
        }
    }
    Ok(())
}

/// The browser now matches a resolution of `sid`.
pub(crate) fn finish_resolving(
    tx: &Transaction<'_>,
    pid: &str,
    sid: &str,
    now: i64,
) -> Result<(), StateError> {
    tx.execute(
        "UPDATE conflicts SET status = 'resolved', resolved_at = ?3, updated_at = ?3
         WHERE profile_id = ?1 AND sid = ?2 AND col = 'bookmarks' AND status = 'resolving'",
        params![pid, sid, now],
    )?;
    Ok(())
}

/// v2 → v3: fold the old one-row-per-held-op records into one conflict per entity.
pub(crate) fn migrate_v2_rows(conn: &Connection) -> Result<(), rusqlite::Error> {
    type Old = (String, String, String, Option<String>, Option<i64>, i64);
    let mut st = conn.prepare(
        "SELECT profile_id, sid, kind, remote_line, remote_seq, created_at FROM conflicts
         WHERE status = 'open' ORDER BY id",
    )?;
    let old: Vec<Old> = st
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    drop(st);
    let mut ids: BTreeMap<(String, String), String> = BTreeMap::new();
    for (pid, sid, kind, line, _seq, created) in old {
        let key = (pid.clone(), sid.clone());
        let id = match ids.get(&key) {
            Some(id) => id.clone(),
            None => {
                let id = Uuid::new_v4().to_string();
                let subtype = match kind.as_str() {
                    "orphan" => "orphan",
                    "apply_failed" => "apply_error",
                    _ => "edit_edit",
                };
                conn.execute(
                    "INSERT INTO conflicts_v3 (conflict_id, account_id, profile_id, sid, kind, subtype,
                                               status, created_at, updated_at)
                     VALUES (?1, COALESCE((SELECT account_id FROM profiles WHERE profile_id = ?2), ''),
                             ?2, ?3, ?4, ?5, 'unresolved', ?6, ?6)",
                    params![id, pid, sid, kind, subtype, created],
                )?;
                ids.insert(key, id.clone());
                id
            }
        };
        if let Some(op) = line.as_deref().and_then(|l| Op::from_line(l).ok()) {
            conn.execute(
                "INSERT OR IGNORE INTO conflict_remote (conflict_id, hlc, remote_device, remote_seq, line)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, op.hlc, op.by, op.seq as i64, op.to_line()],
            )?;
        }
    }
    // snapshots of the folded rows are filled lazily (first refresh or view)
    Ok(())
}

// ------------------------------------------------------------------ resolution

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// Persisted; nothing left to do in the browser.
    Resolved { ops: usize },
    /// Persisted; the browser is brought to the decision by the next apply plan.
    Resolving { ops: usize },
    /// The same resolution was already recorded (idempotent repeat).
    AlreadyResolved,
    /// Closed differently (another resolution, or superseded): nothing changed.
    Closed { status: ConflictStatus },
}

#[derive(Debug, Error)]
pub enum ResolveError {
    /// No such conflict in this account/profile (ids of other profiles are never accepted).
    #[error("conflict not found")]
    NotFound,
    #[error("resolution not available for this conflict")]
    NotAllowed,
    #[error(transparent)]
    Clock(#[from] HlcError),
    #[error(transparent)]
    State(#[from] StateError),
}

impl From<rusqlite::Error> for ResolveError {
    fn from(e: rusqlite::Error) -> Self {
        ResolveError::State(StateError::Db(e))
    }
}

/// One entity changed by a resolution.
struct Edit {
    after: Entity,
    change: Change,
}

fn fields_of(e: &Entity, groups: &BTreeSet<Group>, full: bool) -> Fields {
    let mut f = Fields::new();
    let all = full;
    if all || groups.contains(&Group::Location) {
        f.insert("parentId".into(), e.parent.clone().into());
        f.insert("pos".into(), e.pos.clone().into());
        f.insert("root".into(), e.root.as_str().into());
    }
    if all || groups.contains(&Group::Title) {
        f.insert("title".into(), e.title.clone().into());
    }
    if e.kind == Kind::Url && (all || groups.contains(&Group::Url)) {
        f.insert(
            "url".into(),
            e.url
                .clone()
                .map(Into::into)
                .unwrap_or(serde_json::Value::Null),
        );
    }
    if all {
        f.insert("kind".into(), e.kind.as_str().into());
    }
    f
}

fn set(e: &Entity, groups: &BTreeSet<Group>, full: bool) -> Edit {
    Edit {
        after: e.clone(),
        change: Change::Set {
            sid: e.sid.clone(),
            fields: fields_of(e, groups, full),
        },
    }
}

fn del(e: &Entity) -> Edit {
    Edit {
        after: Entity {
            deleted: true,
            ..e.clone()
        },
        change: Change::Delete { sid: e.sid.clone() },
    }
}

fn alive(state: &BookmarkState, p: &str) -> bool {
    Root::from_sentinel(p).is_some() || state.entities.get(p).is_some_and(|e| !e.deleted)
}

fn root_of(state: &BookmarkState, parent: &str) -> Root {
    Root::from_sentinel(parent)
        .or_else(|| state.entities.get(parent).map(|e| e.root))
        .unwrap_or(Root::Other)
}

/// The nearest alive folder at or above `p` (deleted folders keep their parent pointer).
fn nearest_alive(state: &BookmarkState, p: &str) -> String {
    let mut cur = p.to_string();
    for _ in 0..128 {
        if alive(state, &cur) {
            return cur;
        }
        match state.entities.get(&cur) {
            Some(e) => cur = e.parent.clone(),
            None => break,
        }
    }
    Root::Other.sentinel()
}

fn children(state: &BookmarkState, p: &str, except: Option<&str>) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = state
        .alive()
        .filter(|e| e.parent == p && Some(e.sid.as_str()) != except)
        .map(|e| (e.pos.clone(), e.sid.clone()))
        .collect();
    v.sort();
    v
}

fn append_pos(state: &BookmarkState, p: &str, except: &str) -> String {
    let kids = children(state, p, Some(except));
    key_between(kids.last().map(|k| k.0.as_str()), None)
}

fn alive_descendants(state: &BookmarkState, sid: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![sid.to_string()];
    while let Some(s) = stack.pop() {
        for (_, c) in children(state, &s, None) {
            out.push(c.clone());
            stack.push(c);
        }
    }
    out
}

/// `root` of `sid`'s subtree changed: every live descendant follows.
fn reroot(state: &BookmarkState, sid: &str, root: Root, edits: &mut Vec<Edit>) {
    let loc: BTreeSet<Group> = BTreeSet::new();
    for d in alive_descendants(state, sid) {
        let mut e = state.entities[&d].clone();
        if e.root != root {
            e.root = root;
            let mut ed = set(&e, &loc, false);
            if let Change::Set { fields, .. } = &mut ed.change {
                fields.insert("root".into(), root.as_str().into());
            }
            edits.push(ed);
        }
    }
}

/// Deleted folders above `parent` are restored (top-down) so the kept item has a home; a
/// chain that runs into an unknown entity is re-homed under "Other bookmarks".
fn restore_chain(state: &BookmarkState, parent: &str, edits: &mut Vec<Edit>) {
    let mut chain: Vec<Entity> = Vec::new();
    let mut cur = parent.to_string();
    for _ in 0..128 {
        if alive(state, &cur) {
            break;
        }
        match state.entities.get(&cur) {
            Some(e) => {
                chain.push(e.clone());
                cur = e.parent.clone();
            }
            None => {
                if let Some(top) = chain.last_mut() {
                    top.parent = Root::Other.sentinel();
                    top.root = Root::Other;
                    top.pos = append_pos(state, &top.parent, &top.sid);
                }
                break;
            }
        }
    }
    for mut e in chain.into_iter().rev() {
        e.deleted = false;
        edits.push(set(&e, &BTreeSet::new(), true));
    }
}

/// Descendants deleted on this device in the same local removal as `e` (same capture batch:
/// same HLC wall time and device), to restore a folder with its content.
fn same_removal(
    state: &BookmarkState,
    deleted_hlc: &BTreeMap<String, Hlc>,
    e: &Entity,
) -> Vec<Entity> {
    let Some(mine) = deleted_hlc.get(&e.sid) else {
        return vec![];
    };
    let mut out = Vec::new();
    let mut stack = vec![e.sid.clone()];
    while let Some(p) = stack.pop() {
        let mut kids: Vec<&Entity> = state
            .entities
            .values()
            .filter(|c| c.parent == p && c.deleted)
            .collect();
        kids.sort_by(|a, b| (&a.pos, &a.sid).cmp(&(&b.pos, &b.sid)));
        for k in kids {
            if deleted_hlc
                .get(&k.sid)
                .is_some_and(|h| h.wall_ms == mine.wall_ms && h.device_id == mine.device_id)
            {
                out.push(k.clone());
                stack.push(k.sid.clone());
            }
        }
    }
    out
}

impl StateDb {
    /// Resolve one conflict of `profile_id` (which must belong to `account_id`). Validates
    /// the choice against the conflict's options; repeats are idempotent.
    pub fn resolve_conflict(
        &mut self,
        account_id: &str,
        profile_id: Uuid,
        conflict_id: &str,
        res: Resolution,
        now_ms: u64,
    ) -> Result<ResolveOutcome, ResolveError> {
        let pid = profile_id.to_string();
        let now = (now_ms / 1000) as i64;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prof = load_profile(&tx, &pid)?.ok_or(ResolveError::NotFound)?;
        let c = by_id(&tx, conflict_id)?
            .filter(|c| c.pid == pid && c.account_id == account_id && prof.account_id == account_id)
            .ok_or(ResolveError::NotFound)?;
        match c.status {
            ConflictStatus::Unresolved => {}
            ConflictStatus::Resolving | ConflictStatus::Resolved if c.resolution == Some(res) => {
                return Ok(ResolveOutcome::AlreadyResolved)
            }
            s => return Ok(ResolveOutcome::Closed { status: s }),
        }
        // the decision is made against the current versions
        let published = prof.published_through as i64;
        if c.col == synchrome_sync::extensions::COLLECTION {
            return crate::ext::resolve(tx, &c, res, &prof.device_id, published, now_ms);
        }
        if c.col == synchrome_sync::extension_settings::COLLECTION {
            return crate::ext_settings::resolve(tx, &c, res, &prof.device_id, published, now_ms);
        }
        if c.col == synchrome_sync::browser_prefs::COLLECTION {
            return crate::browser_prefs::resolve(tx, &c, res, &prof.device_id, published, now_ms);
        }
        let c = if c.kind != ConflictKind::Orphan {
            if !recompute(&tx, &c, published, now)? {
                tx.commit()?;
                return Ok(ResolveOutcome::Closed {
                    status: ConflictStatus::Superseded,
                });
            }
            by_id(&tx, conflict_id)?.ok_or(ResolveError::NotFound)?
        } else {
            c
        };
        if !options(&c).contains(&res) {
            return Err(ResolveError::NotAllowed);
        }
        if res == Resolution::Retry {
            return retry(tx, &c, now);
        }
        let mut state = load_state(&tx, &pid)?;
        let Some(cur) = state.entities.get(&c.sid).cloned() else {
            return Err(ResolveError::NotFound);
        };
        let tomb: BTreeMap<String, Hlc> = {
            let mut st = tx.prepare(
                "SELECT sid, deleted_hlc FROM entities WHERE profile_id = ?1 AND deleted = 1
                 AND deleted_hlc IS NOT NULL",
            )?;
            let rows = st.query_map([&pid], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            let mut m = BTreeMap::new();
            for r in rows {
                let (s, h) = r?;
                if let Ok(h) = h.parse() {
                    m.insert(s, h);
                }
            }
            m
        };
        let mut edits: Vec<Edit> = Vec::new();
        let mut keep_alive_conflicts: Vec<Row> = Vec::new();
        let (l, r) = (&c.local, &c.remote);
        let union: BTreeSet<Group> = l
            .changed
            .union(&r.changed)
            .copied()
            .filter(|g| *g != Group::Existence)
            .collect();
        // the chosen version of an entity both sides keep, with a valid home
        let place = |mut e: Entity, edits: &mut Vec<Edit>, state: &BookmarkState| -> Entity {
            if !alive(state, &e.parent) {
                if state.entities.contains_key(&e.parent) {
                    restore_chain(state, &e.parent, edits);
                } else {
                    e.parent = nearest_alive(state, &e.parent);
                    e.pos = append_pos(state, &e.parent, &e.sid);
                }
            }
            e
        };
        match (c.kind, res) {
            (ConflictKind::LocalPending, Resolution::KeepLocal) => {
                if l.exists {
                    edits.push(set(&cur, &union, !r.exists));
                } else {
                    edits.push(del(&cur));
                }
            }
            (ConflictKind::LocalPending, Resolution::KeepRemote | Resolution::Merge) => {
                if !r.exists {
                    for d in alive_descendants(&state, &cur.sid).into_iter().rev() {
                        edits.push(del(&state.entities[&d]));
                    }
                    edits.push(del(&cur));
                } else {
                    let mut e = cur.clone();
                    e.deleted = false;
                    let base = c.base.as_ref();
                    let take = |g: Group, e: &mut Entity, v: &Version| match g {
                        Group::Title => e.title = v.title.clone().unwrap_or_default(),
                        Group::Url => {
                            if e.kind == Kind::Url {
                                e.url = v.url.clone();
                            }
                        }
                        Group::Location => {
                            if let Some(p) = &v.parent {
                                e.parent = p.clone();
                            }
                            if let Some(pos) = &v.pos {
                                e.pos = pos.clone();
                            }
                        }
                        Group::Existence | Group::Enabled | Group::Value => {}
                    };
                    for g in &r.changed {
                        take(*g, &mut e, r);
                    }
                    if res == Resolution::KeepRemote {
                        // local-only changes go back to the BASE when it is known
                        if let Some(b) = base {
                            for g in l.changed.difference(&r.changed) {
                                if b.changed.contains(g) {
                                    take(*g, &mut e, b);
                                }
                            }
                        }
                    }
                    let restoring = !l.exists;
                    let mut e = place(e, &mut edits, &state);
                    e.root = root_of(&state, &e.parent);
                    if e.root != cur.root {
                        reroot(&state, &cur.sid, e.root, &mut edits);
                    }
                    edits.push(set(&e, &union, restoring));
                    if restoring {
                        for d in same_removal(&state, &tomb, &cur) {
                            let mut d = d.clone();
                            d.deleted = false;
                            d.root = e.root;
                            edits.push(set(&d, &BTreeSet::new(), true));
                        }
                    }
                }
            }
            (ConflictKind::ApplyFailed, Resolution::KeepLocal) => {
                edits.push(set(&cur, &BTreeSet::new(), true));
            }
            (ConflictKind::ApplyFailed, Resolution::Relocate) => {
                let target = nearest_alive(&state, &cur.parent);
                let kids = children(&state, &cur.sid, None);
                let (lo, hi) = if target == cur.parent {
                    let sib = children(&state, &target, Some(&cur.sid));
                    let hi = sib.iter().find(|s| s.0 > cur.pos).map(|s| s.0.clone());
                    (Some(cur.pos.clone()), hi)
                } else {
                    let sib = children(&state, &target, None);
                    (sib.last().map(|s| s.0.clone()), None)
                };
                let keys = keys_between(lo.as_deref(), hi.as_deref(), kids.len());
                let root = root_of(&state, &target);
                let loc: BTreeSet<Group> = [Group::Location].into_iter().collect();
                for ((_, k), key) in kids.iter().zip(keys) {
                    let mut e = state.entities[k].clone();
                    e.parent = target.clone();
                    e.pos = key;
                    if e.root != root {
                        e.root = root;
                        reroot(&state, k, root, &mut edits);
                    }
                    edits.push(set(&e, &loc, false));
                }
                edits.push(del(&cur));
            }
            (ConflictKind::ApplyFailed, Resolution::KeepRemote)
            | (ConflictKind::Orphan, Resolution::KeepLocal) => {
                for d in alive_descendants(&state, &cur.sid).into_iter().rev() {
                    edits.push(del(&state.entities[&d]));
                }
                edits.push(del(&cur));
            }
            (ConflictKind::Orphan, Resolution::Relocate) => {
                let mut e = cur.clone();
                e.parent = nearest_alive(&state, &cur.parent);
                e.pos = append_pos(&state, &e.parent, &e.sid);
                e.root = root_of(&state, &e.parent);
                if e.root != cur.root {
                    reroot(&state, &cur.sid, e.root, &mut edits);
                }
                edits.push(set(&e, &[Group::Location].into_iter().collect(), false));
            }
            (ConflictKind::Orphan, Resolution::KeepRemote) => {
                restore_chain(&state, &cur.parent, &mut edits);
            }
            _ => return Err(ResolveError::NotAllowed),
        }
        // A kept item needs its ancestors to exist on the other devices too: ancestors whose
        // own open conflict is a remote delete are kept with it (that conflict is resolved).
        let keeps_main = !edits
            .iter()
            .any(|e| e.after.sid == c.sid && e.after.deleted);
        if keeps_main {
            let mut p = edits
                .iter()
                .find(|e| e.after.sid == c.sid)
                .map(|e| e.after.parent.clone())
                .unwrap_or_else(|| cur.parent.clone());
            for _ in 0..128 {
                if Root::from_sentinel(&p).is_some() {
                    break;
                }
                let Some(a) = state.entities.get(&p).cloned() else {
                    break;
                };
                if let Some(ac) = open_for(&tx, &pid, &p)? {
                    if ac.id != c.id
                        && ac.status == ConflictStatus::Unresolved
                        && !ac.remote.exists
                        && !a.deleted
                    {
                        edits.push(set(&a, &BTreeSet::new(), true));
                        keep_alive_conflicts.push(ac);
                    }
                }
                p = a.parent.clone();
            }
        }

        // --- execute: ordinary local ops (fresh HLC, later than every version involved)
        let newest = c
            .local
            .hlc
            .values()
            .chain(c.remote.hlc.values())
            .max()
            .cloned();
        if let Some(h) = newest {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES ('hlc_last', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = MAX(value, excluded.value)",
                [h],
            )?;
        }
        let indexed: HashSet<String> = state.index.values().cloned().collect();
        let before: BTreeMap<String, Entity> = edits
            .iter()
            .filter_map(|e| {
                state
                    .entities
                    .get(&e.after.sid)
                    .map(|b| (b.sid.clone(), b.clone()))
            })
            .collect();
        let mut changes = Vec::new();
        for ed in &edits {
            state
                .entities
                .insert(ed.after.sid.clone(), ed.after.clone());
            state.dirty_entities.insert(ed.after.sid.clone());
            changes.push(ed.change.clone());
        }
        persist(&tx, &pid, &prof.device_id, &state, &changes, now_ms).map_err(|e| match e {
            crate::CaptureError::Clock(c) => ResolveError::Clock(c),
            crate::CaptureError::State(s) => ResolveError::State(s),
            other => ResolveError::State(StateError::Corrupt(other.to_string())),
        })?;
        // --- browser work: what differs from what the browser shows now
        let mut main_pending = false;
        let sids: BTreeSet<&str> = edits
            .iter()
            .map(|e| e.after.sid.as_str())
            .chain(std::iter::once(c.sid.as_str()))
            .collect();
        for sid in sids {
            let after = &state.entities[sid];
            let in_browser = indexed.contains(sid);
            let mut f: BTreeSet<String> = BTreeSet::new();
            if after.deleted {
                if in_browser {
                    f.insert("deleted".into());
                }
            } else if !in_browser {
                f.insert("created".into());
            } else if let Some(b) = before.get(sid) {
                if b.title != after.title {
                    f.insert("title".into());
                }
                if b.url != after.url {
                    f.insert("url".into());
                }
                if b.parent != after.parent {
                    f.insert("parentId".into());
                }
                if b.pos != after.pos {
                    f.insert("pos".into());
                }
            }
            let ent = load_ent(&tx, &pid, sid)?;
            if let Some(ent) = ent.as_ref().filter(|_| !after.deleted || in_browser) {
                f.extend(ent.pending_fields.iter().cloned());
            }
            let pending = !f.is_empty();
            if sid == c.sid {
                main_pending = pending;
            }
            tx.execute(
                "UPDATE entities SET pending_apply = ?3, pending_fields = ?4, conflict = 0
                 WHERE profile_id = ?1 AND sid = ?2",
                params![
                    pid,
                    sid,
                    pending as i64,
                    serde_json::to_string(&f).unwrap_or_default()
                ],
            )?;
        }
        let main_hlc: Option<String> = tx
            .query_row(
                "SELECT hlc FROM entities WHERE profile_id = ?1 AND sid = ?2",
                params![pid, c.sid],
                |r| r.get(0),
            )
            .optional()?;
        let chosen = local_version(&tx, &pid, &c.sid, i64::MAX)?;
        let status = if main_pending {
            ConflictStatus::Resolving
        } else {
            ConflictStatus::Resolved
        };
        tx.execute(
            "UPDATE conflicts SET status = ?2, resolution = ?3, resolution_json = ?4,
                    resolution_hlc = ?5, resolution_ops = ?6, updated_at = ?7,
                    resolved_at = CASE WHEN ?2 = 'resolved' THEN ?7 ELSE NULL END
             WHERE conflict_id = ?1",
            params![
                c.id,
                status.as_str(),
                res.as_str(),
                serde_json::to_string(&chosen).unwrap_or_default(),
                main_hlc,
                changes.len() as i64,
                now
            ],
        )?;
        for ac in keep_alive_conflicts {
            tx.execute(
                "UPDATE conflicts SET status = 'resolved', resolution = 'keep_local',
                        detail = 'kept because it contains a kept item', resolution_ops = 1,
                        updated_at = ?2, resolved_at = ?2
                 WHERE conflict_id = ?1",
                params![ac.id, now],
            )?;
        }
        tx.commit()?;
        Ok(if main_pending {
            ResolveOutcome::Resolving { ops: changes.len() }
        } else {
            ResolveOutcome::Resolved { ops: changes.len() }
        })
    }

    /// Resolve several conflicts (each in its own transaction; one failure does not undo the
    /// others).
    pub fn resolve_conflicts(
        &mut self,
        account_id: &str,
        profile_id: Uuid,
        items: &[(String, Resolution)],
        now_ms: u64,
    ) -> Vec<Result<ResolveOutcome, ResolveError>> {
        items
            .iter()
            .map(|(id, r)| self.resolve_conflict(account_id, profile_id, id, *r, now_ms))
            .collect()
    }
}

/// Retry the browser operation: the held remote ops are merged (LWW) again and the entity is
/// planned again. If it fails again the conflict comes back (attempts + 1).
fn retry(tx: Transaction<'_>, c: &Row, now: i64) -> Result<ResolveOutcome, ResolveError> {
    for op in held_ops(&tx, &c.id)? {
        crate::pull::apply_lww(&tx, &c.pid, &op)?;
    }
    tx.execute(
        "UPDATE entities SET conflict = 0 WHERE profile_id = ?1 AND sid = ?2",
        params![c.pid, c.sid],
    )?;
    let pending: i64 = tx.query_row(
        "SELECT pending_apply FROM entities WHERE profile_id = ?1 AND sid = ?2",
        params![c.pid, c.sid],
        |r| r.get(0),
    )?;
    let status = if pending != 0 {
        ConflictStatus::Resolving
    } else {
        ConflictStatus::Resolved
    };
    tx.execute(
        "UPDATE conflicts SET status = ?2, resolution = 'retry', updated_at = ?3,
                resolved_at = CASE WHEN ?2 = 'resolved' THEN ?3 ELSE NULL END
         WHERE conflict_id = ?1",
        params![c.id, status.as_str(), now],
    )?;
    tx.commit()?;
    Ok(if pending != 0 {
        ResolveOutcome::Resolving { ops: 0 }
    } else {
        ResolveOutcome::Resolved { ops: 0 }
    })
}

// ------------------------------------------------------------------ views (UI)

/// One side, readable: titles of the folder path, 1-based position among siblings.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SideView {
    pub exists: bool,
    /// Extensions only.
    pub enabled: Option<bool>,
    /// Extension settings only (canonical JSON).
    pub value: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub folder: Option<Vec<String>>,
    pub position: Option<u32>,
    pub siblings: Option<u32>,
    pub changed: Vec<Group>,
    /// Wall time (ms) of this side's newest change, when known.
    pub changed_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OptionView {
    pub resolution: Resolution,
    pub destructive: bool,
}

/// What the app shows for one conflict. No tokens; no internal ids except `conflict_id`
/// (the handle for actions) and the remote device id (mapped to a device name by the UI).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConflictView {
    pub conflict_id: String,
    pub kind: ConflictKind,
    pub subtype: ConflictSubtype,
    pub status: ConflictStatus,
    pub created_at: i64,
    pub updated_at: i64,
    pub item_kind: String,
    pub name: String,
    pub local: SideView,
    pub remote: SideView,
    pub base: Option<SideView>,
    pub remote_device: Option<String>,
    pub remote_seq: Option<u64>,
    pub options: Vec<OptionView>,
    pub resolution: Option<Resolution>,
    pub resolution_ops: i64,
    pub resolved_at: Option<i64>,
    /// Remote ops held by this conflict.
    pub held: usize,
    pub detail: Option<String>,
    pub attempts: i64,
    /// delete_not_empty: items in the folder that would be lost by deleting it.
    pub local_items: usize,
    /// The resolution's ops are not uploaded yet.
    pub upload_pending: bool,
}

fn folder_path(state: &BookmarkState, parent: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = parent.to_string();
    for _ in 0..64 {
        if let Some(r) = Root::from_sentinel(&cur) {
            out.push(
                match r {
                    Root::Bar => "Bookmarks bar",
                    Root::Other => "Other bookmarks",
                    Root::Mobile => "Mobile bookmarks",
                }
                .to_string(),
            );
            break;
        }
        match state.entities.get(&cur) {
            Some(e) => {
                out.push(if e.deleted {
                    format!("{} (deleted here)", e.title)
                } else {
                    e.title.clone()
                });
                cur = e.parent.clone();
            }
            None => {
                out.push("(unknown folder)".into());
                break;
            }
        }
    }
    out.reverse();
    out
}

fn side(state: &BookmarkState, sid: &str, v: &Version, full: bool) -> SideView {
    let show = |g: Group| full || v.changed.contains(&g);
    let mut s = SideView {
        exists: v.exists,
        changed: v.changed.iter().copied().collect(),
        changed_at: v
            .hlc
            .values()
            .max()
            .and_then(|h| h.parse::<Hlc>().ok())
            .map(|h| h.wall_ms),
        ..Default::default()
    };
    if show(Group::Title) {
        s.title = v.title.clone();
    }
    if show(Group::Enabled) || full {
        s.enabled = v.enabled;
    }
    if show(Group::Value) || full {
        s.value = v.value.clone();
    }
    if show(Group::Url) {
        s.url = v.url.clone();
    }
    if show(Group::Location) {
        if let Some(p) = &v.parent {
            s.folder = Some(folder_path(state, p));
            let sib = children(state, p, Some(sid));
            if let Some(pos) = &v.pos {
                s.position = Some(sib.iter().filter(|x| &x.0 < pos).count() as u32 + 1);
                s.siblings = Some(sib.len() as u32 + 1);
            }
        }
    }
    s
}

impl StateDb {
    /// Conflicts of a profile for the UI: open ones, plus closed history when asked.
    pub fn conflict_views(
        &mut self,
        profile_id: Uuid,
        include_closed: bool,
    ) -> Result<Vec<ConflictView>, StateError> {
        let pid = profile_id.to_string();
        let tx = self.conn.transaction()?;
        let published = published_of(&tx, &pid)?;
        let mut st = tx.prepare(&format!(
            "SELECT {ROW_COLS} FROM conflicts WHERE profile_id = ?1 {} ORDER BY created_at DESC, rowid DESC",
            if include_closed {
                ""
            } else {
                "AND status IN ('unresolved', 'resolving')"
            }
        ))?;
        let rows: Vec<Row> = st.query_map([&pid], row_from)?.collect::<Result<_, _>>()?;
        drop(st);
        // lazily fill snapshots of migrated rows
        for c in rows.iter().filter(|c| {
            c.status == ConflictStatus::Unresolved
                && c.local == Version::default()
                && c.kind != ConflictKind::Orphan
                && c.col == "bookmarks"
        }) {
            recompute(&tx, c, published, c.updated_at)?;
        }
        let rows: Vec<Row> = {
            let mut st = tx.prepare(&format!(
                "SELECT {ROW_COLS} FROM conflicts WHERE profile_id = ?1 {} ORDER BY created_at DESC, rowid DESC",
                if include_closed {
                    ""
                } else {
                    "AND status IN ('unresolved', 'resolving')"
                }
            ))?;
            let v = st.query_map([&pid], row_from)?.collect::<Result<_, _>>()?;
            v
        };
        let state = load_state(&tx, &pid)?;
        let mut out = Vec::new();
        for c in rows {
            let held = held_ops(&tx, &c.id)?.len();
            let e = state.entities.get(&c.sid);
            let item_kind = if c.col == synchrome_sync::extensions::COLLECTION {
                "extension"
            } else if c.col == synchrome_sync::extension_settings::COLLECTION {
                "setting"
            } else if c.col == synchrome_sync::browser_prefs::COLLECTION {
                "preference"
            } else {
                e.map(|e| e.kind.as_str()).unwrap_or("url")
            }
            .to_string();
            let name = c
                .local
                .title
                .clone()
                .or_else(|| c.remote.title.clone())
                .or_else(|| e.map(|e| e.title.clone()))
                .unwrap_or_default();
            let local_items = if c.subtype == ConflictSubtype::DeleteNotEmpty {
                alive_descendants(&state, &c.sid).len()
            } else {
                0
            };
            let resolution_seq: Option<i64> = tx
                .query_row(
                    "SELECT local_seq FROM entities WHERE profile_id = ?1 AND sid = ?2",
                    params![pid, c.sid],
                    |r| r.get(0),
                )
                .optional()?;
            let open = matches!(c.status, ConflictStatus::Unresolved);
            out.push(ConflictView {
                conflict_id: c.id.clone(),
                kind: c.kind,
                subtype: c.subtype,
                status: c.status,
                created_at: c.created_at,
                updated_at: c.updated_at,
                item_kind,
                name,
                local: side(&state, &c.sid, &c.local, true),
                remote: side(&state, &c.sid, &c.remote, false),
                base: c.base.as_ref().map(|b| side(&state, &c.sid, b, false)),
                remote_device: c.remote.device.clone(),
                remote_seq: c.remote.seq,
                options: if open {
                    options(&c)
                        .into_iter()
                        .map(|r| OptionView {
                            resolution: r,
                            destructive: destructive(&c, r),
                        })
                        .collect()
                } else {
                    vec![]
                },
                resolution: c.resolution,
                resolution_ops: c.resolution_ops,
                resolved_at: c.resolved_at,
                held,
                detail: c.detail.clone(),
                attempts: c.attempts,
                local_items,
                upload_pending: c.resolution_ops > 0
                    && resolution_seq.is_some_and(|s| s > published),
            });
        }
        tx.commit()?;
        Ok(out)
    }

    /// (unresolved, resolving) conflicts of a profile.
    pub fn conflict_counts(&self, profile_id: Uuid) -> Result<(u64, u64), StateError> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(SUM(status = 'unresolved'), 0), COALESCE(SUM(status = 'resolving'), 0)
             FROM conflicts WHERE profile_id = ?1",
            [profile_id.to_string()],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)),
        )?)
    }

    /// Profile owning a conflict (to validate a UI request against the current mapping).
    pub fn conflict_profile(&self, conflict_id: &str) -> Result<Option<Uuid>, StateError> {
        let p: Option<String> = self
            .conn
            .query_row(
                "SELECT profile_id FROM conflicts WHERE conflict_id = ?1",
                [conflict_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(p.and_then(|p| Uuid::parse_str(&p).ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_rows_fold_into_one_record_per_entity_with_its_held_ops() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::schema::V1).unwrap();
        conn.execute_batch(crate::schema::V2).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        let pid = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO profiles (profile_id, account_id, device_id) VALUES (?1, 'acct', 'dev-b')",
            [&pid],
        )
        .unwrap();
        for (sid, title) in [("x", "Local X"), ("o", "Orphan")] {
            conn.execute(
                "INSERT INTO entities (profile_id, sid, kind, root, parent, pos, title, hlc, conflict)
                 VALUES (?1, ?2, 'url', 'bar', 'root:bar', 'V', ?3, '', 1)",
                params![pid, sid, title],
            )
            .unwrap();
        }
        let op = |seq: u64, t: &str| Op {
            seq,
            hlc: format!("17909000{seq:05}-0000-dev-a"),
            col: "bookmarks".into(),
            id: "x".into(),
            op: OpKind::Set,
            fields: Some(
                [("title".to_string(), serde_json::Value::from(t))]
                    .into_iter()
                    .collect(),
            ),
            by: "dev-a".into(),
        };
        for (seq, t) in [(3, "A1"), (4, "A2")] {
            conn.execute(
                "INSERT INTO conflicts (profile_id, sid, kind, remote_device, remote_seq, remote_line, created_at)
                 VALUES (?1, 'x', 'local_pending', 'dev-a', ?2, ?3, 100)",
                params![pid, seq, op(seq, t).to_line()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO conflicts (profile_id, sid, kind, detail, created_at) VALUES (?1, 'o', 'orphan', 'x', 101)",
            [&pid],
        )
        .unwrap();
        crate::schema::migrate(&conn).unwrap();
        let mut db = StateDb { conn };
        let v = db
            .conflict_views(Uuid::parse_str(&pid).unwrap(), true)
            .unwrap();
        assert_eq!(v.len(), 2);
        let x = v
            .iter()
            .find(|c| c.kind == ConflictKind::LocalPending)
            .unwrap();
        assert_eq!((x.held, x.status), (2, ConflictStatus::Unresolved));
        assert_eq!(
            x.remote.title.as_deref(),
            Some("A2"),
            "held ops folded in order"
        );
        assert_eq!(x.local.title.as_deref(), Some("Local X"));
        assert_eq!(x.remote_seq, Some(4));
        assert!(v.iter().any(|c| c.kind == ConflictKind::Orphan));
        let n: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 6);
    }

    #[test]
    fn fold_takes_the_newest_value_and_a_newer_set_resurrects() {
        let mk = |hlc: &str, kind: OpKind, title: Option<&str>| Op {
            seq: 1,
            hlc: hlc.into(),
            col: "bookmarks".into(),
            id: "x".into(),
            op: kind,
            fields: title.map(|t| {
                [("title".to_string(), serde_json::Value::from(t))]
                    .into_iter()
                    .collect()
            }),
            by: "a".into(),
        };
        let v = fold_remote(&[
            mk("1790000000001-0000-a", OpKind::Set, Some("one")),
            mk("1790000000002-0000-a", OpKind::Delete, None),
        ]);
        assert!(!v.exists && v.changed.contains(&Group::Existence));
        let v = fold_remote(&[
            mk("1790000000001-0000-a", OpKind::Set, Some("one")),
            mk("1790000000002-0000-a", OpKind::Delete, None),
            mk("1790000000003-0000-a", OpKind::Set, Some("three")),
        ]);
        assert!(v.exists);
        assert_eq!(v.title.as_deref(), Some("three"));
    }
}
