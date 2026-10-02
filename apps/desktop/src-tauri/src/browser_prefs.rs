//! Browser preferences cycle (M1 slice 12, `docs/03` §3.6): the app reads and writes the
//! allowlisted paths of a profile's `Preferences` file, only while that profile's browser is
//! closed. Nothing here logs paths' values.

use crate::bookmarks::{self, open_db};
use crate::{local_token, AppState};
use std::path::PathBuf;
use synchrome_browser::{
    apply_preferences, browser_last_version, is_browser_running, read_preferences, PrefEdit,
    PrefsError,
};
use synchrome_sync::browser_prefs::{self as bp, Mode};
use uuid::Uuid;

/// Counts of one cycle (no paths' values).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CycleReport {
    pub browser_running: bool,
    pub captured_ops: usize,
    pub paths_written: usize,
    pub unsupported: usize,
    pub version_mismatch: usize,
    pub invalid: usize,
    pub errors: usize,
}

fn short(e: &PrefsError) -> (&'static str, Option<&'static str>) {
    match e {
        PrefsError::BrowserRunning | PrefsError::Locked => ("waiting", None),
        PrefsError::RolledBack | PrefsError::Verify(_) => (
            "error",
            Some("verification after write failed; Preferences was left unchanged"),
        ),
        PrefsError::InvalidJson(_) => ("error", Some("Preferences is not valid JSON")),
        PrefsError::NotFound => ("error", Some("Preferences not found for this profile")),
        _ => ("error", Some("Preferences could not be read or written")),
    }
}

/// One preferences cycle for one profile of this device.
pub async fn prefs_cycle(state: &AppState, profile_id: Uuid) -> Result<CycleReport, String> {
    let mut rep = CycleReport::default();
    let mut db = open_db()?;
    let cfg = db.prefs_config(profile_id).map_err(|e| e.to_string())?;
    if cfg.mode == Mode::Off {
        return Ok(rep);
    }
    let Some((kind, udd, profile_dir)) = db.prefs_target(profile_id).map_err(|e| e.to_string())?
    else {
        return Ok(rep);
    };
    // debug builds only: simulate a browser kind that is not validated (E2E, disposable profile)
    #[cfg(debug_assertions)]
    let kind = std::env::var("SYNCHROME_DEV_PREFS_BROWSER_KIND").unwrap_or(kind);
    let Some(src) = db.source_of(profile_id).map_err(|e| e.to_string())? else {
        return Ok(rep);
    };
    let udd = PathBuf::from(udd);
    if is_browser_running(&udd) {
        rep.browser_running = true;
        let _ = db.prefs_status(profile_id, "waiting", None);
        drop(db);
        let _ = bookmarks::sync_profile(state, profile_id).await; // download what is new meanwhile
        return Ok(rep);
    }
    let major = browser_last_version(&udd).and_then(|v| bp::major_of(&v));
    // a dataset captured for the first time joins what the account already has
    if cfg.last_capture_at.is_none() {
        drop(db);
        let _ = bookmarks::sync_profile(state, profile_id).await;
        db = open_db()?;
    }
    match read_preferences(&udd, &profile_dir) {
        Ok(doc) => {
            let (values, skipped) = bp::extract(&doc, &kind, major);
            match db.capture_prefs(
                &src,
                &kind,
                major,
                &values,
                &skipped,
                synchrome_sync::hlc::now_ms(),
            ) {
                Ok(c) => rep.captured_ops = c.ops,
                Err(_) => {
                    rep.errors += 1;
                    let _ = db.prefs_status(profile_id, "error", Some("local state unavailable"));
                    return Ok(rep);
                }
            }
        }
        Err(e) => {
            let (st, msg) = short(&e);
            rep.errors += (st == "error") as usize;
            let _ = db.prefs_status(profile_id, st, msg);
            return Ok(rep);
        }
    }
    drop(db);
    let _ = bookmarks::sync_profile(state, profile_id).await;
    let mut db = open_db()?;
    let plan = db
        .plan_prefs(profile_id, &kind, major)
        .map_err(|e| e.to_string())?;
    rep.unsupported = plan.unsupported;
    rep.version_mismatch = plan.version_mismatch;
    rep.invalid = plan.invalid;
    if !plan.edits.is_empty() {
        let raw: Vec<PrefEdit> = plan
            .edits
            .iter()
            .flat_map(|(p, v)| bp::edits_for(&kind, p, v.as_ref()))
            .map(|e| PrefEdit {
                path: e.path,
                value: e.value,
            })
            .collect();
        let backups = local_token::app_data_dir()?
            .join("backups")
            .join("browser-preferences");
        match apply_preferences(&udd, &profile_dir, &raw, &bp::writable, &backups) {
            Ok(_) => {
                rep.paths_written = plan.edits.len();
                db.prefs_written(profile_id, &plan.edits, bookmarks::now_unix())
                    .map_err(|e| e.to_string())?;
            }
            Err(e) => {
                let (st, msg) = short(&e);
                rep.errors += (st == "error") as usize;
                let _ = db.prefs_status(profile_id, st, msg);
            }
        }
    }
    if rep.captured_ops > 0 {
        drop(db);
        let _ = bookmarks::publish_profile(state, profile_id).await;
    }
    Ok(rep)
}

/// Browser preferences state of a mapping's profile on this device (counts only).
pub fn prefs_for_mapping(
    state: &AppState,
    mapping_id: Uuid,
) -> Result<synchrome_state::PrefsView, String> {
    let (_, pid) = bookmarks::own_profile_pub(state, mapping_id)?;
    open_db()?.prefs_view(pid).map_err(|e| e.to_string())
}

/// Off / Sync recommended preferences for a mapping's profile, on this device (opt-in).
pub async fn set_prefs_mode(state: &AppState, mapping_id: Uuid, mode: Mode) -> Result<(), String> {
    let (account, pid) = bookmarks::own_profile_pub(state, mapping_id)?;
    open_db()?
        .set_prefs_mode(&account, pid, mode)
        .map_err(|e| e.to_string())?;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(60), prefs_cycle(state, pid)).await;
    Ok(())
}

/// Run the cycle now for a mapping's profile.
pub async fn prefs_sync_now(state: &AppState, mapping_id: Uuid) -> Result<CycleReport, String> {
    let (_, pid) = bookmarks::own_profile_pub(state, mapping_id)?;
    prefs_cycle(state, pid).await
}
