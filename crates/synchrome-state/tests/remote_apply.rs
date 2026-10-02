//! Cloud -> browser: device A publishes, device B pulls, plans, "applies" to a fake browser
//! with chrome.bookmarks semantics, records the results and reconciles a snapshot (what the
//! extension does after every apply). Shared in-memory cloud with per-account isolation.

mod common;
use common::*;
use synchrome_state::*;
use synchrome_sync::bookmarks::BookmarkEvent;
use uuid::Uuid;

// ------------------------------------------------------------------ tests
#[tokio::test]
async fn first_download_builds_the_same_tree_and_echoes_nothing() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    assert_eq!(w.local(&mut a).await, 7);
    let (applied, echoes) = w.remote(&mut b, 512 * 1024).await;
    assert_eq!(b.br.tree(), a.br.tree(), "nested folders and order");
    assert!(applied >= 7);
    assert_eq!(echoes, 0, "applied changes must not come back as local ops");
    assert_eq!(b.db.pending(w.profile).unwrap(), 0, "B publishes nothing");
    assert_eq!(
        b.db.remote_cursor(w.profile, &a.src.device_id.to_string())
            .unwrap(),
        7
    );
    let st =
        b.db.status(w.profile, b.src.mapping_id)
            .unwrap()
            .download
            .unwrap();
    assert_eq!(
        (st.state, st.remote_pending, st.conflicts),
        (DownloadState::UpToDate, 0, 0)
    );
    // A gets nothing back
    let (applied_a, _) = w.remote(&mut a, 512 * 1024).await;
    assert_eq!(applied_a, 0);
    assert_eq!(
        w.cloud.lock().unwrap().events.len(),
        1,
        "only A's chunk exists"
    );
}

#[tokio::test]
async fn applied_chunks_and_duplicate_events_are_not_reprocessed() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 512 * 1024).await;
    // duplicate event row for the same chunk
    let dup = w.cloud.lock().unwrap().events[0].clone();
    w.cloud.lock().unwrap().events.push(dup);
    let sink = w.sink();
    let out = pull(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    assert_eq!(out, PullOutcome::Pulled(PullStats::default()));
    assert_eq!(b.db.plan_apply(w.profile, 1 << 20).unwrap().ops.len(), 0);
}

#[tokio::test]
async fn a_seq_gap_stops_the_device_until_it_is_filled() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await; // seq 1..7
    let id = a.br.find("Bookmark 1");
    a.br.nodes.get_mut(&id).unwrap().title = "B1 v2".into();
    w.local(&mut a).await; // seq 8
    let id = a.br.find("Bookmark 2");
    a.br.nodes.get_mut(&id).unwrap().title = "B2 v2".into();
    w.local(&mut a).await; // seq 9
    w.cloud.lock().unwrap().hidden.insert(8);
    let sink = w.sink();
    b.db.capture_snapshot(&b.src, "e", 0, &b.br.snapshot(), w.tick())
        .unwrap();
    let out = pull(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    let PullOutcome::Pulled(st) = out else {
        panic!("{out:?}")
    };
    assert_eq!(st.gaps, vec![(a.src.device_id.to_string(), 8)]);
    assert_eq!(
        b.db.remote_cursor(w.profile, &a.src.device_id.to_string())
            .unwrap(),
        7,
        "9 not applied across the gap"
    );
    w.cloud.lock().unwrap().hidden.clear();
    w.remote(&mut b, 1 << 20).await;
    assert_eq!(
        b.db.remote_cursor(w.profile, &a.src.device_id.to_string())
            .unwrap(),
        9
    );
    assert_eq!(b.br.tree(), a.br.tree());
}

