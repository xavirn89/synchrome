//! Bookmarks normaliser (`docs/03` §3, §3.1).
//!
//! Turns what the extension observes through `chrome.bookmarks` (a full tree or incremental
//! events, with *local* Chromium ids) into changes on the Synchrome model (UUID identity,
//! root sentinels, fractional `pos`). Pure: no I/O, no clock. The caller loads a
//! [`BookmarkState`], calls [`reconcile`] / [`apply_event`], stamps the returned
//! [`Change`]s with `seq`/HLC and persists the dirty part of the state in one transaction.

use crate::fractional::{key_between, keys_between};
use crate::op::Fields;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use thiserror::Error;

pub const COLLECTION: &str = "bookmarks";
pub const ROOT_PREFIX: &str = "root:";
/// Bookmarks whose URL is longer than this are not captured (e.g. huge `data:` URLs).
pub const MAX_URL_BYTES: usize = 64 * 1024;
/// Titles are truncated to this many characters (defensive; the extension truncates too).
pub const MAX_TITLE_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Root {
    Bar,
    Other,
    Mobile,
}

impl Root {
    pub fn as_str(self) -> &'static str {
        match self {
            Root::Bar => "bar",
            Root::Other => "other",
            Root::Mobile => "mobile",
        }
    }
    pub fn sentinel(self) -> String {
        format!("{ROOT_PREFIX}{}", self.as_str())
    }
    pub fn from_sentinel(s: &str) -> Option<Root> {
        match s.strip_prefix(ROOT_PREFIX)? {
            "bar" => Some(Root::Bar),
            "other" => Some(Root::Other),
            "mobile" => Some(Root::Mobile),
            _ => None,
        }
    }
    pub fn parse(s: &str) -> Option<Root> {
        Root::from_sentinel(&format!("{ROOT_PREFIX}{s}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Url,
    Folder,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Url => "url",
            Kind::Folder => "folder",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        match s {
            "url" => Some(Kind::Url),
            "folder" => Some(Kind::Folder),
            _ => None,
        }
    }
}

/// A `chrome.bookmarks.BookmarkTreeNode` as the extension sends it (flattened, no children).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChromiumNode {
    pub id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub date_added: Option<f64>,
    #[serde(default)]
    pub folder_type: Option<String>,
    #[serde(default)]
    pub syncing: Option<bool>,
    #[serde(default)]
    pub unmodifiable: Option<String>,
}

impl ChromiumNode {
    fn excluded(&self) -> bool {
        self.syncing == Some(true)
            || self.unmodifiable.as_deref() == Some("managed")
            || self.folder_type.as_deref() == Some("managed")
            || self.url.as_ref().is_some_and(|u| u.len() > MAX_URL_BYTES)
    }
    fn kind(&self) -> Kind {
        if self.url.is_some() {
            Kind::Url
        } else {
            Kind::Folder
        }
    }
    /// The Synchrome root this node *is*, if it is a top-level Chromium root folder.
    fn as_root(&self) -> Option<Root> {
        if self.parent_id.as_deref() != Some("0") || self.excluded() {
            return None;
        }
        match self.folder_type.as_deref() {
            Some("bookmarks-bar") => Some(Root::Bar),
            Some("other") => Some(Root::Other),
            Some("mobile") => Some(Root::Mobile),
            Some(_) => None,
            None => match self.id.as_str() {
                "1" => Some(Root::Bar),
                "2" => Some(Root::Other),
                "3" => Some(Root::Mobile),
                _ => None,
            },
        }
    }
}

fn clean_title(t: &str) -> String {
    if t.chars().count() > MAX_TITLE_CHARS {
        t.chars().take(MAX_TITLE_CHARS).collect()
    } else {
        t.to_string()
    }
}

/// One incremental `chrome.bookmarks` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BookmarkEvent {
    Created {
        id: String,
        node: ChromiumNode,
    },
    Changed {
        id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        url: Option<String>,
    },
    Moved {
        id: String,
        parent_id: String,
        index: u32,
    },
    Removed {
        id: String,
    },
    Reordered {
        id: String,
        child_ids: Vec<String>,
    },
}

/// The materialised local view of one profile's bookmarks.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub sid: String,
    pub kind: Kind,
    pub root: Root,
    /// Parent entity id, or a root sentinel (`root:bar`, …).
    pub parent: String,
    pub pos: String,
    pub title: String,
    pub url: Option<String>,
    pub created_at: Option<i64>,
    pub deleted: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BookmarkState {
    pub entities: HashMap<String, Entity>,
    /// Chromium id → entity id or root sentinel (local only, never published).
    pub index: HashMap<String, String>,
    /// Entities touched since load (to persist).
    pub dirty_entities: BTreeSet<String>,
    /// Chromium ids whose index entry was added, changed or removed since load.
    pub dirty_index: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Set { sid: String, fields: Fields },
    Delete { sid: String },
}

