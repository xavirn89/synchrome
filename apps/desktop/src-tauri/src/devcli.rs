//! Development-only CLI (compiled only with `debug_assertions`) for end-to-end validation.
//! Runs the SAME code paths as the app (Keychain session, cloud client, local token write,
//! mapping views) without a window. Never prints passwords, JWTs or full mapping tokens.

use crate::{bookmarks, local_token, mapping_views, write_token_for_mapping, AppState};
use std::path::PathBuf;
use synchrome_bridge::token_sha256;
use synchrome_types::{BrowserKind, MAPPING_TOKEN_STORAGE_KEY, SYNCHROME_EXTENSION_ID};
use uuid::Uuid;

fn creds(path: &str, who: &str) -> Result<(String, String), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let get = |k: String| {
        text.lines()
            .map(|l| l.trim().trim_start_matches("export ").trim())
            .find_map(|l| {
                l.strip_prefix(&format!("{k}="))
                    .map(|v| v.trim().trim_matches('"').to_string())
            })
            .unwrap_or_default()
    };
    Ok((
        get(format!("USER_{who}_EMAIL")),
        get(format!("USER_{who}_PASSWORD")),
    ))
}

fn out(v: serde_json::Value) -> i32 {
    println!("{v}");
    0
}
fn fail(msg: impl std::fmt::Display) -> i32 {
    println!("{}", serde_json::json!({ "error": msg.to_string() }));
    1
}

pub fn run(args: &[String]) -> i32 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(run_async(args))
}

