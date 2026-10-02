//! Browser preferences (slice 12): allowlisted `Preferences` paths captured from real files
//! (temp profiles), published through the shared journal, merged by LWW per path, written
//! back by the real safe writer, gated by browser kind / major / schema, conflicts through
//! the shared records.

mod common;
use common::*;
use serde_json::{json, Value};
use std::path::PathBuf;
use synchrome_browser::{apply_preferences, preferences_path, read_preferences, PrefEdit};
use synchrome_state::*;
use synchrome_sync::browser_prefs::{self as bp, Mode};
use synchrome_sync::{Op, OpKind};
use uuid::Uuid;

/// The bytes of a Chromium-like `Preferences` with unrelated content around the allowlist.
const SEED: &str = r#"{"account_info":[],"bookmark_bar":{"show_on_all_tabs":false},"browser":{"enable_spellchecking":true,"window_placement":{"bottom":900,"left":10}},"download":{"default_directory":"/Users/someone/Downloads","prompt_for_download":false},"extensions":{"settings":{}},"homepage":"https://example.com/","intl":{"selected_languages":"en-US,en"},"profile":{"content_settings":{"exceptions":{"cookies":{}}},"name":"Person 1"},"zz":{"n":1.50,"s":"\u003Cb>"}}"#;

struct PDev {
    d: Dev,
    _dir: tempfile::TempDir,
    udd: PathBuf,
    kind: String,
    /// Browser open (no capture, no write: the app only syncs).
    open: bool,
}

fn pdev(w: &mut World, kind: &str, major: &str, prefs: &str) -> PDev {
    let dir = tempfile::tempdir().unwrap();
    let udd = dir.path().to_path_buf();
    std::fs::create_dir_all(udd.join("Default")).unwrap();
    std::fs::write(preferences_path(&udd, "Default"), prefs).unwrap();
    std::fs::write(udd.join("Last Version"), major).unwrap();
    let mut d = w.device();
    d.src.identity = format!("{kind}|{}|Default", udd.display());
    PDev {
        d,
        _dir: dir,
        udd,
        kind: kind.into(),
        open: false,
    }
}

fn major(x: &PDev) -> Option<u32> {
    synchrome_browser::browser_last_version(&x.udd).and_then(|v| bp::major_of(&v))
}

fn doc(x: &PDev) -> Value {
    serde_json::from_slice(&std::fs::read(preferences_path(&x.udd, "Default")).unwrap()).unwrap()
}

fn raw(x: &PDev) -> String {
    std::fs::read_to_string(preferences_path(&x.udd, "Default")).unwrap()
}

fn at<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |d, k| d.as_object()?.get(k))
}

/// An edit the user makes in the browser (written as Chromium would, browser closed).
fn user_sets(x: &PDev, path: &str, v: Option<Value>) {
    let mut d = doc(x);
    let segs: Vec<&str> = path.split('.').collect();
    let (last, parents) = segs.split_last().unwrap();
    let mut cur = &mut d;
    for p in parents {
        cur = cur
            .as_object_mut()
            .unwrap()
            .entry(p.to_string())
            .or_insert(json!({}));
    }
    match v {
        Some(v) => {
            cur.as_object_mut().unwrap().insert(last.to_string(), v);
        }
        None => {
            cur.as_object_mut().unwrap().remove(*last);
        }
    }
    std::fs::write(
        preferences_path(&x.udd, "Default"),
        serde_json::to_vec(&d).unwrap(),
    )
    .unwrap();
}

async fn sync_dev(w: &World, x: &mut PDev) {
    sync(
        &mut x.d.db,
        &w.sink(),
        w.profile,
        "h",
        (w.tick() / 1000) as i64,
    )
    .await
    .unwrap();
}

async fn enable(w: &World, x: &mut PDev) {
    // the local profile row exists once anything was captured for the mapping (claim)
    x.d.db
        .capture_ext_snapshot(&x.d.src, "e", 0, &[], w.tick())
        .unwrap();
    sync_dev(w, x).await;
    x.d.db
        .set_prefs_mode(&w.account, w.profile, Mode::Recommended)
        .unwrap();
}