impl Change {
    pub fn sid(&self) -> &str {
        match self {
            Change::Set { sid, .. } | Change::Delete { sid } => sid,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NormalizeError {
    /// The event references a node the local view does not know: the extension must send a
    /// full snapshot so the view can be reconciled.
    #[error("resync required: {0}")]
    ResyncRequired(String),
}

impl BookmarkState {
    pub fn alive(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values().filter(|e| !e.deleted)
    }

    pub fn counts(&self) -> (usize, usize) {
        let mut urls = 0;
        let mut folders = 0;
        for e in self.alive() {
            match e.kind {
                Kind::Url => urls += 1,
                Kind::Folder => folders += 1,
            }
        }
        (urls, folders)
    }

    fn set_index(&mut self, cid: &str, sid: &str) {
        if self.index.get(cid).map(String::as_str) != Some(sid) {
            self.index.insert(cid.to_string(), sid.to_string());
            self.dirty_index.insert(cid.to_string());
        }
    }

    fn unset_index(&mut self, cid: &str) {
        if self.index.remove(cid).is_some() {
            self.dirty_index.insert(cid.to_string());
        }
    }

    /// Resolve a Chromium id to a live entity id or root sentinel.
    fn resolve(&self, cid: &str) -> Option<String> {
        let target = self.index.get(cid)?;
        if Root::from_sentinel(target).is_some() {
            return Some(target.clone());
        }
        match self.entities.get(target) {
            Some(e) if !e.deleted => Some(target.clone()),
            _ => None,
        }
    }

    fn root_of_parent(&self, parent: &str) -> Option<Root> {
        Root::from_sentinel(parent).or_else(|| self.entities.get(parent).map(|e| e.root))
    }

    /// Live children of `parent` sorted by `(pos, sid)`, optionally excluding one entity.
    fn siblings(&self, parent: &str, except: Option<&str>) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .alive()
            .filter(|e| e.parent == parent && Some(e.sid.as_str()) != except)
            .map(|e| (e.pos.clone(), e.sid.clone()))
            .collect();
        v.sort();
        v
    }

    fn descendants(&self, sid: &str) -> Vec<String> {
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in self.alive() {
            children.entry(e.parent.as_str()).or_default().push(&e.sid);
        }
        let mut out = Vec::new();
        let mut stack = vec![sid];
        while let Some(s) = stack.pop() {
            if let Some(cs) = children.get(s) {
                for c in cs {
                    out.push(c.to_string());
                    stack.push(c);
                }
            }
        }
        out
    }

    /// A `pos` for `sid` at `index` among the live children of `parent`. Keeps the current
    /// key when the node already sits between its would-be neighbours (replays are no-ops).
    fn pos_at(&self, sid: &str, parent: &str, index: usize) -> String {
        let sib = self.siblings(parent, Some(sid));
        let i = index.min(sib.len());
        let lo = if i > 0 {
            Some(sib[i - 1].0.as_str())
        } else {
            None
        };
        let hi = sib.get(i).map(|s| s.0.as_str());
        if let Some(e) = self.entities.get(sid) {
            if !e.deleted
                && e.parent == parent
                && lo.map_or(true, |l| l < e.pos.as_str())
                && hi.map_or(true, |h| e.pos.as_str() < h)
            {
                return e.pos.clone();
            }
        }
        match (lo, hi) {
            (Some(l), Some(h)) if l >= h => key_between(Some(l), None), // corrupt order: append
            _ => key_between(lo, hi),
        }
    }

    /// Write `want` into the entity (creating it when absent) and return the changed fields.
    fn upsert(&mut self, want: Entity, creating: bool) -> Option<Change> {
        let mut f = Fields::new();
        let sid = want.sid.clone();
        match self.entities.get(&sid).filter(|e| !e.deleted && !creating) {
            Some(cur) => {
                if cur.parent != want.parent {
                    f.insert("parentId".into(), want.parent.clone().into());
                }
                if cur.pos != want.pos {
                    f.insert("pos".into(), want.pos.clone().into());
                }
                if cur.title != want.title {
                    f.insert("title".into(), want.title.clone().into());
                }
                if cur.kind != want.kind {
                    f.insert("kind".into(), want.kind.as_str().into());
                }
                if cur.root != want.root {
                    f.insert("root".into(), want.root.as_str().into());
                }
                if cur.url != want.url {
                    f.insert(
                        "url".into(),
                        want.url
                            .clone()
                            .map(Into::into)
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
                let created_at = cur.created_at;
                if f.is_empty() {
                    return None;
                }
                self.entities
                    .insert(sid.clone(), Entity { created_at, ..want });
            }
            None => {
                f.insert("parentId".into(), want.parent.clone().into());
                f.insert("pos".into(), want.pos.clone().into());
                f.insert("title".into(), want.title.clone().into());
                f.insert("kind".into(), want.kind.as_str().into());
                f.insert("root".into(), want.root.as_str().into());
                if let Some(u) = &want.url {
                    f.insert("url".into(), u.clone().into());
                }
                if let Some(c) = want.created_at {
                    f.insert("createdAt".into(), c.into());
                }
                self.entities.insert(sid.clone(), want);
            }
        }
        self.dirty_entities.insert(sid.clone());
        Some(Change::Set { sid, fields: f })
    }

    fn delete(&mut self, sid: &str) -> Option<Change> {
        let e = self.entities.get_mut(sid)?;
        if e.deleted {
            return None;
        }
        e.deleted = true;
        self.dirty_entities.insert(sid.to_string());
        Some(Change::Delete {
            sid: sid.to_string(),
        })
    }

    fn cids_of(&self, sids: &HashSet<String>) -> Vec<String> {
        self.index
            .iter()
            .filter(|(_, s)| sids.contains(*s))
            .map(|(c, _)| c.clone())
            .collect()
    }
}

fn mint() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Indices (into `keys`) of a longest strictly increasing subsequence of the present keys.
fn lis(keys: &[Option<String>]) -> HashSet<usize> {
    let idx: Vec<usize> = (0..keys.len()).filter(|&i| keys[i].is_some()).collect();
    let mut tails: Vec<usize> = Vec::new(); // positions in idx
    let mut prev: Vec<Option<usize>> = vec![None; idx.len()];
    for (j, &i) in idx.iter().enumerate() {
        let k = keys[i].as_ref().unwrap();
        let p = tails.partition_point(|&t| keys[idx[t]].as_ref().unwrap() < k);
        if p > 0 {
            prev[j] = Some(tails[p - 1]);
        }
        if p == tails.len() {
            tails.push(j);
        } else {
            tails[p] = j;
        }
    }
    let mut out = HashSet::new();
    let mut cur = tails.last().copied();
    while let Some(j) = cur {
        out.insert(idx[j]);
        cur = prev[j];
    }
    out
}

/// New `pos` keys for `children` (in their desired order): keeps the longest run of current
/// keys that is already ordered, re-keys the rest between their kept neighbours.
fn order_keys(current: &[Option<String>]) -> Vec<String> {
    let keep = lis(current);
    let mut out: Vec<Option<String>> = (0..current.len())
        .map(|i| {
            if keep.contains(&i) {
                current[i].clone()
            } else {
                None
            }
        })
        .collect();
    let mut i = 0;
    while i < out.len() {
        if out[i].is_some() {
            i += 1;
            continue;
        }
        let start = i;
        while i < out.len() && out[i].is_none() {
            i += 1;
        }
        let lo = if start > 0 {
            out[start - 1].clone()
        } else {
            None
        };
        let hi = out.get(i).cloned().flatten();
        for (k, key) in keys_between(lo.as_deref(), hi.as_deref(), i - start)
            .into_iter()
            .enumerate()
        {
            out[start + k] = Some(key);
        }
    }
    out.into_iter().map(|k| k.unwrap()).collect()
}

/// Full-tree reconcile. `nodes` is `chrome.bookmarks.getTree()` flattened (any order).
pub fn reconcile(state: &mut BookmarkState, nodes: &[ChromiumNode]) -> Vec<Change> {
    let mut by_parent: HashMap<&str, Vec<&ChromiumNode>> = HashMap::new();
    for n in nodes {
        if let Some(p) = n.parent_id.as_deref() {
            by_parent.entry(p).or_default().push(n);
        }
    }
    for v in by_parent.values_mut() {
        v.sort_by_key(|n| (n.index.unwrap_or(u32::MAX), n.id.clone()));
    }

    // Un-indexed live entities, for pairing (index lost / source switched).
    let indexed: HashSet<String> = state.index.values().cloned().collect();
    let mut pairable: HashMap<(String, Kind, String, Option<String>), Vec<String>> = HashMap::new();
    let mut unindexed: Vec<&Entity> = state
        .alive()
        .filter(|e| !indexed.contains(&e.sid))
        .collect();
    unindexed.sort_by(|a, b| (&a.pos, &a.sid).cmp(&(&b.pos, &b.sid)));
    for e in unindexed {
        pairable
            .entry((e.parent.clone(), e.kind, e.title.clone(), e.url.clone()))
            .or_default()
            .push(e.sid.clone());
    }

    let mut changes = Vec::new();
    let mut seen: HashSet<String> = HashSet::new(); // chromium ids visited
    let mut queue: Vec<(String, String, Root)> = Vec::new(); // (chromium id, parent sid, root)

    let mut roots: Vec<&ChromiumNode> = nodes.iter().filter(|n| n.as_root().is_some()).collect();
    roots.sort_by_key(|n| n.index.unwrap_or(0));
    for r in roots {
        let root = r.as_root().unwrap();
        state.set_index(&r.id, &root.sentinel());
        seen.insert(r.id.clone());
        queue.push((r.id.clone(), root.sentinel(), root));
    }

    let mut qi = 0;
    while qi < queue.len() {
        let (cid, parent_sid, root) = queue[qi].clone();
        qi += 1;
        let children: Vec<&ChromiumNode> = by_parent
            .get(cid.as_str())
            .map(|v| v.iter().copied().filter(|n| !n.excluded()).collect())
            .unwrap_or_default();
        // identity
        let mut sids = Vec::with_capacity(children.len());
        let mut creating = Vec::with_capacity(children.len());
        for n in &children {
            let kind = n.kind();
            let title = clean_title(&n.title);
            let existing = state
                .index
                .get(&n.id)
                .filter(|s| state.entities.get(*s).is_some_and(|e| !e.deleted))
                .cloned();
            let (sid, new) = match existing {
                Some(s) => (s, false),
                None => {
                    let key = (parent_sid.clone(), kind, title, n.url.clone());
                    match pairable.get_mut(&key).and_then(|v| {
                        if v.is_empty() {
                            None
                        } else {
                            Some(v.remove(0))
                        }
                    }) {
                        Some(s) => (s, false),
                        None => (mint(), true),
                    }
                }
            };
            state.set_index(&n.id, &sid);
            seen.insert(n.id.clone());
            sids.push(sid);
            creating.push(new);
        }
        // order
        let current: Vec<Option<String>> = sids
            .iter()
            .zip(&creating)
            .map(|(s, &new)| {
                if new {
                    return None;
                }
                state
                    .entities
                    .get(s)
                    .filter(|e| e.parent == parent_sid)
                    .map(|e| e.pos.clone())
            })
            .collect();
        let keys = order_keys(&current);
        for ((n, sid), (pos, new)) in children
            .iter()
            .zip(&sids)
            .zip(keys.into_iter().zip(&creating))
        {
            let want = Entity {
                sid: sid.clone(),
                kind: n.kind(),
                root,
                parent: parent_sid.clone(),
                pos,
                title: clean_title(&n.title),
                url: n.url.clone(),
                created_at: n.date_added.map(|d| d as i64),
                deleted: false,
            };
            if let Some(c) = state.upsert(want, *new) {
                changes.push(c);
            }
            if n.kind() == Kind::Folder {
                queue.push((n.id.clone(), sid.clone(), root));
            }
        }
    }

    // Indexed nodes that disappeared → deletes (a folder's whole subtree, explicitly).
    let gone: Vec<(String, String)> = state
        .index
        .iter()
        .filter(|(c, _)| !seen.contains(*c))
        .map(|(c, s)| (c.clone(), s.clone()))
        .collect();
    for (cid, sid) in gone {
        state.unset_index(&cid);
        if Root::from_sentinel(&sid).is_none() {
            if let Some(c) = state.delete(&sid) {
                changes.push(c);
            }
        }
    }
    changes
}

/// Apply one incremental event.
pub fn apply_event(
    state: &mut BookmarkState,
    ev: &BookmarkEvent,
) -> Result<Vec<Change>, NormalizeError> {
    let resync = |what: &str| Err(NormalizeError::ResyncRequired(what.to_string()));
    match ev {
        BookmarkEvent::Created { id, node } => {
            if node.excluded() || node.parent_id.as_deref() == Some("0") {
                return Ok(vec![]);
            }
            let Some(parent) = node.parent_id.as_deref().and_then(|p| state.resolve(p)) else {
                return resync("created under an unknown parent");
            };
            let root = state.root_of_parent(&parent).unwrap_or(Root::Other);
            let existing = state
                .resolve(id)
                .filter(|s| Root::from_sentinel(s).is_none());
            let (sid, new) = match existing {
                Some(s) => (s, false),
                None => (mint(), true),
            };
            let pos = state.pos_at(&sid, &parent, node.index.unwrap_or(u32::MAX) as usize);
            state.set_index(id, &sid);
            let want = Entity {
                sid: sid.clone(),
                kind: node.kind(),
                root,
                parent,
                pos,
                title: clean_title(&node.title),
                url: node.url.clone(),
                created_at: node.date_added.map(|d| d as i64),
                deleted: false,
            };
            Ok(state.upsert(want, new).into_iter().collect())
        }
        BookmarkEvent::Changed { id, title, url } => {
            let Some(sid) = state
                .resolve(id)
                .filter(|s| Root::from_sentinel(s).is_none())
            else {
                return resync("changed an unknown node");
            };
            let mut want = state.entities[&sid].clone();
            if let Some(t) = title {
                want.title = clean_title(t);
            }
            if want.kind == Kind::Url {
                if let Some(u) = url {
                    if u.len() > MAX_URL_BYTES {
                        return Ok(state.delete(&sid).into_iter().collect());
                    }
                    want.url = Some(u.clone());
                }
            }
            Ok(state.upsert(want, false).into_iter().collect())
        }
        BookmarkEvent::Moved {
            id,
            parent_id,
            index,
        } => {
            let Some(sid) = state
                .resolve(id)
                .filter(|s| Root::from_sentinel(s).is_none())
            else {
                return resync("moved an unknown node");
            };
            let Some(parent) = state.resolve(parent_id) else {
                return resync("moved under an unknown parent");
            };
            let root = state.root_of_parent(&parent).unwrap_or(Root::Other);
            let pos = state.pos_at(&sid, &parent, *index as usize);
            let mut want = state.entities[&sid].clone();
            let root_changed = want.root != root;
            want.parent = parent;
            want.pos = pos;
            want.root = root;
            let mut out: Vec<Change> = state.upsert(want, false).into_iter().collect();
            if root_changed {
                for d in state.descendants(&sid) {
                    let mut w = state.entities[&d].clone();
                    w.root = root;
                    out.extend(state.upsert(w, false));
                }
            }
            Ok(out)
        }
        BookmarkEvent::Removed { id } => {
            let Some(sid) = state
                .resolve(id)
                .filter(|s| Root::from_sentinel(s).is_none())
            else {
                return Ok(vec![]); // already gone: idempotent
            };
            let mut doomed = state.descendants(&sid);
            doomed.push(sid);
            let set: HashSet<String> = doomed.iter().cloned().collect();
            for cid in state.cids_of(&set) {
                state.unset_index(&cid);
            }
            Ok(doomed.iter().filter_map(|s| state.delete(s)).collect())
        }
        BookmarkEvent::Reordered { id, child_ids } => {
            if state.resolve(id).is_none() {
                return resync("reordered an unknown folder");
            }
            let mut sids = Vec::with_capacity(child_ids.len());
            for c in child_ids {
                match state.resolve(c) {
                    Some(s) => sids.push(s),
                    None => continue, // excluded child (e.g. oversize URL): not ours to order
                }
            }
            let current: Vec<Option<String>> = sids
                .iter()
                .map(|s| state.entities.get(s).map(|e| e.pos.clone()))
                .collect();
            let keys = order_keys(&current);
            let mut out = Vec::new();
            for (sid, pos) in sids.iter().zip(keys) {
                let mut w = state.entities[sid].clone();
                w.pos = pos;
                out.extend(state.upsert(w, false));
            }
            Ok(out)
        }
    }
}

/// Synchrome-model tree (for tests, diagnostics and validation): `(depth, kind, title, url)`
/// in display order, per root.
pub fn render_tree(state: &BookmarkState) -> Vec<(Root, usize, Kind, String, Option<String>)> {
    let mut out = Vec::new();
    for root in [Root::Bar, Root::Other, Root::Mobile] {
        let mut stack: Vec<(String, usize)> = vec![(root.sentinel(), 0)];
        while let Some((p, depth)) = stack.pop() {
            let kids = state.siblings(&p, None);
            for (_, sid) in kids.iter().rev() {
                stack.push((sid.clone(), depth + 1));
            }
            if depth > 0 {
                let e = &state.entities[&p];
                out.push((root, depth - 1, e.kind, e.title.clone(), e.url.clone()));
            }
        }
    }
    out
}

/// Rebuild a materialised state from journal ops (any order): ops are applied in HLC order,
/// which is per-field last-writer-wins for a total order; a `set` after a `delete` resurrects.
/// Used to verify what the store holds; the download slice builds on it.
pub fn materialize(ops: &[crate::op::Op]) -> BookmarkState {
    use crate::op::OpKind;
    let mut sorted: Vec<&crate::op::Op> = ops.iter().filter(|o| o.col == COLLECTION).collect();
    sorted.sort_by(|a, b| (&a.hlc, a.seq).cmp(&(&b.hlc, b.seq)));
    let mut st = BookmarkState::default();
    for o in sorted {
        match o.op {
            OpKind::Delete => {
                if let Some(e) = st.entities.get_mut(&o.id) {
                    e.deleted = true;
                }
            }
            OpKind::Set => {
                let f = o.fields.clone().unwrap_or_default();
                let s = |k: &str| f.get(k).and_then(|v| v.as_str()).map(str::to_string);
                let e = st.entities.entry(o.id.clone()).or_insert_with(|| Entity {
                    sid: o.id.clone(),
                    kind: Kind::Folder,
                    root: Root::Other,
                    parent: Root::Other.sentinel(),
                    pos: String::new(),
                    title: String::new(),
                    url: None,
                    created_at: None,
                    deleted: false,
                });
                e.deleted = false;
                if let Some(v) = s("parentId") {
                    e.parent = v;
                }
                if let Some(v) = s("pos") {
                    e.pos = v;
                }
                if let Some(v) = s("title") {
                    e.title = v;
                }
                if let Some(k) = s("kind").and_then(|k| Kind::parse(&k)) {
                    e.kind = k;
                }
                if let Some(r) = s("root").and_then(|r| Root::parse(&r)) {
                    e.root = r;
                }
                if f.contains_key("url") {
                    e.url = s("url");
                }
                if let Some(c) = f.get("createdAt").and_then(|v| v.as_i64()) {
                    e.created_at = Some(c);
                }
            }
        }
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, parent: &str, index: u32, title: &str) -> ChromiumNode {
        ChromiumNode {
            id: id.into(),
            parent_id: Some(parent.into()),
            index: Some(index),
            title: title.into(),
            ..Default::default()
        }
    }
    fn link(id: &str, parent: &str, index: u32, title: &str, url: &str) -> ChromiumNode {
        ChromiumNode {
            url: Some(url.into()),
            date_added: Some(1_790_000_000_000.0),
            ..folder(id, parent, index, title)
        }
    }
    fn roots() -> Vec<ChromiumNode> {
        vec![
            ChromiumNode {
                folder_type: Some("bookmarks-bar".into()),
                ..folder("1", "0", 0, "Bookmarks bar")
            },
            ChromiumNode {
                folder_type: Some("other".into()),
                ..folder("2", "0", 1, "Other bookmarks")
            },
            ChromiumNode {
                folder_type: Some("mobile".into()),
                ..folder("3", "0", 2, "Mobile bookmarks")
            },
        ]
    }
    /// Synchrome Test / Folder A [B1, B2] / Folder B [B3]
    fn known_tree() -> Vec<ChromiumNode> {
        let mut v = roots();
        v.extend([
            folder("10", "1", 0, "Synchrome Test"),
            folder("11", "10", 0, "Folder A"),
            link("12", "11", 0, "Bookmark 1", "https://example.com/1"),
            link("13", "11", 1, "Bookmark 2", "https://example.com/2"),
            folder("14", "10", 1, "Folder B"),
            link("15", "14", 0, "Bookmark 3", "https://example.com/3"),
        ]);
        v
    }
    fn titles(s: &BookmarkState) -> Vec<(usize, String)> {
        render_tree(s)
            .into_iter()
            .map(|(_, d, _, t, _)| (d, t))
            .collect()
    }
    fn tr(v: &[(usize, &str)]) -> Vec<(usize, String)> {
        v.iter().map(|(d, t)| (*d, t.to_string())).collect()
    }

    #[test]
    fn empty_tree_snapshot_creates_nothing() {
        let mut s = BookmarkState::default();
        assert!(reconcile(&mut s, &roots()).is_empty());
        assert_eq!(s.index.len(), 3);
        assert!(s.entities.is_empty());
        assert!(reconcile(&mut s, &roots()).is_empty());
    }

    #[test]
    fn known_tree_snapshot_is_structured_ordered_and_idempotent() {
        let mut s = BookmarkState::default();
        let ch = reconcile(&mut s, &known_tree());
        assert_eq!(ch.len(), 6);
        assert!(ch.iter().all(|c| matches!(c, Change::Set { .. })));
        assert_eq!(
            titles(&s),
            tr(&[
                (0, "Synchrome Test"),
                (1, "Folder A"),
                (2, "Bookmark 1"),
                (2, "Bookmark 2"),
                (1, "Folder B"),
                (2, "Bookmark 3"),
            ])
        );
        // creation ops carry every field, chromium ids never leak
        let first_url = ch
            .iter()
            .find(|c| matches!(c, Change::Set { fields, .. } if fields.contains_key("url")))
            .unwrap();
        if let Change::Set { fields, sid } = first_url {
            for k in [
                "parentId",
                "pos",
                "title",
                "kind",
                "root",
                "url",
                "createdAt",
            ] {
                assert!(fields.contains_key(k), "{k}");
            }
            assert!(uuid::Uuid::parse_str(sid).is_ok());
        }
        assert!(ch.iter().all(|c| {
            let Change::Set { fields, .. } = c else {
                return true;
            };
            fields["parentId"].as_str().unwrap().len() > 3
        }));
        assert_eq!(s.counts(), (3, 3));
        // duplicate snapshot: nothing
        s.dirty_entities.clear();
        assert!(reconcile(&mut s, &known_tree()).is_empty());
        assert!(s.dirty_entities.is_empty());
    }

    #[test]
    fn nested_folders_and_same_title_url_in_different_folders_are_distinct() {
        let mut v = roots();
        v.extend([
            folder("10", "1", 0, "L1"),
            folder("11", "10", 0, "L2"),
            folder("12", "11", 0, "L3"),
            folder("13", "12", 0, "L4"),
            link("14", "13", 0, "Same", "https://same.example/"),
            link("15", "2", 0, "Same", "https://same.example/"),
            link("16", "10", 0, "Same", "https://same.example/"),
        ]);
        let mut s = BookmarkState::default();
        reconcile(&mut s, &v);
        assert_eq!(s.counts(), (3, 4));
        let sids: HashSet<&String> = ["14", "15", "16"].iter().map(|c| &s.index[*c]).collect();
        assert_eq!(sids.len(), 3);
        assert_eq!(s.entities[&s.index["15"]].root, Root::Other);
        assert_eq!(s.entities[&s.index["14"]].root, Root::Bar);
        assert_eq!(s.entities[&s.index["13"]].parent, s.index["12"]);
    }

    #[test]
    fn create_rename_url_change_move_and_delete() {
        let mut s = BookmarkState::default();
        reconcile(&mut s, &known_tree());
        // create at index 1 in Folder A
        let ch = apply_event(
            &mut s,
            &BookmarkEvent::Created {
                id: "20".into(),
                node: link("20", "11", 1, "Bookmark 1.5", "https://example.com/15"),
            },
        )
        .unwrap();
        assert_eq!(ch.len(), 1);
        assert_eq!(
            titles(&s)[1..5].to_vec(),
            tr(&[
                (1, "Folder A"),
                (2, "Bookmark 1"),
                (2, "Bookmark 1.5"),
                (2, "Bookmark 2")
            ])
        );
        // rename: only title
        let ch = apply_event(
            &mut s,
            &BookmarkEvent::Changed {
                id: "12".into(),
                title: Some("Bookmark One".into()),
                url: Some("https://example.com/1".into()),
            },
        )
        .unwrap();
        let Change::Set { fields, .. } = &ch[0] else {
            panic!()
        };
        assert_eq!(fields.keys().collect::<Vec<_>>(), vec!["title"]);
        // URL change: only url
        let ch = apply_event(
            &mut s,
            &BookmarkEvent::Changed {
                id: "13".into(),
                title: Some("Bookmark 2".into()),
                url: Some("https://example.org/2".into()),
            },
        )
        .unwrap();
        let Change::Set { fields, .. } = &ch[0] else {
            panic!()
        };
        assert_eq!(fields.keys().collect::<Vec<_>>(), vec!["url"]);
        // move Bookmark 3 into Folder A at index 0
        let ch = apply_event(
            &mut s,
            &BookmarkEvent::Moved {
                id: "15".into(),
                parent_id: "11".into(),
                index: 0,
            },
        )
        .unwrap();
        let Change::Set { fields, .. } = &ch[0] else {
            panic!()
        };
        assert_eq!(fields.keys().collect::<Vec<_>>(), vec!["parentId", "pos"]);
        assert_eq!(titles(&s)[2], (2, "Bookmark 3".to_string()));
        // replayed move is a no-op
        assert!(apply_event(
            &mut s,
            &BookmarkEvent::Moved {
                id: "15".into(),
                parent_id: "11".into(),
                index: 0
            }
        )
        .unwrap()
        .is_empty());
        // move a folder to another root: root recomputed for its subtree
        let ch = apply_event(
            &mut s,
            &BookmarkEvent::Moved {
                id: "11".into(),
                parent_id: "2".into(),
                index: 0,
            },
        )
        .unwrap();
        assert_eq!(ch.len(), 1 + 4, "folder + its 4 children get root=other");
        assert!(s.alive().filter(|e| e.root == Root::Other).count() == 5);
    }

    #[test]
    fn delete_folder_with_children_tombstones_the_whole_subtree() {
        let mut s = BookmarkState::default();
        reconcile(&mut s, &known_tree());
        let ch = apply_event(&mut s, &BookmarkEvent::Removed { id: "10".into() }).unwrap();
        assert_eq!(ch.len(), 6);
        assert!(ch.iter().all(|c| matches!(c, Change::Delete { .. })));
        assert_eq!(s.counts(), (0, 0));
        assert_eq!(s.index.len(), 3, "only roots remain indexed");
        // duplicate removal event: no-op
        assert!(
            apply_event(&mut s, &BookmarkEvent::Removed { id: "10".into() })
                .unwrap()
                .is_empty()
        );
        // the same deletion seen by a later snapshot: no-op too
        assert!(reconcile(&mut s, &roots()).is_empty());
    }

    #[test]
    fn reorder_rekeys_only_what_moved() {
        let mut v = roots();
        for i in 0..6u32 {
            v.push(link(
                &format!("{}", 30 + i),
                "2",
                i,
                &format!("L{i}"),
                "https://x.example/",
            ));
        }
        let mut s = BookmarkState::default();
        reconcile(&mut s, &v);
        // move L5 to the front via reorder
        let order: Vec<String> = [35, 30, 31, 32, 33, 34]
            .iter()
            .map(|i| i.to_string())
            .collect();
        let ch = apply_event(
            &mut s,
            &BookmarkEvent::Reordered {
                id: "2".into(),
                child_ids: order,
            },
        )
        .unwrap();
        assert_eq!(ch.len(), 1, "LIS keeps 5 keys, re-keys one");
        let t: Vec<String> = titles(&s).into_iter().map(|(_, t)| t).collect();
        assert_eq!(t, vec!["L5", "L0", "L1", "L2", "L3", "L4"]);
    }

    #[test]
    fn snapshot_detects_missed_events_and_pairs_after_index_loss() {
        let mut s = BookmarkState::default();
        reconcile(&mut s, &known_tree());
        // missed events: rename 12, remove 15, add 16
        let mut v = known_tree();
        v.retain(|n| n.id != "15");
        v.iter_mut().find(|n| n.id == "12").unwrap().title = "Renamed".into();
        v.push(link("16", "14", 0, "New", "https://example.com/new"));
        let ch = reconcile(&mut s, &v);
        let sets = ch
            .iter()
            .filter(|c| matches!(c, Change::Set { .. }))
            .count();
        let dels = ch
            .iter()
            .filter(|c| matches!(c, Change::Delete { .. }))
            .count();
        assert_eq!((sets, dels), (2, 1));
        // index lost (e.g. app reinstalled state, profile ids reassigned): pairing adopts ids
        let before: HashSet<String> = s.alive().map(|e| e.sid.clone()).collect();
        s.index.clear();
        let mut renumbered = roots();
        for n in v.iter().filter(|n| n.parent_id.as_deref() != Some("0")) {
            let mut m = n.clone();
            m.id = format!("9{}", n.id);
            if let Some(p) = &n.parent_id {
                if !["1", "2", "3"].contains(&p.as_str()) {
                    m.parent_id = Some(format!("9{p}"));
                }
            }
            renumbered.push(m);
        }
        let ch = reconcile(&mut s, &renumbered);
        assert!(
            ch.is_empty(),
            "everything paired, nothing duplicated: {ch:?}"
        );
        let after: HashSet<String> = s.alive().map(|e| e.sid.clone()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn unknown_references_require_resync_and_exclusions_are_ignored() {
        let mut s = BookmarkState::default();
        reconcile(&mut s, &known_tree());
        assert!(matches!(
            apply_event(
                &mut s,
                &BookmarkEvent::Changed {
                    id: "999".into(),
                    title: Some("x".into()),
                    url: None
                }
            ),
            Err(NormalizeError::ResyncRequired(_))
        ));
        assert!(matches!(
            apply_event(
                &mut s,
                &BookmarkEvent::Created {
                    id: "50".into(),
                    node: link("50", "777", 0, "x", "https://x/")
                }
            ),
            Err(NormalizeError::ResyncRequired(_))
        ));
        // account-storage / managed / oversize nodes are not captured
        let mut v = known_tree();
        v.push(ChromiumNode {
            syncing: Some(true),
            ..link("60", "1", 5, "acct", "https://acct/")
        });
        v.push(link(
            "61",
            "1",
            6,
            "huge",
            &format!("data:,{}", "a".repeat(MAX_URL_BYTES)),
        ));
        v.push(ChromiumNode {
            folder_type: Some("managed".into()),
            unmodifiable: Some("managed".into()),
            ..folder("62", "0", 3, "Managed")
        });
        v.push(link("63", "62", 0, "corp", "https://corp/"));
        assert!(reconcile(&mut s, &v).is_empty());
        assert!(
            !s.index.contains_key("60")
                && !s.index.contains_key("61")
                && !s.index.contains_key("63")
        );
    }

    #[test]
    fn duplicate_created_event_is_a_noop() {
        let mut s = BookmarkState::default();
        reconcile(&mut s, &known_tree());
        let ev = BookmarkEvent::Created {
            id: "20".into(),
            node: link("20", "14", 1, "X", "https://x.example/"),
        };
        assert_eq!(apply_event(&mut s, &ev).unwrap().len(), 1);
        assert!(apply_event(&mut s, &ev).unwrap().is_empty());
        assert_eq!(s.counts(), (4, 3));
    }

    #[test]
    fn materialize_from_ops_equals_the_capturing_view() {
        use crate::hlc::HlcClock;
        use crate::op::{Op, OpKind};
        let mut s = BookmarkState::default();
        let mut changes = reconcile(&mut s, &known_tree());
        changes.extend(
            apply_event(
                &mut s,
                &BookmarkEvent::Moved {
                    id: "15".into(),
                    parent_id: "11".into(),
                    index: 0,
                },
            )
            .unwrap(),
        );
        changes.extend(apply_event(&mut s, &BookmarkEvent::Removed { id: "13".into() }).unwrap());
        let mut clk = HlcClock::new("d", None);
        let mut ops: Vec<Op> = changes
            .iter()
            .enumerate()
            .map(|(i, c)| Op {
                seq: i as u64 + 1,
                hlc: clk.tick(1000).unwrap().to_string(),
                col: COLLECTION.into(),
                id: c.sid().into(),
                op: if matches!(c, Change::Delete { .. }) {
                    OpKind::Delete
                } else {
                    OpKind::Set
                },
                fields: match c {
                    Change::Set { fields, .. } => Some(fields.clone()),
                    _ => None,
                },
                by: "d".into(),
            })
            .collect();
        ops.reverse(); // order of arrival does not matter
        let m = materialize(&ops);
        assert_eq!(render_tree(&m), render_tree(&s));
    }

    #[test]
    fn large_tree_is_fast_and_keys_stay_short() {
        let mut v = roots();
        let mut id = 100;
        for f in 0..50 {
            let fid = id;
            v.push(folder(&fid.to_string(), "1", f, &format!("F{f}")));
            id += 1;
            for b in 0..200 {
                v.push(link(
                    &id.to_string(),
                    &fid.to_string(),
                    b,
                    &format!("B{b}"),
                    "https://e/",
                ));
                id += 1;
            }
        }
        let mut s = BookmarkState::default();
        let t = std::time::Instant::now();
        assert_eq!(reconcile(&mut s, &v).len(), 50 + 50 * 200);
        assert!(reconcile(&mut s, &v).is_empty());
        assert!(t.elapsed().as_secs() < 5);
        assert!(s.alive().all(|e| e.pos.len() <= 4));
    }
}