async fn run_async(args: &[String]) -> i32 {
    let state = AppState::new();
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    match args.first().map(String::as_str).unwrap_or("") {
        "--dev-sign-in" => {
            let (email, pw) = match creds(&arg(1), &arg(2)) {
                Ok(c) => c,
                Err(e) => return fail(e),
            };
            match state.auth() {
                Ok(m) => match m.sign_in(&email, &pw).await {
                    Ok(st) => out(serde_json::to_value(st).unwrap()),
                    Err(e) => fail(e),
                },
                Err(e) => fail(e),
            }
        }
        "--dev-sign-out" => match state.auth() {
            Ok(m) => {
                let r = m.sign_out().await;
                out(serde_json::json!({ "signed_out": true, "remote": r.is_ok() }))
            }
            Err(e) => fail(e),
        },
        "--dev-account" => match state.auth() {
            Ok(m) => out(serde_json::to_value(m.account_status()).unwrap()),
            Err(e) => fail(e),
        },
        // Creates a cloud profile + mapping WITHOUT the "detected local profile" guard, so a
        // disposable --user-data-dir profile can be used. Debug builds only.
        "--dev-create-mapping" => {
            let (name, kind, dir) = (arg(1), arg(2), arg(3));
            let kind: BrowserKind = match serde_json::from_value(serde_json::json!(kind)) {
                Ok(k) => k,
                Err(e) => return fail(e),
            };
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let (cloud, me) = match (state.cloud(), state.device()) {
                (Ok(c), Ok(d)) => (c, d.clone()),
                _ => return fail("not configured"),
            };
            if let Err(e) = cloud.upsert_device(&session, &me).await {
                return fail(e);
            }
            let profile = match cloud.create_profile(&session, &name).await {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            match cloud
                .map_local_profile(&session, profile.id, me.device_id, kind, &dir)
                .await
            {
                Ok(m) => out(serde_json::json!({
                    "mapping_id": m.id, "profile_id": m.profile_id, "device_id": m.device_id,
                    "token_hint": m.mapping_token.hint(), "token_state": m.token_state,
                })),
                Err(e) => {
                    let _ = cloud.delete_profile(&session, profile.id).await;
                    fail(e)
                }
            }
        }
        // Delete THIS device's row (cascades to its mappings) and forget its local bookmarks state.
        "--dev-delete-this-device" => {
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let (cloud, me) = match (state.cloud(), state.device()) {
                (Ok(c), Ok(d)) => (c, d.clone()),
                _ => return fail("not configured"),
            };
            let deleted = cloud.delete_device(&session, me.device_id).await;
            if let Ok(mut db) = bookmarks::open_db() {
                for p in db.profile_ids().unwrap_or_default() {
                    let _ = db.forget_profile(p);
                }
            }
            out(serde_json::json!({ "device_id": me.device_id, "deleted": deleted.is_ok() }))
        }
        // Map a local (disposable) profile to an EXISTING cloud profile (second device).
        "--dev-map-existing" => {
            let (Ok(pid), kind, dir) = (Uuid::parse_str(&arg(1)), arg(2), arg(3)) else {
                return fail("bad profile id");
            };
            let kind: BrowserKind = match serde_json::from_value(serde_json::json!(kind)) {
                Ok(k) => k,
                Err(e) => return fail(e),
            };
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let (cloud, me) = match (state.cloud(), state.device()) {
                (Ok(c), Ok(d)) => (c, d.clone()),
                _ => return fail("not configured"),
            };
            if let Err(e) = cloud.upsert_device(&session, &me).await {
                return fail(e);
            }
            match cloud
                .map_local_profile(&session, pid, me.device_id, kind, &dir)
                .await
            {
                Ok(m) => out(serde_json::json!({
                    "mapping_id": m.id, "profile_id": m.profile_id, "device_id": m.device_id,
                    "token_hint": m.mapping_token.hint(), "token_state": m.token_state,
                })),
                Err(e) => fail(e),
            }
        }
        "--dev-write-token" => {
            let (id, udd) = (arg(1), PathBuf::from(arg(2)));
            let Ok(id) = Uuid::parse_str(&id) else {
                return fail("bad mapping id");
            };
            match write_token_for_mapping(&state, id, Some(udd.clone())).await {
                Ok((view, report)) => {
                    // Inspect the pre-write backup (keys only; never the token value).
                    let backup = report
                        .backup_path
                        .as_deref()
                        .map(synchrome_browser::read_store_copy);
                    let (backup_keys, backup_has_token) = match &backup {
                        Some(Ok(kv)) => (
                            kv.keys()
                                .filter(|k| k.as_str() != MAPPING_TOKEN_STORAGE_KEY)
                                .cloned()
                                .collect::<Vec<_>>(),
                            kv.contains_key(MAPPING_TOKEN_STORAGE_KEY),
                        ),
                        _ => (vec![], false),
                    };
                    let mut v = serde_json::to_value(view).unwrap();
                    v["write"] = serde_json::json!({
                        "created": report.created,
                        "preserved_keys": report.preserved_keys,
                        "backup_exists": report.backup_path.as_ref().is_some_and(|p| p.exists()),
                        "backup_outside_profile": report.backup_path.as_ref().is_some_and(|p| !p.starts_with(&udd)),
                        "backup_keys": backup_keys,
                        "backup_has_token": backup_has_token,
                    });
                    out(v)
                }
                Err(e) => fail(e),
            }
        }
        // Writes a fresh random, UNMAPPED token with the production writer (negative tests).
        "--dev-seed-random-token" => {
            let (udd, dir) = (PathBuf::from(arg(1)), arg(2));
            let token = match synchrome_cloud::MappingToken::generate() {
                Ok(t) => t,
                Err(e) => return fail(e),
            };
            let backups = match local_token::app_data_dir() {
                Ok(d) => d.join("backups").join("ext-storage"),
                Err(e) => return fail(e),
            };
            let value = serde_json::to_string(token.expose()).unwrap();
            match synchrome_browser::write_extension_setting(
                &udd,
                &dir,
                SYNCHROME_EXTENSION_ID,
                MAPPING_TOKEN_STORAGE_KEY,
                &value,
                &backups,
            ) {
                Ok(r) => out(
                    serde_json::json!({ "token_hint": token.hint(), "created": r.created, "preserved_keys": r.preserved_keys }),
                ),
                Err(e) => fail(e),
            }
        }
        "--dev-install-host" => {
            let exe = std::env::current_exe().unwrap();
            match synchrome_browser::install_native_host(&PathBuf::from(arg(1)), &exe) {
                Ok(p) => out(serde_json::json!({ "manifest": p, "host_path": exe })),
                Err(e) => fail(e),
            }
        }
        "--dev-list-mappings" => {
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let all = match state.cloud() {
                Ok(c) => match c.list_mappings(&session).await {
                    Ok(m) => m,
                    Err(e) => return fail(e),
                },
                Err(e) => return fail(e),
            };
            match mapping_views(&state, &session, all).await {
                Ok(v) => out(serde_json::to_value(v).unwrap()),
                Err(e) => fail(e),
            }
        }
        // Reads the extension's own store (from a copy) and reports its status record, and
        // only WHETHER a mapping token is present.
        "--dev-ext-status" => {
            let (udd, dir) = (PathBuf::from(arg(1)), arg(2));
            match synchrome_browser::read_extension_settings(&udd, &dir, SYNCHROME_EXTENSION_ID) {
                Ok(kv) => out(serde_json::json!({
                    "has_mapping_token": kv.contains_key(MAPPING_TOKEN_STORAGE_KEY),
                    // 12-hex prefix of SHA-256(token): lets a test compare tokens without seeing one.
                    "token_sha256_prefix": kv.get(MAPPING_TOKEN_STORAGE_KEY)
                        .and_then(|v| serde_json::from_str::<String>(v).ok())
                        .map(|t| token_sha256(&t)[..12].to_string()),
                    "status": kv.get("synchromeStatus").and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
                    "keys": kv.keys().filter(|k| k.as_str() != MAPPING_TOKEN_STORAGE_KEY).collect::<Vec<_>>(),
                })),
                Err(e) => fail(e),
            }
        }
        "--dev-registry" => {
            let Ok(id) = Uuid::parse_str(&arg(1)) else {
                return fail("bad mapping id");
            };
            match local_token::registry().find_by_mapping(id) {
                Some(r) => out(serde_json::json!({
                    "found": true, "state": r.state, "account_id": r.account_id,
                    "token_sha256_prefix": r.token_sha256[..12].to_string(),
                    "last_error": r.last_error,
                })),
                None => out(serde_json::json!({ "found": false })),
            }
        }
        "--dev-register-device" => {
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let (cloud, me) = match (state.cloud(), state.device()) {
                (Ok(c), Ok(d)) => (c, d.clone()),
                _ => return fail("not configured"),
            };
            match cloud.upsert_device(&session, &me).await {
                Ok(d) => {
                    out(serde_json::json!({ "device_id": d.device_id, "is_current": d.is_current }))
                }
                Err(e) => fail(e),
            }
        }
        // Counts what the signed-in account can see (RLS): profiles, mappings, devices.
        "--dev-inventory" => {
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let (cloud, me) = match (state.cloud(), state.device()) {
                (Ok(c), Ok(d)) => (c, d.clone()),
                _ => return fail("not configured"),
            };
            let profiles = cloud.list_profiles(&session).await.map(|v| v.len());
            let mappings = cloud.list_mappings(&session).await.map(|v| v.len());
            let devices = cloud
                .list_devices(&session, me.device_id)
                .await
                .map(|v| v.into_iter().map(|d| d.device_id).collect::<Vec<_>>());
            match (profiles, mappings, devices) {
                (Ok(p), Ok(m), Ok(d)) => out(serde_json::json!({
                    "user_id": session.user_id, "this_device": me.device_id,
                    "profiles": p, "mappings": m, "devices": d,
                })),
                _ => fail("inventory query failed"),
            }
        }
        // ---- bookmarks (slice 7) ----
        "--dev-bookmarks-status" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("bad mapping id");
            };
            let db = match bookmarks::open_db() {
                Ok(d) => d,
                Err(e) => return fail(e),
            };
            match db.profile_for_mapping(mid) {
                Ok(Some(pid)) => match db.status(pid, mid) {
                    Ok(st) => out(serde_json::json!({ "profile_id": pid, "status": st })),
                    Err(e) => fail(e),
                },
                Ok(None) => out(serde_json::json!({ "profile_id": null, "status": null })),
                Err(e) => fail(e),
            }
        }
        "--dev-publish" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("bad mapping id");
            };
            let pid = match bookmarks::open_db()
                .and_then(|d| d.profile_for_mapping(mid).map_err(|e| e.to_string()))
            {
                Ok(Some(p)) => p,
                Ok(None) => return fail("no local bookmarks state for this mapping"),
                Err(e) => return fail(e),
            };
            match bookmarks::publish_profile(&state, pid).await {
                Ok(o) => out(serde_json::json!({
                    "outcome": format!("{o:?}"),
                    "status": bookmarks::status_for_mapping(mid, pid),
                })),
                Err(e) => fail(e),
            }
        }
        // Publication with a fault injected AFTER the chunk upload and BEFORE the checkpoint
        // (the store_events insert fails): proves the checkpoint does not advance and that
        // the retry re-sends the very same sealed chunk.
        "--dev-publish-fault" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("bad mapping id");
            };
            let pid = match bookmarks::open_db()
                .and_then(|d| d.profile_for_mapping(mid).map_err(|e| e.to_string()))
            {
                Ok(Some(p)) => p,
                Ok(None) => return fail("no local bookmarks state for this mapping"),
                Err(e) => return fail(e),
            };
            let sink = FaultAfterUpload {
                inner: bookmarks::CloudSink::new(&state),
                uploads: Default::default(),
            };
            match bookmarks::publish_with(&sink, pid).await {
                Ok(o) => out(serde_json::json!({
                    "outcome": format!("{o:?}"),
                    "uploads_before_fault": sink.uploads.load(std::sync::atomic::Ordering::Relaxed),
                    "status": bookmarks::status_for_mapping(mid, pid),
                })),
                Err(e) => fail(e),
            }
        }
        // Publication whose cached access token is deliberately invalid: the first cloud call
        // gets 401, the sink force-refreshes the real session and retries.
        "--dev-publish-401" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("bad mapping id");
            };
            let pid = match bookmarks::open_db()
                .and_then(|d| d.profile_for_mapping(mid).map_err(|e| e.to_string()))
            {
                Ok(Some(p)) => p,
                Ok(None) => return fail("no local bookmarks state for this mapping"),
                Err(e) => return fail(e),
            };
            let mut bad = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let before = token_sha256(bad.refresh_token.expose())[..12].to_string();
            bad.access_token = synchrome_auth::Secret::new("eyJinvalid.invalid.invalid");
            let sink = bookmarks::CloudSink::with_session(&state, bad);
            let r = bookmarks::publish_with(&sink, pid).await;
            let after = state
                .session()
                .await
                .map(|s| token_sha256(s.refresh_token.expose())[..12].to_string())
                .unwrap_or_default();
            match r {
                Ok(o) => out(serde_json::json!({
                    "outcome": format!("{o:?}"),
                    "forced_refreshes": sink.refreshes(),
                    "refresh_token_rotated": before != after,
                    "status": bookmarks::status_for_mapping(mid, pid),
                })),
                Err(e) => fail(e),
            }
        }
        "--dev-sync" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("bad mapping id");
            };
            let pid = match bookmarks::open_db()
                .and_then(|d| d.profile_for_mapping(mid).map_err(|e| e.to_string()))
            {
                Ok(Some(p)) => p,
                Ok(None) => return fail("no local bookmarks state for this mapping"),
                Err(e) => return fail(e),
            };
            match bookmarks::sync_profile(&state, pid).await {
                Ok((pulled, published)) => out(serde_json::json!({
                    "pull": format!("{pulled:?}"),
                    "publish": format!("{published:?}"),
                    "status": bookmarks::status_for_mapping(mid, pid),
                })),
                Err(e) => fail(e),
            }
        }
        // GAP harness: remove one store_events row of a device (the chunk stays) and print it,
        // so it can be put back with --dev-insert-event. Own account only (RLS).
        "--dev-delete-event" => {
            let (Ok(pid), Ok(did), Ok(seq_to)) = (
                Uuid::parse_str(&arg(1)),
                Uuid::parse_str(&arg(2)),
                arg(3).parse::<i64>(),
            ) else {
                return fail("usage: --dev-delete-event <profile> <device> <seq_to>");
            };
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let cloud = match state.cloud() {
                Ok(c) => c,
                Err(e) => return fail(e),
            };
            let row = cloud
                .list_store_events(&session, pid)
                .await
                .ok()
                .and_then(|v| {
                    v.into_iter()
                        .find(|e| e.device_id == did && e.seq_to == seq_to)
                });
            match cloud.delete_store_event(&session, pid, did, seq_to).await {
                Ok(deleted) => out(serde_json::json!({ "deleted": deleted, "event": row })),
                Err(e) => fail(e),
            }
        }
        "--dev-insert-event" => {
            let ev: synchrome_cloud::StoreEvent = match serde_json::from_str(&arg(1)) {
                Ok(e) => e,
                Err(e) => return fail(e),
            };
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            match state.cloud() {
                Ok(c) => match c.record_store_event(&session, &ev).await {
                    Ok(()) => out(serde_json::json!({ "inserted": true })),
                    Err(e) => fail(e),
                },
                Err(e) => fail(e),
            }
        }
        // DUPLICATE harness: pull with every store_event presented twice (and once more after
        // the real list), as a replayed / duplicated delivery would.
        "--dev-pull-duplicates" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("bad mapping id");
            };
            let pid = match bookmarks::open_db()
                .and_then(|d| d.profile_for_mapping(mid).map_err(|e| e.to_string()))
            {
                Ok(Some(p)) => p,
                Ok(None) => return fail("no local bookmarks state for this mapping"),
                Err(e) => return fail(e),
            };
            let sink = DuplicateEvents {
                inner: bookmarks::CloudSink::new(&state),
            };
            let mut db = match bookmarks::open_db() {
                Ok(d) => d,
                Err(e) => return fail(e),
            };
            let pending = |db: &synchrome_state::StateDb| {
                db.status(pid, mid)
                    .ok()
                    .and_then(|s| s.download)
                    .map(|d| d.remote_pending)
            };
            let before = pending(&db);
            match synchrome_state::pull(&mut db, &sink, pid, "dup-harness", bookmarks::now_unix())
                .await
            {
                Ok(o) => {
                    let after = pending(&db);
                    out(
                        serde_json::json!({ "pull": format!("{o:?}"), "remote_pending_before": before, "remote_pending_after": after }),
                    )
                }
                Err(e) => fail(e),
            }
        }
        // Open conflicts (entity ids and kinds only) and remote cursors.
        "--dev-conflicts" => {
            let Ok(pid) = Uuid::parse_str(&arg(1)) else {
                return fail("bad profile id");
            };
            match bookmarks::open_db()
                .and_then(|d| d.open_conflicts(pid).map_err(|e| e.to_string()))
            {
                Ok(c) => out(serde_json::json!({
                    "conflicts": c.iter().map(|(sid, kind, dev, seq)| serde_json::json!({"id": sid, "kind": kind, "remote_device": dev, "remote_seq": seq})).collect::<Vec<_>>(),
                })),
                Err(e) => fail(e),
            }
        }
        // Conflict records of a mapping's profile (URLs reduced to their host).
        "--dev-conflict-views" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("usage: --dev-conflict-views <mapping> [all]");
            };
            match bookmarks::conflicts_for_mapping(&state, mid, arg(2) == "all") {
                Ok(v) => {
                    let mut j = serde_json::to_value(&v).unwrap_or_default();
                    host_only(&mut j);
                    out(serde_json::json!({ "conflicts": j }))
                }
                Err(e) => fail(e),
            }
        }
        // Extensions of a mapping's profile: this device vs the synchronised state.
        "--dev-ext-views" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("usage: --dev-ext-views <mapping>");
            };
            match bookmarks::extensions_for_mapping(&state, mid) {
                Ok(v) => out(serde_json::json!({ "extensions": v })),
                Err(e) => fail(e),
            }
        }
        // An explicit user action, exactly as the app's buttons do.
        "--dev-ext-action" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("usage: --dev-ext-action <mapping> <ext_id> <install|enable|disable|remove|keep|retry>");
            };
            let action = match serde_json::from_value::<synchrome_state::UserExtAction>(
                serde_json::json!(arg(3)),
            ) {
                Ok(a) => a,
                Err(_) => return fail("bad action"),
            };
            match bookmarks::extension_action(&state, mid, &arg(2), action).await {
                Ok(()) => out(serde_json::json!({ "ok": true })),
                Err(e) => fail(e),
            }
        }
        // Keys of one extension store area (browser must be closed): names + value digests only.
        "--dev-read-ext-area" => {
            let area = match arg(4).as_str() {
                "sync" => synchrome_browser::Area::Sync,
                _ => synchrome_browser::Area::Local,
            };
            match synchrome_browser::read_extension_area(
                std::path::Path::new(&arg(1)),
                &arg(2),
                &arg(3),
                area,
            ) {
                Ok(kv) => out(
                    serde_json::json!({ "keys": kv.iter().map(|(k, v)| (k.clone(), synchrome_bridge::token_sha256(v)[..12].to_string())).collect::<std::collections::BTreeMap<_, _>>() }),
                ),
                Err(e) => fail(e),
            }
        }
        // Extension settings: mode (off | local | local_sync), one cycle now, per-extension state.
        "--dev-settings-mode" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail(
                    "usage: --dev-settings-mode <mapping> <ext_id> <off|local|local_sync>",
                );
            };
            let mode = synchrome_sync::extension_settings::Mode::parse(&arg(3));
            // optional 4th argument: the per-extension denylist as a JSON array of keys/prefix*
            let exclude = (!arg(4).is_empty())
                .then(|| serde_json::from_str::<Vec<String>>(&arg(4)).ok())
                .flatten();
            match crate::ext_settings::set_settings_mode(&state, mid, &arg(2), mode, exclude).await
            {
                Ok(()) => out(serde_json::json!({ "ok": true, "mode": mode.as_str() })),
                Err(e) => fail(e),
            }
        }
        "--dev-settings-cycle" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("usage: --dev-settings-cycle <mapping>");
            };
            match crate::ext_settings::settings_sync_now(&state, mid).await {
                Ok(r) => out(serde_json::json!({ "report": r })),
                Err(e) => fail(e),
            }
        }
        "--dev-settings-views" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("usage: --dev-settings-views <mapping>");
            };
            match crate::ext_settings::settings_for_mapping(&state, mid) {
                Ok(v) => out(serde_json::json!({ "settings": v })),
                Err(e) => fail(e),
            }
        }
        // Browser preferences: mode (off | recommended), one cycle now, the profile's state.
        "--dev-prefs-mode" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("usage: --dev-prefs-mode <mapping> <off|recommended>");
            };
            let mode = synchrome_sync::browser_prefs::Mode::parse(&arg(2));
            match crate::browser_prefs::set_prefs_mode(&state, mid, mode).await {
                Ok(()) => out(serde_json::json!({ "ok": true, "mode": mode.as_str() })),
                Err(e) => fail(e),
            }
        }
        "--dev-prefs-cycle" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("usage: --dev-prefs-cycle <mapping>");
            };
            match crate::browser_prefs::prefs_sync_now(&state, mid).await {
                Ok(r) => out(serde_json::json!({ "report": r })),
                Err(e) => fail(e),
            }
        }
        "--dev-prefs-view" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("usage: --dev-prefs-view <mapping>");
            };
            match crate::browser_prefs::prefs_for_mapping(&state, mid) {
                Ok(v) => out(serde_json::json!({ "prefs": v })),
                Err(e) => fail(e),
            }
        }
        // Write logical allowlisted values into a closed profile's Preferences through the
        // product writer (no cloud): --dev-write-prefs <udd> <profile> <kind> <{"path": value|null}>
        "--dev-write-prefs" => {
            let udd = std::path::PathBuf::from(arg(1));
            let Ok(map) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&arg(4))
            else {
                return fail("usage: --dev-write-prefs <udd> <profile> <kind> <json object>");
            };
            let raw: Vec<synchrome_browser::PrefEdit> = map
                .iter()
                .flat_map(|(p, v)| {
                    synchrome_sync::browser_prefs::edits_for(
                        &arg(3),
                        p,
                        (!v.is_null()).then_some(v),
                    )
                })
                .map(|e| synchrome_browser::PrefEdit {
                    path: e.path,
                    value: e.value,
                })
                .collect();
            let backups = match crate::local_token::app_data_dir() {
                Ok(d) => d.join("backups").join("browser-preferences"),
                Err(e) => return fail(e),
            };
            match synchrome_browser::apply_preferences(
                &udd,
                &arg(2),
                &raw,
                &synchrome_sync::browser_prefs::writable,
                &backups,
            ) {
                Ok(r) => out(
                    serde_json::json!({ "changed": r.changed, "backup": r.backup_path.is_some(), "bytes_before": r.bytes_before, "bytes_after": r.bytes_after }),
                ),
                Err(e) => fail(e),
            }
        }
        // A closed profile's Preferences: the allowlisted logical values (non-sensitive by
        // construction) and digests of everything else (to prove it stays unchanged).
        "--dev-read-prefs" => {
            let udd = std::path::PathBuf::from(arg(1));
            let kind = if arg(3).is_empty() {
                "helium".to_string()
            } else {
                arg(3)
            };
            let path = synchrome_browser::preferences_path(&udd, &arg(2));
            let doc = match synchrome_browser::read_preferences(&udd, &arg(2)) {
                Ok(d) => d,
                Err(e) => return fail(e),
            };
            let major = synchrome_browser::browser_last_version(&udd)
                .and_then(|v| synchrome_sync::browser_prefs::major_of(&v));
            let (values, skipped) = synchrome_sync::browser_prefs::extract(&doc, &kind, major);
            let mut rest = doc.clone();
            let mut raw = serde_json::Map::new();
            for p in synchrome_sync::browser_prefs::ALLOWLIST
                .iter()
                .map(|s| s.path)
                .chain([synchrome_sync::browser_prefs::HELIUM_BOOKMARK_BAR_STATE])
            {
                let segs: Vec<&str> = p.split('.').collect();
                let (last, parents) = segs.split_last().unwrap();
                if let Some(serde_json::Value::Object(m)) = parents
                    .iter()
                    .try_fold(&mut rest, |d, k| d.as_object_mut()?.get_mut(*k))
                {
                    if let Some(v) = m.remove(*last) {
                        raw.insert(p.to_string(), v);
                    }
                }
                // parents left empty are pruned (both before and after a write alike)
                for depth in (1..segs.len()).rev() {
                    let (k, pp) = segs[..depth].split_last().unwrap();
                    let Some(serde_json::Value::Object(m)) = pp
                        .iter()
                        .try_fold(&mut rest, |d, k| d.as_object_mut()?.get_mut(*k))
                    else {
                        break;
                    };
                    if m.get(*k)
                        .is_some_and(|v| v.as_object().is_some_and(|o| o.is_empty()))
                    {
                        m.remove(*k);
                    } else {
                        break;
                    }
                }
            }
            let bytes = std::fs::read(&path).unwrap_or_default();
            let secure =
                std::fs::read(udd.join(arg(2)).join("Secure Preferences")).unwrap_or_default();
            let sha = |b: &[u8]| {
                synchrome_bridge::token_sha256(&String::from_utf8_lossy(b))[..12].to_string()
            };
            out(serde_json::json!({
                "values": values, "skipped": skipped, "raw": raw, "major": major,
                "rest_sha": sha(serde_json::to_string(&rest).unwrap_or_default().as_bytes()),
                "file_sha": sha(&bytes), "size": bytes.len(),
                "secure_sha": sha(&secure),
            }))
        }
        // Held remote ops of one conflict, in merge order: device, seq, HLC.
        "--dev-conflict-held" => match bookmarks::open_db()
            .and_then(|d| d.conflict_held(&arg(1)).map_err(|e| e.to_string()))
        {
            Ok(v) => out(
                serde_json::json!({ "held": v.iter().map(|(d, s, h)| serde_json::json!({"device": d, "seq": s, "hlc": h})).collect::<Vec<_>>() }),
            ),
            Err(e) => fail(e),
        },
        // Resolve one conflict exactly as the app's Resolve buttons do.
        "--dev-resolve" => {
            let Ok(mid) = Uuid::parse_str(&arg(1)) else {
                return fail("usage: --dev-resolve <mapping> <conflict_id> <resolution>");
            };
            let Some(res) = synchrome_state::Resolution::parse(&arg(3)) else {
                return fail("resolution: keep_local | keep_remote | merge | retry | relocate");
            };
            match bookmarks::resolve_for_mapping(&state, mid, vec![(arg(2), res)]).await {
                Ok(r) => out(serde_json::json!({ "results": r })),
                Err(e) => fail(e),
            }
        }
        "--dev-remote-cursor" => {
            let (Ok(pid), dev) = (Uuid::parse_str(&arg(1)), arg(2)) else {
                return fail("bad profile id");
            };
            match bookmarks::open_db()
                .and_then(|d| d.remote_cursor(pid, &dev).map_err(|e| e.to_string()))
            {
                Ok(c) => out(serde_json::json!({ "applied_through": c })),
                Err(e) => fail(e),
            }
        }
        // Local materialised tree: depth/kind/title + URL host only (never full URLs).
        "--dev-local-tree" => {
            let Ok(pid) = Uuid::parse_str(&arg(1)) else {
                return fail("bad profile id");
            };
            match bookmarks::open_db().and_then(|mut d| d.tree(pid).map_err(|e| e.to_string())) {
                Ok(t) => out(tree_json(&t)),
                Err(e) => fail(e),
            }
        }
        // What the CLOUD holds: download every journal chunk of the profile, check seq
        // continuity, materialise, render like --dev-local-tree.
        "--dev-cloud-tree" => {
            let Ok(pid) = Uuid::parse_str(&arg(1)) else {
                return fail("bad profile id");
            };
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let cloud = match state.cloud() {
                Ok(c) => c,
                Err(e) => return fail(e),
            };
            let paths = match cloud
                .list_tree(&session, &format!("profiles/{pid}/journal"))
                .await
            {
                Ok(p) => p,
                Err(e) => return fail(e),
            };
            let mut ops = Vec::new();
            let mut chunks = Vec::new();
            for p in &paths {
                match cloud.get_object(&session, p).await {
                    Ok(Some(b)) => {
                        let mut mine = Vec::new();
                        for line in String::from_utf8_lossy(&b).lines() {
                            match synchrome_sync::Op::from_line(line) {
                                Ok(o) => mine.push(o),
                                Err(e) => return fail(format!("bad journal line: {e}")),
                            }
                        }
                        // per chunk: seq range, byte hash, and per op only id/kind/field NAMES
                        chunks.push(serde_json::json!({
                            "path": p,
                            "bytes": b.len(),
                            "sha256_12": token_sha256(&String::from_utf8_lossy(&b))[..12].to_string(),
                            "seq_from": mine.first().map(|o| o.seq),
                            "seq_to": mine.last().map(|o| o.seq),
                            "ops": mine.iter().map(|o| serde_json::json!({
                                "seq": o.seq, "id": o.id, "hlc": o.hlc,
                                "op": if o.op == synchrome_sync::OpKind::Delete { "delete" } else { "set" },
                                "fields": o.fields.as_ref().map(|f| f.keys().cloned().collect::<Vec<_>>()),
                            })).collect::<Vec<_>>(),
                        }));
                        ops.extend(mine);
                    }
                    Ok(None) => return fail("listed object vanished"),
                    Err(e) => return fail(e),
                }
            }
            let mut seqs: Vec<u64> = ops.iter().map(|o| o.seq).collect();
            seqs.sort_unstable();
            let contiguous = seqs.iter().enumerate().all(|(i, s)| *s == i as u64 + 1);
            let events = cloud.list_store_events(&session, pid).await.ok();
            let t = synchrome_sync::bookmarks::materialize(&ops);
            let mut v = tree_json(&t);
            v["objects"] = serde_json::json!(paths.len());
            v["paths"] = serde_json::json!(paths);
            v["ops"] = serde_json::json!(ops.len());
            v["max_seq"] = serde_json::json!(seqs.last());
            v["seq_contiguous"] = serde_json::json!(contiguous);
            v["store_events"] = serde_json::json!(events.as_ref().map(|e| e.len()));
            v["store_event_ranges"] = serde_json::json!(events
                .unwrap_or_default()
                .iter()
                .map(|e| (e.seq_from, e.seq_to, e.path.clone()))
                .collect::<Vec<_>>());
            v["chunks"] = serde_json::json!(chunks);
            v["deletes"] = serde_json::json!(ops
                .iter()
                .filter(|o| o.op == synchrome_sync::OpKind::Delete)
                .count());
            // extensions ops: ids, kinds, field names and the enabled flag (no names)
            v["settings_ops"] = serde_json::json!(ops
                .iter()
                .filter(|o| o.col == synchrome_sync::extension_settings::COLLECTION)
                .map(|o| {
                    let f = o.fields.as_ref();
                    let value = f.and_then(|f| f.get("value"));
                    serde_json::json!({
                        "seq": o.seq, "by": o.by, "id": o.id, "hlc": o.hlc,
                        "op": if o.op == synchrome_sync::OpKind::Delete { "delete" } else { "set" },
                        "fields": f.map(|f| f.keys().cloned().collect::<Vec<_>>()),
                        "ver": f.and_then(|f| f.get("ver").cloned()),
                        // a digest only: values never leave the harness output
                        "value_sha": value.map(|v| synchrome_bridge::token_sha256(&synchrome_sync::extension_settings::canonical(v))[..16].to_string()),
                    })
                })
                .collect::<Vec<_>>());
            // browser preference ops: allowlisted paths and their (non-sensitive) values
            v["prefs_ops"] = serde_json::json!(ops
                .iter()
                .filter(|o| o.col == synchrome_sync::browser_prefs::COLLECTION)
                .map(|o| {
                    let f = o.fields.as_ref();
                    serde_json::json!({
                        "seq": o.seq, "by": o.by, "id": o.id, "hlc": o.hlc,
                        "op": if o.op == synchrome_sync::OpKind::Delete { "delete" } else { "set" },
                        "fields": f.map(|f| f.keys().cloned().collect::<Vec<_>>()),
                        "value": f.and_then(|f| f.get("value").cloned()),
                        "bk": f.and_then(|f| f.get("bk").cloned()),
                        "maj": f.and_then(|f| f.get("maj").cloned()),
                    })
                })
                .collect::<Vec<_>>());
            v["ext_ops"] = serde_json::json!(ops
                .iter()
                .filter(|o| o.col == synchrome_sync::extensions::COLLECTION)
                .map(|o| serde_json::json!({
                    "seq": o.seq, "by": o.by, "id": o.id, "hlc": o.hlc,
                    "op": if o.op == synchrome_sync::OpKind::Delete { "delete" } else { "set" },
                    "fields": o.fields.as_ref().map(|f| f.keys().cloned().collect::<Vec<_>>()),
                    "enabled": o.fields.as_ref().and_then(|f| f.get("enabled").cloned()),
                }))
                .collect::<Vec<_>>());
            out(v)
        }
        // Isolation probes run as ANOTHER account (B) against A's ids: everything must fail
        // closed. Prints only booleans / error kinds.
        "--dev-cloud-probe-foreign" => {
            let (a_account, a_profile, a_device) = (arg(1), arg(2), arg(3));
            let (Ok(a_pid), Ok(a_did)) = (Uuid::parse_str(&a_profile), Uuid::parse_str(&a_device))
            else {
                return fail("bad ids");
            };
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let cloud = match state.cloud() {
                Ok(c) => c,
                Err(e) => return fail(e),
            };
            let rel = format!("profiles/{a_pid}/journal/{a_did}");
            let list = cloud.list_tree(&session, &rel).await;
            // full name of one of A's real objects (from A's --dev-cloud-tree "paths")
            let first = format!("{a_account}/{}", arg(4));
            let get = cloud.get_object_by_full_name(&session, &first).await;
            let put = cloud
                .put_object(
                    &session,
                    &format!("{rel}/2026/10/00000001.jsonl"),
                    b"{}\n".to_vec(),
                    synchrome_cloud::JOURNAL_CONTENT_TYPE,
                )
                .await;
            let put_foreign_prefix = cloud
                .put_object_by_full_name(
                    &session,
                    &format!("{a_account}/{rel}/2026/10/99999999.jsonl"),
                    b"{}\n".to_vec(),
                )
                .await;
            let overwrite_foreign = cloud
                .put_object_by_full_name(&session, &first, b"{}\n".to_vec())
                .await;
            let list_foreign = {
                let mut n = 0usize;
                let mut err = None;
                for pre in [
                    a_account.clone(),
                    format!("{a_account}/profiles"),
                    format!("{a_account}/profiles/{a_pid}/journal/{a_did}"),
                ] {
                    match cloud.list_full_prefix(&session, &pre).await {
                        Ok(k) => n += k,
                        Err(e) => err = Some(e.to_string()),
                    }
                }
                (n, err)
            };
            let profile_visible = cloud.get_profile(&session, a_pid).await;
            let mappings_visible = cloud
                .list_mappings(&session)
                .await
                .map(|m| m.iter().filter(|x| x.profile_id == a_pid).count());
            let events = cloud.list_store_events(&session, a_pid).await;
            let ev_insert = cloud
                .record_store_event(
                    &session,
                    &synchrome_cloud::StoreEvent {
                        profile_id: a_pid,
                        device_id: a_did,
                        path: format!("{rel}/2026/10/00000001.jsonl"),
                        seq_from: 1,
                        seq_to: 1,
                        hlc: format!("0000000000000-0000-{a_did}"),
                        created_at: None,
                    },
                )
                .await;
            out(serde_json::json!({
                "as_user": session.user_id,
                "list_sees": list.as_ref().map(|v| v.len()).map_err(|e| e.to_string()),
                "get_first_chunk": get.as_ref().map(|o| o.is_some()).map_err(|e| e.to_string()),
                "put_into_foreign_profile": put.map(|_| "accepted").map_err(|e| format!("{e:?}")),
                "put_under_foreign_prefix": put_foreign_prefix.map(|_| "accepted").map_err(|e| format!("{e:?}")),
                "overwrite_foreign_chunk": overwrite_foreign.map(|_| "accepted").map_err(|e| format!("{e:?}")),
                "list_foreign_prefix_sees": list_foreign.0,
                "list_foreign_prefix_error": list_foreign.1,
                "foreign_profile_visible": profile_visible.map(|p| p.is_some()).map_err(|e| e.to_string()),
                "foreign_mappings_visible": mappings_visible.map_err(|e| e.to_string()),
                "store_events_sees": events.map(|v| v.len()).map_err(|e| e.to_string()),
                "store_event_insert": ev_insert.map(|_| "accepted").map_err(|e| format!("{e:?}")),
            }))
        }
        // Unmap in the cloud but KEEP the local registry entry (same as the app's Unmap).
        "--dev-unmap" => {
            let Ok(id) = Uuid::parse_str(&arg(1)) else {
                return fail("bad mapping id");
            };
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            match state.cloud() {
                Ok(c) => match c.unmap_local_profile(&session, id).await {
                    Ok(()) => out(serde_json::json!({ "unmapped": true })),
                    Err(e) => fail(e),
                },
                Err(e) => fail(e),
            }
        }
        "--dev-delete-profile" => {
            let Ok(id) = Uuid::parse_str(&arg(1)) else {
                return fail("bad profile id");
            };
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let purged = bookmarks::purge_profile(&state, &session, id).await;
            match state.cloud() {
                Ok(c) => match c.delete_profile(&session, id).await {
                    Ok(()) => out(
                        serde_json::json!({ "deleted_profile": true, "purged_objects": purged.ok() }),
                    ),
                    Err(e) => fail(e),
                },
                Err(e) => fail(e),
            }
        }
        "--dev-cleanup" => {
            let Ok(id) = Uuid::parse_str(&arg(1)) else {
                return fail("bad mapping id");
            };
            let session = match state.session().await {
                Ok(s) => s,
                Err(e) => return fail(e),
            };
            let (cloud, me) = match (state.cloud(), state.device()) {
                (Ok(c), Ok(d)) => (c, d.clone()),
                _ => return fail("not configured"),
            };
            let all = cloud.list_mappings(&session).await.unwrap_or_default();
            let profile = all.iter().find(|m| m.id == id).map(|m| m.profile_id);
            let unmapped = cloud.unmap_local_profile(&session, id).await.is_ok();
            let purged = match profile {
                Some(p) => bookmarks::purge_profile(&state, &session, p).await.ok(),
                None => None,
            };
            let deleted_profile = match profile {
                Some(p) => cloud.delete_profile(&session, p).await.is_ok(),
                None => false,
            };
            let _ = local_token::registry().remove_mapping(id);
            let remaining = cloud
                .list_mappings(&session)
                .await
                .unwrap_or_default()
                .len();
            let device_deleted = if remaining == 0 {
                cloud.delete_device(&session, me.device_id).await.is_ok()
            } else {
                false
            };
            out(serde_json::json!({
                "unmapped": unmapped, "deleted_profile": deleted_profile, "purged_objects": purged,
                "remaining_mappings": remaining, "device_deleted": device_deleted,
            }))
        }
        other => fail(format!("unknown dev command {other:?}")),
    }
}

