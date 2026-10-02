//! Extension settings (slice 11): per-key chrome.storage.local/.sync of other extensions,
//! captured from (fake) LevelDB snapshots, published through the shared journal, merged by
//! LWW per key, written back version-gated, conflicts through the shared records.

mod common;
use common::*;
use std::collections::BTreeMap;
use synchrome_state::*;
use synchrome_sync::extension_settings::{Area, Mode};
use synchrome_sync::extensions::ExtInfo;
use synchrome_sync::Op;
use uuid::Uuid;

const X: &str = "aaaabbbbccccddddeeeeffffgggghhhh";
const Z: &str = "ppppoooonnnnmmmmllllkkkkjjjjiiii";

type Store = BTreeMap<String, String>;

struct SDev {
    d: Dev,
    /// extensions installed on this device (the inventory snapshot lists them all)
    installed: Vec<String>,
    /// (ext, area) -> the extension's LevelDB store as key -> JSON text
    stores: BTreeMap<(String, Area), Store>,
    version: String,
}

fn info(id: &str, version: &str) -> ExtInfo {
    ExtInfo {
        id: id.into(),
        name: "Dummy".into(),
        version: version.into(),
        enabled: true,
        kind: "extension".into(),
        install_type: "development".into(),
        may_disable: true,
        update_url: None,
        disabled_reason: None,
    }
}

fn sdev(w: &mut World, version: &str) -> SDev {
    SDev {
        d: w.device(),
        installed: vec![],
        stores: BTreeMap::new(),
        version: version.into(),
    }
}

fn store<'a>(x: &'a mut SDev, ext: &str, area: Area) -> &'a mut Store {
    x.stores.entry((ext.to_string(), area)).or_default()
}

fn set(x: &mut SDev, ext: &str, area: Area, k: &str, json: &str) {
    store(x, ext, area).insert(k.into(), json.into());
}

async fn sync_dev(w: &World, x: &mut SDev) {
    let sink = w.sink();
    sync(&mut x.d.db, &sink, w.profile, "h", (w.tick() / 1000) as i64)
        .await
        .unwrap();
}

/// The extension is installed here (inventory) and settings sync is on for it.
async fn install(w: &World, x: &mut SDev, ext: &str, mode: Mode) {
    let v = x.version.clone();
    if !x.installed.iter().any(|e| e == ext) {
        x.installed.push(ext.to_string());
    }
    let inv: Vec<ExtInfo> = x.installed.iter().map(|e| info(e, &v)).collect();
    x.d.db
        .capture_ext_snapshot(&x.d.src, "e", 0, &inv, w.tick())
        .unwrap();
    x.d.db
        .set_settings_mode(&w.account, w.profile, ext, mode, None)
        .unwrap();
    sync_dev(w, x).await;
}

/// The app's settings cycle with the browser closed: capture every enabled area, sync, then
/// write what is planned. Returns (ops captured, writes applied).
async fn cycle(w: &World, x: &mut SDev) -> (usize, usize) {
    let mut ops = 0;
    for job in x.d.db.settings_jobs(w.profile).unwrap() {
        for area in job.areas {
            let st = x
                .stores
                .get(&(job.ext_id.clone(), area))
                .cloned()
                .unwrap_or_default();
            ops +=
                x.d.db
                    .capture_settings(&x.d.src, &job.ext_id, area, &st, &job.exclude, w.tick())
                    .unwrap()
                    .ops;
        }
    }
    sync_dev(w, x).await;
    let mut writes = 0;
    for wr in x.d.db.plan_settings(w.profile).unwrap() {
        let st = store(x, &wr.ext_id, wr.area);
        for (k, v) in &wr.puts {
            st.insert(k.clone(), v.clone());
        }
        for k in &wr.deletes {
            st.remove(k);
        }
        writes += wr.puts.len() + wr.deletes.len();
        x.d.db.settings_written(w.profile, &wr, 1).unwrap();
    }
    (ops, writes)
}

fn get<'a>(x: &'a SDev, ext: &str, area: Area, k: &str) -> Option<&'a str> {
    x.stores
        .get(&(ext.to_string(), area))
        .and_then(|s| s.get(k))
        .map(String::as_str)
}

