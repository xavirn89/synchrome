//! Development-only CLI (compiled only with `debug_assertions`) for end-to-end validation.
//! Runs the SAME code paths as the app (Keychain session, cloud client, local token write,
//! mapping views) without a window. Never prints passwords, JWTs or full mapping tokens.

use crate::{local_token, mapping_views, write_token_for_mapping, AppState};
use std::path::PathBuf;
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
            match write_token_for_mapping(&state, id, Some(udd)).await {
                Ok(v) => out(serde_json::to_value(v).unwrap()),
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
                    "status": kv.get("synchromeStatus").and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()),
                    "keys": kv.keys().filter(|k| k.as_str() != MAPPING_TOKEN_STORAGE_KEY).collect::<Vec<_>>(),
                })),
                Err(e) => fail(e),
            }
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
            match state.cloud() {
                Ok(c) => match c.delete_profile(&session, id).await {
                    Ok(()) => out(serde_json::json!({ "deleted_profile": true })),
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
                "unmapped": unmapped, "deleted_profile": deleted_profile,
                "remaining_mappings": remaining, "device_deleted": device_deleted,
            }))
        }
        other => fail(format!("unknown dev command {other:?}")),
    }
}
