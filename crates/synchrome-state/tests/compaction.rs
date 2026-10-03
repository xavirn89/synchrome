//! Slice 13: snapshot compaction, journal GC one generation behind, snapshot bootstrap and the
//! merge convergence fix, against a fake cloud that implements the migration-0005 contract
//! (claims, server-side watermark validation, GC rule, blockers, protected ready objects,
//! database clock).

mod common;
use common::*;
use serde_json::{json, Value};
use synchrome_state::compaction::*;
use synchrome_state::*;
use synchrome_sync::{Op, OpKind};
use uuid::Uuid;

const X: &str = "00000000-0000-4000-8000-00000000000a";
const Y: &str = "00000000-0000-4000-8000-00000000000b";
const EXT: &str = "aaaabbbbccccddddeeeeffffgggghhhh";

/// A synthetic source device publishing hand-made ops (other collections, exact HLCs).
struct Src {
    id: &'static str,
    seq: u64,
}

fn hlc(ms: u64, dev: &str) -> String {
    format!("{ms:013}-0000-{dev}")
}

fn publish(w: &World, src: &mut Src, ops: &[(u64, &str, &str, Option<Value>)]) {
    let p = w.profile;
    let first = src.seq + 1;
    let mut body = String::new();
    let mut last = String::new();
    for (ms, col, id, fields) in ops {
        src.seq += 1;
        let op = Op {
            seq: src.seq,
            hlc: hlc(*ms, src.id),
            col: col.to_string(),
            id: id.to_string(),
            op: if fields.is_some() {
                OpKind::Set
            } else {
                OpKind::Delete
            },
            fields: fields.clone().map(|f| serde_json::from_value(f).unwrap()),
            by: src.id.to_string(),
        };
        last = op.hlc.clone();
        body.push_str(&op.to_line());
        body.push('\n');
    }
    let path = format!("profiles/{p}/journal/{}/2026/10/{first:08}.jsonl", src.id);
    let mut c = w.cloud.lock().unwrap();
    c.objects
        .insert((w.account.clone(), path.clone()), body.into_bytes());
    c.events.push((
        w.account.clone(),
        PublishedEvent {
            profile_id: p,
            device_id: Uuid::parse_str(src.id).unwrap(),
            path,
            seq_from: first,
            seq_to: src.seq,
            hlc: last,
        },
    ));
}

/// Ops of every non-bookmark collection, including concurrent deletes around a set.
fn other_collections(w: &World, x: &mut Src, y: &mut Src, base: u64) {
    let pref = "intl.selected_languages";
    let setting = format!("{EXT}/local/theme");
    publish(
        w,
        x,
        &[
            (
                base + 1,
                "extensions",
                EXT,
                Some(
                    json!({"name": "Dummy", "version": "1.0", "source": "webstore", "enabled": true}),
                ),
            ),
            (
                base + 2,
                "extension_settings",
                &setting,
                Some(json!({"value": "dark", "ver": "1.0"})),
            ),
            (
                base + 3,
                "browser_preferences",
                pref,
                Some(json!({"value": "es,en", "bk": "helium", "maj": 154})),
            ),
            (base + 5, "extension_settings", &setting, None),
            (base + 5, "extensions", EXT, None),
        ],
    );
    publish(
        w,
        y,
        &[
            (
                base + 6,
                "extension_settings",
                &setting,
                Some(json!({"value": "light", "ver": "1.0"})),
            ),
            (base + 6, "extensions", EXT, Some(json!({"enabled": false}))),
            (
                base + 6,
                "browser_preferences",
                pref,
                Some(json!({"value": "fr", "bk": "helium", "maj": 154})),
            ),
            (
                base + 8,
                "browser_preferences",
                "download.prompt_for_download",
                Some(json!({"value": true, "bk": "chrome", "maj": 154})),
            ),
        ],
    );
    publish(
        w,
        x,
        &[
            (base + 7, "extension_settings", &setting, None),
            (base + 7, "extensions", EXT, None),
            (
                base + 9,
                "browser_preferences",
                "download.prompt_for_download",
                None,
            ),
        ],
    );
}

