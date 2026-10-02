//! Conflict resolution (slice 9): every decision becomes ordinary ops with a fresh HLC that
//! converge on both devices; nothing is lost silently; records persist with their history.

mod common;
use common::*;
use synchrome_state::*;
use synchrome_sync::{Hlc, Op};
use uuid::Uuid;

fn retitle(d: &mut Dev, from: &str, to: &str) {
    let id = d.br.find(from);
    d.br.nodes.get_mut(&id).unwrap().title = to.into();
}

fn reurl(d: &mut Dev, title: &str, url: &str) {
    let id = d.br.find(title);
    d.br.nodes.get_mut(&id).unwrap().url = Some(url.into());
}

/// Capture the browser as it is now, without publishing (an unpublished local change).
fn capture(w: &World, d: &mut Dev) -> usize {
    match d
        .db
        .capture_snapshot(&d.src, "e", 0, &d.br.snapshot(), w.tick())
        .unwrap()
    {
        CaptureOutcome::Applied { ops, .. } => ops,
        o => panic!("{o:?}"),
    }
}

async fn pull_only(w: &World, d: &mut Dev) -> PullOutcome {
    let sink = w.sink();
    pull(&mut d.db, &sink, w.profile, "h", (w.tick() / 1000) as i64)
        .await
        .unwrap()
}

fn open(w: &World, d: &mut Dev) -> Vec<ConflictView> {
    d.db.conflict_views(w.profile, false).unwrap()
}

fn all(w: &World, d: &mut Dev) -> Vec<ConflictView> {
    d.db.conflict_views(w.profile, true).unwrap()
}

fn resolve(w: &World, d: &mut Dev, id: &str, r: Resolution) -> ResolveOutcome {
    d.db.resolve_conflict(&w.account, w.profile, id, r, w.tick())
        .unwrap()
}

fn sid_of(w: &World, d: &mut Dev, title: &str) -> String {
    d.db.tree(w.profile)
        .unwrap()
        .entities
        .values()
        .find(|e| e.title == title)
        .unwrap_or_else(|| panic!("no entity {title}"))
        .sid
        .clone()
}

/// Every op in the cloud (all devices).
fn cloud_ops(w: &World) -> Vec<Op> {
    let c = w.cloud.lock().unwrap();
    let mut out = Vec::new();
    for body in c.objects.values() {
        for l in std::str::from_utf8(body).unwrap().lines() {
            out.push(Op::from_line(l).unwrap());
        }
    }
    out
}

/// Unpublished ops of a device (its outbox).
fn outbox(w: &World, d: &Dev) -> Vec<Op> {
    d.db.chunk_bodies(w.profile)
        .unwrap()
        .into_iter()
        .flat_map(|(_, b)| {
            b.lines()
                .map(|l| Op::from_line(l).unwrap())
                .collect::<Vec<_>>()
        })
        .collect()
}

fn titles(d: &Dev) -> Vec<String> {
    d.br.tree().into_iter().map(|n| n.1).collect()
}

/// A and B synced on the known tree.
async fn setup() -> (World, Dev, Dev) {
    let mut w = World::new();
    let mut a = w.device();
    let mut b = w.device();
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;
    assert_eq!(a.br.tree(), b.br.tree());
    (w, a, b)
}

/// B renames X without publishing; A renames X and publishes; B pulls: one conflict.
async fn title_conflict() -> (World, Dev, Dev, ConflictView) {
    let (mut w, mut a, mut b) = setup().await;
    retitle(&mut b, "Bookmark 2", "B's title");
    assert_eq!(capture(&w, &mut b), 1);
    retitle(&mut a, "Bookmark 2", "A's title");
    w.local(&mut a).await;
    let out = pull_only(&w, &mut b).await;
    assert!(
        matches!(out, PullOutcome::Pulled(PullStats { conflicts: 1, .. })),
        "{out:?}"
    );
    let v = open(&w, &mut b);
    assert_eq!(v.len(), 1);
    (w, a, b, v[0].clone())
}

/// Sync both ways until quiet; returns (B echoes, A echoes) of the last rounds.
async fn converge(w: &mut World, a: &mut Dev, b: &mut Dev) -> (usize, usize) {
    let (_, eb) = w.remote(b, 1 << 20).await;
    let (_, ea) = w.remote(a, 1 << 20).await;
    let (_, eb2) = w.remote(b, 1 << 20).await;
    (eb + eb2, ea)
}

// ------------------------------------------------------------------ local_pending