/// Replace every `url` string in a JSON value by its host (validation output policy).
fn host_only(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if k == "url" {
                    if let Some(u) = x.as_str() {
                        *x = url_host(u).into();
                    }
                } else {
                    host_only(x);
                }
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(host_only),
        _ => {}
    }
}

fn url_host(u: &str) -> String {
    let rest = u.split_once("://").map(|(_, r)| r).unwrap_or(u);
    rest.split(['/', '?', '#']).next().unwrap_or("").to_string()
}

/// Tree rendering for validation: `[depth, kind, title, url_host]` per root, in order.
fn tree_json(t: &synchrome_sync::bookmarks::BookmarkState) -> serde_json::Value {
    // [root, depth, kind, title, url host, 8-hex url hash, entity id]
    let rows: Vec<serde_json::Value> = synchrome_sync::bookmarks::render_tree_with_ids(t)
        .into_iter()
        .map(|(root, depth, kind, title, url, sid)| {
            serde_json::json!([
                root.as_str(),
                depth,
                kind.as_str(),
                title,
                url.as_deref().map(url_host),
                url.as_deref().map(|u| token_sha256(u)[..8].to_string()),
                sid
            ])
        })
        .collect();
    let (b, f) = t.counts();
    serde_json::json!({ "bookmarks": b, "folders": f, "tree": rows })
}

