//! Shared harness: an in-memory cloud with per-account isolation, a fake browser with
//! chrome.bookmarks semantics (create appends, non-recursive folder removal…) and simulated
//! devices that capture, sync, plan, apply and reconcile like the host + extension do.
#![allow(dead_code)]
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use synchrome_state::compaction::*;
use synchrome_state::*;
use synchrome_sync::bookmarks::ChromiumNode;
use uuid::Uuid;

// ------------------------------------------------------------------ fake cloud
#[derive(Default)]
pub struct Cloud {
    pub objects: HashMap<(String, String), Vec<u8>>,
    pub events: Vec<(String, PublishedEvent)>,
    pub hidden: HashSet<u64>, // seq_to of events hidden from listings (simulates a gap)
    // ---- migration 0005 contract (compaction)
    pub now_ms: u64, // the DATABASE clock
    pub snaps: Vec<FSnap>,
    pub cursors: HashMap<(String, Uuid, String), FCur>,
    pub mapped: HashSet<(String, Uuid, String)>,
}

#[derive(Clone, Debug)]
pub struct FSnap {
    pub account: String,
    pub profile: Uuid,
    pub generation: u64,
    pub state: String,
    pub created_by: String,
    pub manifest_path: Option<String>,
    pub sha: Option<String>,
    pub watermarks: BTreeMap<String, u64>,
    pub bytes: u64,
    pub claimed: u64,
    pub expires: u64,
    pub ready_at: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct FCur {
    pub applied: BTreeMap<String, u64>,
    pub published: u64,
    pub protocol: i32,
    pub retired: bool,
    pub updated: u64,
}

pub const MIN: u64 = 60_000;
pub const DAY: u64 = 24 * 60 * MIN;

pub struct Sink<'a> {
    pub cloud: &'a Mutex<Cloud>,
    pub account: String,
    pub offline: bool,
    pub mappings: HashSet<Uuid>,
    /// Never bootstrap from a snapshot unless history is gone (measures full replay).
    pub full_replay: bool,
}

impl JournalSink for Sink<'_> {
    async fn account_id(&self) -> Result<String, SinkError> {
        if self.offline {
            return Err(SinkError::Offline("down".into()));
        }
        Ok(self.account.clone())
    }
    async fn mapping(&self, m: Uuid) -> Result<MappingCheck, SinkError> {
        self.account_id().await?;
        Ok(if self.mappings.contains(&m) {
            MappingCheck::Active
        } else {
            MappingCheck::Missing
        })
    }
    async fn journal_devices(&self, _p: Uuid) -> Result<Vec<String>, SinkError> {
        Ok(vec![])
    }
    async fn put_chunk(&self, path: &str, body: &[u8]) -> Result<(), SinkError> {
        let a = self.account_id().await?;
        self.cloud
            .lock()
            .unwrap()
            .objects
            .insert((a, path.into()), body.to_vec());
        Ok(())
    }
    async fn record_event(&self, ev: &PublishedEvent) -> Result<(), SinkError> {
        let a = self.account_id().await?;
        let mut c = self.cloud.lock().unwrap();
        if !c.events.iter().any(|(x, e)| {
            x == &a
                && e.profile_id == ev.profile_id
                && e.device_id == ev.device_id
                && e.seq_to == ev.seq_to
        }) {
            c.events.push((a, ev.clone()));
        }
        Ok(())
    }
    async fn list_events(&self, p: Uuid) -> Result<Vec<RemoteEvent>, SinkError> {
        let a = self.account_id().await?;
        let c = self.cloud.lock().unwrap();
        Ok(c.events
            .iter()
            .filter(|(x, e)| x == &a && e.profile_id == p && !c.hidden.contains(&e.seq_to))
            .map(|(_, e)| RemoteEvent {
                device_id: e.device_id.to_string(),
                path: e.path.clone(),
                seq_from: e.seq_from,
                seq_to: e.seq_to,
            })
            .collect())
    }
    async fn ready_snapshots(&self, p: Uuid) -> Result<Vec<SnapshotRow>, SinkError> {
        let a = self.account_id().await?;
        let c = self.cloud.lock().unwrap();
        Ok(c.ready(&a, p)
            .iter()
            .take(2)
            .map(|s| Cloud::row(s, c.now_ms))
            .collect())
    }
    fn prefer_snapshot_bootstrap(&self) -> bool {
        !self.full_replay
    }
    async fn get_chunk(&self, path: &str) -> Result<Option<Vec<u8>>, SinkError> {
        let a = self.account_id().await?;
        Ok(self
            .cloud
            .lock()
            .unwrap()
            .objects
            .get(&(a, path.to_string()))
            .cloned())
    }
}