fn shared(d: &Dev, w: &World) -> std::collections::BTreeMap<String, Vec<String>> {
    d.db.shared_state(w.profile).unwrap()
}

async fn compact_now(w: &World, d: &mut Dev) -> CompactionOutcome {
    let policy = Policy {
        force: true,
        ..Default::default()
    };
    compact(
        &mut d.db,
        &w.sink(),
        w.profile,
        &d.src.device_id.to_string(),
        policy,
        1,
    )
    .await
    .unwrap()
}

fn advance(w: &World, ms: u64) {
    w.cloud.lock().unwrap().now_ms += ms;
}

fn events_of(w: &World, dev: &str) -> Vec<(u64, u64)> {
    let c = w.cloud.lock().unwrap();
    let mut v: Vec<(u64, u64)> = c
        .events
        .iter()
        .filter(|(_, e)| e.device_id.to_string() == dev)
        .map(|(_, e)| (e.seq_from, e.seq_to))
        .collect();
    v.sort();
    v
}

/// A and B with a real bookmarks history plus X/Y journals of the other collections.
async fn history() -> (World, Dev, Dev, Src, Src) {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 512 * 1024).await;
    let f = b.br.find("Folder A");
    let n = b.br.find("Bookmark 3");
    b.br.move_to(&n, &f, None);
    b.br.nodes.get_mut(&f).unwrap().title = "Folder A renamed".into();
    w.local(&mut b).await;
    w.remote(&mut a, 512 * 1024).await;
    let gone = a.br.find("Bookmark 2");
    a.br.remove(&gone, false).unwrap();
    w.local(&mut a).await;
    w.remote(&mut b, 512 * 1024).await;
    let mut x = Src { id: X, seq: 0 };
    let mut y = Src { id: Y, seq: 0 };
    other_collections(&w, &mut x, &mut y, 1_790_000_000_000);
    w.remote(&mut a, 512 * 1024).await;
    w.remote(&mut b, 512 * 1024).await;
    (w, a, b, x, y)
}

// ------------------------------------------------------------------ convergence fix

#[tokio::test]
async fn merges_converge_whatever_the_delivery_order_of_concurrent_deletes() {
    let mut w = World::new();
    let mut inc = w.device(); // pulls after every publication
    let mut all = w.device(); // pulls once at the end
    let mut x = Src { id: X, seq: 0 };
    let mut y = Src { id: Y, seq: 0 };
    let base = 1_790_000_000_000;
    let setting = format!("{EXT}/local/k");
    publish(
        &w,
        &mut x,
        &[
            (
                base + 1,
                "extension_settings",
                &setting,
                Some(json!({"value": 1, "ver": "1"})),
            ),
            (
                base + 1,
                "browser_preferences",
                "intl.selected_languages",
                Some(json!({"value": "es", "bk": "helium", "maj": 154})),
            ),
            (
                base + 1,
                "extensions",
                EXT,
                Some(json!({"name": "E", "enabled": true})),
            ),
            (base + 5, "extension_settings", &setting, None),
            (
                base + 5,
                "browser_preferences",
                "intl.selected_languages",
                None,
            ),
            (base + 5, "extensions", EXT, None),
        ],
    );
    w.remote(&mut inc, 512 * 1024).await;
    publish(
        &w,
        &mut y,
        &[
            (
                base + 6,
                "extension_settings",
                &setting,
                Some(json!({"value": 6, "ver": "1"})),
            ),
            (
                base + 6,
                "browser_preferences",
                "intl.selected_languages",
                Some(json!({"value": "fr", "bk": "helium", "maj": 154})),
            ),
            (base + 6, "extensions", EXT, Some(json!({"enabled": false}))),
        ],
    );
    w.remote(&mut inc, 512 * 1024).await;
    publish(
        &w,
        &mut x,
        &[
            (base + 7, "extension_settings", &setting, None),
            (
                base + 7,
                "browser_preferences",
                "intl.selected_languages",
                None,
            ),
            (base + 7, "extensions", EXT, None),
        ],
    );
    w.remote(&mut inc, 512 * 1024).await;
    w.remote(&mut all, 512 * 1024).await; // X's two deletes, then Y's set
    assert_eq!(shared(&inc, &w), shared(&all, &w));
    let s = shared(&all, &w);
    for col in ["extensions", "extension_settings", "browser_preferences"] {
        let row = &s[col][0];
        let fields: Vec<&str> = row.split('\u{1f}').collect();
        assert!(
            fields.contains(&"1"),
            "{col}: deleted (the newest tombstone wins): {row}"
        );
        assert!(
            row.ends_with(&hlc(base + 7, X)),
            "{col}: the newest tombstone HLC is kept: {row}"
        );
    }
}

