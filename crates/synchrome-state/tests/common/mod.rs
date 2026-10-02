//! Shared harness: an in-memory cloud with per-account isolation, a fake browser with
//! chrome.bookmarks semantics (create appends, non-recursive folder removal…) and simulated
//! devices that capture, sync, plan, apply and reconcile like the host + extension do.
#![allow(dead_code)]
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use synchrome_state::*;
use synchrome_sync::bookmarks::ChromiumNode;
use uuid::Uuid;

// ------------------------------------------------------------------ fake cloud
#[derive(Default)]
pub struct Cloud {
    pub objects: HashMap<(String, String), Vec<u8>>,
    pub events: Vec<(String, PublishedEvent)>,
    pub hidden: HashSet<u64>, // seq_to of events hidden from listings (simulates a gap)
}

pub struct Sink<'a> {
    pub cloud: &'a Mutex<Cloud>,
    pub account: String,
    pub offline: bool,
    pub mappings: HashSet<Uuid>,
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
        Dev {
            db: StateDb::open_in_memory().unwrap(),
            br: Browser::new(),
            src: Source {
                mapping_id: m,
                account_id: self.account.clone(),
                device_id: Uuid::new_v4(),
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