#[tokio::test]
async fn remote_rename_url_move_reorder_delete_apply_with_explicit_ops() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;

    let check = |w: &World, a: &Dev, b: &Dev| assert_eq!(b.br.tree(), a.br.tree(), "{}", w.t.get());
    // rename
    let x = a.br.find("Bookmark 1");
    a.br.nodes.get_mut(&x).unwrap().title = "Bookmark 1 renamed".into();
    w.local(&mut a).await;
    let sink = w.sink();
    sync(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    assert_eq!(
        plan_kinds(&b.db.plan_apply(w.profile, 1 << 20).unwrap()),
        vec!["rename"]
    );
    let (_, e) = w.remote(&mut b, 1 << 20).await;
    assert_eq!(e, 0);
    check(&w, &a, &b);
    // URL change
    a.br.nodes.get_mut(&x).unwrap().url = Some("https://example.org/1".into());
    w.local(&mut a).await;
    let sink = w.sink();
    sync(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    assert_eq!(
        plan_kinds(&b.db.plan_apply(w.profile, 1 << 20).unwrap()),
        vec!["set_url"]
    );
    w.remote(&mut b, 1 << 20).await;
    check(&w, &a, &b);
    // move into another folder
    let fb = a.br.find("Folder B");
    a.br.move_to(&x, &fb, Some(0));
    w.local(&mut a).await;
    let sink = w.sink();
    sync(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    assert_eq!(
        plan_kinds(&b.db.plan_apply(w.profile, 1 << 20).unwrap()),
        vec!["move", "reorder"]
    );
    let (_, e) = w.remote(&mut b, 1 << 20).await;
    assert_eq!(e, 0);
    check(&w, &a, &b);
    // reorder inside a folder
    let n = a.br.find("Nested");
    a.br.move_to(&n, &fb, Some(0));
    w.local(&mut a).await;
    let sink = w.sink();
    sync(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    assert_eq!(
        plan_kinds(&b.db.plan_apply(w.profile, 1 << 20).unwrap()),
        vec!["reorder"]
    );
    w.remote(&mut b, 1 << 20).await;
    check(&w, &a, &b);
    // delete a folder with children (Nested + Bookmark 3)
    a.br.remove(&n, true).unwrap();
    w.local(&mut a).await;
    let sink = w.sink();
    sync(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    let plan = b.db.plan_apply(w.profile, 1 << 20).unwrap();
    assert_eq!(
        plan_kinds(&plan),
        vec!["delete", "delete"],
        "child first, then the folder"
    );
    let (_, e) = w.remote(&mut b, 1 << 20).await;
    assert_eq!(e, 0);
    check(&w, &a, &b);
    assert_eq!(
        b.db.pending(w.profile).unwrap(),
        0,
        "B never published anything"
    );
}

#[tokio::test]
async fn deletes_are_idempotent_and_missing_nodes_are_fine() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;
    let fa = a.br.find("Folder A");
    a.br.remove(&fa, true).unwrap();
    w.local(&mut a).await;
    // the user already deleted Bookmark 1 on B (not yet captured)
    let b1 = b.br.find("Bookmark 1");
    b.br.remove(&b1, false).unwrap();
    let (applied, echoes) = w.remote(&mut b, 1 << 20).await;
    assert_eq!(applied, 3);
    assert_eq!(echoes, 0);
    assert_eq!(b.br.tree(), a.br.tree());
    // a second pull of the same tombstones changes nothing
    let (applied, _) = w.remote(&mut b, 1 << 20).await;
    assert_eq!(applied, 0);
}

#[tokio::test]
async fn remote_folder_delete_never_removes_a_local_child_conflict_instead() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;
    // B adds a bookmark into Folder A and has not published it yet
    let fa_b = b.br.find("Folder A");
    b.br.create(&fa_b, "Only on B", Some("https://b.example/"));
    let t = w.tick();
    b.db.capture_snapshot(&b.src, "e", 0, &b.br.snapshot(), t)
        .unwrap();
    // A deletes Folder A
    let fa = a.br.find("Folder A");
    a.br.remove(&fa, true).unwrap();
    w.local(&mut a).await;
    let (_, echoes) = w.remote(&mut b, 1 << 20).await;
    assert_eq!(
        echoes, 0,
        "the kept folder is not re-created under a new identity"
    );
    assert_eq!(
        b.db.pending(w.profile).unwrap(),
        0,
        "B published only its own child"
    );
    assert!(
        b.br.tree().iter().any(|n| n.1 == "Only on B"),
        "local data kept"
    );
    assert!(
        b.br.tree().iter().any(|n| n.1 == "Folder A"),
        "folder kept because it is not empty"
    );
    let st =
        b.db.status(w.profile, b.src.mapping_id)
            .unwrap()
            .download
            .unwrap();
    assert_eq!(st.state, DownloadState::Conflict);
    assert!(b
        .db
        .open_conflicts(w.profile)
        .unwrap()
        .iter()
        .any(|c| c.1 == "apply_failed"));
}

#[tokio::test]
async fn app_restart_between_pull_and_apply_and_lost_apply_results() {
    let mut w = World::new();
    let mut a = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.sqlite");
    let mut b = w.device();
    b.db = StateDb::open(&path).unwrap();
    b.db.capture_snapshot(&b.src, "e", 0, &b.br.snapshot(), w.tick())
        .unwrap();
    let sink = w.sink();
    sync(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    // app restarts: the materialised remote state survives
    b.db = StateDb::open(&path).unwrap();
    let plan = b.db.plan_apply(w.profile, 1 << 20).unwrap();
    assert_eq!(
        plan.ops
            .iter()
            .filter(|o| matches!(o.action, ApplyAction::CreateBookmark { .. }))
            .count(),
        3
    );
    // the extension applies, but its worker dies before reporting the results
    b.br.apply(&plan.ops);
    // next start: reconcile pairs the created nodes with the pending entities (no duplicate)
    let r =
        b.db.capture_snapshot(&b.src, "e2", 0, &b.br.snapshot(), w.tick())
            .unwrap();
    assert!(matches!(r, CaptureOutcome::Applied { ops: 0, .. }), "{r:?}");
    let (_, echoes) = w.remote(&mut b, 1 << 20).await;
    assert_eq!(echoes, 0);
    assert_eq!(
        b.br.tree(),
        a.br.tree(),
        "no duplicate nodes in the browser"
    );
    assert_eq!(b.br.nodes.len(), 3 + 7);
}

#[tokio::test]
async fn offline_device_catches_up_in_order() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;
    let x = a.br.find("Bookmark 2");
    a.br.nodes.get_mut(&x).unwrap().title = "v1".into();
    w.local(&mut a).await;
    a.br.nodes.get_mut(&x).unwrap().title = "v2".into();
    w.local(&mut a).await;
    let mut sink = w.sink();
    sink.offline = true;
    let out = pull(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    assert_eq!(
        out,
        PullOutcome::Failed {
            kind: "offline".into()
        }
    );
    assert_eq!(
        b.db.status(w.profile, b.src.mapping_id)
            .unwrap()
            .download
            .unwrap()
            .state,
        DownloadState::Offline
    );
    w.remote(&mut b, 1 << 20).await;
    assert!(b.br.tree().iter().any(|n| n.1 == "v2") && !b.br.tree().iter().any(|n| n.1 == "v1"));
    assert_eq!(
        b.db.status(w.profile, b.src.mapping_id)
            .unwrap()
            .download
            .unwrap()
            .state,
        DownloadState::UpToDate
    );
}

#[tokio::test]
async fn deleted_mapping_and_other_account_block_downloads() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    b.db.capture_snapshot(&b.src, "e", 0, &b.br.snapshot(), w.tick())
        .unwrap();
    let mut sink = w.sink();
    sink.mappings.remove(&b.src.mapping_id);
    assert_eq!(
        pull(&mut b.db, &sink, w.profile, "h", 1).await.unwrap(),
        PullOutcome::Blocked {
            kind: "mapping_deleted".into()
        }
    );
    let other = Sink {
        account: Uuid::new_v4().to_string(),
        ..w.sink()
    };
    assert_eq!(
        pull(&mut b.db, &other, w.profile, "h", 1).await.unwrap(),
        PullOutcome::Blocked {
            kind: "other_account".into()
        }
    );
    assert!(
        other.list_events(w.profile).await.unwrap().is_empty(),
        "another account sees no events"
    );
    assert_eq!(b.db.plan_apply(w.profile, 1 << 20).unwrap().ops.len(), 0);
}

#[tokio::test]
async fn a_legit_local_change_right_after_an_apply_is_published() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;
    // the user renames on B immediately after the apply (a normal event)
    let x = b.br.find("Bookmark 1");
    b.br.nodes.get_mut(&x).unwrap().title = "edited on B".into();
    let r =
        b.db.capture_events(
            &b.src,
            "eB",
            &[(
                1,
                BookmarkEvent::Changed {
                    id: x,
                    title: Some("edited on B".into()),
                    url: None,
                },
            )],
            w.tick(),
        )
        .unwrap();
    assert!(matches!(r, CaptureOutcome::Applied { ops: 1, .. }));
    let sink = w.sink();
    sync(&mut b.db, &sink, w.profile, "h", 2).await.unwrap();
    w.remote(&mut a, 1 << 20).await;
    assert!(a.br.tree().iter().any(|n| n.1 == "edited on B"));
    assert_eq!(a.br.tree(), b.br.tree());
}

#[tokio::test]
async fn pending_local_change_is_never_overwritten_by_a_remote_one() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;
    // B (offline) renames X: captured, not published
    let xb = b.br.find("Bookmark 2");
    b.br.nodes.get_mut(&xb).unwrap().title = "B's title".into();
    b.db.capture_snapshot(&b.src, "e", 0, &b.br.snapshot(), w.tick())
        .unwrap();
    assert_eq!(b.db.pending(w.profile).unwrap(), 1);
    // A renames the same entity and publishes
    let xa = a.br.find("Bookmark 2");
    a.br.nodes.get_mut(&xa).unwrap().title = "A's title".into();
    w.local(&mut a).await;
    // B pulls (only): conflict, nothing overwritten, nothing planned for X
    let sink = w.sink();
    let out = pull(&mut b.db, &sink, w.profile, "h", 3).await.unwrap();
    assert!(
        matches!(out, PullOutcome::Pulled(PullStats { conflicts: 1, .. })),
        "{out:?}"
    );
    assert!(b.db.plan_apply(w.profile, 1 << 20).unwrap().ops.is_empty());
    assert!(b.br.tree().iter().any(|n| n.1 == "B's title"));
    let st =
        b.db.status(w.profile, b.src.mapping_id)
            .unwrap()
            .download
            .unwrap();
    assert_eq!((st.state, st.conflicts), (DownloadState::Conflict, 1));
    let c = b.db.open_conflicts(w.profile).unwrap();
    assert_eq!(c[0].1, "local_pending");
}