// ------------------------------------------------------------------ build / bootstrap

#[tokio::test]
async fn snapshot_bootstrap_is_equivalent_to_full_replay_and_gc_runs_one_generation_behind() {
    let (mut w, mut a, mut b, mut x, mut y) = history().await;
    let g1 = compact_now(&w, &mut a).await;
    assert!(
        matches!(g1, CompactionOutcome::Ready { generation: 1, .. }),
        "{g1:?}"
    );
    // more history after G1, then G2
    let t = a.br.find("Folder B");
    a.br.nodes.get_mut(&t).unwrap().title = "Folder B v2".into();
    w.local(&mut a).await;
    other_collections(&w, &mut x, &mut y, 1_790_000_001_000);
    w.remote(&mut b, 512 * 1024).await;
    let g2 = compact_now(&w, &mut a).await;
    assert!(
        matches!(g2, CompactionOutcome::Ready { generation: 2, .. }),
        "{g2:?}"
    );
    // a tail after G2
    let n = b.br.find("Bookmark 1");
    b.br.nodes.get_mut(&n).unwrap().title = "Bookmark 1 tail".into();
    w.local(&mut b).await;
    w.remote(&mut a, 512 * 1024).await;

    // C (full replay) and D (snapshot G2 + tail) converge to the same shared state as A
    let mut c = w.device();
    let mut d = w.device();
    let full = Sink {
        full_replay: true,
        ..w.sink()
    };
    c.db.capture_snapshot(&c.src, "e", 0, &c.br.snapshot(), w.tick())
        .unwrap();
    pull(&mut c.db, &full, w.profile, "h", 1).await.unwrap();
    w.remote(&mut d, 512 * 1024).await;
    w.remote(&mut c, 512 * 1024).await;
    assert_eq!(
        d.db.compaction_info(w.profile).unwrap().snapshot_generation,
        Some(2)
    );
    assert_eq!(
        c.db.compaction_info(w.profile).unwrap().snapshot_generation,
        None
    );
    assert_eq!(shared(&c, &w), shared(&a, &w));
    assert_eq!(shared(&d, &w), shared(&a, &w));
    assert_eq!(
        d.br.tree(),
        a.br.tree(),
        "the browser of D holds the same tree"
    );

    // GC: refused before the 10 min grace and while a mapped device has not reported
    for dev in [&mut a, &mut b, &mut c] {
        report_as(&w, dev).await;
    }
    let early = gc(&mut a.db, &w.sink(), w.profile, 1).await.unwrap();
    assert!(!early.allowed && early.events_deleted == 0);
    advance(&w, 11 * MIN);
    let blocked = gc(&mut a.db, &w.sink(), w.profile, 1).await.unwrap();
    assert_eq!(
        (blocked.allowed, blocked.blockers),
        (false, 1),
        "D has not reported yet"
    );
    report_as(&w, &mut d).await;
    let before: Vec<(u64, u64)> = events_of(&w, X);
    let r = gc(&mut a.db, &w.sink(), w.profile, 1).await.unwrap();
    assert!(
        r.allowed && r.events_deleted > 0 && r.objects_deleted > 0 && r.leftover_events == 0,
        "{r:?}"
    );
    let w1 = w
        .cloud
        .lock()
        .unwrap()
        .snaps
        .iter()
        .find(|s| s.generation == 1)
        .unwrap()
        .watermarks
        .clone();
    let w2 = w
        .cloud
        .lock()
        .unwrap()
        .snaps
        .iter()
        .find(|s| s.generation == 2)
        .unwrap()
        .watermarks
        .clone();
    let after = events_of(&w, X);
    assert!(
        after.iter().all(|(_, to)| *to > w1[X]),
        "only ≤ W(G1) collected"
    );
    assert!(
        before
            .iter()
            .filter(|(_, to)| *to > w1[X])
            .all(|e| after.contains(e)),
        "the tail after W(G1) intact"
    );
    assert!(w2[X] > w1[X]);
    // both snapshot generations kept (current + fallback)
    assert_eq!(
        w.cloud
            .lock()
            .unwrap()
            .snaps
            .iter()
            .filter(|s| s.state == "ready")
            .count(),
        2
    );

    // E arrives after GC, even preferring full replay: history is gone → snapshot + tail
    let mut e = w.device();
    let full = Sink {
        full_replay: true,
        ..w.sink()
    };
    e.db.capture_snapshot(&e.src, "e", 0, &e.br.snapshot(), w.tick())
        .unwrap();
    pull(&mut e.db, &full, w.profile, "h", 1).await.unwrap();
    assert_eq!(
        e.db.compaction_info(w.profile).unwrap().snapshot_generation,
        Some(2)
    );
    assert_eq!(shared(&e, &w), shared(&a, &w));
    // A/B keep syncing normally after GC
    let k = a.br.find("Folder B v2");
    a.br.create(&k, "After GC", Some("https://example.com/after"));
    w.local(&mut a).await;
    w.remote(&mut b, 512 * 1024).await;
    assert_eq!(shared(&b, &w), shared(&a, &w));
}