fn cloud_settings_ops(w: &World) -> Vec<Op> {
    let c = w.cloud.lock().unwrap();
    let mut out = Vec::new();
    for body in c.objects.values() {
        for l in std::str::from_utf8(body).unwrap().lines() {
            let o = Op::from_line(l).unwrap();
            if o.col == "extension_settings" {
                out.push(o);
            }
        }
    }
    out.sort_by(|a, b| a.hlc.cmp(&b.hlc));
    out
}

/// A and B both have X v1.0 with settings sync (local + sync) enabled; A has the defaults.
async fn two() -> (World, SDev, SDev) {
    let mut w = World::new();
    let mut a = sdev(&mut w, "1.0");
    let mut b = sdev(&mut w, "1.0");
    set(&mut a, X, Area::Local, "localTheme", "\"dark\"");
    set(&mut a, X, Area::Local, "localCount", "1");
    set(&mut a, X, Area::Sync, "syncMode", "\"compact\"");
    set(&mut a, X, Area::Sync, "syncEnabled", "true");
    install(&w, &mut a, X, Mode::LocalSync).await;
    install(&w, &mut b, X, Mode::LocalSync).await;
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    (w, a, b)
}

// ------------------------------------------------------------------ capture / values

#[tokio::test]
async fn empty_local_and_sync_stores_publish_nothing() {
    let mut w = World::new();
    let mut a = sdev(&mut w, "1.0");
    install(&w, &mut a, X, Mode::LocalSync).await;
    assert_eq!(cycle(&w, &mut a).await, (0, 0));
    assert!(cloud_settings_ops(&w).is_empty());
    let v = a.d.db.settings_view(w.profile, X).unwrap();
    assert_eq!(
        (v.state, v.keys_local, v.keys_sync),
        (SettingsState::Synced, 0, 0)
    );
}

#[tokio::test]
async fn every_json_type_reaches_the_other_device_identical() {
    let (w, mut a, mut b) = two().await;
    for (k, v) in [
        ("n", "null"),
        ("t", "true"),
        ("f", "-2.5"),
        ("s", "\"héllo\""),
        ("arr", "[1,\"x\",null,[true]]"),
        ("obj", "{\"z\":{\"y\":[1,2]},\"a\":\"b\"}"),
    ] {
        set(&mut a, X, Area::Local, k, v);
    }
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    assert_eq!(get(&b, X, Area::Local, "localTheme"), Some("\"dark\""));
    assert_eq!(get(&b, X, Area::Sync, "syncMode"), Some("\"compact\""));
    assert_eq!(get(&b, X, Area::Sync, "syncEnabled"), Some("true"));
    assert_eq!(
        get(&b, X, Area::Local, "arr"),
        Some("[1,\"x\",null,[true]]")
    );
    assert_eq!(
        get(&b, X, Area::Local, "obj"),
        Some("{\"a\":\"b\",\"z\":{\"y\":[1,2]}}"),
        "canonical JSON"
    );
    assert_eq!(get(&b, X, Area::Local, "f"), Some("-2.5"));
    // areas are never mixed
    assert_eq!(get(&b, X, Area::Sync, "localTheme"), None);
    // echo 0: B's next capture of what it just wrote publishes nothing
    assert_eq!(cycle(&w, &mut b).await, (0, 0));
}

#[tokio::test]
async fn delete_key_and_duplicate_snapshots() {
    let (w, mut a, mut b) = two().await;
    store(&mut a, X, Area::Local).remove("localCount");
    assert_eq!(cycle(&w, &mut a).await.0, 1);
    assert_eq!(cycle(&w, &mut a).await.0, 0, "same snapshot again: nothing");
    cycle(&w, &mut b).await;
    assert_eq!(get(&b, X, Area::Local, "localCount"), None);
    assert_eq!(
        get(&b, X, Area::Local, "localTheme"),
        Some("\"dark\""),
        "other keys kept"
    );
    let last = cloud_settings_ops(&w).pop().unwrap();
    assert_eq!(
        (last.op, last.id),
        (
            synchrome_sync::OpKind::Delete,
            format!("{X}/local/localCount")
        )
    );
}