// ------------------------------------------------------------------ fake browser
#[derive(Clone)]
pub struct FNode {
    pub parent: String,
    pub title: String,
    pub url: Option<String>,
}

/// chrome.bookmarks semantics needed here: create appends, move without index appends,
/// move with index places, remove is non-recursive for folders.
pub struct Browser {
    pub nodes: HashMap<String, FNode>,
    pub kids: HashMap<String, Vec<String>>,
    pub next: u64,
    pub ops_executed: usize,
}

impl Browser {
    pub fn new() -> Self {
        let mut b = Browser {
            nodes: HashMap::new(),
            kids: HashMap::new(),
            next: 100,
            ops_executed: 0,
        };
        for (id, t) in [
            ("1", "Bookmarks bar"),
            ("2", "Other bookmarks"),
            ("3", "Mobile bookmarks"),
        ] {
            b.nodes.insert(
                id.into(),
                FNode {
                    parent: "0".into(),
                    title: t.into(),
                    url: None,
                },
            );
            b.kids.entry("0".into()).or_default().push(id.into());
        }
        b
    }
    pub fn create(&mut self, parent: &str, title: &str, url: Option<&str>) -> String {
        self.next += 1;
        let id = self.next.to_string();
        self.nodes.insert(
            id.clone(),
            FNode {
                parent: parent.into(),
                title: title.into(),
                url: url.map(Into::into),
            },
        );
        self.kids.entry(parent.into()).or_default().push(id.clone());
        id
    }
    pub fn detach(&mut self, id: &str) {
        let p = self.nodes[id].parent.clone();
        self.kids.get_mut(&p).unwrap().retain(|x| x != id);
    }
    pub fn move_to(&mut self, id: &str, parent: &str, index: Option<usize>) {
        self.detach(id);
        let k = self.kids.entry(parent.into()).or_default();
        let i = index.unwrap_or(k.len()).min(k.len());
        k.insert(i, id.into());
        self.nodes.get_mut(id).unwrap().parent = parent.into();
    }
    pub fn remove(&mut self, id: &str, recursive: bool) -> Result<(), &'static str> {
        if !self.nodes.contains_key(id) {
            return Err("not_found");
        }
        let has_kids = self.kids.get(id).is_some_and(|k| !k.is_empty());
        if has_kids && !recursive {
            return Err("not_empty");
        }
        for k in self.kids.get(id).cloned().unwrap_or_default() {
            self.remove(&k, true).unwrap();
        }
        self.kids.remove(id);
        self.detach(id);
        self.nodes.remove(id);
        Ok(())
    }
    pub fn find(&self, title: &str) -> String {
        self.nodes
            .iter()
            .find(|(_, n)| n.title == title)
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| panic!("no node {title}"))
    }
    pub fn snapshot(&self) -> Vec<ChromiumNode> {
        let mut out = Vec::new();
        for (p, kids) in &self.kids {
            for (i, id) in kids.iter().enumerate() {
                let n = &self.nodes[id];
                let ft = match id.as_str() {
                    "1" => Some("bookmarks-bar".into()),
                    "2" => Some("other".into()),
                    "3" => Some("mobile".into()),
                    _ => None,
                };
                out.push(ChromiumNode {
                    id: id.clone(),
                    parent_id: Some(p.clone()),
                    index: Some(i as u32),
                    title: n.title.clone(),
                    url: n.url.clone(),
                    date_added: Some(1.0),
                    folder_type: ft,
                    ..Default::default()
                });
            }
        }
        out
    }
    /// (depth, title) under the bookmarks bar, in order.
    pub fn tree(&self) -> Vec<(usize, String, Option<String>)> {
        let mut out = Vec::new();
        fn walk(b: &Browser, p: &str, d: usize, out: &mut Vec<(usize, String, Option<String>)>) {
            for k in b.kids.get(p).cloned().unwrap_or_default() {
                let n = &b.nodes[&k];
                out.push((d, n.title.clone(), n.url.clone()));
                walk(b, &k, d + 1, out);
            }
        }
        walk(self, "1", 0, &mut out);
        out
    }
    /// The extension's executor.
    pub fn apply(&mut self, ops: &[ApplyOp]) -> Vec<ApplyResult> {
        let mut made: HashMap<u32, String> = HashMap::new();
        let resolve = |r: &NodeRef, made: &HashMap<u32, String>| match r {
            NodeRef::Id { id } => Some(id.clone()),
            NodeRef::Op { op } => made.get(op).cloned(),
        };
        let mut res = Vec::new();
        for op in ops {
            self.ops_executed += 1;
            let r = match &op.action {
                ApplyAction::CreateFolder { parent, title } => match resolve(parent, &made) {
                    Some(p) => {
                        let id = self.create(&p, title, None);
                        made.insert(op.op_id, id.clone());
                        Ok(Some(id))
                    }
                    None => Err("not_found"),
                },
                ApplyAction::CreateBookmark { parent, title, url } => {
                    match resolve(parent, &made) {
                        Some(p) => {
                            let id = self.create(&p, title, Some(url));
                            made.insert(op.op_id, id.clone());
                            Ok(Some(id))
                        }
                        None => Err("not_found"),
                    }
                }
                ApplyAction::Rename { id, title } => match self.nodes.get_mut(id) {
                    Some(n) => {
                        n.title = title.clone();
                        Ok(None)
                    }
                    None => Err("not_found"),
                },
                ApplyAction::SetUrl { id, url } => match self.nodes.get_mut(id) {
                    Some(n) => {
                        n.url = Some(url.clone());
                        Ok(None)
                    }
                    None => Err("not_found"),
                },
                ApplyAction::Move { id, parent } => {
                    match (resolve(parent, &made), self.nodes.contains_key(id)) {
                        (Some(p), true) => {
                            if self.nodes[id].parent != p {
                                self.move_to(id, &p, None);
                            }
                            Ok(None)
                        }
                        _ => Err("not_found"),
                    }
                }
                ApplyAction::Reorder { parent, children } => match resolve(parent, &made) {
                    Some(p) => {
                        for (i, c) in children.iter().enumerate() {
                            if let Some(c) = resolve(c, &made) {
                                if self.nodes.get(&c).is_some_and(|n| n.parent == p) {
                                    self.move_to(&c, &p, Some(i));
                                }
                            }
                        }
                        Ok(None)
                    }
                    None => Err("not_found"),
                },
                ApplyAction::Delete { id, folder } => self.remove(id, !folder).map(|_| None),
            };
            res.push(match r {
                Ok(id) => ApplyResult {
                    op_id: op.op_id,
                    ok: true,
                    id,
                    error: None,
                },
                Err(e) => ApplyResult {
                    op_id: op.op_id,
                    ok: false,
                    id: None,
                    error: Some(e.into()),
                },
            });
        }
        res
    }
}

