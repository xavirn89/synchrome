//! Extensions inventory + enabled state (slice 10): capture from a fake chrome.management,
//! publication through the shared journal, pull + LWW, explicit apply plans, user actions,
//! conflicts through the shared conflict records.

mod common;
use common::*;
use std::collections::BTreeMap;
use synchrome_state::*;
use synchrome_sync::extensions::{ExtEvent, ExtInfo};
use synchrome_sync::Op;
use uuid::Uuid;

const X: &str = "aaaabbbbccccddddeeeeffffgggghhhh";
const Y: &str = "ppppoooonnnnmmmmllllkkkkjjjjiiii";
const ME: &str = "gmpmcnknoaogpjdoonlddhpdpgkebbog";

fn info(id: &str, name: &str, install_type: &str) -> ExtInfo {
    ExtInfo {
        id: id.into(),
        name: name.into(),
        version: "1.0.0".into(),
        enabled: true,
        kind: "extension".into(),
        install_type: install_type.into(),
        may_disable: true,
        update_url: (install_type == "normal")
            .then(|| "https://clients2.google.com/service/update2/crx".to_string()),
        disabled_reason: None,
    }
}

/// chrome.management, as far as Synchrome uses it.
#[derive(Default)]
struct Mgmt {
    exts: BTreeMap<String, ExtInfo>,
    /// setEnabled / uninstall refused without a user gesture (as Chrome may do)
    gesture_required: bool,
    opened: Vec<String>,
}

impl Mgmt {
    fn add(&mut self, i: ExtInfo) {
        self.exts.insert(i.id.clone(), i);
    }
    fn items(&self) -> Vec<ExtInfo> {
        self.exts.values().cloned().collect()
    }
    fn set(&mut self, id: &str, on: bool) {
        let e = self.exts.get_mut(id).unwrap();
        e.enabled = on;
        e.disabled_reason = (!on).then(|| "unknown".into());
    }
    fn execute(&mut self, ops: &[ExtApplyOp]) -> Vec<ExtApplyResult> {
        ops.iter()
            .map(|op| {
                let r: Result<(), &str> = match &op.action {
                    ExtAction::SetEnabled { id, enabled } => match self.exts.get(id) {
                        None => Err("not_found"),
                        Some(e) if !e.may_disable => Err("protected"),
                        Some(_) if self.gesture_required => Err("gesture_required"),
                        Some(_) => {
                            self.set(id, *enabled);
                            Ok(())
                        }
                    },
                    ExtAction::Uninstall { id } => {
                        if self.gesture_required {
                            Err("gesture_required")
                        } else if self.exts.remove(id).is_some() {
                            Ok(())
                        } else {
                            Err("not_found")
                        }
                    }
                    ExtAction::OpenInstall { url, .. } => {
                        self.opened.push(url.clone());
                        Ok(())
                    }
                };
                ExtApplyResult {
                    op_id: op.op_id,
                    ok: r.is_ok(),
                    error: r.err().map(Into::into),
                }
            })
            .collect()
    }
}

struct XDev {
    d: Dev,
    m: Mgmt,
}

fn xdev(w: &mut World) -> XDev {
    XDev {
        d: w.device(),
        m: Mgmt::default(),
    }
}

fn snap(w: &World, x: &mut XDev) -> usize {
    match x
        .d
        .db
        .capture_ext_snapshot(&x.d.src, "e", 0, &x.m.items(), w.tick())
        .unwrap()
    {
        CaptureOutcome::Applied { ops, .. } => ops,
        o => panic!("{o:?}"),
    }
}

async fn sync_dev(w: &World, x: &mut XDev) {
    let sink = w.sink();
    sync(&mut x.d.db, &sink, w.profile, "h", (w.tick() / 1000) as i64)
        .await
        .unwrap();
}

/// A local change on `x`: reconcile its inventory and publish.
async fn local(w: &World, x: &mut XDev) -> usize {
    let n = snap(w, x);
    sync_dev(w, x).await;
    n
}

