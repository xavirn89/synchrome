//! Capture → outbox → publication, against an in-memory store that behaves like Synchrome
//! Cloud (per-account isolation, upsert objects, unique store events) and can fail on demand.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use synchrome_state::*;
use synchrome_sync::bookmarks::{render_tree, BookmarkEvent, ChromiumNode, Kind, Root};
use synchrome_sync::{Op, OpKind};
use uuid::Uuid;

const T0: u64 = 1_790_900_000_000; // 2026-10-02

#[derive(Default)]
struct Cloud {
    objects: HashMap<(String, String), Vec<u8>>, // (account, path) -> bytes
    events: HashSet<(String, String, u64)>,      // (account, profile, seq_to)
    puts: usize,
}

struct Sink<'a> {
    cloud: &'a Mutex<Cloud>,
    account: Option<String>,
    offline: bool,
    fail_record_once: Mutex<bool>,
    mappings: HashSet<Uuid>,
}

impl<'a> Sink<'a> {
    fn new(cloud: &'a Mutex<Cloud>, account: &str, mapping: Uuid) -> Self {
        Sink {
            cloud,
            account: Some(account.into()),
            offline: false,
            fail_record_once: Mutex::new(false),
            mappings: [mapping].into(),
        }
    }
    fn acct(&self) -> Result<String, SinkError> {
        if self.offline {
            return Err(SinkError::Offline("down".into()));
        }
        self.account.clone().ok_or(SinkError::NotSignedIn)
    }
}

impl JournalSink for Sink<'_> {
    async fn account_id(&self) -> Result<String, SinkError> {
        self.acct()
    }
    async fn mapping(&self, m: Uuid) -> Result<MappingCheck, SinkError> {
        self.acct()?;
        Ok(if self.mappings.contains(&m) {
            MappingCheck::Active
        } else {
            MappingCheck::Missing
        })
    }
    async fn journal_devices(&self, profile: Uuid) -> Result<Vec<String>, SinkError> {
        let a = self.acct()?;
        let prefix = format!("profiles/{profile}/journal/");
        let c = self.cloud.lock().unwrap();
        let mut d: Vec<String> = c
            .objects
            .keys()
            .filter(|(acc, p)| acc == &a && p.starts_with(&prefix))
            .map(|(_, p)| p[prefix.len()..].split('/').next().unwrap().to_string())
            .collect();
        d.dedup();
        Ok(d)
    }
    async fn put_chunk(&self, path: &str, body: &[u8]) -> Result<(), SinkError> {
        let a = self.acct()?;
        let mut c = self.cloud.lock().unwrap();
        c.puts += 1;
        c.objects.insert((a, path.into()), body.to_vec());
        Ok(())
    }
    async fn record_event(&self, ev: &PublishedEvent) -> Result<(), SinkError> {
        let a = self.acct()?;
        let mut f = self.fail_record_once.lock().unwrap();
        if *f {
            *f = false;
            return Err(SinkError::Offline("connection reset".into()));
        }
        self.cloud
            .lock()
            .unwrap()
            .events
            .insert((a, ev.profile_id.to_string(), ev.seq_to));
        Ok(())
    }
}

struct Fx {
    src: Source,
    account: String,
}

fn fx() -> Fx {
    let account = Uuid::new_v4().to_string();
    Fx {
        src: Source {
            mapping_id: Uuid::new_v4(),
            account_id: account.clone(),
            device_id: Uuid::new_v4(),
            profile_id: Uuid::new_v4(),
            identity: "helium|/tmp/x|Default".into(),
        },
        account,
    }
}