#[tokio::test]
async fn local_pending_keep_local_publishes_a_newer_op_and_converges() {
    let (mut w, mut a, mut b, c) = title_conflict().await;
    assert_eq!(
        (c.kind, c.subtype, c.status),
        (
            ConflictKind::LocalPending,
            ConflictSubtype::EditEdit,
            ConflictStatus::Unresolved
        )
    );
    assert_eq!(c.local.title.as_deref(), Some("B's title"));
    assert_eq!(c.remote.title.as_deref(), Some("A's title"));
    assert_eq!(
        c.base.as_ref().and_then(|x| x.title.as_deref()),
        Some("Bookmark 2"),
        "BASE known"
    );
    assert_eq!(
        c.remote_device.as_deref(),
        Some(a.src.device_id.to_string().as_str())
    );
    let opts: Vec<Resolution> = c.options.iter().map(|o| o.resolution).collect();
    assert_eq!(
        opts,
        vec![Resolution::KeepLocal, Resolution::KeepRemote],
        "same field: no merge"
    );
    // B published its own change meanwhile (sync = pull + publish); the conflict stays
    w.remote(&mut b, 1 << 20).await;
    assert_eq!(open(&w, &mut b).len(), 1);
    // control: B's own (older) op alone does not win on A
    w.remote(&mut a, 1 << 20).await;
    assert!(titles(&a).contains(&"A's title".to_string()));
    assert_eq!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::KeepLocal),
        ResolveOutcome::Resolved { ops: 1 }
    );
    let (eb, ea) = converge(&mut w, &mut a, &mut b).await;
    assert_eq!((eb, ea), (0, 0), "no echoes on either side");
    assert!(
        titles(&a).contains(&"B's title".to_string()),
        "A ends with B's value"
    );
    assert_eq!(a.br.tree(), b.br.tree());
    let h = all(&w, &mut b);
    assert_eq!(
        (h[0].status, h[0].resolution),
        (ConflictStatus::Resolved, Some(Resolution::KeepLocal))
    );
    assert_eq!(
        b.db.status(w.profile, b.src.mapping_id)
            .unwrap()
            .download
            .unwrap()
            .conflicts,
        0
    );
}

#[tokio::test]
async fn resolution_op_has_an_hlc_later_than_both_versions() {
    let (w, _a, mut b, c) = title_conflict().await;
    let sid = sid_of(&w, &mut b, "B's title");
    let local_op = outbox(&w, &b).into_iter().rfind(|o| o.id == sid).unwrap();
    let remote_op = cloud_ops(&w)
        .into_iter()
        .filter(|o| o.id == sid)
        .max_by(|x, y| x.hlc.cmp(&y.hlc))
        .unwrap();
    resolve(&w, &mut b, &c.conflict_id, Resolution::KeepLocal);
    let res_op = outbox(&w, &b).into_iter().rfind(|o| o.id == sid).unwrap();
    let (l, r, x): (Hlc, Hlc, Hlc) = (
        local_op.hlc.parse().unwrap(),
        remote_op.hlc.parse().unwrap(),
        res_op.hlc.parse().unwrap(),
    );
    assert!(x > l && x > r, "{x:?} after {l:?} and {r:?}");
    assert_eq!(res_op.by, b.src.device_id.to_string());
    assert_eq!(
        res_op.fields.unwrap().get("title").and_then(|v| v.as_str()),
        Some("B's title")
    );
}

#[tokio::test]
async fn local_pending_keep_remote_applies_the_held_version_without_echo() {
    let (mut w, mut a, mut b, c) = title_conflict().await;
    let out = resolve(&w, &mut b, &c.conflict_id, Resolution::KeepRemote);
    assert!(matches!(out, ResolveOutcome::Resolving { .. }), "{out:?}");
    assert_eq!(
        open(&w, &mut b)[0].status,
        ConflictStatus::Resolving,
        "resolved only after the apply"
    );
    let (applied, echoes) = w.remote(&mut b, 1 << 20).await;
    assert!(applied >= 1);
    assert_eq!(
        echoes, 0,
        "applying the remote version produces no local op"
    );
    assert!(titles(&b).contains(&"A's title".to_string()));
    let (_, ea) = w.remote(&mut a, 1 << 20).await;
    assert_eq!(ea, 0);
    assert_eq!(a.br.tree(), b.br.tree());
    let h = all(&w, &mut b);
    assert_eq!(
        (h[0].status, h[0].resolution),
        (ConflictStatus::Resolved, Some(Resolution::KeepRemote))
    );
}