// ------------------------------------------------------------------ devices
pub const T0: u64 = 1_790_900_000_000;

pub struct Dev {
    pub db: StateDb,
    pub br: Browser,
    pub src: Source,
}

pub struct World {
    pub cloud: Mutex<Cloud>,
    pub account: String,
    pub profile: Uuid,
    pub mappings: HashSet<Uuid>,
    pub t: std::cell::Cell<u64>,
}

impl World {
    pub fn new() -> Self {
        World {
            cloud: Mutex::new(Cloud::default()),
            account: Uuid::new_v4().to_string(),
            profile: Uuid::new_v4(),
            mappings: HashSet::new(),
            t: std::cell::Cell::new(T0),
        }
    }
    pub fn device(&mut self) -> Dev {
        let m = Uuid::new_v4();
        self.mappings.insert(m);
        let id = Uuid::new_v4();
        // mapped to the profile in the cloud (profile_mappings): a GC blocker until it reports
        self.cloud.lock().unwrap().mapped.insert((
            self.account.clone(),
            self.profile,
            id.to_string(),
        ));
        Dev {
            db: StateDb::open_in_memory().unwrap(),
            br: Browser::new(),
            src: Source {
                mapping_id: m,
                account_id: self.account.clone(),
                device_id: id,
                profile_id: self.profile,
                identity: format!("helium|/tmp/{m}|Default"),
            },
        }
    }
    pub fn sink(&self) -> Sink<'_> {
        Sink {
            cloud: &self.cloud,
            account: self.account.clone(),
            offline: false,
            mappings: self.mappings.clone(),
            full_replay: false,
        }
    }
    pub fn tick(&self) -> u64 {
        self.t.set(self.t.get() + 1000);
        self.t.get()
    }
    /// A local change on `d` (captured through a snapshot, as at a worker restart) + sync.
    pub async fn local(&mut self, d: &mut Dev) -> usize {
        let t = self.tick();
        let ops = match d
            .db
            .capture_snapshot(&d.src, "e", 0, &d.br.snapshot(), t)
            .unwrap()
        {
            CaptureOutcome::Applied { ops, .. } => ops,
            o => panic!("{o:?}"),
        };
        let sink = self.sink();
        sync(&mut d.db, &sink, self.profile, "h", (t / 1000) as i64)
            .await
            .unwrap();
        ops
    }
    /// Pull + plan/apply/record rounds + post-apply reconcile. Returns (#ops applied in the
    /// browser, #local ops the reconcile produced = echoes).
    pub async fn remote(&mut self, d: &mut Dev, max_bytes: usize) -> (usize, usize) {
        let t = self.tick();
        let sink = self.sink();
        // a device first captures its own (possibly empty) tree, then syncs
        if d.db
            .status(self.profile, d.src.mapping_id)
            .unwrap()
            .download
            .is_none()
        {
            d.db.capture_snapshot(&d.src, "e", 0, &d.br.snapshot(), t)
                .unwrap();
        }
        sync(&mut d.db, &sink, self.profile, "h", (t / 1000) as i64)
            .await
            .unwrap();
        let mut applied = 0;
        let mut echoes = 0;
        for _ in 0..50 {
            let plan = d.db.plan_apply(self.profile, max_bytes).unwrap();
            if plan.ops.is_empty() {
                break;
            }
            let res = d.br.apply(&plan.ops);
            applied += plan.ops.len();
            d.db.record_apply(self.profile, &plan, &res, (t / 1000) as i64)
                .unwrap();
            if let CaptureOutcome::Applied { ops, .. } =
                d.db.capture_snapshot(&d.src, "e", 0, &d.br.snapshot(), self.tick())
                    .unwrap()
            {
                echoes += ops;
            }
            if !plan.more {
                break;
            }
        }
        (applied, echoes)
    }
}