#[tokio::test]
async fn large_remote_batches_are_split_and_converge() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    let big = a.br.create("1", "Big", None);
    for i in 0..1500 {
        a.br.create(
            &big,
            &format!("Item {i} {}", "t".repeat(200)),
            Some(&format!("https://e.example/{i}")),
        );
    }
    w.local(&mut a).await;
    let mut rounds = 0;
    b.db.capture_snapshot(&b.src, "e", 0, &b.br.snapshot(), w.tick())
        .unwrap();
    let sink = w.sink();
    sync(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    loop {
        let plan = b.db.plan_apply(w.profile, 64 * 1024).unwrap();
        if plan.ops.is_empty() {
            break;
        }
        let enc: usize = plan
            .ops
            .iter()
            .map(|o| serde_json::to_string(o).unwrap().len() + 1)
            .sum();
        assert!(enc <= 64 * 1024, "plan respects its byte budget ({enc})");
        let res = b.br.apply(&plan.ops);
        b.db.record_apply(w.profile, &plan, &res, 1).unwrap();
        rounds += 1;
        assert!(rounds < 100);
    }
    let r =
        b.db.capture_snapshot(&b.src, "e", 0, &b.br.snapshot(), w.tick())
            .unwrap();
    assert!(matches!(r, CaptureOutcome::Applied { ops: 0, .. }), "{r:?}");
    assert!(rounds > 3);
    assert_eq!(b.br.tree(), a.br.tree());
}