#[tokio::test]
async fn merge_takes_the_local_title_and_the_remote_url() {
    let (mut w, mut a, mut b) = setup().await;
    retitle(&mut b, "Bookmark 1", "B title");
    capture(&w, &mut b);
    reurl(&mut a, "Bookmark 1", "https://a.example/new");
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    let c = open(&w, &mut b).remove(0);
    assert_eq!(
        c.options[0].resolution,
        Resolution::Merge,
        "disjoint fields: merge offered first"
    );
    assert_eq!(c.local.changed, vec![Group::Title]);
    assert_eq!(c.remote.changed, vec![Group::Url]);
    assert!(matches!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::Merge),
        ResolveOutcome::Resolving { .. }
    ));
    converge(&mut w, &mut a, &mut b).await;
    for d in [&a, &b] {
        let n =
            d.br.tree()
                .into_iter()
                .find(|n| n.1 == "B title")
                .expect("local title");
        assert_eq!(n.2.as_deref(), Some("https://a.example/new"), "remote URL");
    }
    assert_eq!(a.br.tree(), b.br.tree());
}

#[tokio::test]
async fn merge_is_refused_when_both_changed_the_same_field() {
    let (w, _a, mut b, c) = title_conflict().await;
    assert!(c.options.iter().all(|o| o.resolution != Resolution::Merge));
    let r = b.db.resolve_conflict(
        &w.account,
        w.profile,
        &c.conflict_id,
        Resolution::Merge,
        w.tick(),
    );
    assert!(matches!(r, Err(ResolveError::NotAllowed)), "{r:?}");
    assert_eq!(
        open(&w, &mut b)[0].status,
        ConflictStatus::Unresolved,
        "nothing changed"
    );
    // both moved it: location is one group, never split
    let (mut w, mut a, mut b) = setup().await;
    let (fb_b, fb_a) = (b.br.find("Folder B"), a.br.find("Folder B"));
    let x = b.br.find("Bookmark 1");
    b.br.move_to(&x, &fb_b, None);
    capture(&w, &mut b);
    let x = a.br.find("Bookmark 1");
    a.br.move_to(&x, &fb_a, Some(0));
    let t = a.br.find("Bookmark 1");
    a.br.nodes.get_mut(&t).unwrap().title = "moved by A".into();
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    let c = open(&w, &mut b).remove(0);
    assert!(
        c.options.iter().all(|o| o.resolution != Resolution::Merge),
        "{:?}",
        c.options
    );
}

// ------------------------------------------------------------------ deletes

#[tokio::test]
async fn local_edit_vs_remote_delete_keep_local_restores_it_everywhere() {
    let (mut w, mut a, mut b) = setup().await;
    retitle(&mut b, "Bookmark 1", "B keeps this");
    capture(&w, &mut b);
    let x = a.br.find("Bookmark 1");
    a.br.remove(&x, false).unwrap();
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    let c = open(&w, &mut b).remove(0);
    assert_eq!(c.subtype, ConflictSubtype::EditDelete);
    assert!(!c.remote.exists && c.local.exists);
    let keep_remote = c
        .options
        .iter()
        .find(|o| o.resolution == Resolution::KeepRemote)
        .unwrap();
    assert!(
        keep_remote.destructive,
        "deleting a locally edited item asks for confirmation"
    );
    assert!(
        titles(&b).contains(&"B keeps this".to_string()),
        "nothing deleted while unresolved"
    );
    assert_eq!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::KeepLocal),
        ResolveOutcome::Resolved { ops: 1 }
    );
    converge(&mut w, &mut a, &mut b).await;
    assert!(
        titles(&a).contains(&"B keeps this".to_string()),
        "resurrected on A"
    );
    assert_eq!(a.br.tree(), b.br.tree());
}

#[tokio::test]
async fn local_edit_vs_remote_delete_keep_remote_deletes_it() {
    let (mut w, mut a, mut b) = setup().await;
    retitle(&mut b, "Bookmark 1", "B edit");
    capture(&w, &mut b);
    let x = a.br.find("Bookmark 1");
    a.br.remove(&x, false).unwrap();
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    let c = open(&w, &mut b).remove(0);
    assert!(matches!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::KeepRemote),
        ResolveOutcome::Resolving { .. }
    ));
    let (_, echoes) = w.remote(&mut b, 1 << 20).await;
    assert_eq!(echoes, 0);
    converge(&mut w, &mut a, &mut b).await;
    assert!(!titles(&b).contains(&"B edit".to_string()));
    assert_eq!(a.br.tree(), b.br.tree());
    assert_eq!(all(&w, &mut b)[0].status, ConflictStatus::Resolved);
}