/// Pull, apply plans, reconcile after each (as the extension does). Returns (ops executed,
/// ops produced by the post-apply reconcile = echoes).
async fn remote(w: &World, x: &mut XDev) -> (usize, usize) {
    snap(w, x);
    sync_dev(w, x).await;
    let (mut done, mut echoes) = (0, 0);
    for _ in 0..5 {
        let plan = x.d.db.plan_ext_apply(w.profile).unwrap();
        if plan.ops.is_empty() {
            break;
        }
        let res = x.m.execute(&plan.ops);
        done += plan.ops.len();
        x.d.db.record_ext_apply(w.profile, &plan, &res, 1).unwrap();
        echoes += snap(w, x);
    }
    sync_dev(w, x).await;
    (done, echoes)
}

fn view(w: &World, x: &XDev, id: &str) -> ExtView {
    x.d.db
        .ext_views(w.profile)
        .unwrap()
        .into_iter()
        .find(|v| v.ext_id == id)
        .unwrap_or_else(|| panic!("no view for {id}"))
}

fn cloud_ext_ops(w: &World) -> Vec<Op> {
    let c = w.cloud.lock().unwrap();
    let mut out = Vec::new();
    for body in c.objects.values() {
        for l in std::str::from_utf8(body).unwrap().lines() {
            let o = Op::from_line(l).unwrap();
            if o.col == "extensions" {
                out.push(o);
            }
        }
    }
    out.sort_by(|a, b| a.hlc.cmp(&b.hlc));
    out
}

async fn two() -> (World, XDev, XDev) {
    let mut w = World::new();
    let mut a = xdev(&mut w);
    let mut b = xdev(&mut w);
    // both devices start (empty inventories) and have pulled once
    local(&w, &mut a).await;
    local(&w, &mut b).await;
    (w, a, b)
}

/// X (Web Store) installed on both, synced.
async fn both_have_x() -> (World, XDev, XDev) {
    let (w, mut a, mut b) = two().await;
    a.m.add(info(X, "Dummy X", "normal"));
    local(&w, &mut a).await;
    b.m.add(info(X, "Dummy X", "normal"));
    remote(&w, &mut b).await;
    assert_eq!(view(&w, &b, X).state, ExtState::Synced);
    (w, a, b)
}

// ------------------------------------------------------------------ capture

#[tokio::test]
async fn empty_snapshot_publishes_nothing() {
    let (w, mut a, _b) = two().await;
    assert_eq!(snap(&w, &mut a), 0);
    assert!(cloud_ext_ops(&w).is_empty());
    assert!(a.d.db.ext_views(w.profile).unwrap().is_empty());
}

#[tokio::test]
async fn snapshot_with_several_extensions_publishes_their_inventory_only() {
    let (w, mut a, _b) = two().await;
    a.m.add(info(X, "Dummy X", "normal"));
    let mut y = info(Y, "Dev Y", "development");
    y.enabled = false;
    y.disabled_reason = Some("unknown".into());
    a.m.add(y);
    assert_eq!(local(&w, &mut a).await, 2);
    let ops = cloud_ext_ops(&w);
    assert_eq!(ops.len(), 2);
    for o in &ops {
        let keys: Vec<&String> = o.fields.as_ref().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["enabled", "name", "source", "type", "version"],
            "no URLs, paths or permissions"
        );
    }
    let y_op = ops.iter().find(|o| o.id == Y).unwrap();
    assert_eq!(y_op.fields.as_ref().unwrap()["enabled"], false);
    assert_eq!(y_op.fields.as_ref().unwrap()["source"], "development");
    assert_eq!(
        local(&w, &mut a).await,
        0,
        "repeating the snapshot emits nothing"
    );
}

#[tokio::test]
async fn synchrome_itself_apps_and_policy_extensions_are_never_published() {
    let (w, mut a, _b) = two().await;
    a.m.add(info(ME, "Synchrome", "development"));
    let mut app = info(Y, "An app", "normal");
    app.kind = "hosted_app".into();
    a.m.add(app);
    let mut pol = info(X, "Policy ext", "admin");
    pol.may_disable = false;
    a.m.add(pol);
    assert_eq!(local(&w, &mut a).await, 0);
    assert!(cloud_ext_ops(&w).is_empty());
    let v = a.d.db.ext_views(w.profile).unwrap();
    assert_eq!(
        v.len(),
        1,
        "only the policy extension is listed (as protected)"
    );
    assert_eq!((v[0].ext_id.as_str(), v[0].state), (X, ExtState::Protected));
    assert!(v[0].actions.is_empty());
}