#[tokio::test]
async fn corrupt_current_falls_back_to_previous_and_corrupt_both_change_nothing() {
    let (mut w, mut a, mut b, mut x, mut y) = history().await;
    compact_now(&w, &mut a).await;
    other_collections(&w, &mut x, &mut y, 1_790_000_002_000);
    w.remote(&mut a, 512 * 1024).await;
    compact_now(&w, &mut a).await;
    for dev in [&mut a, &mut b] {
        report_as(&w, dev).await;
    }
    advance(&w, 11 * MIN);
    assert!(
        gc(&mut a.db, &w.sink(), w.profile, 1)
            .await
            .unwrap()
            .allowed
    );
    // corrupt one part of the current generation (bit flip)
    {
        let mut c = w.cloud.lock().unwrap();
        let key = c
            .objects
            .keys()
            .find(|(_, p)| p.contains("/snapshots/0000000002/") && p.ends_with(".jsonl"))
            .cloned()
            .unwrap();
        c.objects.get_mut(&key).unwrap()[3] ^= 1;
    }
    let mut mut_w = w;
    let mut f = mut_w.device();
    mut_w.remote(&mut f, 512 * 1024).await;
    let info = f.db.compaction_info(mut_w.profile).unwrap();
    assert_eq!(
        (info.snapshot_generation, info.bootstrap_fallback),
        (Some(1), true)
    );
    assert_eq!(
        shared(&f, &mut_w),
        shared(&a, &mut_w),
        "previous + retained tail rebuild the same state"
    );
    // corrupt the previous manifest too: a new device imports nothing and reports why
    {
        let mut c = mut_w.cloud.lock().unwrap();
        let key = c
            .objects
            .keys()
            .find(|(_, p)| p.contains("/snapshots/0000000001/manifest.json"))
            .cloned()
            .unwrap();
        c.objects.get_mut(&key).unwrap()[5] ^= 1;
    }
    let mut g = mut_w.device();
    g.db.capture_snapshot(&g.src, "e", 0, &g.br.snapshot(), mut_w.tick())
        .unwrap();
    let out = pull(&mut g.db, &mut_w.sink(), mut_w.profile, "h", 1)
        .await
        .unwrap();
    assert_eq!(
        out,
        PullOutcome::Blocked {
            kind: "snapshot_invalid".into()
        }
    );
    assert!(
        shared(&g, &mut_w).values().all(|v| v.is_empty()),
        "nothing imported"
    );
}