pub fn build_known(b: &mut Browser) {
    let t = b.create("1", "Synchrome Test", None);
    let a = b.create(&t, "Folder A", None);
    b.create(&a, "Bookmark 1", Some("https://example.com/1"));
    b.create(&a, "Bookmark 2", Some("https://example.com/2"));
    let fb = b.create(&t, "Folder B", None);
    let n = b.create(&fb, "Nested", None);
    b.create(&n, "Bookmark 3", Some("https://example.com/3"));
}

pub fn plan_kinds(plan: &ApplyPlan) -> Vec<&'static str> {
    plan.ops
        .iter()
        .map(|o| match o.action {
            ApplyAction::CreateFolder { .. } => "create_folder",
            ApplyAction::CreateBookmark { .. } => "create_bookmark",
            ApplyAction::Rename { .. } => "rename",
            ApplyAction::SetUrl { .. } => "set_url",
            ApplyAction::Move { .. } => "move",
            ApplyAction::Reorder { .. } => "reorder",
            ApplyAction::Delete { .. } => "delete",
        })
        .collect()
}

// ------------------------------------------------------------------ fake 0005 contract
impl Cloud {
    fn ready(&self, a: &str, p: Uuid) -> Vec<FSnap> {
        let mut v: Vec<FSnap> = self
            .snaps
            .iter()
            .filter(|s| s.account == a && s.profile == p && s.state == "ready")
            .cloned()
            .collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.generation));
        v
    }
    pub fn status(&self, a: &str, p: Uuid) -> CompactionStatus {
        let ready = self.ready(a, p);
        let blockers = self
            .mapped
            .iter()
            .filter(|(x, q, _d)| x == a && *q == p)
            .filter(|(x, q, d)| {
                !self
                    .cursors
                    .get(&(x.clone(), *q, d.clone()))
                    .is_some_and(|c| c.retired || c.protocol >= 1)
            })
            .count() as u32;
        let cur = ready.first();
        let prev = ready.get(1);
        let mine: Vec<&FSnap> = self
            .snaps
            .iter()
            .filter(|s| s.account == a && s.profile == p)
            .collect();
        let live = mine
            .iter()
            .any(|s| s.state == "building" && s.expires > self.now_ms);
        let last = mine.iter().map(|s| s.claimed).max();
        CompactionStatus {
            current_generation: cur.map(|c| c.generation),
            previous_generation: prev.map(|c| c.generation),
            journal_gc_watermarks: prev.map(|c| c.watermarks.clone()),
            gc_blockers: blockers,
            journal_gc_ok: prev.is_some()
                && cur.is_some_and(|c| self.now_ms >= c.ready_at.unwrap() + 10 * MIN)
                && blockers == 0,
            generation_cleanup_ok: cur.is_some_and(|c| self.now_ms >= c.ready_at.unwrap() + DAY),
            claim_allowed: !live && last.map_or(true, |l| self.now_ms >= l + 6 * 60 * MIN),
        }
    }
    fn gc_allowed(&self, a: &str, p: Uuid, d: &str, seq: u64) -> bool {
        let st = self.status(a, p);
        st.journal_gc_ok
            && st
                .journal_gc_watermarks
                .and_then(|w| w.get(d).copied())
                .is_some_and(|w| w >= seq)
    }
    fn row(s: &FSnap, now: u64) -> SnapshotRow {
        SnapshotRow {
            generation: s.generation,
            state: s.state.clone(),
            created_by: s.created_by.clone(),
            manifest_path: s.manifest_path.clone(),
            manifest_sha256: s.sha.clone(),
            watermarks: s.watermarks.clone(),
            bytes: s.bytes,
            claimed_at: Some(s.claimed.to_string()),
            expires_at: Some(s.expires.to_string()),
            ready_at: s.ready_at.map(|r| r.to_string()),
            expired: s.state == "building" && s.expires <= now,
        }
    }
}