#[tokio::test]
async fn keys_still_in_the_store_but_over_a_limit_never_become_tombstones() {
    let (w, mut a, mut b) = two().await;
    // ~5.6 MiB of keys sorting before "localCount"/"localTheme": the area budget is spent
    // before those two (already synchronised) keys are reached
    let blob = format!("\"{}\"", "b".repeat(60 * 1024));
    for i in 0..95 {
        set(&mut a, X, Area::Local, &format!("bulk{i:03}"), &blob);
    }
    // an already synchronised key whose value grows past 64 KiB
    set(
        &mut a,
        X,
        Area::Sync,
        "syncMode",
        &format!("\"{}\"", "x".repeat(70 * 1024)),
    );
    let (ops, _) = cycle(&w, &mut a).await;
    assert!(ops > 0 && ops < 95, "only what fits is published: {ops}");
    assert!(cloud_settings_ops(&w)
        .iter()
        .all(|o| o.op != synchrome_sync::OpKind::Delete));
    let v = a.d.db.settings_view(w.profile, X).unwrap();
    assert!(v.excluded_other >= 3);
    cycle(&w, &mut b).await;
    assert_eq!(get(&b, X, Area::Local, "localTheme"), Some("\"dark\""));
    assert_eq!(get(&b, X, Area::Local, "localCount"), Some("1"));
    assert_eq!(get(&b, X, Area::Sync, "syncMode"), Some("\"compact\""));
    let total: usize = b.stores[&(X.to_string(), Area::Local)]
        .iter()
        .map(|(k, v)| k.len() + v.len())
        .sum();
    assert!(total <= synchrome_sync::extension_settings::MAX_AREA_BYTES);
    // stable: nothing more on the next snapshots
    assert_eq!(cycle(&w, &mut a).await.0, 0);
    assert_eq!(cycle(&w, &mut b).await.0, 0);
}

#[tokio::test]
async fn sensitive_oversized_and_invalid_keys_are_excluded_and_kept_local() {
    let (w, mut a, mut b) = two().await;
    set(
        &mut a,
        X,
        Area::Local,
        "apiToken",
        "\"fixture-not-a-secret\"",
    );
    set(
        &mut a,
        X,
        Area::Local,
        "huge",
        &format!("\"{}\"", "x".repeat(70 * 1024)),
    );
    set(&mut a, X, Area::Local, "broken", "{not json");
    set(&mut a, X, Area::Local, "mine", "1");
    a.d.db
        .set_settings_mode(
            &w.account,
            w.profile,
            X,
            Mode::LocalSync,
            Some(vec!["mine".into()]),
        )
        .unwrap();
    assert_eq!(cycle(&w, &mut a).await.0, 0);
    let v = a.d.db.settings_view(w.profile, X).unwrap();
    assert_eq!((v.excluded_sensitive, v.excluded_other), (2, 2));
    let ids: Vec<String> = cloud_settings_ops(&w).into_iter().map(|o| o.id).collect();
    assert!(!ids.iter().any(|i| i.ends_with("apiToken")
        || i.ends_with("huge")
        || i.ends_with("broken")
        || i.ends_with("mine")));
    // B keeps its own unrelated keys untouched by writes
    set(&mut b, X, Area::Local, "bOnly", "\"b\"");
    b.d.db
        .set_settings_mode(
            &w.account,
            w.profile,
            X,
            Mode::LocalSync,
            Some(vec!["bOnly".into()]),
        )
        .unwrap();
    cycle(&w, &mut b).await;
    assert_eq!(get(&b, X, Area::Local, "bOnly"), Some("\"b\""));
}