#[tokio::test]
async fn local_delete_vs_remote_edit_both_choices() {
    for choice in [Resolution::KeepRemote, Resolution::KeepLocal] {
        let (mut w, mut a, mut b) = setup().await;
        let x = b.br.find("Bookmark 2");
        b.br.remove(&x, false).unwrap();
        capture(&w, &mut b);
        retitle(&mut a, "Bookmark 2", "A edited");
        w.local(&mut a).await;
        pull_only(&w, &mut b).await;
        let c = open(&w, &mut b).remove(0);
        assert_eq!(c.subtype, ConflictSubtype::DeleteEdit);
        assert!(!c.local.exists && c.remote.exists);
        let keep_local = c
            .options
            .iter()
            .find(|o| o.resolution == Resolution::KeepLocal)
            .unwrap();
        assert!(keep_local.destructive);
        resolve(&w, &mut b, &c.conflict_id, choice);
        converge(&mut w, &mut a, &mut b).await;
        let has = titles(&b).contains(&"A edited".to_string());
        assert_eq!(has, choice == Resolution::KeepRemote, "{choice:?}");
        assert_eq!(a.br.tree(), b.br.tree(), "{choice:?}");
        assert_eq!(
            all(&w, &mut b)[0].status,
            ConflictStatus::Resolved,
            "{choice:?}"
        );
    }
}

#[tokio::test]
async fn restoring_a_locally_deleted_folder_brings_back_its_content() {
    let (mut w, mut a, mut b) = setup().await;
    let fa = b.br.find("Folder A");
    b.br.remove(&fa, true).unwrap(); // B deletes the folder with its 2 bookmarks
    capture(&w, &mut b);
    retitle(&mut a, "Folder A", "Folder A (A)");
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    let c = open(&w, &mut b).remove(0);
    assert_eq!(c.subtype, ConflictSubtype::DeleteEdit);
    resolve(&w, &mut b, &c.conflict_id, Resolution::KeepRemote);
    converge(&mut w, &mut a, &mut b).await;
    let t = titles(&b);
    assert!(
        t.contains(&"Folder A (A)".into())
            && t.contains(&"Bookmark 1".into())
            && t.contains(&"Bookmark 2".into()),
        "{t:?}"
    );
    assert_eq!(a.br.tree(), b.br.tree());
}

// ------------------------------------------------------------------ apply_failed / orphan

/// B adds a bookmark into Folder A (unpublished), A deletes Folder A: B keeps the folder
/// (delete_not_empty) and publishes its child; A gets the child as an orphan.
async fn folder_with_local_child() -> (World, Dev, Dev) {
    let (mut w, mut a, mut b) = setup().await;
    let fa_b = b.br.find("Folder A");
    b.br.create(&fa_b, "Only on B", Some("https://b.example/"));
    capture(&w, &mut b);
    let fa = a.br.find("Folder A");
    a.br.remove(&fa, true).unwrap();
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;
    let c = open(&w, &mut b);
    assert_eq!(c.len(), 1);
    assert_eq!(
        (c[0].kind, c[0].subtype),
        (ConflictKind::ApplyFailed, ConflictSubtype::DeleteNotEmpty)
    );
    assert_eq!(c[0].local_items, 1);
    assert_eq!(c[0].attempts, 1);
    w.remote(&mut a, 1 << 20).await;
    (w, a, b)
}

#[tokio::test]
async fn apply_failed_retry_success_after_the_folder_was_emptied() {
    let (mut w, mut a, mut b) = folder_with_local_child().await;
    let c = open(&w, &mut b).remove(0);
    // the user moves the local item out by hand, then retries
    let x = b.br.find("Only on B");
    let t = b.br.find("Synchrome Test");
    b.br.move_to(&x, &t, None);
    capture(&w, &mut b);
    assert!(matches!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::Retry),
        ResolveOutcome::Resolving { .. }
    ));
    w.remote(&mut b, 1 << 20).await;
    let t = titles(&b);
    assert!(
        !t.contains(&"Folder A".into()) && t.contains(&"Only on B".into()),
        "{t:?}"
    );
    assert_eq!(
        all(&w, &mut b)
            .iter()
            .find(|x| x.conflict_id == c.conflict_id)
            .unwrap()
            .status,
        ConflictStatus::Resolved
    );
    // A's orphan is fixed by B's move
    converge(&mut w, &mut a, &mut b).await;
    assert_eq!(a.br.tree(), b.br.tree());
    assert!(
        open(&w, &mut a).is_empty(),
        "A's orphan superseded: {:?}",
        open(&w, &mut a)
    );
}