// ------------------------------------------------------------------ watermarks / blockers

#[tokio::test]
async fn watermarks_follow_active_readers_dormancy_and_the_server_validation() {
    let (mut w, mut a, mut b, mut x, _y) = history().await;
    report_as(&w, &mut a).await;
    report_as(&w, &mut b).await;
    // B stops pulling; X keeps publishing: the new watermark of X stops at what B applied
    let b_applied = b.db.cursor_report(w.profile).unwrap().applied[X];
    publish(
        &w,
        &mut x,
        &[(
            1_790_000_003_000,
            "browser_preferences",
            "intl.selected_languages",
            Some(json!({"value": "it", "bk": "helium", "maj": 154})),
        )],
    );
    w.remote(&mut a, 512 * 1024).await;
    report_as(&w, &mut a).await;
    compact_now(&w, &mut a).await;
    let g = w.cloud.lock().unwrap().snaps.last().unwrap().clone();
    assert_eq!(
        g.watermarks[X], b_applied,
        "an active reader holds the watermark back"
    );
    // a watermark inside a chunk / beyond the chain is refused by the server; journals stay
    let n_events = w.cloud.lock().unwrap().events.len();
    let sink = w.sink();
    sink.claim_snapshot(w.profile, 2).await.unwrap();
    let mut bad = g.watermarks.clone();
    *bad.get_mut(X).unwrap() += 1_000;
    let info = ReadyInfo {
        manifest_path: "profiles/x/snapshots/0000000002/manifest.json".into(),
        manifest_sha256: "a".repeat(64),
        watermarks: bad,
        ..Default::default()
    };
    let r = sink.mark_ready(w.profile, 2, &info).await;
    assert!(matches!(r, Err(SinkError::Rejected(_))));
    assert_eq!(w.cloud.lock().unwrap().events.len(), n_events);
    // B dormant (> 30 days without a report): it no longer holds the watermark
    advance(&w, 31 * DAY);
    report_as(&w, &mut a).await;
    let out = compact_now(&w, &mut a).await; // removes the expired claim first
    assert!(matches!(out, CompactionOutcome::Ready { .. }), "{out:?}");
    let g3 = w
        .cloud
        .lock()
        .unwrap()
        .snaps
        .iter()
        .filter(|s| s.state == "ready")
        .max_by_key(|s| s.generation)
        .unwrap()
        .clone();
    assert!(
        g3.watermarks[X] > b_applied,
        "dormant readers do not hold compaction back"
    );
}