#[tokio::test]
async fn unrelated_extensions_and_disabled_extensions_are_never_touched() {
    let (w, mut a, mut b) = two().await;
    install(&w, &mut a, Z, Mode::Off).await;
    install(&w, &mut b, Z, Mode::Off).await;
    set(&mut a, Z, Area::Local, "z", "1");
    set(&mut b, Z, Area::Local, "z", "2");
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    assert!(
        !cloud_settings_ops(&w).iter().any(|o| o.id.starts_with(Z)),
        "Off: nothing read or published"
    );
    assert_eq!(get(&b, Z, Area::Local, "z"), Some("2"));
    // Local only: the sync area is neither read nor written
    a.d.db
        .set_settings_mode(&w.account, w.profile, X, Mode::Local, None)
        .unwrap();
    set(&mut a, X, Area::Sync, "syncMode", "\"wide\"");
    assert_eq!(cycle(&w, &mut a).await.0, 0);
    assert_eq!(get(&b, X, Area::Sync, "syncMode"), Some("\"compact\""));
}

// ------------------------------------------------------------------ multi-device

#[tokio::test]
async fn changes_flow_both_ways_and_different_keys_merge() {
    let (w, mut a, mut b) = two().await;
    set(&mut a, X, Area::Local, "localTheme", "\"light\"");
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    assert_eq!(get(&b, X, Area::Local, "localTheme"), Some("\"light\""));
    set(&mut b, X, Area::Sync, "syncMode", "\"wide\"");
    cycle(&w, &mut b).await;
    cycle(&w, &mut a).await;
    assert_eq!(get(&a, X, Area::Sync, "syncMode"), Some("\"wide\""));
    // concurrent edits of different keys: both survive, no conflict
    set(&mut a, X, Area::Local, "localCount", "2");
    set(&mut b, X, Area::Local, "localTheme", "\"blue\"");
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    cycle(&w, &mut a).await;
    for d in [&a, &b] {
        assert_eq!(get(d, X, Area::Local, "localCount"), Some("2"));
        assert_eq!(get(d, X, Area::Local, "localTheme"), Some("\"blue\""));
    }
    assert!(b.d.db.conflict_views(w.profile, false).unwrap().is_empty());
}

#[tokio::test]
async fn same_key_pending_is_an_explicit_conflict_published_is_lww() {
    for choice in [Resolution::KeepLocal, Resolution::KeepRemote] {
        let (w, mut a, mut b) = two().await;
        // B captured a change (unpublished) while A changed the same key and published
        set(&mut b, X, Area::Local, "localTheme", "\"b-theme\"");
        let st = b.stores[&(X.to_string(), Area::Local)].clone();
        b.d.db
            .capture_settings(&b.d.src, X, Area::Local, &st, &[], w.tick())
            .unwrap();
        set(&mut a, X, Area::Local, "localTheme", "\"a-theme\"");
        cycle(&w, &mut a).await;
        let sink = w.sink();
        let out = pull(&mut b.d.db, &sink, w.profile, "h", 1).await.unwrap();
        assert!(
            matches!(out, PullOutcome::Pulled(PullStats { conflicts: 1, .. })),
            "{out:?}"
        );
        let c = b.d.db.conflict_views(w.profile, false).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].item_kind.as_str(), c[0].subtype),
            ("setting", ConflictSubtype::EditEdit)
        );
        assert_eq!(
            (c[0].local.value.as_deref(), c[0].remote.value.as_deref()),
            (Some("\"b-theme\""), Some("\"a-theme\""))
        );
        assert_eq!(
            b.d.db.settings_view(w.profile, X).unwrap().state,
            SettingsState::Conflict
        );
        b.d.db
            .resolve_conflict(&w.account, w.profile, &c[0].conflict_id, choice, w.tick())
            .unwrap();
        cycle(&w, &mut b).await;
        cycle(&w, &mut a).await;
        let want = if choice == Resolution::KeepLocal {
            "\"b-theme\""
        } else {
            "\"a-theme\""
        };
        assert_eq!(
            get(&a, X, Area::Local, "localTheme"),
            Some(want),
            "{choice:?}"
        );
        assert_eq!(
            get(&b, X, Area::Local, "localTheme"),
            Some(want),
            "{choice:?}"
        );
    }
    // both published: LWW, no conflict
    let (w, mut a, mut b) = two().await;
    set(&mut a, X, Area::Local, "localTheme", "\"first\"");
    cycle(&w, &mut a).await;
    set(&mut b, X, Area::Local, "localCount", "5"); // b syncs (pulls first) and then edits
    cycle(&w, &mut b).await;
    set(&mut b, X, Area::Local, "localTheme", "\"second\"");
    cycle(&w, &mut b).await;
    cycle(&w, &mut a).await;
    assert_eq!(get(&a, X, Area::Local, "localTheme"), Some("\"second\""));
    assert!(a.d.db.conflict_views(w.profile, false).unwrap().is_empty());
}