/// The app's preferences cycle: (browser closed) capture → sync → gated plan → real writer.
/// Returns (ops captured, paths written).
async fn cycle(w: &World, x: &mut PDev) -> (usize, usize) {
    if x.open {
        x.d.db.prefs_status(w.profile, "waiting", None).unwrap();
        sync_dev(w, x).await;
        return (0, 0);
    }
    let maj = major(x);
    let joining =
        x.d.db
            .prefs_config(w.profile)
            .unwrap()
            .last_capture_at
            .is_none();
    if joining {
        sync_dev(w, x).await;
    }
    let d = read_preferences(&x.udd, "Default").unwrap();
    let (vals, skipped) = bp::extract(&d, &x.kind, maj);
    let ops =
        x.d.db
            .capture_prefs(&x.d.src, &x.kind, maj, &vals, &skipped, w.tick())
            .unwrap()
            .ops;
    sync_dev(w, x).await;
    let plan = x.d.db.plan_prefs(w.profile, &x.kind, maj).unwrap();
    let raw: Vec<PrefEdit> = plan
        .edits
        .iter()
        .flat_map(|(p, v)| bp::edits_for(&x.kind, p, v.as_ref()))
        .map(|e| PrefEdit {
            path: e.path,
            value: e.value,
        })
        .collect();
    if !raw.is_empty() {
        let backups = x.udd.join("..").join("backups");
        apply_preferences(&x.udd, "Default", &raw, &bp::writable, &backups).unwrap();
        x.d.db.prefs_written(w.profile, &plan.edits, 1).unwrap();
    }
    (ops, plan.edits.len())
}

fn cloud_ops(w: &World) -> Vec<Op> {
    let c = w.cloud.lock().unwrap();
    let mut out = Vec::new();
    for body in c.objects.values() {
        for l in std::str::from_utf8(body).unwrap().lines() {
            let o = Op::from_line(l).unwrap();
            if o.col == bp::COLLECTION {
                out.push(o);
            }
        }
    }
    out.sort_by(|a, b| a.hlc.cmp(&b.hlc));
    out
}

fn view(x: &PDev, w: &World) -> PrefsView {
    x.d.db.prefs_view(w.profile).unwrap()
}

/// A (Helium 154) with SEED + its own choices, B (Helium 154) with other values; both on.
async fn two() -> (World, PDev, PDev) {
    let mut w = World::new();
    let mut a = pdev(&mut w, "helium", "154.0.8037.92", SEED);
    let mut b = pdev(&mut w, "helium", "154.0.8037.92", SEED);
    user_sets(&a, "bookmark_bar.show_on_all_tabs", Some(json!(true)));
    user_sets(&a, "download.prompt_for_download", Some(json!(true)));
    user_sets(&a, "intl.selected_languages", Some(json!("es,en")));
    user_sets(&b, "intl.selected_languages", Some(json!("fr")));
    enable(&w, &mut a).await;
    enable(&w, &mut b).await;
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    (w, a, b)
}

// ------------------------------------------------------------------ capture / publish / apply

#[tokio::test]
async fn off_by_default_reads_and_publishes_nothing() {
    let mut w = World::new();
    let mut a = pdev(&mut w, "helium", "154.0.8037.92", SEED);
    sync_dev(&w, &mut a).await;
    assert_eq!(view(&a, &w).state, PrefsState::Off);
    assert_eq!(
        a.d.db.plan_prefs(w.profile, "helium", Some(154)).unwrap(),
        PrefsPlan::default()
    );
    assert!(cloud_ops(&w).is_empty());
}