fn node(id: &str, parent: &str, index: u32, title: &str, url: Option<&str>) -> ChromiumNode {
    ChromiumNode {
        id: id.into(),
        parent_id: Some(parent.into()),
        index: Some(index),
        title: title.into(),
        url: url.map(Into::into),
        date_added: url.map(|_| 1_790_000_000_000.0),
        ..Default::default()
    }
}
fn roots() -> Vec<ChromiumNode> {
    let mut b = node("1", "0", 0, "Bookmarks bar", None);
    b.folder_type = Some("bookmarks-bar".into());
    let mut o = node("2", "0", 1, "Other bookmarks", None);
    o.folder_type = Some("other".into());
    let mut m = node("3", "0", 2, "Mobile bookmarks", None);
    m.folder_type = Some("mobile".into());
    vec![b, o, m]
}
fn known() -> Vec<ChromiumNode> {
    let mut v = roots();
    v.extend([
        node("10", "1", 0, "Synchrome Test", None),
        node("11", "10", 0, "Folder A", None),
        node("12", "11", 0, "Bookmark 1", Some("https://example.com/1")),
        node("13", "11", 1, "Bookmark 2", Some("https://example.com/2")),
        node("14", "10", 1, "Folder B", None),
        node("15", "14", 0, "Bookmark 3", Some("https://example.com/3")),
    ]);
    v
}

/// Rebuild the Synchrome tree from what the CLOUD holds for one account (per-field LWW by
/// op order within this single device's journal).
fn cloud_tree(cloud: &Mutex<Cloud>, account: &str, profile: Uuid) -> Vec<(usize, String)> {
    let c = cloud.lock().unwrap();
    let prefix = format!("profiles/{profile}/journal/");
    let mut ops: Vec<Op> = c
        .objects
        .iter()
        .filter(|((a, p), _)| a == account && p.starts_with(&prefix))
        .flat_map(|(_, b)| {
            String::from_utf8(b.clone())
                .unwrap()
                .lines()
                .map(|l| Op::from_line(l).unwrap())
                .collect::<Vec<_>>()
        })
        .collect();
    ops.sort_by_key(|o| o.seq);
    let seqs: Vec<u64> = ops.iter().map(|o| o.seq).collect();
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0] + 1),
        "no gap, no duplicate seq"
    );
    let mut ent: BTreeMap<String, BTreeMap<String, serde_json::Value>> = BTreeMap::new();
    let mut dead: HashSet<String> = HashSet::new();
    for o in ops {
        match o.op {
            OpKind::Set => {
                dead.remove(&o.id);
                ent.entry(o.id).or_default().extend(o.fields.unwrap());
            }
            OpKind::Delete => {
                dead.insert(o.id);
            }
        }
    }
    let get = |m: &BTreeMap<String, serde_json::Value>, k: &str| {
        m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    let mut out = Vec::new();
    let mut stack = vec![("root:bar".to_string(), 0usize)];
    while let Some((p, d)) = stack.pop() {
        let mut kids: Vec<(&String, &BTreeMap<String, serde_json::Value>)> = ent
            .iter()
            .filter(|(id, f)| !dead.contains(*id) && get(f, "parentId") == p)
            .collect();
        kids.sort_by_key(|(id, f)| (get(f, "pos"), (*id).clone()));
        for (id, f) in kids.iter().rev() {
            stack.push(((*id).clone(), d + 1));
            let _ = f;
        }
        if d > 0 {
            out.push((d - 1, get(&ent[&p], "title")));
        }
    }
    out
}

fn tr(v: &[(usize, &str)]) -> Vec<(usize, String)> {
    v.iter().map(|(d, t)| (*d, t.to_string())).collect()
}