#[tokio::test]
async fn edit_vs_delete_and_delete_vs_edit_conflicts() {
    let (w, mut a, mut b) = two().await;
    // B edits (pending), A deletes
    set(&mut b, X, Area::Local, "localCount", "9");
    let st = b.stores[&(X.to_string(), Area::Local)].clone();
    b.d.db
        .capture_settings(&b.d.src, X, Area::Local, &st, &[], w.tick())
        .unwrap();
    store(&mut a, X, Area::Local).remove("localCount");
    cycle(&w, &mut a).await;
    let sink = w.sink();
    pull(&mut b.d.db, &sink, w.profile, "h", 1).await.unwrap();
    let c = b.d.db.conflict_views(w.profile, false).unwrap();
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
        get(&a, X, Area::Local, "localCount"),
        Some("9"),
        "kept and restored on A"
    );
    // B deletes (pending), A edits
    store(&mut b, X, Area::Sync).remove("syncEnabled");
    let st = b.stores[&(X.to_string(), Area::Sync)].clone();
    b.d.db
        .capture_settings(&b.d.src, X, Area::Sync, &st, &[], w.tick())
        .unwrap();
    set(&mut a, X, Area::Sync, "syncEnabled", "false");
    cycle(&w, &mut a).await;
    pull(&mut b.d.db, &sink, w.profile, "h", 1).await.unwrap();
    let c = b.d.db.conflict_views(w.profile, false).unwrap();
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
        get(&b, X, Area::Sync, "syncEnabled"),
        Some("false"),
        "restored with A's value"
    );
}

#[tokio::test]
async fn version_mismatch_never_applies_until_versions_match() {
    let mut w = World::new();
    let mut a = sdev(&mut w, "1.0");
    let mut b = sdev(&mut w, "2.0");
    set(&mut a, X, Area::Local, "localTheme", "\"dark\"");
    install(&w, &mut a, X, Mode::LocalSync).await;
    install(&w, &mut b, X, Mode::LocalSync).await;
    cycle(&w, &mut a).await;
    let (_, writes) = cycle(&w, &mut b).await;
    assert_eq!(writes, 0, "values of v1.0 are not written into v2.0");
    assert_eq!(get(&b, X, Area::Local, "localTheme"), None);
    assert_eq!(
        b.d.db.settings_view(w.profile, X).unwrap().state,
        SettingsState::VersionMismatch
    );
    // B updates to 1.0 (same version): applied
    b.version = "1.0".into();
    b.d.db
        .capture_ext_snapshot(&b.d.src, "e2", 0, &[info(X, "1.0")], w.tick())
        .unwrap();
    cycle(&w, &mut b).await;
    assert_eq!(get(&b, X, Area::Local, "localTheme"), Some("\"dark\""));
    assert_eq!(
        b.d.db.settings_view(w.profile, X).unwrap().state,
        SettingsState::Synced
    );
}

#[tokio::test]
async fn joining_device_adopts_the_synchronised_values_without_echo() {
    let mut w = World::new();
    let mut a = sdev(&mut w, "1.0");
    let mut b = sdev(&mut w, "1.0");
    set(&mut a, X, Area::Local, "localTheme", "\"dark\"");
    install(&w, &mut a, X, Mode::Local).await;
    cycle(&w, &mut a).await;
    // B already has the same default and one key of its own
    set(&mut b, X, Area::Local, "localTheme", "\"dark\"");
    set(&mut b, X, Area::Local, "bExtra", "3");
    install(&w, &mut b, X, Mode::Local).await; // pulls first (sync)
    let (ops, _) = cycle(&w, &mut b).await;
    assert_eq!(
        ops, 1,
        "only B's own new key is published; the equal one pairs"
    );
    cycle(&w, &mut a).await;
    assert_eq!(get(&a, X, Area::Local, "bExtra"), Some("3"));
    assert!(!b
        .d
        .db
        .conflict_views(w.profile, true)
        .unwrap()
        .iter()
        .any(|c| c.item_kind == "setting"));
}