#[tokio::test]
async fn install_uninstall_enable_disable_events_become_ops() {
    let (w, mut a, _b) = two().await;
    let ev = |n, e| (n, e);
    let r =
        a.d.db
            .capture_ext_events(
                &a.d.src,
                "e2",
                &[ev(
                    1,
                    ExtEvent::Installed {
                        info: info(X, "X", "normal"),
                    },
                )],
                w.tick(),
            )
            .unwrap();
    assert!(matches!(r, CaptureOutcome::Applied { ops: 1, .. }), "{r:?}");
    let mut off = info(X, "X", "normal");
    off.enabled = false;
    off.disabled_reason = Some("unknown".into());
    let r =
        a.d.db
            .capture_ext_events(
                &a.d.src,
                "e2",
                &[ev(2, ExtEvent::Disabled { info: off })],
                w.tick(),
            )
            .unwrap();
    assert!(matches!(r, CaptureOutcome::Applied { ops: 1, .. }), "{r:?}");
    let r =
        a.d.db
            .capture_ext_events(
                &a.d.src,
                "e2",
                &[ev(
                    3,
                    ExtEvent::Enabled {
                        info: info(X, "X", "normal"),
                    },
                )],
                w.tick(),
            )
            .unwrap();
    assert!(matches!(r, CaptureOutcome::Applied { ops: 1, .. }), "{r:?}");
    let r =
        a.d.db
            .capture_ext_events(
                &a.d.src,
                "e2",
                &[ev(4, ExtEvent::Uninstalled { id: X.into() })],
                w.tick(),
            )
            .unwrap();
    assert!(matches!(r, CaptureOutcome::Applied { ops: 1, .. }), "{r:?}");
    // never seen here: an uninstall event of something else is no op
    let r =
        a.d.db
            .capture_ext_events(
                &a.d.src,
                "e2",
                &[ev(5, ExtEvent::Uninstalled { id: Y.into() })],
                w.tick(),
            )
            .unwrap();
    assert!(matches!(r, CaptureOutcome::Applied { ops: 0, .. }), "{r:?}");
    // redelivery of an applied event
    let r =
        a.d.db
            .capture_ext_events(
                &a.d.src,
                "e2",
                &[ev(4, ExtEvent::Uninstalled { id: X.into() })],
                w.tick(),
            )
            .unwrap();
    assert!(matches!(r, CaptureOutcome::Duplicate { .. }), "{r:?}");
    sync_dev(&w, &mut a).await;
    let kinds: Vec<String> = cloud_ext_ops(&w)
        .iter()
        .map(|o| match &o.fields {
            None => "delete".into(),
            Some(f) if f.len() > 1 => "install".into(),
            Some(f) => format!("enabled={}", f["enabled"]),
        })
        .collect();
    assert_eq!(
        kinds,
        ["install", "enabled=false", "enabled=true", "delete"]
    );
}

#[tokio::test]
async fn a_chromium_disable_after_a_permission_increase_is_not_published() {
    let (w, mut a, _b) = both_have_x().await;
    let n = cloud_ext_ops(&w).len();
    let e = a.m.exts.get_mut(X).unwrap();
    e.enabled = false;
    e.disabled_reason = Some("permissions_increase".into());
    assert_eq!(local(&w, &mut a).await, 0);
    assert_eq!(cloud_ext_ops(&w).len(), n);
    assert_eq!(view(&w, &a, X).state, ExtState::UserActionRequired);
}

// ------------------------------------------------------------------ remote apply