#[tokio::test]
async fn apply_failed_retry_failure_keeps_the_conflict_and_the_child() {
    let (w, _a, mut b) = folder_with_local_child().await;
    let mut w = w;
    let c = open(&w, &mut b).remove(0);
    assert!(matches!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::Retry),
        ResolveOutcome::Resolving { .. }
    ));
    w.remote(&mut b, 1 << 20).await;
    let c2 = open(&w, &mut b).remove(0);
    assert_eq!(c2.conflict_id, c.conflict_id, "same identity");
    assert_eq!((c2.status, c2.attempts), (ConflictStatus::Unresolved, 2));
    let t = titles(&b);
    assert!(
        t.contains(&"Folder A".into()) && t.contains(&"Only on B".into()),
        "nothing lost: {t:?}"
    );
}

#[tokio::test]
async fn delete_not_empty_relocate_moves_local_items_then_deletes_the_folder() {
    let (mut w, mut a, mut b) = folder_with_local_child().await;
    let c = open(&w, &mut b).remove(0);
    assert!(matches!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::Relocate),
        ResolveOutcome::Resolving { .. }
    ));
    converge(&mut w, &mut a, &mut b).await;
    let tb = b.br.tree();
    assert!(!tb.iter().any(|n| n.1 == "Folder A"));
    assert!(
        tb.iter().any(|n| n.1 == "Only on B" && n.0 == 1),
        "moved into Synchrome Test: {tb:?}"
    );
    assert_eq!(a.br.tree(), tb);
    assert!(open(&w, &mut a).is_empty() && open(&w, &mut b).is_empty());
}

#[tokio::test]
async fn delete_not_empty_keep_folder_restores_it_on_the_other_device() {
    let (mut w, mut a, mut b) = folder_with_local_child().await;
    let c = open(&w, &mut b).remove(0);
    assert_eq!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::KeepLocal),
        ResolveOutcome::Resolved { ops: 1 }
    );
    converge(&mut w, &mut a, &mut b).await;
    assert!(titles(&a).contains(&"Folder A".into()) && titles(&a).contains(&"Only on B".into()));
    assert_eq!(a.br.tree(), b.br.tree());
    assert!(
        open(&w, &mut a).is_empty(),
        "A's orphan superseded once its folder exists"
    );
}

#[tokio::test]
async fn orphan_relocate_restore_and_accept_deletion() {
    for choice in [
        Resolution::Relocate,
        Resolution::KeepRemote,
        Resolution::KeepLocal,
    ] {
        let (mut w, mut a, mut b) = folder_with_local_child().await;
        let c = open(&w, &mut a);
        assert_eq!(c.len(), 1, "{choice:?}");
        assert_eq!(c[0].kind, ConflictKind::Orphan);
        assert!(
            !titles(&a).contains(&"Only on B".into()),
            "not created anywhere yet"
        );
        let accept = c[0]
            .options
            .iter()
            .find(|o| o.resolution == Resolution::KeepLocal)
            .unwrap();
        assert!(accept.destructive);
        resolve(&w, &mut a, &c[0].conflict_id, choice);
        w.remote(&mut a, 1 << 20).await;
        let ta = titles(&a);
        match choice {
            Resolution::Relocate => assert!(
                ta.contains(&"Only on B".into()) && !ta.contains(&"Folder A".into()),
                "{ta:?}"
            ),
            Resolution::KeepRemote => assert!(
                ta.contains(&"Only on B".into()) && ta.contains(&"Folder A".into()),
                "{ta:?}"
            ),
            _ => assert!(!ta.contains(&"Only on B".into()), "{ta:?}"),
        }
        // B: its delete_not_empty conflict either becomes moot or can now be retried
        w.remote(&mut b, 1 << 20).await;
        for c in open(&w, &mut b) {
            resolve(&w, &mut b, &c.conflict_id, Resolution::Retry);
        }
        converge(&mut w, &mut a, &mut b).await;
        assert_eq!(a.br.tree(), b.br.tree(), "{choice:?}");
        assert!(
            open(&w, &mut a).is_empty() && open(&w, &mut b).is_empty(),
            "{choice:?}"
        );
    }
}

// ------------------------------------------------------------------ persistence, order, flow

fn dev_at(w: &mut World, path: &std::path::Path) -> Dev {
    let mut d = w.device();
    d.db = StateDb::open(path).unwrap();
    d
}

