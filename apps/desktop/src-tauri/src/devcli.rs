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
            for p in &paths {
                match cloud.get_object(&session, p).await {
                    Ok(Some(b)) => {
                        for line in String::from_utf8_lossy(&b).lines() {
                            match synchrome_sync::Op::from_line(line) {
                                Ok(o) => ops.push(o),
                                Err(e) => return fail(format!("bad journal line: {e}")),
                            }
                        }
                    }
                    Ok(None) => return fail("listed object vanished"),
                    Err(e) => return fail(e),
                }
            }
            let mut seqs: Vec<u64> = ops.iter().map(|o| o.seq).collect();
            seqs.sort_unstable();
            let contiguous = seqs.iter().enumerate().all(|(i, s)| *s == i as u64 + 1);
            let events = cloud
                .list_store_events(&session, pid)
                .await
                .map(|v| v.len())
                .ok();
            let t = synchrome_sync::bookmarks::materialize(&ops);
            let mut v = tree_json(&t);
            v["objects"] = serde_json::json!(paths.len());
            v["paths"] = serde_json::json!(paths);
            v["ops"] = serde_json::json!(ops.len());
            v["max_seq"] = serde_json::json!(seqs.last());
            v["seq_contiguous"] = serde_json::json!(contiguous);
            v["store_events"] = serde_json::json!(events);
            v["deletes"] = serde_json::json!(ops
                .iter()
                .filter(|o| o.op == synchrome_sync::OpKind::Delete)
                .count());
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

fn url_host(u: &str) -> String {
    let rest = u.split_once("://").map(|(_, r)| r).unwrap_or(u);
    rest.split(['/', '?', '#']).next().unwrap_or("").to_string()
}

/// Tree rendering for validation: `[depth, kind, title, url_host]` per root, in order.
fn tree_json(t: &synchrome_sync::bookmarks::BookmarkState) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = synchrome_sync::bookmarks::render_tree(t)
        .into_iter()
        .map(|(root, depth, kind, title, url)| {
            serde_json::json!([
                root.as_str(),
                depth,
                kind.as_str(),
                title,
                url.as_deref().map(url_host)
            ])
        })
        .collect();
    let (b, f) = t.counts();
    serde_json::json!({ "bookmarks": b, "folders": f, "tree": rows })
}