#[tokio::test]
async fn remote_disable_and_enable_are_applied_without_echo() {
    let (w, mut a, mut b) = both_have_x().await;
    a.m.set(X, false);
    assert_eq!(local(&w, &mut a).await, 1);
    let (done, echoes) = remote(&w, &mut b).await;
    assert_eq!((done, echoes), (1, 0), "one setEnabled, no echo");
    assert!(!b.m.exts[X].enabled);
    let n = cloud_ext_ops(&w).len();
    a.m.set(X, true);
    local(&w, &mut a).await;
    let (done, echoes) = remote(&w, &mut b).await;
    assert_eq!((done, echoes), (1, 0));
    assert!(b.m.exts[X].enabled);
    assert_eq!(cloud_ext_ops(&w).len(), n + 1, "B published nothing back");
    assert_eq!(view(&w, &b, X).state, ExtState::Synced);
}

#[tokio::test]
async fn may_disable_false_is_never_touched() {
    let (w, mut a, mut b) = both_have_x().await;
    b.m.exts.get_mut(X).unwrap().may_disable = false;
    snap(&w, &mut b);
    a.m.set(X, false);
    local(&w, &mut a).await;
    let (done, _) = remote(&w, &mut b).await;
    assert_eq!(done, 0, "no setEnabled attempted");
    assert!(b.m.exts[X].enabled);
    let v = view(&w, &b, X);
    assert_eq!(v.state, ExtState::Protected);
    assert!(v.actions.is_empty());
}

#[tokio::test]
async fn a_refused_apply_needs_the_user_and_completes_without_echo() {
    let (w, mut a, mut b) = both_have_x().await;
    b.m.gesture_required = true;
    a.m.set(X, false);
    local(&w, &mut a).await;
    let (done, _) = remote(&w, &mut b).await;
    assert_eq!(done, 1);
    assert!(b.m.exts[X].enabled, "refused");
    assert_eq!(view(&w, &b, X).state, ExtState::UserActionRequired);
    assert_eq!(
        b.d.db.ext_user_actions(w.profile).unwrap(),
        vec![(X.to_string(), "disable".to_string())]
    );
    // the next pulls do not hammer the browser
    assert!(b.d.db.plan_ext_apply(w.profile).unwrap().ops.is_empty());
    // the popup applies it with a gesture; the reconcile completes the apply, no echo
    b.m.set(X, false);
    let n = cloud_ext_ops(&w).len();
    assert_eq!(snap(&w, &mut b), 0);
    sync_dev(&w, &mut b).await;
    assert_eq!(cloud_ext_ops(&w).len(), n);
    assert_eq!(view(&w, &b, X).state, ExtState::Synced);
}

#[tokio::test]
async fn duplicate_events_and_restarts_reprocess_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.sqlite");
    let mut w = World::new();
    let mut a = xdev(&mut w);
    let mut b = xdev(&mut w);
    b.d.db = StateDb::open(&path).unwrap();
    local(&w, &mut a).await;
    local(&w, &mut b).await;
    a.m.add(info(X, "Dummy X", "normal"));
    local(&w, &mut a).await;
    b.m.add(info(X, "Dummy X", "normal"));
    remote(&w, &mut b).await;
    let n = cloud_ext_ops(&w).len();
    // app restart (database reopened) + extension restart (new epoch snapshot)
    b.d.db = StateDb::open(&path).unwrap();
    let r =
        b.d.db
            .capture_ext_snapshot(&b.d.src, "epoch-2", 0, &b.m.items(), w.tick())
            .unwrap();
    assert!(matches!(r, CaptureOutcome::Applied { ops: 0, .. }), "{r:?}");
    let (done, echoes) = remote(&w, &mut b).await;
    assert_eq!((done, echoes), (0, 0));
    assert_eq!(cloud_ext_ops(&w).len(), n);
    assert_eq!(view(&w, &b, X).state, ExtState::Synced);
}

#[tokio::test]
async fn offline_device_catches_up() {
    let (mut w, mut a, mut b) = both_have_x().await;
    let _ = &mut w;
    a.m.set(X, false);
    local(&w, &mut a).await;
    // B offline: pull fails, nothing applied, nothing lost
    let sink = Sink {
        offline: true,
        ..w.sink()
    };
    let out = pull(&mut b.d.db, &sink, w.profile, "h", 1).await.unwrap();
    assert!(matches!(out, PullOutcome::Failed { .. }), "{out:?}");
    assert!(b.m.exts[X].enabled);
    a.m.set(X, true);
    local(&w, &mut a).await;
    a.m.set(X, false);
    local(&w, &mut a).await;
    remote(&w, &mut b).await;
    assert!(!b.m.exts[X].enabled, "the newest state");
}