#[tokio::test]
async fn conflict_and_resolution_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.sqlite");
    let mut w = World::new();
    let mut a = w.device();
    let mut b = dev_at(&mut w, &path);
    build_known(&mut a.br);
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;
    retitle(&mut b, "Bookmark 2", "B's title");
    capture(&w, &mut b);
    retitle(&mut a, "Bookmark 2", "A's title");
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    let c = open(&w, &mut b).remove(0);
    // restart: conflict persisted with its identity and snapshots
    b.db = StateDb::open(&path).unwrap();
    let c2 = open(&w, &mut b).remove(0);
    assert_eq!(c2, c);
    assert!(matches!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::KeepRemote),
        ResolveOutcome::Resolving { .. }
    ));
    // restart between the decision and the apply
    b.db = StateDb::open(&path).unwrap();
    assert_eq!(open(&w, &mut b)[0].status, ConflictStatus::Resolving);
    w.remote(&mut b, 1 << 20).await;
    b.db = StateDb::open(&path).unwrap();
    let h = all(&w, &mut b).remove(0);
    assert_eq!(
        (h.status, h.resolution),
        (ConflictStatus::Resolved, Some(Resolution::KeepRemote))
    );
    assert!(h.resolved_at.is_some());
    assert_eq!(
        h.local.title.as_deref(),
        Some("B's title"),
        "history keeps both versions"
    );
    assert_eq!(h.remote.title.as_deref(), Some("A's title"));
}

#[tokio::test]
async fn held_remote_ops_stay_in_order_and_later_ops_flow_after_resolution() {
    let (mut w, mut a, mut b) = setup().await;
    retitle(&mut b, "Bookmark 1", "B title");
    capture(&w, &mut b);
    retitle(&mut a, "Bookmark 1", "A v1");
    w.local(&mut a).await;
    retitle(&mut a, "A v1", "A v2");
    reurl(&mut a, "A v2", "https://a.example/v2");
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    let c = open(&w, &mut b);
    assert_eq!(c.len(), 1, "one conflict per entity");
    assert_eq!(c[0].held, 2);
    assert_eq!(
        c[0].remote.title.as_deref(),
        Some("A v2"),
        "held ops folded in HLC order"
    );
    assert_eq!(c[0].remote_seq, Some(9));
    let ad = a.src.device_id.to_string();
    assert_eq!(
        b.db.remote_cursor(w.profile, &ad).unwrap(),
        9,
        "the cursor moved; nothing is re-downloaded"
    );
    resolve(&w, &mut b, &c[0].conflict_id, Resolution::KeepRemote);
    w.remote(&mut b, 1 << 20).await;
    let n =
        b.br.tree()
            .into_iter()
            .find(|n| n.1 == "A v2")
            .expect("both held ops applied");
    assert_eq!(n.2.as_deref(), Some("https://a.example/v2"));
    // a later remote op on the same entity applies normally
    w.remote(&mut a, 1 << 20).await;
    retitle(&mut a, "A v2", "A v3");
    w.local(&mut a).await;
    let out = pull_only(&w, &mut b).await;
    assert!(
        matches!(out, PullOutcome::Pulled(PullStats { conflicts: 0, .. })),
        "{out:?}"
    );
    w.remote(&mut b, 1 << 20).await;
    assert!(titles(&b).contains(&"A v3".into()));
    assert_eq!(a.br.tree(), b.br.tree());
}

#[tokio::test]
async fn other_entities_keep_syncing_while_one_is_in_conflict() {
    let (mut w, mut a, mut b, c) = title_conflict().await;
    retitle(&mut a, "Bookmark 1", "Y changed by A");
    w.local(&mut a).await;
    w.remote(&mut b, 1 << 20).await;
    let t = titles(&b);
    assert!(t.contains(&"Y changed by A".into()), "Y applied: {t:?}");
    assert!(
        t.contains(&"B's title".into()),
        "X still frozen at the local value"
    );
    assert_eq!(open(&w, &mut b)[0].conflict_id, c.conflict_id);
    let st =
        b.db.status(w.profile, b.src.mapping_id)
            .unwrap()
            .download
            .unwrap();
    assert_eq!(
        (st.state, st.conflicts, st.remote_pending),
        (DownloadState::Conflict, 1, 0)
    );
}

#[tokio::test]
async fn repeated_resolution_is_idempotent() {
    let (w, _a, mut b, c) = title_conflict().await;
    assert_eq!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::KeepLocal),
        ResolveOutcome::Resolved { ops: 1 }
    );
    let n = outbox(&w, &b).len();
    assert_eq!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::KeepLocal),
        ResolveOutcome::AlreadyResolved
    );
    assert_eq!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::KeepRemote),
        ResolveOutcome::Closed {
            status: ConflictStatus::Resolved
        }
    );
    assert_eq!(outbox(&w, &b).len(), n, "no new ops");
}