/// Dev harness: a sink whose store_events insert always fails after a successful upload.
struct FaultAfterUpload<'a> {
    inner: bookmarks::CloudSink<'a>,
    uploads: std::sync::atomic::AtomicUsize,
}

impl synchrome_state::JournalSink for FaultAfterUpload<'_> {
    async fn account_id(&self) -> Result<String, synchrome_state::SinkError> {
        self.inner.account_id().await
    }
    async fn mapping(
        &self,
        m: Uuid,
    ) -> Result<synchrome_state::MappingCheck, synchrome_state::SinkError> {
        self.inner.mapping(m).await
    }
    async fn journal_devices(&self, p: Uuid) -> Result<Vec<String>, synchrome_state::SinkError> {
        self.inner.journal_devices(p).await
    }
    async fn put_chunk(&self, path: &str, body: &[u8]) -> Result<(), synchrome_state::SinkError> {
        let r = self.inner.put_chunk(path, body).await;
        if r.is_ok() {
            self.uploads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        r
    }
    async fn list_events(
        &self,
        p: Uuid,
    ) -> Result<Vec<synchrome_state::RemoteEvent>, synchrome_state::SinkError> {
        self.inner.list_events(p).await
    }
    async fn get_chunk(&self, path: &str) -> Result<Option<Vec<u8>>, synchrome_state::SinkError> {
        self.inner.get_chunk(path).await
    }
    async fn record_event(
        &self,
        _ev: &synchrome_state::PublishedEvent,
    ) -> Result<(), synchrome_state::SinkError> {
        Err(synchrome_state::SinkError::Offline(
            "injected fault between upload and checkpoint".into(),
        ))
    }
}