// ------------------------------------------------------------------ missing / install / removal

#[tokio::test]
async fn missing_on_b_is_never_installed_automatically() {
    let (w, mut a, mut b) = two().await;
    a.m.add(info(X, "Dummy X", "normal"));
    a.m.add(info(Y, "Dev Y", "development"));
    local(&w, &mut a).await;
    let (done, _) = remote(&w, &mut b).await;
    assert_eq!(done, 0, "nothing installed by itself");
    let vx = view(&w, &b, X);
    assert_eq!(
        (vx.state, vx.availability),
        (
            ExtState::Missing,
            synchrome_sync::extensions::Availability::Available
        )
    );
    assert_eq!(vx.actions, vec![UserExtAction::Install]);
    let vy = view(&w, &b, Y);
    assert_eq!(
        (vy.state, vy.availability),
        (
            ExtState::Missing,
            synchrome_sync::extensions::Availability::Unavailable
        )
    );
    assert!(vy.actions.is_empty());
    assert_eq!(
        b.d.db
            .ext_user_action(&w.account, w.profile, Y, UserExtAction::Install, w.tick()),
        Err(ExtActionError::NotInstallable)
    );
    // explicit Install: the store page is opened (the user installs there)
    b.d.db
        .ext_user_action(&w.account, w.profile, X, UserExtAction::Install, w.tick())
        .unwrap();
    remote(&w, &mut b).await;
    assert_eq!(
        b.m.opened,
        vec![format!("https://chromewebstore.google.com/detail/{X}")]
    );
    assert_eq!(view(&w, &b, X).state, ExtState::InstallOpened);
    // the user installs it: B converges to the synchronised state, publishing nothing
    let n = cloud_ext_ops(&w).len();
    b.m.add(info(X, "Dummy X", "normal"));
    remote(&w, &mut b).await;
    assert_eq!(view(&w, &b, X).state, ExtState::Synced);
    assert_eq!(cloud_ext_ops(&w).len(), n);
}

#[tokio::test]
async fn an_install_here_follows_the_synchronised_enabled_state() {
    let (w, mut a, mut b) = two().await;
    let mut x = info(X, "Dummy X", "normal");
    x.enabled = false;
    x.disabled_reason = Some("unknown".into());
    a.m.add(x);
    local(&w, &mut a).await;
    remote(&w, &mut b).await;
    b.m.add(info(X, "Dummy X", "normal")); // freshly installed = enabled
    let (done, echoes) = remote(&w, &mut b).await;
    assert_eq!((done, echoes), (1, 0));
    assert!(!b.m.exts[X].enabled);
}

#[tokio::test]
async fn removed_on_another_device_waits_for_an_explicit_remove() {
    let (w, mut a, mut b) = both_have_x().await;
    a.m.exts.remove(X);
    assert_eq!(local(&w, &mut a).await, 1);
    let (done, echoes) = remote(&w, &mut b).await;
    assert_eq!(
        (done, echoes),
        (0, 0),
        "not uninstalled automatically, no resurrection either"
    );
    assert!(b.m.exts.contains_key(X));
    let v = view(&w, &b, X);
    assert_eq!(v.state, ExtState::RemovedElsewhere);
    assert_eq!(v.actions, vec![UserExtAction::Remove, UserExtAction::Keep]);
    let n = cloud_ext_ops(&w).len();
    b.d.db
        .ext_user_action(&w.account, w.profile, X, UserExtAction::Remove, w.tick())
        .unwrap();
    let (done, echoes) = remote(&w, &mut b).await;
    assert_eq!((done, echoes), (1, 0));
    assert!(!b.m.exts.contains_key(X));
    assert_eq!(
        cloud_ext_ops(&w).len(),
        n,
        "the removal was already published by A"
    );
    assert!(b.d.db.ext_views(w.profile).unwrap().is_empty());
}