#[tokio::test]
async fn same_change_on_both_sides_is_superseded_and_cannot_be_resolved() {
    let (mut w, mut a, mut b) = setup().await;
    retitle(&mut b, "Bookmark 2", "Same");
    capture(&w, &mut b);
    retitle(&mut a, "Bookmark 2", "Same");
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    assert!(open(&w, &mut b).is_empty());
    let h = all(&w, &mut b).remove(0);
    assert_eq!(h.status, ConflictStatus::Superseded);
    assert_eq!(
        resolve(&w, &mut b, &h.conflict_id, Resolution::KeepLocal),
        ResolveOutcome::Closed {
            status: ConflictStatus::Superseded
        }
    );
    // and a conflict made moot later by a local edit (the user typed the remote value)
    retitle(&mut b, "Same", "B again");
    capture(&w, &mut b);
    retitle(&mut a, "Same", "A again");
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    let c = open(&w, &mut b).remove(0);
    retitle(&mut b, "B again", "A again");
    capture(&w, &mut b);
    assert!(open(&w, &mut b).is_empty());
    assert_eq!(
        resolve(&w, &mut b, &c.conflict_id, Resolution::KeepRemote),
        ResolveOutcome::Closed {
            status: ConflictStatus::Superseded
        }
    );
    converge(&mut w, &mut a, &mut b).await;
    assert_eq!(a.br.tree(), b.br.tree());
}

#[tokio::test]
async fn a_batch_of_conflicts_is_resolved_independently() {
    let (mut w, mut a, mut b) = setup().await;
    for t in ["Bookmark 1", "Bookmark 2", "Bookmark 3"] {
        retitle(&mut b, t, &format!("{t} B"));
        retitle(&mut a, t, &format!("{t} A"));
    }
    capture(&w, &mut b);
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    let cs = open(&w, &mut b);
    assert_eq!(cs.len(), 3);
    let ids: std::collections::HashSet<_> = cs.iter().map(|c| c.conflict_id.clone()).collect();
    assert_eq!(ids.len(), 3, "distinct identities");
    let pick = |c: &ConflictView| {
        if c.local.title.as_deref() == Some("Bookmark 2 B") {
            Resolution::KeepRemote
        } else {
            Resolution::KeepLocal
        }
    };
    let items: Vec<(String, Resolution)> = cs
        .iter()
        .map(|c| (c.conflict_id.clone(), pick(c)))
        .collect();
    let out =
        b.db.resolve_conflicts(&w.account, w.profile, &items, w.tick());
    assert!(out.iter().all(|r| r.is_ok()), "{out:?}");
    converge(&mut w, &mut a, &mut b).await;
    let t = titles(&a);
    assert!(
        t.contains(&"Bookmark 1 B".into())
            && t.contains(&"Bookmark 2 A".into())
            && t.contains(&"Bookmark 3 B".into()),
        "{t:?}"
    );
    assert_eq!(a.br.tree(), b.br.tree());
    assert!(open(&w, &mut b).is_empty());
}

#[tokio::test]
async fn conflicts_of_another_account_or_profile_are_never_accepted() {
    let (w, _a, mut b, c) = title_conflict().await;
    let other_account = Uuid::new_v4().to_string();
    let r = b.db.resolve_conflict(
        &other_account,
        w.profile,
        &c.conflict_id,
        Resolution::KeepLocal,
        w.tick(),
    );
    assert!(matches!(r, Err(ResolveError::NotFound)), "{r:?}");
    let r = b.db.resolve_conflict(
        &w.account,
        Uuid::new_v4(),
        &c.conflict_id,
        Resolution::KeepLocal,
        w.tick(),
    );
    assert!(matches!(r, Err(ResolveError::NotFound)), "{r:?}");
    let r = b.db.resolve_conflict(
        &w.account,
        w.profile,
        &Uuid::new_v4().to_string(),
        Resolution::KeepLocal,
        w.tick(),
    );
    assert!(matches!(r, Err(ResolveError::NotFound)), "{r:?}");
    assert_eq!(open(&w, &mut b)[0].status, ConflictStatus::Unresolved);
}

#[tokio::test]
async fn an_emptied_folder_delete_is_superseded_and_applied() {
    let (mut w, mut a, mut b) = folder_with_local_child().await;
    let c = open(&w, &mut b).remove(0);
    let x = b.br.find("Only on B");
    let t = b.br.find("Synchrome Test");
    b.br.move_to(&x, &t, None);
    capture(&w, &mut b);
    w.remote(&mut b, 1 << 20).await;
    let h = all(&w, &mut b);
    let h = h.iter().find(|x| x.conflict_id == c.conflict_id).unwrap();
    assert_eq!(h.status, ConflictStatus::Superseded, "{:?}", h.detail);
    assert!(!titles(&b).contains(&"Folder A".into()));
    assert!(
        titles(&b).contains(&"Only on B".into()),
        "the moved item is kept"
    );
    converge(&mut w, &mut a, &mut b).await;
    assert_eq!(a.br.tree(), b.br.tree());
    assert!(open(&w, &mut a).is_empty() && open(&w, &mut b).is_empty());
}