/// Dev harness: every store_event presented twice.
struct DuplicateEvents<'a> {
    inner: bookmarks::CloudSink<'a>,
}

impl synchrome_state::JournalSink for DuplicateEvents<'_> {
    async fn account_id(&self) -> Result<String, synchrome_state::SinkError> {
        self.inner.account_id().await
    }
    async fn mapping(
        &self,
        m: Uuid,
    ) -> Result<synchrome_state::MappingCheck, synchrome_state::SinkError> {
        self.inner.mapping(m).await
    }
    async fn journal_devices(&self, p: Uuid) -> Result<Vec<String>, synchrome_state::SinkError> {
        self.inner.journal_devices(p).await
    }
    async fn put_chunk(&self, path: &str, body: &[u8]) -> Result<(), synchrome_state::SinkError> {
        self.inner.put_chunk(path, body).await
    }
    async fn record_event(
        &self,
        ev: &synchrome_state::PublishedEvent,
    ) -> Result<(), synchrome_state::SinkError> {
        self.inner.record_event(ev).await
    }
    async fn list_events(
        &self,
        p: Uuid,
    ) -> Result<Vec<synchrome_state::RemoteEvent>, synchrome_state::SinkError> {
        let v = self.inner.list_events(p).await?;
        Ok(v.iter().chain(v.iter()).cloned().collect())
    }
    async fn get_chunk(&self, path: &str) -> Result<Option<Vec<u8>>, synchrome_state::SinkError> {
        self.inner.get_chunk(path).await
    }
}