#[tokio::test]
async fn only_the_allowlist_is_published_and_the_joining_device_adopts_it() {
    let (w, a, b) = two().await;
    let ops = cloud_ops(&w);
    let ids: Vec<&str> = ops.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(
        ids.iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "bookmark_bar.show_on_all_tabs",
            "browser.enable_spellchecking",
            "download.prompt_for_download",
            "intl.selected_languages"
        ]
        .into_iter()
        .collect()
    );
    for o in &ops {
        let f = o.fields.as_ref().unwrap();
        assert_eq!(
            f.keys().cloned().collect::<Vec<_>>(),
            vec!["bk", "maj", "value"]
        );
        assert_eq!(
            (f["bk"].as_str(), f["maj"].as_u64()),
            (Some("helium"), Some(154))
        );
    }
    let all =
        serde_json::to_string(&ops.iter().map(|o| o.fields.clone()).collect::<Vec<_>>()).unwrap();
    for leak in [
        "Downloads",
        "/Users",
        "Person 1",
        "example.com",
        "window_placement",
        "cookies",
    ] {
        assert!(!all.contains(leak), "{leak}");
    }
    // B joined (pull first) and adopted A's values; its own "fr" never left B
    let d = doc(&b);
    assert_eq!(at(&d, "intl.selected_languages"), Some(&json!("es,en")));
    assert_eq!(at(&d, "download.prompt_for_download"), Some(&json!(true)));
    assert_eq!(at(&d, "bookmark_bar.show_on_all_tabs"), Some(&json!(true)));
    assert!(!ops
        .iter()
        .any(|o| o.fields.as_ref().and_then(|f| f.get("value")) == Some(&json!("fr"))));
    // unrelated bytes untouched on B: the file minus the allowlist equals the seed minus it
    let strip = |mut v: Value| {
        for s in bp::ALLOWLIST {
            let segs: Vec<&str> = s.path.split('.').collect();
            let (l, p) = segs.split_last().unwrap();
            if let Some(Value::Object(m)) = p
                .iter()
                .try_fold(&mut v, |d, k| d.as_object_mut()?.get_mut(*k))
            {
                m.remove(*l);
            }
        }
        v
    };
    assert_eq!(strip(doc(&b)), strip(serde_json::from_str(SEED).unwrap()));
    // a device whose file nobody else rewrote: every unrelated byte verbatim
    let mut w = w;
    let mut c = pdev(&mut w, "helium", "154.0.8037.92", SEED);
    enable(&w, &mut c).await;
    cycle(&w, &mut c).await;
    let got = raw(&c);
    assert!(
        got.contains(r#""zz":{"n":1.50,"s":"\u003Cb>"}"#),
        "unrelated bytes kept verbatim"
    );
    assert!(got.starts_with(r#"{"account_info":[],"bookmark_bar":{"show_on_all_tabs":true},"browser":{"enable_spellchecking":true,"window_placement":{"bottom":900,"left":10}},"download":{"default_directory":"/Users/someone/Downloads","prompt_for_download":true},"extensions":{"settings":{}},"homepage":"https://example.com/","intl":{"selected_languages":"es,en"},"profile":"#), "{got}");
    assert_eq!(view(&a, &w).state, PrefsState::Synced);
    assert_eq!(
        (view(&b, &w).state, view(&b, &w).synced),
        (PrefsState::Synced, 4)
    );
    let _ = a;
}

#[tokio::test]
async fn duplicate_snapshots_and_echo_are_zero_ops() {
    let (w, mut a, mut b) = two().await;
    let n = cloud_ops(&w).len();
    assert_eq!(cycle(&w, &mut a).await, (0, 0));
    assert_eq!(
        cycle(&w, &mut b).await,
        (0, 0),
        "what B wrote is not captured back"
    );
    assert_eq!(cycle(&w, &mut b).await, (0, 0));
    assert_eq!(cloud_ops(&w).len(), n);
}

#[tokio::test]
async fn boolean_and_string_changes_flow_both_ways_and_different_paths_merge() {
    let (w, mut a, mut b) = two().await;
    user_sets(&b, "download.prompt_for_download", Some(json!(false)));
    assert_eq!(cycle(&w, &mut b).await.0, 1);
    cycle(&w, &mut a).await;
    assert_eq!(
        at(&doc(&a), "download.prompt_for_download"),
        Some(&json!(false))
    );
    // different paths changed on both before either syncs: merge, no conflict
    user_sets(&a, "browser.enable_spellchecking", Some(json!(false)));
    user_sets(&b, "intl.selected_languages", Some(json!("de,en-GB")));
    let off = Sink {
        offline: true,
        ..w.sink()
    };
    for x in [&mut a, &mut b] {
        let d = read_preferences(&x.udd, "Default").unwrap();
        let (v, s) = bp::extract(&d, &x.kind, major(x));
        x.d.db
            .capture_prefs(&x.d.src, &x.kind, major(x), &v, &s, w.tick())
            .unwrap();
        let _ = sync(&mut x.d.db, &off, w.profile, "h", 1).await;
    }
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    cycle(&w, &mut a).await;
    for x in [&mut a, &mut b] {
        let d = doc(x);
        assert_eq!(at(&d, "browser.enable_spellchecking"), Some(&json!(false)));
        assert_eq!(at(&d, "intl.selected_languages"), Some(&json!("de,en-GB")));
        assert!(x.d.db.conflict_views(w.profile, false).unwrap().is_empty());
    }
}

#[tokio::test]
async fn published_changes_are_lww_and_a_reset_is_a_tombstone() {
    let (w, mut a, mut b) = two().await;
    user_sets(&a, "intl.selected_languages", Some(json!("it")));
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    user_sets(&b, "intl.selected_languages", Some(json!("pt-BR")));
    cycle(&w, &mut b).await;
    cycle(&w, &mut a).await;
    assert_eq!(
        at(&doc(&a), "intl.selected_languages"),
        Some(&json!("pt-BR"))
    );
    assert!(a.d.db.conflict_views(w.profile, false).unwrap().is_empty());
    // reset to default on A (Chromium drops the key): tombstone; B drops it too
    user_sets(&a, "download.prompt_for_download", None);
    assert_eq!(cycle(&w, &mut a).await.0, 1);
    assert!(cloud_ops(&w)
        .iter()
        .any(|o| o.op == OpKind::Delete && o.id == "download.prompt_for_download"));
    cycle(&w, &mut b).await;
    assert_eq!(at(&doc(&b), "download.prompt_for_download"), None);
    assert_eq!(cycle(&w, &mut b).await, (0, 0));
    // a path never seen here is not a delete
    let mut w2 = World::new();
    let mut c = pdev(
        &mut w2,
        "chrome",
        "154.0.8037.93",
        r#"{"intl":{"selected_languages":"en"}}"#,
    );
    enable(&w2, &mut c).await;
    assert_eq!(cycle(&w2, &mut c).await.0, 1);
    assert!(!cloud_ops(&w2).iter().any(|o| o.op == OpKind::Delete));
}

#[tokio::test]
async fn helium_representation_of_the_bookmark_bar() {
    let mut w = World::new();
    // Chrome A: plain boolean; Helium B: visibility_state 0 overrides show_on_all_tabs=false
    let mut a = pdev(
        &mut w,
        "chrome",
        "154.0.8037.93",
        r#"{"bookmark_bar":{"show_on_all_tabs":true}}"#,
    );
    let mut b = pdev(
        &mut w,
        "helium",
        "154.0.8037.92",
        r#"{"bookmark_bar":{"show_on_all_tabs":false,"visibility_state":0}}"#,
    );
    enable(&w, &mut a).await;
    enable(&w, &mut b).await;
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    assert_eq!(
        cycle(&w, &mut b).await,
        (0, 0),
        "B's logical value is already true"
    );
    // A hides the bar: B gets show_on_all_tabs=false and no visibility_state (Helium's own shape)
    user_sets(&a, "bookmark_bar.show_on_all_tabs", Some(json!(false)));
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    assert_eq!(doc(&b)["bookmark_bar"], json!({"show_on_all_tabs": false}));
    assert_eq!(cycle(&w, &mut b).await, (0, 0));
    // Helium shows it again through its UI (visibility_state 0): A (Chrome) gets the boolean
    user_sets(&b, "bookmark_bar.visibility_state", Some(json!(0)));
    assert_eq!(cycle(&w, &mut b).await.0, 1);
    cycle(&w, &mut a).await;
    assert_eq!(doc(&a)["bookmark_bar"], json!({"show_on_all_tabs": true}));
    assert!(
        at(&doc(&a), "bookmark_bar.visibility_state").is_none(),
        "never written on Chrome"
    );
}

// ------------------------------------------------------------------ schema / compat / version

#[tokio::test]
async fn invalid_values_are_never_published_written_or_deleted() {
    let (mut w, mut a, mut b) = two().await;
    let n = cloud_ops(&w).len();
    // local values outside the schema (an array, a string for a boolean): skipped, counted,
    // no op, and not a delete
    user_sets(&a, "intl.selected_languages", Some(json!(["es", "en"])));
    user_sets(&a, "download.prompt_for_download", Some(json!("yes")));
    assert_eq!(cycle(&w, &mut a).await.0, 0);
    assert_eq!(cloud_ops(&w).len(), n);
    let d = read_preferences(&a.udd, "Default").unwrap();
    eprintln!("DBG skipped={:?}", bp::extract(&d, "helium", Some(154)).1);
    assert_eq!(view(&a, &w).invalid, 2);
    // a remote value with a wrong type (a defective writer bypassing the schema): kept in
    // the shared state, never written into Preferences
    let mut c = pdev(&mut w, "helium", "154.0.8037.92", SEED);
    enable(&w, &mut c).await;
    cycle(&w, &mut c).await;
    let (mut bad, _) = bp::extract(
        &read_preferences(&c.udd, "Default").unwrap(),
        "helium",
        Some(154),
    );
    bad.insert("intl.selected_languages", json!("../../etc"));
    c.d.db
        .capture_prefs(
            &c.d.src,
            "helium",
            Some(154),
            &bad,
            &Default::default(),
            w.tick(),
        )
        .unwrap();
    sync_dev(&w, &mut c).await;
    let before = raw(&b);
    assert_eq!(cycle(&w, &mut b).await.1, 0);
    assert_eq!(raw(&b), before);
    assert_eq!(
        b.d.db
            .plan_prefs(w.profile, "helium", Some(154))
            .unwrap()
            .invalid,
        1
    );
}

#[tokio::test]
async fn incompatible_browser_kind_is_unsupported_and_untouched() {
    let mut w = World::new();
    let mut a = pdev(&mut w, "helium", "154.0.8037.92", SEED);
    let mut b = pdev(&mut w, "brave", "154.1.80.1", SEED);
    user_sets(&a, "intl.selected_languages", Some(json!("es")));
    enable(&w, &mut a).await;
    enable(&w, &mut b).await;
    cycle(&w, &mut a).await;
    let before = raw(&b);
    assert_eq!(cycle(&w, &mut b).await, (0, 0));
    assert_eq!(raw(&b), before, "nothing written to an unvalidated browser");
    let v = view(&b, &w);
    assert_eq!(
        (v.state, v.unsupported, v.pending),
        (PrefsState::Unsupported, 4, 0)
    );
    assert!(!cloud_ops(&w)
        .iter()
        .any(|o| o.fields.as_ref().and_then(|f| f.get("bk")) == Some(&json!("brave"))));
}

#[tokio::test]
async fn version_mismatch_waits_until_the_majors_match() {
    let (w, mut a, mut b) = two().await;
    // A updates to an unvalidated major and changes a value
    std::fs::write(a.udd.join("Last Version"), "155.0.1.1").unwrap();
    user_sets(&a, "intl.selected_languages", Some(json!("nl")));
    cycle(&w, &mut a).await;
    let before = raw(&b);
    assert_eq!(cycle(&w, &mut b).await.1, 0);
    assert_eq!(raw(&b), before);
    let v = view(&b, &w);
    assert_eq!((v.state, v.pending), (PrefsState::VersionMismatch, 1));
    // B reaches the same major: applied
    std::fs::write(b.udd.join("Last Version"), "155.0.1.1").unwrap();
    assert_eq!(cycle(&w, &mut b).await.1, 1);
    assert_eq!(at(&doc(&b), "intl.selected_languages"), Some(&json!("nl")));
    assert_eq!(view(&b, &w).state, PrefsState::Synced);
    // an older browser than the validated one: unsupported for every path
    std::fs::write(b.udd.join("Last Version"), "150.0.1.1").unwrap();
    cycle(&w, &mut b).await;
    assert_eq!(view(&b, &w).state, PrefsState::Unsupported);
}

// ------------------------------------------------------------------ conflicts

fn setting(x: &mut PDev, w: &World) -> Vec<ConflictView> {
    x.d.db
        .conflict_views(w.profile, false)
        .unwrap()
        .into_iter()
        .filter(|c| c.item_kind == "preference")
        .collect()
}

async fn offline_capture(w: &World, x: &mut PDev) {
    let d = read_preferences(&x.udd, "Default").unwrap();
    let (v, s) = bp::extract(&d, &x.kind, major(x));
    x.d.db
        .capture_prefs(&x.d.src, &x.kind, major(x), &v, &s, w.tick())
        .unwrap();
}

#[tokio::test]
async fn same_path_with_a_local_pending_change_is_an_explicit_conflict() {
    for keep_local in [true, false] {
        let (w, mut a, mut b) = two().await;
        user_sets(&b, "intl.selected_languages", Some(json!("ca")));
        offline_capture(&w, &mut b).await; // captured, unpublished
        user_sets(&a, "intl.selected_languages", Some(json!("eu")));
        cycle(&w, &mut a).await;
        cycle(&w, &mut b).await;
        let c = setting(&mut b, &w);
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].kind, c[0].subtype),
            (ConflictKind::LocalPending, ConflictSubtype::EditEdit)
        );
        assert_eq!(
            at(&doc(&b), "intl.selected_languages"),
            Some(&json!("ca")),
            "never overwritten"
        );
        assert_eq!(view(&b, &w).state, PrefsState::Conflict);
        let res = if keep_local {
            Resolution::KeepLocal
        } else {
            Resolution::KeepRemote
        };
        b.d.db
            .resolve_conflict(&w.account, w.profile, &c[0].conflict_id, res, w.tick())
            .unwrap();
        cycle(&w, &mut b).await;
        cycle(&w, &mut a).await;
        let want = if keep_local { json!("ca") } else { json!("eu") };
        assert_eq!(at(&doc(&a), "intl.selected_languages"), Some(&want));
        assert_eq!(at(&doc(&b), "intl.selected_languages"), Some(&want));
        assert!(setting(&mut b, &w).is_empty());
        assert_eq!(cycle(&w, &mut b).await, (0, 0));
    }
}