#[tokio::test]
async fn a_remote_change_that_only_restores_the_base_does_not_conflict() {
    let (mut w, mut a, mut b) = setup().await;
    // B renames X (unpublished) ...
    retitle(&mut b, "Bookmark 2", "B's edit");
    capture(&w, &mut b);
    // ... while A renames it away and back: its newest value is B's BASE again (as when a
    // Keep-remote resolution re-asserts this device's previous value)
    retitle(&mut a, "Bookmark 2", "A tmp");
    w.local(&mut a).await;
    retitle(&mut a, "A tmp", "Bookmark 2");
    w.local(&mut a).await;
    let out = pull_only(&w, &mut b).await;
    assert!(
        matches!(out, PullOutcome::Pulled(PullStats { conflicts: 2, .. })),
        "{out:?}"
    );
    assert!(
        open(&w, &mut b).is_empty(),
        "nothing new arrived: no decision asked"
    );
    let h = all(&w, &mut b).remove(0);
    assert_eq!(h.status, ConflictStatus::Superseded);
    // B's edit wins everywhere although A's ops are newer (B re-asserted it with a fresh HLC)
    converge(&mut w, &mut a, &mut b).await;
    assert!(
        titles(&a).contains(&"B's edit".to_string()),
        "{:?}",
        titles(&a)
    );
    assert_eq!(a.br.tree(), b.br.tree());
}

#[tokio::test]
async fn keep_remote_resolution_reaching_a_device_that_edits_again_is_not_a_conflict() {
    let (mut w, mut a, mut b) = setup().await;
    retitle(&mut b, "Bookmark 1", "H1 by B");
    capture(&w, &mut b);
    retitle(&mut a, "Bookmark 1", "H1 a1");
    w.local(&mut a).await;
    retitle(&mut a, "H1 a1", "H1 a2");
    w.local(&mut a).await;
    reurl(&mut a, "H1 a2", "https://a.example/a3");
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    let c = open(&w, &mut b).remove(0);
    resolve(&w, &mut b, &c.conflict_id, Resolution::KeepRemote);
    w.remote(&mut b, 1 << 20).await; // publishes the resolution op, applies it
                                     // A edits again before it has seen B's resolution, then syncs (pull before publish)
    retitle(&mut a, "H1 a2", "H1 a4");
    capture(&w, &mut a);
    let out = pull_only(&w, &mut a).await;
    let all_a = all(&w, &mut a);
    assert!(open(&w, &mut a).is_empty(), "{out:?} {:#?}", all_a);
    converge(&mut w, &mut a, &mut b).await;
    assert!(titles(&b).contains(&"H1 a4".to_string()));
    assert_eq!(a.br.tree(), b.br.tree());
}

#[tokio::test]
async fn a_stale_remote_op_published_late_does_not_conflict_with_a_newer_local_edit() {
    let (mut w, mut a, mut b) = setup().await;
    // B edits X (unpublished, older), A edits X later and publishes: B is in conflict
    retitle(&mut b, "Bookmark 1", "old B edit");
    capture(&w, &mut b);
    retitle(&mut a, "Bookmark 1", "A one");
    w.local(&mut a).await;
    pull_only(&w, &mut b).await;
    assert_eq!(open(&w, &mut b).len(), 1);
    // A edits X again (unpublished) while B publishes its old edit (B's sync = pull + publish)
    retitle(&mut a, "A one", "A two");
    capture(&w, &mut a);
    w.remote(&mut b, 1 << 20).await;
    let out = pull_only(&w, &mut a).await;
    // B's op is older than A's already-published "A one": LWW discards it anyway
    assert!(
        matches!(out, PullOutcome::Pulled(PullStats { conflicts: 0, .. })),
        "{out:?}"
    );
    assert!(open(&w, &mut a).is_empty());
    assert!(titles(&a).contains(&"A two".to_string()));
    // B decides; everyone converges
    let c = open(&w, &mut b).remove(0);
    resolve(&w, &mut b, &c.conflict_id, Resolution::KeepRemote);
    converge(&mut w, &mut a, &mut b).await;
    assert!(open(&w, &mut a).is_empty() && open(&w, &mut b).is_empty());
    assert_eq!(a.br.tree(), b.br.tree());
}