#[tokio::test]
async fn keep_after_a_removal_elsewhere_restores_it_in_the_synchronised_state() {
    let (w, mut a, mut b) = both_have_x().await;
    a.m.exts.remove(X);
    local(&w, &mut a).await;
    remote(&w, &mut b).await;
    b.d.db
        .ext_user_action(&w.account, w.profile, X, UserExtAction::Keep, w.tick())
        .unwrap();
    remote(&w, &mut b).await;
    remote(&w, &mut a).await;
    assert_eq!(
        view(&w, &a, X).state,
        ExtState::Missing,
        "A sees it as missing again (install available)"
    );
    assert_eq!(view(&w, &b, X).state, ExtState::Synced);
}

// ------------------------------------------------------------------ conflicts / LWW

#[tokio::test]
async fn published_concurrent_changes_merge_by_lww() {
    let (w, mut a, mut b) = both_have_x().await;
    a.m.set(X, false);
    local(&w, &mut a).await; // older
    b.m.exts.get_mut(X).unwrap().enabled = true; // no-op on B ...
    b.m.set(X, false);
    b.m.set(X, true);
    // ... B disables then re-enables after A's op was published, B pulls first then publishes
    remote(&w, &mut b).await;
    remote(&w, &mut a).await;
    let (va, vb) = (view(&w, &a, X), view(&w, &b, X));
    assert_eq!(
        va.enabled_synced, vb.enabled_synced,
        "same winner everywhere"
    );
    assert!(
        b.d.db.conflict_views(w.profile, true).unwrap().is_empty()
            || vb.state != ExtState::Conflict
    );
}

#[tokio::test]
async fn an_unpublished_toggle_vs_a_remote_uninstall_is_an_explicit_conflict() {
    for choice in [Resolution::KeepLocal, Resolution::KeepRemote] {
        let (w, mut a, mut b) = both_have_x().await;
        b.m.set(X, false);
        snap(&w, &mut b); // B disables: captured, not published
        a.m.exts.remove(X);
        local(&w, &mut a).await; // A uninstalls and publishes
        let sink = w.sink();
        let out = pull(&mut b.d.db, &sink, w.profile, "h", 1).await.unwrap();
        assert!(
            matches!(out, PullOutcome::Pulled(PullStats { conflicts: 1, .. })),
            "{out:?}"
        );
        let c = b.d.db.conflict_views(w.profile, false).unwrap();
        assert_eq!(c.len(), 1, "{choice:?}");
        assert_eq!(
            (c[0].item_kind.as_str(), c[0].kind, c[0].subtype),
            (
                "extension",
                ConflictKind::LocalPending,
                ConflictSubtype::EditDelete
            )
        );
        assert_eq!(
            (c[0].local.exists, c[0].local.enabled, c[0].remote.exists),
            (true, Some(false), false)
        );
        assert_eq!(view(&w, &b, X).state, ExtState::Conflict);
        let r =
            b.d.db
                .resolve_conflict(&w.account, w.profile, &c[0].conflict_id, choice, w.tick())
                .unwrap();
        assert!(matches!(r, ResolveOutcome::Resolved { ops: 1 }), "{r:?}");
        remote(&w, &mut b).await;
        remote(&w, &mut a).await;
        assert!(
            b.m.exts.contains_key(X),
            "never uninstalled automatically ({choice:?})"
        );
        match choice {
            Resolution::KeepLocal => {
                assert_eq!(view(&w, &b, X).state, ExtState::Synced);
                assert_eq!(
                    view(&w, &a, X).state,
                    ExtState::Missing,
                    "kept in the synchronised state"
                );
                assert!(!view(&w, &a, X).enabled_synced);
            }
            _ => assert_eq!(view(&w, &b, X).state, ExtState::RemovedElsewhere),
        }
        assert!(b.d.db.conflict_views(w.profile, false).unwrap().is_empty());
        // (an earlier record: both devices installed X independently — superseded at once)
        let all = b.d.db.conflict_views(w.profile, true).unwrap();
        let h = all
            .iter()
            .find(|c| c.subtype == ConflictSubtype::EditDelete)
            .unwrap();
        assert_eq!(
            (h.status, h.resolution),
            (ConflictStatus::Resolved, Some(choice))
        );
    }
}

