//! Extension settings cycle (M1 slice 11, `docs/03` §3.5): the app reads and writes the
//! `chrome.storage.local` / `.sync` LevelDB stores of the extensions the user opted in,
//! only while the browser of that profile is closed. Chromium gives no extension access to
//! another extension's storage, so there is no live path. Nothing here logs keys or values.

use crate::bookmarks::{self, open_db};
use crate::{local_token, AppState};
use std::path::PathBuf;
use synchrome_browser::{
    apply_extension_area, is_browser_running, read_extension_area, ExtStorageError,
};
use synchrome_sync::extension_settings::Area;
use uuid::Uuid;

/// Counts of one cycle (no keys, no values).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CycleReport {
    pub browser_running: bool,
    pub captured_ops: usize,
    pub keys_written: usize,
    pub excluded_sensitive: usize,
    pub errors: usize,
}

fn browser_area(a: Area) -> synchrome_browser::Area {
    match a {
        Area::Local => synchrome_browser::Area::Local,
        Area::Sync => synchrome_browser::Area::Sync,
    }
}

fn short(e: &ExtStorageError) -> (&'static str, Option<&'static str>) {
    match e {
        ExtStorageError::BrowserRunning | ExtStorageError::Locked => ("waiting", None),
        ExtStorageError::Verify(_) => (
            "error",
            Some("verification after write failed; the store was left unchanged"),
        ),
        _ => (
            "error",
            Some("the extension store could not be read or written"),
        ),
    }
}

/// One settings cycle for one profile of this device.
pub async fn settings_cycle(state: &AppState, profile_id: Uuid) -> Result<CycleReport, String> {
    let mut rep = CycleReport::default();
    let mut db = open_db()?;
    let jobs = db.settings_jobs(profile_id).map_err(|e| e.to_string())?;
    if jobs.is_empty() {
        return Ok(rep);
    }
    let Some((_browser, udd, profile_dir)) =
        db.settings_target(profile_id).map_err(|e| e.to_string())?
    else {
        return Ok(rep);
    };
    let Some(src) = db.source_of(profile_id).map_err(|e| e.to_string())? else {
        return Ok(rep);
    };
    let udd = PathBuf::from(udd);
    if is_browser_running(&udd) {
        rep.browser_running = true;
        for j in &jobs {
            let _ = db.settings_status(profile_id, &j.ext_id, "waiting", None);
        }
        drop(db);
        let _ = bookmarks::sync_profile(state, profile_id).await; // download what is new meanwhile
        return Ok(rep);
    }
    // a dataset captured for the first time joins what the account already has
    let joining = jobs.iter().any(|j| {
        db.settings_view(profile_id, &j.ext_id)
            .map(|v| v.last_capture_at.is_none())
            .unwrap_or(false)
    });
    if joining {
        drop(db);
        let _ = bookmarks::sync_profile(state, profile_id).await;
        db = open_db()?;
    }
    let now_ms = synchrome_sync::hlc::now_ms;
    for j in &jobs {
        for area in &j.areas {
            match read_extension_area(&udd, &profile_dir, &j.ext_id, browser_area(*area)) {
                Ok(store) => {
                    match db.capture_settings(&src, &j.ext_id, *area, &store, &j.exclude, now_ms())
                    {
                        Ok(c) => {
                            rep.captured_ops += c.ops;
                            rep.excluded_sensitive += c.excluded_sensitive;
                        }
                        Err(_) => {
                            rep.errors += 1;
                            let _ = db.settings_status(
                                profile_id,
                                &j.ext_id,
                                "error",
                                Some("local state unavailable"),
                            );
                        }
                    }
                }
                Err(e) => {
                    let (st, msg) = short(&e);
                    rep.errors += (st == "error") as usize;
                    let _ = db.settings_status(profile_id, &j.ext_id, st, msg);
                }
            }
        }
    }
    drop(db);
    let _ = bookmarks::sync_profile(state, profile_id).await;
    let mut db = open_db()?;
    let backups = local_token::app_data_dir()?
        .join("backups")
        .join("extension-settings");
    let _ = std::fs::create_dir_all(&backups);
    for w in db.plan_settings(profile_id).map_err(|e| e.to_string())? {
        match apply_extension_area(
            &udd,
            &profile_dir,
            &w.ext_id,
            browser_area(w.area),
            &w.puts,
            &w.deletes,
            &backups,
        ) {
            Ok(_) => {
                rep.keys_written += w.puts.len() + w.deletes.len();
                db.settings_written(profile_id, &w, bookmarks::now_unix())
                    .map_err(|e| e.to_string())?;
            }
            Err(e) => {
                let (st, msg) = short(&e);
                rep.errors += (st == "error") as usize;
                let _ = db.settings_status(profile_id, &w.ext_id, st, msg);
            }
        }
    }
    if rep.captured_ops > 0 {
        drop(db);
        let _ = bookmarks::publish_profile(state, profile_id).await;
    }
    Ok(rep)
}

/// Settings state of every extension installed here (synchronised scope) of a mapping.
pub fn settings_for_mapping(
    state: &AppState,
    mapping_id: Uuid,
) -> Result<Vec<synchrome_state::SettingsView>, String> {
    let exts = bookmarks::extensions_for_mapping(state, mapping_id)?;
    let db = open_db()?;
    let pid = db
        .profile_for_mapping(mapping_id)
        .map_err(|e| e.to_string())?
        .ok_or("no local state for this mapping")?;
    exts.iter()
        .filter(|x| x.installed_here)
        .map(|x| db.settings_view(pid, &x.ext_id).map_err(|e| e.to_string()))
        .collect()
}

/// Turn settings sync of one extension on (local / local+sync) or off, on this device.
pub async fn set_settings_mode(
    state: &AppState,
    mapping_id: Uuid,
    ext_id: &str,
    mode: synchrome_sync::extension_settings::Mode,
    exclude: Option<Vec<String>>,
) -> Result<(), String> {
    let (account, pid) = bookmarks::own_profile_pub(state, mapping_id)?;
    let mut db = open_db()?;
    db.set_settings_mode(&account, pid, ext_id, mode, exclude)
        .map_err(|e| e.to_string())?;
    drop(db);
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        settings_cycle(state, pid),
    )
    .await;
    Ok(())
}

/// Run a cycle now for a mapping's profile.
pub async fn settings_sync_now(state: &AppState, mapping_id: Uuid) -> Result<CycleReport, String> {
    let (_, pid) = bookmarks::own_profile_pub(state, mapping_id)?;
    settings_cycle(state, pid).await
}