#[tokio::test]
async fn edit_vs_delete_and_delete_vs_edit() {
    // B edits while A resets to default
    let (w, mut a, mut b) = two().await;
    user_sets(&b, "download.prompt_for_download", Some(json!(false)));
    offline_capture(&w, &mut b).await;
    user_sets(&a, "download.prompt_for_download", None);
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    let c = setting(&mut b, &w);
    assert_eq!(c[0].subtype, ConflictSubtype::EditDelete);
    b.d.db
        .resolve_conflict(
            &w.account,
            w.profile,
            &c[0].conflict_id,
            Resolution::KeepLocal,
            w.tick(),
        )
        .unwrap();
    cycle(&w, &mut b).await;
    cycle(&w, &mut a).await;
    assert_eq!(
        at(&doc(&a), "download.prompt_for_download"),
        Some(&json!(false)),
        "the edit came back"
    );
    // B resets while A edits
    user_sets(&b, "intl.selected_languages", None);
    offline_capture(&w, &mut b).await;
    user_sets(&a, "intl.selected_languages", Some(json!("sv")));
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    let c = setting(&mut b, &w);
    assert_eq!(c[0].subtype, ConflictSubtype::DeleteEdit);
    b.d.db
        .resolve_conflict(
            &w.account,
            w.profile,
            &c[0].conflict_id,
            Resolution::KeepRemote,
            w.tick(),
        )
        .unwrap();
    cycle(&w, &mut b).await;
    assert_eq!(
        at(&doc(&b), "intl.selected_languages"),
        Some(&json!("sv")),
        "restored"
    );
    assert_eq!(cycle(&w, &mut b).await, (0, 0));
}