fn path_parts(path: &str) -> Vec<&str> {
    path.split('/').collect()
}

impl CompactionSink for Sink<'_> {
    async fn compaction_status(&self, p: Uuid) -> Result<CompactionStatus, SinkError> {
        let a = self.account_id().await?;
        Ok(self.cloud.lock().unwrap().status(&a, p))
    }
    async fn list_snapshots(&self, p: Uuid) -> Result<Vec<SnapshotRow>, SinkError> {
        let a = self.account_id().await?;
        let c = self.cloud.lock().unwrap();
        Ok(c.snaps
            .iter()
            .filter(|s| s.account == a && s.profile == p)
            .map(|s| Cloud::row(s, c.now_ms))
            .collect())
    }
    async fn list_cursors(&self, p: Uuid) -> Result<Vec<CursorRow>, SinkError> {
        let a = self.account_id().await?;
        let c = self.cloud.lock().unwrap();
        Ok(c.cursors
            .iter()
            .filter(|((x, q, _), _)| x == &a && *q == p)
            .map(|((_, _, d), r)| CursorRow {
                device_id: d.clone(),
                applied: r.applied.clone(),
                snapshot_protocol: r.protocol,
                retired: r.retired,
                active: c.now_ms.saturating_sub(r.updated) <= 30 * DAY,
            })
            .collect())
    }
    async fn report_cursors(&self, p: Uuid, rep: &CursorReport) -> Result<(), SinkError> {
        let a = self.account_id().await?;
        let mut c = self.cloud.lock().unwrap();
        let now = c.now_ms;
        // the reporting device is the one whose own journal is `published_through`; tests
        // report through `report_as`
        let dev = REPORTER.with(|r| r.borrow().clone());
        let e = c.cursors.entry((a, p, dev)).or_default();
        if e.applied != rep.applied
            || e.published != rep.published_through
            || e.protocol != rep.snapshot_protocol
        {
            e.updated = now;
        }
        e.applied = rep.applied.clone();
        e.published = rep.published_through;
        e.protocol = rep.snapshot_protocol;
        e.retired = false;
        Ok(())
    }
    async fn claim_snapshot(&self, p: Uuid, generation: u64) -> Result<(), SinkError> {
        let a = self.account_id().await?;
        let mut c = self.cloud.lock().unwrap();
        let latest = c
            .snaps
            .iter()
            .filter(|s| s.account == a && s.profile == p)
            .map(|s| s.generation)
            .max()
            .unwrap_or(0);
        if generation != latest + 1 {
            return Err(SinkError::Rejected("generation".into()));
        }
        let now = c.now_ms;
        c.snaps.push(FSnap {
            account: a,
            profile: p,
            generation,
            state: "building".into(),
            created_by: String::new(),
            manifest_path: None,
            sha: None,
            watermarks: BTreeMap::new(),
            bytes: 0,
            claimed: now,
            expires: now + 30 * MIN,
            ready_at: None,
        });
        Ok(())
    }
    async fn put_snapshot_object(
        &self,
        path: &str,
        bytes: &[u8],
        _ct: &str,
    ) -> Result<(), SinkError> {
        let a = self.account_id().await?;
        let mut c = self.cloud.lock().unwrap();
        let parts = path_parts(path);
        let ok = parts.len() == 5
            && parts[2] == "snapshots"
            && c.snaps.iter().any(|s| {
                s.account == a
                    && s.profile.to_string() == parts[1]
                    && format!("{:010}", s.generation) == parts[3]
                    && s.state == "building"
                    && s.expires > c.now_ms
            });
        if !ok {
            return Err(SinkError::Forbidden(
                "snapshot object outside a building claim".into(),
            ));
        }
        c.objects.insert((a, path.into()), bytes.to_vec());
        Ok(())
    }
    async fn mark_ready(
        &self,
        p: Uuid,
        generation: u64,
        info: &ReadyInfo,
    ) -> Result<(), SinkError> {
        let (manifest_path, sha, wm, bytes) = (
            &info.manifest_path,
            &info.manifest_sha256,
            &info.watermarks,
            info.bytes,
        );
        let a = self.account_id().await?;
        let mut c = self.cloud.lock().unwrap();
        let now = c.now_ms;
        let Some(i) = c
            .snaps
            .iter()
            .position(|s| s.account == a && s.profile == p && s.generation == generation)
        else {
            return Err(SinkError::Rejected("no claim".into()));
        };
        if c.snaps[i].state != "building" || c.snaps[i].expires <= now {
            return Err(SinkError::Rejected("claim expired or not building".into()));
        }
        let prev = c
            .snaps
            .iter()
            .filter(|s| {
                s.account == a && s.profile == p && s.state == "ready" && s.generation < generation
            })
            .max_by_key(|s| s.generation)
            .map(|s| s.watermarks.clone())
            .unwrap_or_default();
        for (d, w) in &prev {
            if wm.get(d).map_or(true, |x| x < w) {
                return Err(SinkError::Rejected("watermark lowered".into()));
            }
        }
        for (d, w) in wm {
            let lo = prev.get(d).copied().unwrap_or(0);
            if *w <= lo {
                continue;
            }
            let mut evs: Vec<&PublishedEvent> = c
                .events
                .iter()
                .filter(|(x, e)| x == &a && e.profile_id == p && e.device_id.to_string() == *d)
                .map(|(_, e)| e)
                .collect();
            evs.sort_by_key(|e| e.seq_to);
            let straddle = evs.iter().any(|e| {
                (e.seq_from <= lo && e.seq_to > lo) || (e.seq_from <= *w && e.seq_to > *w)
            });
            let inside: Vec<&&PublishedEvent> = evs
                .iter()
                .filter(|e| e.seq_from > lo && e.seq_to <= *w)
                .collect();
            let at_w = inside.iter().filter(|e| e.seq_to == *w).count();
            let mut prev_to = lo;
            let mut breaks = 0;
            for e in &inside {
                if e.seq_from != prev_to + 1 {
                    breaks += 1;
                }
                prev_to = e.seq_to;
            }
            if at_w != 1 || breaks != 0 || straddle {
                return Err(SinkError::Rejected(
                    "watermark is not the end of a contiguous chain".into(),
                ));
            }
            let min_applied = c
                .cursors
                .iter()
                .filter(|((x, q, r), cur)| {
                    x == &a
                        && *q == p
                        && r != d
                        && !cur.retired
                        && cur.protocol >= 1
                        && now.saturating_sub(cur.updated) <= 30 * DAY
                })
                .map(|(_, cur)| cur.applied.get(d).copied().unwrap_or(0))
                .min();
            if min_applied.is_some_and(|m| *w > m) {
                return Err(SinkError::Rejected(
                    "watermark not applied by every active reader".into(),
                ));
            }
        }
        let s = &mut c.snaps[i];
        s.state = "ready".into();
        s.manifest_path = Some(manifest_path.clone());
        s.sha = Some(sha.clone());
        s.watermarks = wm.clone();
        s.bytes = bytes;
        s.ready_at = Some(now);
        Ok(())
    }
    async fn mark_superseded(&self, p: Uuid, generation: u64) -> Result<(), SinkError> {
        let a = self.account_id().await?;
        let mut c = self.cloud.lock().unwrap();
        let ready = c.ready(&a, p);
        let newer = ready.iter().filter(|s| s.generation > generation).count();
        let stable = ready
            .first()
            .is_some_and(|s| c.now_ms >= s.ready_at.unwrap() + DAY);
        if newer < 2 || !stable {
            return Err(SinkError::Rejected(
                "still current/previous or not stable".into(),
            ));
        }
        if let Some(s) = c.snaps.iter_mut().find(|s| {
            s.account == a && s.profile == p && s.generation == generation && s.state == "ready"
        }) {
            s.state = "superseded".into();
        }
        Ok(())
    }
    async fn delete_snapshot_row(&self, p: Uuid, generation: u64) -> Result<bool, SinkError> {
        let a = self.account_id().await?;
        let mut c = self.cloud.lock().unwrap();
        let now = c.now_ms;
        let before = c.snaps.len();
        c.snaps.retain(|s| {
            !(s.account == a
                && s.profile == p
                && s.generation == generation
                && (s.state == "superseded" || (s.state == "building" && s.expires <= now)))
        });
        Ok(c.snaps.len() < before)
    }
    async fn list_objects(&self, prefix: &str) -> Result<Vec<ObjectInfo>, SinkError> {
        let a = self.account_id().await?;
        let c = self.cloud.lock().unwrap();
        let mut v: Vec<ObjectInfo> = c
            .objects
            .iter()
            .filter(|((x, path), _)| x == &a && path.starts_with(prefix))
            .map(|((_, path), b)| ObjectInfo {
                path: path.clone(),
                bytes: b.len() as u64,
            })
            .collect();
        v.sort_by(|x, y| x.path.cmp(&y.path));
        Ok(v)
    }
    async fn delete_objects(&self, paths: &[String]) -> Result<usize, SinkError> {
        let a = self.account_id().await?;
        let mut c = self.cloud.lock().unwrap();
        let mut n = 0;
        for path in paths {
            let parts = path_parts(path);
            let allowed = if parts.len() >= 3 && parts[2] == "snapshots" {
                !c.snaps.iter().any(|s| {
                    s.account == a
                        && s.profile.to_string() == parts[1]
                        && format!("{:010}", s.generation) == parts[3]
                        && s.state == "ready"
                })
            } else if parts.len() == 7 && parts[2] == "journal" {
                let p = Uuid::parse_str(parts[1]).unwrap();
                let first: u64 = parts[6].trim_end_matches(".jsonl").parse().unwrap();
                c.gc_allowed(&a, p, parts[3], first)
            } else {
                true
            };
            if allowed && c.objects.remove(&(a.clone(), path.clone())).is_some() {
                n += 1;
            }
        }
        Ok(n)
    }
    async fn gc_events(&self, p: Uuid, d: &str, through: u64) -> Result<u64, SinkError> {
        let a = self.account_id().await?;
        let mut c = self.cloud.lock().unwrap();
        if !c.gc_allowed(&a, p, d, through) {
            return Err(SinkError::Rejected("journal GC not allowed".into()));
        }
        let before = c.events.len();
        c.events.retain(|(x, e)| {
            !(x == &a && e.profile_id == p && e.device_id.to_string() == d && e.seq_to <= through)
        });
        Ok((before - c.events.len()) as u64)
    }
}

thread_local! {
    /// Which device the fake's `report_cursors` records (the real sink knows its own device).
    pub static REPORTER: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

pub async fn report_as(w: &World, d: &mut Dev) {
    REPORTER.with(|r| *r.borrow_mut() = d.src.device_id.to_string());
    let sink = w.sink();
    report_cursors(&mut d.db, &sink, w.profile).await.unwrap();
}