#[tokio::test]
async fn devices_without_snapshot_support_block_gc_but_not_snapshots_until_retired() {
    let (mut w, mut a, mut b, mut x, mut y) = history().await;
    let old = w.device(); // mapped, never reports (an older build)
    compact_now(&w, &mut a).await;
    other_collections(&w, &mut x, &mut y, 1_790_000_004_000);
    w.remote(&mut a, 512 * 1024).await;
    assert!(matches!(
        compact_now(&w, &mut a).await,
        CompactionOutcome::Ready { generation: 2, .. }
    ));
    report_as(&w, &mut a).await;
    report_as(&w, &mut b).await;
    advance(&w, 11 * MIN);
    let r = gc(&mut a.db, &w.sink(), w.profile, 1).await.unwrap();
    assert_eq!((r.allowed, r.blockers, r.events_deleted), (false, 1, 0));
    // reporting snapshot_protocol = 0 does not unblock
    REPORTER.with(|r| *r.borrow_mut() = old.src.device_id.to_string());
    w.sink()
        .report_cursors(
            w.profile,
            &CursorReport {
                snapshot_protocol: 0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        gc(&mut a.db, &w.sink(), w.profile, 1)
            .await
            .unwrap()
            .blockers,
        1
    );
    // explicit retirement by the user unblocks
    w.cloud
        .lock()
        .unwrap()
        .cursors
        .get_mut(&(w.account.clone(), w.profile, old.src.device_id.to_string()))
        .unwrap()
        .retired = true;
    let r = gc(&mut a.db, &w.sink(), w.profile, 1).await.unwrap();
    assert!(r.allowed && r.events_deleted > 0);
    // the retired device comes back and reports (even with snapshot support): it stays
    // retired — no silent reactivation — so it neither blocks GC nor holds watermarks
    REPORTER.with(|r| *r.borrow_mut() = old.src.device_id.to_string());
    w.sink()
        .report_cursors(
            w.profile,
            &CursorReport {
                snapshot_protocol: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let still = w
        .sink()
        .list_cursors(w.profile)
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.device_id == old.src.device_id.to_string())
        .unwrap();
    assert!(still.retired, "a report never clears retired_at");
    assert_eq!(
        gc(&mut a.db, &w.sink(), w.profile, 1)
            .await
            .unwrap()
            .blockers,
        0
    );
    assert!(matches!(
        compact_now(&w, &mut a).await,
        CompactionOutcome::NotNeeded { .. }
    ));
    // explicit reactivation (user action): it is an active reader again, holding watermarks
    w.cloud
        .lock()
        .unwrap()
        .cursors
        .get_mut(&(w.account.clone(), w.profile, old.src.device_id.to_string()))
        .unwrap()
        .retired = false;
    other_collections(&w, &mut x, &mut y, 1_790_000_005_000);
    w.remote(&mut a, 512 * 1024).await;
    report_as(&w, &mut a).await;
    assert!(
        matches!(
            compact_now(&w, &mut a).await,
            CompactionOutcome::NotNeeded { .. }
        ),
        "the reactivated reader (applied nothing of the new history) holds the watermarks"
    );
}

// ------------------------------------------------------------------ tombstones

#[tokio::test]
async fn tombstones_survive_compaction_and_block_late_resurrection() {
    let (mut w, mut a, mut b, mut x, mut y) = history().await;
    compact_now(&w, &mut a).await;
    other_collections(&w, &mut x, &mut y, 1_790_000_005_000);
    w.remote(&mut a, 512 * 1024).await;
    compact_now(&w, &mut a).await;
    report_as(&w, &mut a).await;
    report_as(&w, &mut b).await;
    advance(&w, 11 * MIN);
    assert!(
        gc(&mut a.db, &w.sink(), w.profile, 1)
            .await
            .unwrap()
            .events_deleted
            > 0
    );
    // the bookmark deleted in history() is a tombstone inside both snapshots
    let snap = w
        .cloud
        .lock()
        .unwrap()
        .objects
        .iter()
        .filter(|((_, p), _)| p.contains("/snapshots/") && p.contains("bookmarks-"))
        .map(|(_, b)| String::from_utf8(b.clone()).unwrap())
        .collect::<String>();
    assert!(
        snap.contains("\"d\":true"),
        "tombstones kept in the snapshot"
    );
    // a stale set (older than every tombstone) published late cannot resurrect anything
    let setting = format!("{EXT}/local/theme");
    let mut z = Src {
        id: "00000000-0000-4000-8000-00000000000c",
        seq: 0,
    };
    publish(
        &w,
        &mut z,
        &[(
            1_790_000_000_002,
            "extension_settings",
            &setting,
            Some(json!({"value": "stale", "ver": "1.0"})),
        )],
    );
    let mut n = w.device();
    w.remote(&mut n, 512 * 1024).await;
    w.remote(&mut a, 512 * 1024).await;
    let row = shared(&n, &w)["extension_settings"][0].clone();
    assert!(
        row.split('\u{1f}').nth(2) == Some("1"),
        "still deleted: {row}"
    );
    assert_eq!(shared(&n, &w), shared(&a, &w));
}

// ------------------------------------------------------------------ generations / claims

#[tokio::test]
async fn current_and_previous_are_kept_older_ones_cleaned_once_stable() {
    let (mut w, mut a, mut b, mut x, mut y) = history().await;
    for i in 0..3u64 {
        other_collections(&w, &mut x, &mut y, 1_790_000_010_000 + i * 1000);
        w.remote(&mut a, 512 * 1024).await;
        assert!(matches!(
            compact_now(&w, &mut a).await,
            CompactionOutcome::Ready { .. }
        ));
    }
    report_as(&w, &mut a).await;
    report_as(&w, &mut b).await;
    advance(&w, 11 * MIN);
    let r = gc(&mut a.db, &w.sink(), w.profile, 1).await.unwrap();
    assert!(
        r.generations_removed.is_empty(),
        "G1 kept until G3 is stable"
    );
    assert_eq!(
        w.cloud
            .lock()
            .unwrap()
            .snaps
            .iter()
            .filter(|s| s.state == "ready")
            .count(),
        3
    );
    advance(&w, DAY);
    let r = gc(&mut a.db, &w.sink(), w.profile, 1).await.unwrap();
    assert_eq!(r.generations_removed, vec![1]);
    let c = w.cloud.lock().unwrap();
    assert_eq!(
        c.snaps.iter().map(|s| s.generation).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(!c
        .objects
        .keys()
        .any(|(_, p)| p.contains("/snapshots/0000000001/")));
    assert!(
        c.objects
            .keys()
            .any(|(_, p)| p.contains("/snapshots/0000000002/")),
        "the fallback's objects stay"
    );
}

#[tokio::test]
async fn an_expired_claim_is_recovered_and_a_live_one_means_busy() {
    let (w, mut a, _b, _x, _y) = history().await;
    w.sink().claim_snapshot(w.profile, 1).await.unwrap();
    w.sink()
        .put_snapshot_object(
            &format!("profiles/{}/snapshots/0000000001/x-0001.jsonl", w.profile),
            b"half",
            "application/x-ndjson",
        )
        .await
        .unwrap();
    assert_eq!(compact_now(&w, &mut a).await, CompactionOutcome::Busy);
    advance(&w, 31 * MIN);
    assert!(matches!(
        compact_now(&w, &mut a).await,
        CompactionOutcome::Ready { generation: 1, .. }
    ));
    assert!(
        !w.cloud
            .lock()
            .unwrap()
            .objects
            .keys()
            .any(|(_, p)| p.ends_with("x-0001.jsonl")),
        "the stale upload was removed"
    );
    // a ready generation can never be written again
    let r = w
        .sink()
        .put_snapshot_object(
            &format!("profiles/{}/snapshots/0000000001/manifest.json", w.profile),
            b"{}",
            "application/json",
        )
        .await;
    assert!(matches!(r, Err(SinkError::Forbidden(_))));
}

#[tokio::test]
async fn thresholds_restart_offline_reports_and_other_accounts() {
    let (mut w, mut a, _b, _x, _y) = history().await;
    // thresholds: far below 5 000 ops / 300 chunks / 16 MiB → not needed
    let out = compact(&mut a.db, &w.sink(), w.profile, "d", Policy::default(), 1)
        .await
        .unwrap();
    assert!(
        matches!(out, CompactionOutcome::NotNeeded { .. }),
        "{out:?}"
    );
    // offline: an error, the cloud unchanged
    let off = Sink {
        offline: true,
        ..w.sink()
    };
    assert!(compact(
        &mut a.db,
        &off,
        w.profile,
        "d",
        Policy {
            force: true,
            ..Default::default()
        },
        1
    )
    .await
    .is_err());
    assert!(w.cloud.lock().unwrap().snaps.is_empty());
    // a report is sent only when something changed
    REPORTER.with(|r| *r.borrow_mut() = a.src.device_id.to_string());
    assert!(report_cursors(&mut a.db, &w.sink(), w.profile)
        .await
        .unwrap());
    assert!(!report_cursors(&mut a.db, &w.sink(), w.profile)
        .await
        .unwrap());
    // restart (file database) keeps bootstrap/compaction bookkeeping
    compact_now(&w, &mut a).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("n.sqlite");
    let mut n = w.device();
    n.db = StateDb::open(&path).unwrap();
    w.remote(&mut n, 512 * 1024).await;
    n.db = StateDb::open(&path).unwrap();
    assert_eq!(
        n.db.compaction_info(w.profile).unwrap().snapshot_generation,
        Some(1)
    );
    let again = pull(&mut n.db, &w.sink(), w.profile, "h", 1).await.unwrap();
    assert!(
        matches!(again, PullOutcome::Pulled(PullStats { ops: 0, .. })),
        "{again:?}"
    );
    // another account sees no snapshot and cannot collect anything
    let other = Sink {
        account: Uuid::new_v4().to_string(),
        ..w.sink()
    };
    assert!(other.ready_snapshots(w.profile).await.unwrap().is_empty());
    assert!(matches!(
        other.gc_events(w.profile, X, 1).await,
        Err(SinkError::Rejected(_))
    ));
    let _ = &mut w;
}

// ------------------------------------------------------------------ determinism

#[tokio::test]
async fn a_generation_rebuilt_from_journals_alone_or_from_the_previous_one_is_identical() {
    let (mut w, mut a, mut b, mut x, mut y) = history().await;
    assert!(matches!(
        compact_now(&w, &mut a).await,
        CompactionOutcome::Ready { generation: 1, .. }
    ));
    let t = a.br.find("Folder B");
    a.br.nodes.get_mut(&t).unwrap().title = "Folder B v2".into();
    w.local(&mut a).await;
    other_collections(&w, &mut x, &mut y, 1_790_000_001_000);
    w.remote(&mut b, 512 * 1024).await;
    assert!(matches!(
        compact_now(&w, &mut a).await,
        CompactionOutcome::Ready { generation: 2, .. }
    ));
    let sink = w.sink();
    let rows = sink.list_snapshots(w.profile).await.unwrap();
    for g in [1u64, 2] {
        let row = rows.iter().find(|r| r.generation == g).unwrap();
        let (m, _) = fetch_snapshot(&sink, row).await.unwrap();
        let parts =
            |c: &std::collections::BTreeMap<String, synchrome_sync::snapshot::CollectionInfo>| {
                c.iter()
                    .map(|(k, i)| (k.clone(), i.entities, i.tombstones, i.parts.clone()))
                    .collect::<Vec<_>>()
            };
        let alone = rebuild(&sink, w.profile, g, false).await.unwrap();
        assert_eq!(alone.manifest.base_generation, None);
        assert_eq!(
            parts(&alone.manifest.collections),
            parts(&m.collections),
            "G{g} from journals alone"
        );
        let again = rebuild(&sink, w.profile, g, false).await.unwrap();
        assert_eq!(again.parts, alone.parts, "G{g} twice: same bytes");
        let prev = rebuild(&sink, w.profile, g, true).await.unwrap();
        assert_eq!(
            prev.manifest.base_generation,
            g.checked_sub(1).filter(|x| *x > 0)
        );
        assert_eq!(
            parts(&prev.manifest.collections),
            parts(&m.collections),
            "G{g} from G{}",
            g - 1
        );
    }
    // nothing was uploaded or claimed by the rebuilds
    assert_eq!(sink.list_snapshots(w.profile).await.unwrap().len(), 2);
}