#[tokio::test]
async fn joining_device_with_the_same_bookmarks_pairs_instead_of_duplicating() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    // B already has the very same tree locally (e.g. imported earlier)
    build_known(&mut b.br);
    let sink = w.sink();
    b.db.ensure_claimed(&b.src).unwrap();
    pull(&mut b.db, &sink, w.profile, "h", 1).await.unwrap();
    let r =
        b.db.capture_snapshot(&b.src, "e", 0, &b.br.snapshot(), w.tick())
            .unwrap();
    assert!(
        matches!(r, CaptureOutcome::Applied { ops: 0, .. }),
        "paired, nothing new: {r:?}"
    );
    let (_, echoes) = w.remote(&mut b, 1 << 20).await;
    assert_eq!(echoes, 0);
    assert_eq!(b.br.tree(), a.br.tree(), "no duplicates");
    assert_eq!(b.br.nodes.len(), 3 + 7);
}

#[tokio::test]
async fn a_remote_child_of_a_locally_deleted_folder_is_an_explicit_orphan_conflict() {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;
    // B (not yet published) adds a child into Folder A
    let fa_b = b.br.find("Folder A");
    b.br.create(&fa_b, "B child", Some("https://b.example/c"));
    b.db.capture_snapshot(&b.src, "e", 0, &b.br.snapshot(), w.tick())
        .unwrap();
    // A deletes Folder A (with its synced children)
    let fa = a.br.find("Folder A");
    a.br.remove(&fa, true).unwrap();
    w.local(&mut a).await;
    // B: folder kept (not empty), its own child kept, then B publishes the child
    w.remote(&mut b, 1 << 20).await;
    assert!(b.br.tree().iter().any(|n| n.1 == "B child"));
    // A receives a create whose parent it deleted: explicit conflict, nothing stuck
    w.remote(&mut a, 1 << 20).await;
    let st =
        a.db.status(w.profile, a.src.mapping_id)
            .unwrap()
            .download
            .unwrap();
    assert_eq!((st.state, st.remote_pending), (DownloadState::Conflict, 0));
    assert!(a
        .db
        .open_conflicts(w.profile)
        .unwrap()
        .iter()
        .any(|c| c.1 == "orphan"));
    assert!(!a.br.tree().iter().any(|n| n.1 == "B child"));
}