fn applied(o: CaptureOutcome) -> usize {
    match o {
        CaptureOutcome::Applied { ops, .. } => ops,
        other => panic!("expected Applied, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_and_known_snapshot_reach_the_cloud_once() {
    let f = fx();
    let cloud = Mutex::new(Cloud::default());
    let sink = Sink::new(&cloud, &f.account, f.src.mapping_id);
    let mut db = StateDb::open_in_memory().unwrap();
    let st = |db: &StateDb| db.status(f.src.profile_id, f.src.mapping_id).unwrap();
    assert_eq!(st(&db).state, BookmarksSyncState::NotSynced);

    // empty tree: snapshot recorded, nothing to upload
    assert_eq!(
        applied(db.capture_snapshot(&f.src, "e1", 0, &roots(), T0).unwrap()),
        0
    );
    assert_eq!(st(&db).state, BookmarksSyncState::UpToDate);
    assert_eq!(
        publish(&mut db, &sink, f.src.profile_id, "h", 1)
            .await
            .unwrap(),
        PublishOutcome::NothingToDo
    );

    // real tree
    assert_eq!(
        applied(db.capture_snapshot(&f.src, "e1", 0, &known(), T0).unwrap()),
        6
    );
    let s = st(&db);
    assert_eq!(
        (s.state, s.bookmarks, s.folders, s.pending),
        (BookmarksSyncState::PendingChanges, 3, 3, 6)
    );
    let out = publish(&mut db, &sink, f.src.profile_id, "h", 2)
        .await
        .unwrap();
    assert_eq!(
        out,
        PublishOutcome::Published {
            chunks: 1,
            through: 6
        }
    );
    let s = st(&db);
    assert_eq!(
        (s.state, s.pending, s.last_upload_at),
        (BookmarksSyncState::UpToDate, 0, Some(2))
    );
    let want = tr(&[
        (0, "Synchrome Test"),
        (1, "Folder A"),
        (2, "Bookmark 1"),
        (2, "Bookmark 2"),
        (1, "Folder B"),
        (2, "Bookmark 3"),
    ]);
    assert_eq!(cloud_tree(&cloud, &f.account, f.src.profile_id), want);

    // duplicate snapshot (same epoch, then a new epoch = extension restart): no ops, no upload
    assert_eq!(
        applied(db.capture_snapshot(&f.src, "e1", 0, &known(), T0).unwrap()),
        0
    );
    assert_eq!(
        applied(db.capture_snapshot(&f.src, "e2", 0, &known(), T0).unwrap()),
        0
    );
    assert_eq!(
        publish(&mut db, &sink, f.src.profile_id, "h", 3)
            .await
            .unwrap(),
        PublishOutcome::NothingToDo
    );
    assert_eq!(cloud.lock().unwrap().puts, 1);
    // chromium ids never reach the cloud
    let c = cloud.lock().unwrap();
    let body = String::from_utf8(c.objects.values().next().unwrap().clone()).unwrap();
    assert!(!body.contains("\"12\"") && !body.contains("chromium"));
    assert!(c.objects.keys().next().unwrap().1.starts_with(&format!(
        "profiles/{}/journal/{}/2026/10/00000001.jsonl",
        f.src.profile_id, f.src.device_id
    )));
}

#[tokio::test]
async fn events_publish_only_deltas_and_duplicates_are_ignored() {
    let f = fx();
    let cloud = Mutex::new(Cloud::default());
    let sink = Sink::new(&cloud, &f.account, f.src.mapping_id);
    let mut db = StateDb::open_in_memory().unwrap();
    db.capture_snapshot(&f.src, "e1", 0, &known(), T0).unwrap();
    publish(&mut db, &sink, f.src.profile_id, "h", 1)
        .await
        .unwrap();

    let evs = vec![
        (
            1,
            BookmarkEvent::Created {
                id: "20".into(),
                node: node("20", "11", 2, "Bookmark 4", Some("https://example.com/4")),
            },
        ),
        (
            2,
            BookmarkEvent::Changed {
                id: "12".into(),
                title: Some("Bookmark One".into()),
                url: None,
            },
        ),
        (
            3,
            BookmarkEvent::Changed {
                id: "13".into(),
                title: None,
                url: Some("https://example.org/2".into()),
            },
        ),
        (
            4,
            BookmarkEvent::Moved {
                id: "15".into(),
                parent_id: "11".into(),
                index: 0,
            },
        ),
        (
            5,
            BookmarkEvent::Reordered {
                id: "11".into(),
                child_ids: vec!["15".into(), "20".into(), "12".into(), "13".into()],
            },
        ),
        (6, BookmarkEvent::Removed { id: "14".into() }),
    ];
    assert_eq!(
        applied(db.capture_events(&f.src, "e1", &evs, T0 + 10).unwrap()),
        6
    );
    assert_eq!(db.pending(f.src.profile_id).unwrap(), 6);
    // redelivery of the same batch, and of a prefix of it
    assert!(matches!(
        db.capture_events(&f.src, "e1", &evs, T0 + 11).unwrap(),
        CaptureOutcome::Duplicate { pending: 6 }
    ));
    assert!(matches!(
        db.capture_events(&f.src, "e1", &evs[..2], T0 + 11).unwrap(),
        CaptureOutcome::Duplicate { .. }
    ));
    let out = publish(&mut db, &sink, f.src.profile_id, "h", 5)
        .await
        .unwrap();
    assert_eq!(
        out,
        PublishOutcome::Published {
            chunks: 1,
            through: 12
        }
    );
    assert_eq!(
        cloud_tree(&cloud, &f.account, f.src.profile_id),
        tr(&[
            (0, "Synchrome Test"),
            (1, "Folder A"),
            (2, "Bookmark 3"),
            (2, "Bookmark 4"),
            (2, "Bookmark One"),
            (2, "Bookmark 2"),
        ])
    );
    // the local tree matches the cloud tree
    let local: Vec<(usize, String)> = render_tree(&db.tree(f.src.profile_id).unwrap())
        .into_iter()
        .filter(|(r, ..)| *r == Root::Bar)
        .map(|(_, d, _, t, _)| (d, t))
        .collect();
    assert_eq!(local, cloud_tree(&cloud, &f.account, f.src.profile_id));
    // the delta went up as a second, separate object holding only the 6 new ops
    let c = cloud.lock().unwrap();
    assert_eq!(c.objects.len(), 2);
    let delta = c
        .objects
        .iter()
        .find(|((_, p), _)| p.ends_with("00000007.jsonl"))
        .map(|(_, b)| String::from_utf8(b.clone()).unwrap())
        .expect("delta chunk starts at seq 7");
    assert_eq!(delta.lines().count(), 6);
    // the delete of Folder B tombstoned it explicitly (Bookmark 3 had moved out before)
    let all: String = c
        .objects
        .values()
        .map(|b| String::from_utf8(b.clone()).unwrap())
        .collect();
    assert_eq!(all.matches("\"op\":\"delete\"").count(), 1);
}

#[tokio::test]
async fn unknown_reference_rolls_back_and_asks_for_a_snapshot() {
    let f = fx();
    let mut db = StateDb::open_in_memory().unwrap();
    db.capture_snapshot(&f.src, "e1", 0, &known(), T0).unwrap();
    let evs = vec![
        (
            1,
            BookmarkEvent::Changed {
                id: "12".into(),
                title: Some("x".into()),
                url: None,
            },
        ),
        (
            2,
            BookmarkEvent::Changed {
                id: "999".into(),
                title: Some("y".into()),
                url: None,
            },
        ),
    ];
    assert!(matches!(
        db.capture_events(&f.src, "e1", &evs, T0).unwrap(),
        CaptureOutcome::ResyncRequired(_)
    ));
    assert_eq!(
        db.pending(f.src.profile_id).unwrap(),
        6,
        "nothing from the batch was kept"
    );
    // the cursor did not advance either: after the snapshot, a redelivered n=1 is skipped
    // because the snapshot covered it
    db.capture_snapshot(&f.src, "e1", 2, &known(), T0).unwrap();
    assert!(matches!(
        db.capture_events(&f.src, "e1", &evs[..1], T0).unwrap(),
        CaptureOutcome::Duplicate { .. }
    ));
}

#[tokio::test]
async fn app_restart_continues_from_the_durable_state() {
    let f = fx();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let cloud = Mutex::new(Cloud::default());
    let sink = Sink::new(&cloud, &f.account, f.src.mapping_id);
    {
        let mut db = StateDb::open(&path).unwrap();
        db.capture_snapshot(&f.src, "e1", 0, &known(), T0).unwrap();
        publish(&mut db, &sink, f.src.profile_id, "h", 1)
            .await
            .unwrap();
        db.capture_events(
            &f.src,
            "e1",
            &[(1, BookmarkEvent::Removed { id: "15".into() })],
            T0 + 1,
        )
        .unwrap();
    } // process exits with 1 op pending
    let mut db = StateDb::open(&path).unwrap();
    assert_eq!(db.pending(f.src.profile_id).unwrap(), 1);
    // redelivery after restart is still recognised; a new epoch snapshot changes nothing
    assert!(matches!(
        db.capture_events(
            &f.src,
            "e1",
            &[(1, BookmarkEvent::Removed { id: "15".into() })],
            T0 + 2
        )
        .unwrap(),
        CaptureOutcome::Duplicate { .. }
    ));
    let mut after = known();
    after.retain(|n| n.id != "15");
    assert_eq!(
        applied(
            db.capture_snapshot(&f.src, "e2", 0, &after, T0 + 3)
                .unwrap()
        ),
        0
    );
    let out = publish(&mut db, &sink, f.src.profile_id, "h2", 2)
        .await
        .unwrap();
    assert_eq!(
        out,
        PublishOutcome::Published {
            chunks: 1,
            through: 7
        }
    );
    assert_eq!(cloud_tree(&cloud, &f.account, f.src.profile_id).len(), 5);
}

#[tokio::test]
async fn offline_keeps_everything_and_retry_is_idempotent() {
    let f = fx();
    let cloud = Mutex::new(Cloud::default());
    let mut sink = Sink::new(&cloud, &f.account, f.src.mapping_id);
    let mut db = StateDb::open_in_memory().unwrap();
    db.capture_snapshot(&f.src, "e1", 0, &known(), T0).unwrap();
    sink.offline = true;
    let out = publish(&mut db, &sink, f.src.profile_id, "h", 1)
        .await
        .unwrap();
    assert_eq!(
        out,
        PublishOutcome::Failed {
            kind: "offline".into(),
            chunks: 0
        }
    );
    let s = db.status(f.src.profile_id, f.src.mapping_id).unwrap();
    assert_eq!((s.state, s.pending), (BookmarksSyncState::Offline, 6));
    assert!(s.error.unwrap().contains("unreachable"));
    // more changes while offline
    db.capture_events(
        &f.src,
        "e1",
        &[(
            1,
            BookmarkEvent::Changed {
                id: "12".into(),
                title: Some("B1'".into()),
                url: None,
            },
        )],
        T0 + 5,
    )
    .unwrap();
    // network back, but the connection drops after the upload and before the checkpoint
    sink.offline = false;
    *sink.fail_record_once.lock().unwrap() = true;
    let out = publish(&mut db, &sink, f.src.profile_id, "h", 2)
        .await
        .unwrap();
    assert_eq!(
        out,
        PublishOutcome::Failed {
            kind: "offline".into(),
            chunks: 0
        }
    );
    assert_eq!(db.pending(f.src.profile_id).unwrap(), 7);
    let first = cloud
        .lock()
        .unwrap()
        .objects
        .values()
        .next()
        .unwrap()
        .clone();
    // retry: same object, same bytes, event deduplicated
    let out = publish(&mut db, &sink, f.src.profile_id, "h", 3)
        .await
        .unwrap();
    assert_eq!(
        out,
        PublishOutcome::Published {
            chunks: 1,
            through: 7
        }
    );
    let c = cloud.lock().unwrap();
    assert_eq!(c.objects.len(), 1);
    assert_eq!(c.objects.values().next().unwrap(), &first);
    assert_eq!(c.events.len(), 1);
    drop(c);
    let s = db.status(f.src.profile_id, f.src.mapping_id).unwrap();
    assert_eq!(
        (s.state, s.pending, s.error),
        (BookmarksSyncState::UpToDate, 0, None)
    );
}

#[tokio::test]
async fn deleted_mapping_other_account_and_sign_out_block_without_losing_data() {
    let f = fx();
    let cloud = Mutex::new(Cloud::default());
    let mut db = StateDb::open_in_memory().unwrap();
    db.capture_snapshot(&f.src, "e1", 0, &known(), T0).unwrap();
    // mapping deleted
    let mut sink = Sink::new(&cloud, &f.account, Uuid::new_v4());
    assert_eq!(
        publish(&mut db, &sink, f.src.profile_id, "h", 1)
            .await
            .unwrap(),
        PublishOutcome::Blocked {
            kind: "mapping_deleted".into()
        }
    );
    assert_eq!(
        db.status(f.src.profile_id, f.src.mapping_id).unwrap().state,
        BookmarksSyncState::Error
    );
    // user B signed in on the app: A's captured data is never uploaded into B's account
    sink.mappings.insert(f.src.mapping_id);
    sink.account = Some(Uuid::new_v4().to_string());
    assert_eq!(
        publish(&mut db, &sink, f.src.profile_id, "h", 2)
            .await
            .unwrap(),
        PublishOutcome::Blocked {
            kind: "other_account".into()
        }
    );
    // signed out (expired session that could not be refreshed)
    sink.account = None;
    assert_eq!(
        publish(&mut db, &sink, f.src.profile_id, "h", 3)
            .await
            .unwrap(),
        PublishOutcome::Blocked {
            kind: "not_signed_in".into()
        }
    );
    assert!(cloud.lock().unwrap().objects.is_empty());
    // signed in again as A (session refreshed): everything goes up
    sink.account = Some(f.account.clone());
    assert_eq!(
        publish(&mut db, &sink, f.src.profile_id, "h", 4)
            .await
            .unwrap(),
        PublishOutcome::Published {
            chunks: 1,
            through: 6
        }
    );
    // B's view of the store holds nothing of A
    let b = Sink::new(&cloud, &Uuid::new_v4().to_string(), f.src.mapping_id);
    assert!(b
        .journal_devices(f.src.profile_id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn one_source_per_profile_and_remote_data_gate() {
    let f = fx();
    let cloud = Mutex::new(Cloud::default());
    let mut db = StateDb::open_in_memory().unwrap();
    db.capture_snapshot(&f.src, "e1", 0, &known(), T0).unwrap();
    // a second local profile mapped to the same Synchrome profile on this device
    let other = Source {
        mapping_id: Uuid::new_v4(),
        identity: "chrome|/x|Profile 3".into(),
        ..f.src.clone()
    };
    assert!(matches!(
        db.capture_snapshot(&other, "e9", 0, &known(), T0),
        Err(CaptureError::SourceConflict)
    ));
    assert_eq!(
        db.status(f.src.profile_id, other.mapping_id).unwrap().state,
        BookmarksSyncState::Error
    );
    // after the first mapping is released (unmapped), the other may claim; its nodes pair
    db.release_source(f.src.mapping_id).unwrap();
    assert_eq!(
        applied(db.capture_snapshot(&other, "e9", 0, &known(), T0).unwrap()),
        0
    );

    // remote data gate: another device already published this profile
    let f2 = fx();
    let mut sink = Sink::new(&cloud, &f2.account, f2.src.mapping_id);
    cloud.lock().unwrap().objects.insert(
        (
            f2.account.clone(),
            format!(
                "profiles/{}/journal/{}/2026/10/00000001.jsonl",
                f2.src.profile_id,
                Uuid::new_v4()
            ),
        ),
        b"{}\n".to_vec(),
    );
    db.capture_snapshot(&f2.src, "e1", 0, &known(), T0).unwrap();
    assert_eq!(
        publish(&mut db, &sink, f2.src.profile_id, "h", 1)
            .await
            .unwrap(),
        PublishOutcome::Blocked {
            kind: "remote_data".into()
        }
    );
    sink.offline = false;
    assert_eq!(db.pending(f2.src.profile_id).unwrap(), 6);
}

#[tokio::test]
async fn large_batches_rotate_chunks_and_keep_order() {
    let f = fx();
    let cloud = Mutex::new(Cloud::default());
    let sink = Sink::new(&cloud, &f.account, f.src.mapping_id);
    let mut db = StateDb::open_in_memory().unwrap();
    let mut v = roots();
    v.push(node("100", "2", 0, "Big", None));
    for i in 0..3000u32 {
        let url = format!("https://example.com/item/{i}/{}", "p".repeat(80));
        v.push(node(
            &(1000 + i).to_string(),
            "100",
            i,
            &format!("Item {i}"),
            Some(&url),
        ));
    }
    assert_eq!(
        applied(db.capture_snapshot(&f.src, "e1", 0, &v, T0).unwrap()),
        3001
    );
    let evs: Vec<(u64, BookmarkEvent)> = (0..1000u64)
        .map(|i| {
            (
                i + 1,
                BookmarkEvent::Changed {
                    id: (1000 + i).to_string(),
                    title: Some(format!("Renamed {i}")),
                    url: None,
                },
            )
        })
        .collect();
    assert_eq!(
        applied(db.capture_events(&f.src, "e1", &evs, T0 + 1).unwrap()),
        1000
    );
    let PublishOutcome::Published { chunks, through } =
        publish(&mut db, &sink, f.src.profile_id, "h", 1)
            .await
            .unwrap()
    else {
        panic!()
    };
    assert!(chunks >= 3, "rotated at 256 KB, got {chunks}");
    assert_eq!(through, 4001);
    let c = cloud.lock().unwrap();
    assert!(c.objects.values().all(|b| b.len() <= 256 * 1024));
    drop(c);
    // cloud_tree checks contiguous seq across chunks; the folder lives under "other"
    let _ = cloud_tree(&cloud, &f.account, f.src.profile_id);
    let t = db.tree(f.src.profile_id).unwrap();
    assert_eq!(t.counts(), (3000, 1));
    let titles: Vec<String> = render_tree(&t)
        .into_iter()
        .filter(|(_, d, k, ..)| *d == 1 && *k == Kind::Url)
        .map(|(_, _, _, t, _)| t)
        .take(3)
        .collect();
    assert_eq!(titles, vec!["Renamed 0", "Renamed 1", "Renamed 2"]);
    // closed, fully published chunks were pruned from the outbox; nothing pending
    assert_eq!(db.pending(f.src.profile_id).unwrap(), 0);
    assert!(
        db.chunk_bodies(f.src.profile_id).unwrap().is_empty(),
        "published chunks pruned"
    );
}

#[tokio::test]
async fn lease_prevents_two_publishers() {
    let f = fx();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let cloud = Mutex::new(Cloud::default());
    let sink = Sink::new(&cloud, &f.account, f.src.mapping_id);
    let mut a = StateDb::open(&path).unwrap();
    a.capture_snapshot(&f.src, "e1", 0, &known(), T0).unwrap();
    // simulate a crashed publisher holding a fresh lease
    let mut b = StateDb::open(&path).unwrap();
    b.capture_events(
        &f.src,
        "e1",
        &[(1, BookmarkEvent::Removed { id: "15".into() })],
        T0,
    )
    .unwrap();
    {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.execute(
            "INSERT INTO lease (profile_id, holder, expires_at) VALUES (?1, 'crashed', 1000)",
            [f.src.profile_id.to_string()],
        )
        .unwrap();
    }
    assert_eq!(
        publish(&mut a, &sink, f.src.profile_id, "app", 999)
            .await
            .unwrap(),
        PublishOutcome::Busy
    );
    // lease expired: next publisher takes over
    assert_eq!(
        publish(&mut a, &sink, f.src.profile_id, "app", 1001)
            .await
            .unwrap(),
        PublishOutcome::Published {
            chunks: 1,
            through: 7
        }
    );
}