#[tokio::test]
async fn restart_offline_recovery_and_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.sqlite");
    let mut w = World::new();
    let mut a = sdev(&mut w, "1.0");
    let mut b = sdev(&mut w, "1.0");
    b.d.db = StateDb::open(&path).unwrap();
    set(&mut a, X, Area::Local, "localTheme", "\"dark\"");
    install(&w, &mut a, X, Mode::Local).await;
    install(&w, &mut b, X, Mode::Local).await;
    cycle(&w, &mut a).await;
    cycle(&w, &mut b).await;
    // B offline: A changes; nothing reaches B; nothing lost
    set(&mut a, X, Area::Local, "localTheme", "\"light\"");
    cycle(&w, &mut a).await;
    let off = Sink {
        offline: true,
        ..w.sink()
    };
    let out = pull(&mut b.d.db, &off, w.profile, "h", 1).await.unwrap();
    assert!(matches!(out, PullOutcome::Failed { .. }));
    assert_eq!(get(&b, X, Area::Local, "localTheme"), Some("\"dark\""));
    // app restart, then recovery
    b.d.db = StateDb::open(&path).unwrap();
    cycle(&w, &mut b).await;
    assert_eq!(get(&b, X, Area::Local, "localTheme"), Some("\"light\""));
    // duplicate events presented again: nothing reprocessed
    let dup = w.cloud.lock().unwrap().events.clone();
    w.cloud.lock().unwrap().events.extend(dup);
    let n = cloud_settings_ops(&w).len();
    assert_eq!(cycle(&w, &mut b).await, (0, 0));
    assert_eq!(cloud_settings_ops(&w).len(), n);
}

#[tokio::test]
async fn a_remote_value_waits_while_the_browser_is_open_then_applies() {
    let (w, mut a, mut b) = two().await;
    set(&mut a, X, Area::Local, "localTheme", "\"light\"");
    cycle(&w, &mut a).await;
    // browser open on B: the app only syncs (no capture, no write)
    b.d.db
        .settings_status(w.profile, X, "waiting", None)
        .unwrap();
    sync_dev(&w, &mut b).await;
    assert_eq!(get(&b, X, Area::Local, "localTheme"), Some("\"dark\""));
    let v = b.d.db.settings_view(w.profile, X).unwrap();
    assert_eq!(
        (v.state, v.pending),
        (SettingsState::WaitingForBrowserClose, 1)
    );
    // the extension changed another key meanwhile; the browser closes; the cycle captures it
    // first, and the remote value is written (the stale store value is not a local decision)
    set(&mut b, X, Area::Local, "localCount", "4");
    b.d.db
        .settings_status(w.profile, X, "synced", None)
        .unwrap();
    let (ops, writes) = cycle(&w, &mut b).await;
    assert_eq!((ops, writes), (1, 1));
    assert_eq!(get(&b, X, Area::Local, "localTheme"), Some("\"light\""));
    cycle(&w, &mut a).await;
    assert_eq!(get(&a, X, Area::Local, "localCount"), Some("4"));
}

#[tokio::test]
async fn other_accounts_and_profiles_are_refused() {
    let (w, mut a, _b) = two().await;
    let other = Uuid::new_v4().to_string();
    assert_eq!(
        a.d.db
            .set_settings_mode(&other, w.profile, X, Mode::Off, None),
        Err(SettingsError::NotFound)
    );
    assert_eq!(
        a.d.db
            .set_settings_mode(&w.account, Uuid::new_v4(), X, Mode::Off, None),
        Err(SettingsError::NotFound)
    );
    assert_eq!(
        a.d.db
            .set_settings_mode(&w.account, w.profile, Z, Mode::Local, None),
        Err(SettingsError::NotFound),
        "not an extension of this profile"
    );
}