// ------------------------------------------------------------------ browser open / restart / offline / isolation

#[tokio::test]
async fn browser_open_waits_then_applies() {
    let (w, mut a, mut b) = two().await;
    user_sets(&a, "browser.enable_spellchecking", Some(json!(false)));
    cycle(&w, &mut a).await;
    b.open = true;
    let before = raw(&b);
    cycle(&w, &mut b).await;
    assert_eq!(raw(&b), before);
    let v = view(&b, &w);
    assert_eq!(
        (v.state, v.pending),
        (PrefsState::WaitingForBrowserClose, 1)
    );
    b.open = false;
    cycle(&w, &mut b).await;
    assert_eq!(
        at(&doc(&b), "browser.enable_spellchecking"),
        Some(&json!(false))
    );
    assert_eq!(view(&b, &w).state, PrefsState::Synced);
}

#[tokio::test]
async fn restart_offline_recovery_and_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.sqlite");
    let mut w = World::new();
    let mut a = pdev(&mut w, "helium", "154.0.8037.92", SEED);
    let mut b = pdev(&mut w, "helium", "154.0.8037.92", SEED);
    b.d.db = StateDb::open(&path).unwrap();
    enable(&w, &mut a).await;
    enable(&w, &mut b).await;
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    user_sets(&a, "intl.selected_languages", Some(json!("ja")));
    cycle(&w, &mut a).await;
    // B offline: nothing reaches B, nothing lost
    let off = Sink {
        offline: true,
        ..w.sink()
    };
    assert!(matches!(
        pull(&mut b.d.db, &off, w.profile, "h", 1).await.unwrap(),
        PullOutcome::Failed { .. }
    ));
    assert_eq!(
        at(&doc(&b), "intl.selected_languages"),
        Some(&json!("en-US,en"))
    );
    // app restart: config persisted; recovery converges
    b.d.db = StateDb::open(&path).unwrap();
    assert_eq!(
        b.d.db.prefs_config(w.profile).unwrap().mode,
        Mode::Recommended
    );
    cycle(&w, &mut b).await;
    assert_eq!(at(&doc(&b), "intl.selected_languages"), Some(&json!("ja")));
    // duplicate events: nothing reprocessed, nothing published
    let dup = w.cloud.lock().unwrap().events.clone();
    w.cloud.lock().unwrap().events.extend(dup);
    let n = cloud_ops(&w).len();
    assert_eq!(cycle(&w, &mut b).await, (0, 0));
    assert_eq!(cloud_ops(&w).len(), n);
}

#[tokio::test]
async fn other_accounts_and_profiles_are_refused() {
    let (w, mut a, _b) = two().await;
    let other = Uuid::new_v4().to_string();
    assert_eq!(
        a.d.db.set_prefs_mode(&other, w.profile, Mode::Off),
        Err(PrefsError::NotFound)
    );
    assert_eq!(
        a.d.db
            .set_prefs_mode(&w.account, Uuid::new_v4(), Mode::Recommended),
        Err(PrefsError::NotFound)
    );
    assert_eq!(
        a.d.db.prefs_config(w.profile).unwrap().mode,
        Mode::Recommended
    );
}