#[tokio::test]
async fn a_remote_toggle_back_to_the_base_does_not_beat_an_unpublished_local_toggle() {
    let (w, mut a, mut b) = both_have_x().await;
    b.m.set(X, false);
    snap(&w, &mut b); // B disables (unpublished; its BASE is enabled)
    a.m.set(X, false);
    local(&w, &mut a).await;
    a.m.set(X, true);
    local(&w, &mut a).await; // A's newest value is B's BASE again
    let sink = w.sink();
    pull(&mut b.d.db, &sink, w.profile, "h", 1).await.unwrap();
    assert!(
        b.d.db.conflict_views(w.profile, false).unwrap().is_empty(),
        "nothing to decide"
    );
    assert_eq!(
        b.d.db.conflict_views(w.profile, true).unwrap()[0].status,
        ConflictStatus::Superseded
    );
    remote(&w, &mut b).await;
    remote(&w, &mut a).await;
    assert!(
        !a.m.exts[X].enabled && !b.m.exts[X].enabled,
        "B's change wins everywhere"
    );
}

#[tokio::test]
async fn other_accounts_and_profiles_are_refused() {
    let (w, _a, mut b) = both_have_x().await;
    let other = Uuid::new_v4().to_string();
    assert_eq!(
        b.d.db
            .ext_user_action(&other, w.profile, X, UserExtAction::Disable, w.tick()),
        Err(ExtActionError::NotFound)
    );
    assert_eq!(
        b.d.db.ext_user_action(
            &w.account,
            Uuid::new_v4(),
            X,
            UserExtAction::Disable,
            w.tick()
        ),
        Err(ExtActionError::NotFound)
    );
    assert_eq!(
        b.d.db
            .ext_user_action(&w.account, w.profile, Y, UserExtAction::Disable, w.tick()),
        Err(ExtActionError::NotFound)
    );
}

#[tokio::test]
async fn a_user_toggle_in_the_app_is_published_and_applied_here() {
    let (w, mut a, mut b) = both_have_x().await;
    b.d.db
        .ext_user_action(&w.account, w.profile, X, UserExtAction::Disable, w.tick())
        .unwrap();
    let (done, echoes) = remote(&w, &mut b).await;
    assert_eq!((done, echoes), (1, 0));
    assert!(!b.m.exts[X].enabled);
    remote(&w, &mut a).await;
    assert!(!a.m.exts[X].enabled);
}

#[tokio::test]
async fn bookmarks_and_extensions_share_one_journal_without_mixing() {
    let (w, mut a, mut b) = two().await;
    a.m.add(info(X, "Dummy X", "normal"));
    local(&w, &mut a).await;
    remote(&w, &mut b).await;
    // B's bookmark state got no stray entity from the extensions op
    assert!(b.d.db.tree(w.profile).unwrap().entities.is_empty());
    assert_eq!(view(&w, &b, X).state, ExtState::Missing);
}

#[tokio::test]
async fn remove_here_needing_a_gesture_is_finished_from_the_popup() {
    let (w, mut a, mut b) = both_have_x().await;
    a.m.exts.remove(X);
    local(&w, &mut a).await;
    remote(&w, &mut b).await;
    b.m.gesture_required = true; // chrome.management.uninstall requires a user gesture
    b.d.db
        .ext_user_action(&w.account, w.profile, X, UserExtAction::Remove, w.tick())
        .unwrap();
    remote(&w, &mut b).await;
    assert!(b.m.exts.contains_key(X), "never removed without the user");
    assert_eq!(view(&w, &b, X).state, ExtState::UserActionRequired);
    assert_eq!(
        b.d.db.ext_user_actions(w.profile).unwrap(),
        vec![(X.to_string(), "remove".to_string())]
    );
    // the popup click uninstalls it; the reconcile records it, nothing is published
    let n = cloud_ext_ops(&w).len();
    b.m.exts.remove(X);
    assert_eq!(snap(&w, &mut b), 0);
    sync_dev(&w, &mut b).await;
    assert_eq!(cloud_ext_ops(&w).len(), n);
    assert!(b.d.db.ext_views(w.profile).unwrap().is_empty());
}
