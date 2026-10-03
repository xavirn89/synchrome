//! Synchrome desktop (Tauri 2) application entry point.
//!
//! Commands are read-only with respect to browser profiles. Auth commands manage the user's
//! session, which lives exclusively in the OS credential store (macOS Keychain); cloud
//! commands use that session inside Rust only. The webview receives token-free models:
//! [`AccountStatus`], [`DeviceIdentity`], [`DeviceSummary`], [`ProfileSummary`],
//! [`MappingView`]. The only write into a browser profile is the mapping token, placed in the
//! Synchrome extension's own `chrome.storage.local` store while the browser is closed
//! (see [`local_token`]); the same binary serves as native messaging host (see [`host`]).

mod bookmarks;
mod browser_prefs;
mod compaction;
#[cfg(debug_assertions)]
mod devcli;
mod ext_settings;
mod host;
mod local_token;
mod realtime;

use local_token::MappingView;
use std::sync::Arc;
use synchrome_auth::{
    AccountStatus, AuthClient, AuthError, KeychainStore, ReqwestTransport, Session, SessionManager,
};
use synchrome_cloud::{
    CloudClient, DeviceSummary, Mapping, MappingSummary, ProfileSummary, ReqwestRest,
};
use synchrome_config::SupabaseConfig;
use synchrome_device::DeviceIdentity;
use synchrome_supabase::CloudStatus;
use synchrome_types::{BrowserKind, DiscoveredBrowser};
use tauri::State;
use uuid::Uuid;

/// Shared app state. `auth`/`cloud` are `None` when the client configuration is missing.
pub struct AppState {
    auth: Option<SessionManager<ReqwestTransport>>,
    cloud: Option<CloudClient<ReqwestRest>>,
    device: Result<DeviceIdentity, String>,
    config_error: Option<String>,
}

impl AppState {
    fn new() -> Self {
        let device = synchrome_device::load_or_create().map_err(|e| e.to_string());
        match synchrome_config::AppConfig::load() {
            Ok(cfg) => Self::with_config(cfg.supabase, device),
            Err(e) => Self {
                auth: None,
                cloud: None,
                device,
                config_error: Some(e.to_string()),
            },
        }
    }

    fn with_config(cfg: SupabaseConfig, device: Result<DeviceIdentity, String>) -> Self {
        let auth = AuthClient::new(cfg.clone())
            .ok()
            .map(|c| SessionManager::new(c, Arc::new(KeychainStore::new())));
        let cloud = CloudClient::new(cfg).ok();
        let config_error = if auth.is_none() || cloud.is_none() {
            Some("could not build HTTP client".into())
        } else {
            None
        };
        Self {
            auth,
            cloud,
            device,
            config_error,
        }
    }

    fn auth(&self) -> Result<&SessionManager<ReqwestTransport>, String> {
        self.auth
            .as_ref()
            .ok_or_else(|| self.config_error.clone().unwrap_or_default())
    }

    fn cloud(&self) -> Result<&CloudClient<ReqwestRest>, String> {
        self.cloud
            .as_ref()
            .ok_or_else(|| self.config_error.clone().unwrap_or_default())
    }

    fn device(&self) -> Result<&DeviceIdentity, String> {
        self.device.as_ref().map_err(|e| e.clone())
    }

    /// A session whose access token is valid now (refreshing if needed). Stays in Rust.
    async fn session(&self) -> Result<Session, String> {
        match self.auth()?.ensure_valid_session().await {
            Ok(Some(s)) => Ok(s),
            Ok(None) => Err("not signed in".into()),
            Err(AuthError::SessionInvalid) => Err("session expired; please sign in again".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}

// ---------- read-only local discovery ----------

/// Extra browser data folders the user declared in `<app data>/browsers.json`
/// (`[{"kind": "chrome", "user_data_dir": "/path", "label": "Work"}]`), for browsers run
/// with a custom `--user-data-dir`. Absent or invalid file = none.
fn extra_browser_dirs() -> Vec<synchrome_browser::ExtraDataDir> {
    local_token::app_data_dir()
        .ok()
        .and_then(|d| std::fs::read(d.join("browsers.json")).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn discovered() -> Vec<DiscoveredBrowser> {
    // debug harness only: list just the declared extra folders, so a Full Disk Access test
    // never reads the user's real browser profiles (not even their `Local State`)
    #[cfg(debug_assertions)]
    if std::env::var("SYNCHROME_DEV_EXTRA_DIRS_ONLY").is_ok_and(|v| v == "1") {
        return synchrome_browser::discover_extra_only(&extra_browser_dirs());
    }
    synchrome_browser::discover_browsers_with(&extra_browser_dirs())
}

#[cfg(debug_assertions)]
pub(crate) fn discovered_pub() -> Vec<DiscoveredBrowser> {
    discovered()
}

#[tauri::command]
fn list_browsers() -> Vec<DiscoveredBrowser> {
    discovered()
}

/// Where this app runs from, and whether that is a stable install location. A copy running
/// from a disk image or from Gatekeeper's app translocation would leave the native messaging
/// host pointing at a path that disappears.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppLocation {
    pub path: String,
    pub stable: bool,
    pub reason: Option<String>,
}

fn app_location() -> AppLocation {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let reason = if exe.starts_with("/Volumes/") {
        Some("Synchrome is running from a disk image. Drag it to Applications and open it from there.")
    } else if exe.contains("/AppTranslocation/") {
        Some("macOS opened a temporary copy of Synchrome. Move it to Applications and open it from there.")
    } else if exe.contains("/Downloads/")
        || exe.contains("/Desktop/")
        || exe.contains("/Documents/")
    {
        Some(
            "Synchrome is running from a user folder that macOS protects. Move it to Applications.",
        )
    } else {
        None
    };
    AppLocation {
        path: exe,
        stable: reason.is_none(),
        reason: reason.map(str::to_string),
    }
}

#[tauri::command]
fn install_location() -> AppLocation {
    app_location()
}

/// Create an account (email + password) on the project's Synchrome Cloud.
#[tauri::command]
async fn sign_up(
    state: State<'_, AppState>,
    email: String,
    password: String,
) -> Result<AccountStatus, String> {
    let st = state
        .auth()?
        .sign_up(email.trim(), &password)
        .await
        .map_err(|e| e.to_string())?;
    realtime::set_account(
        state
            .auth()?
            .stored_session()
            .ok()
            .flatten()
            .map(|s| s.user_id),
    );
    Ok(st)
}

/// The companion extension shipped with the app (bundle `Resources/extension`; the source
/// folder in development). The user loads it in the browser: Extensions → Developer mode →
/// Load unpacked. Its id is fixed by the manifest `key`.
#[tauri::command]
fn extension_folder(app: tauri::AppHandle) -> Result<String, String> {
    use tauri::Manager;
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(res) = app.path().resource_dir() {
        candidates.push(res.join("extension"));
    }
    #[cfg(debug_assertions)]
    candidates.push(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../extension")
            .to_path_buf(),
    );
    candidates
        .into_iter()
        .find(|p| p.join("manifest.json").is_file())
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.display().to_string())
        .ok_or_else(|| "the companion extension is not bundled with this build".to_string())
}

#[tauri::command]
fn reveal_extension_folder(app: tauri::AppHandle) -> Result<(), String> {
    let p = extension_folder(app)?;
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(std::path::Path::new(&p).join("manifest.json"))
            .status()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Open System Settings → Privacy & Security → Full Disk Access (macOS shows no prompt for
/// it; the user adds Synchrome there).
#[tauri::command]
fn open_full_disk_access_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
            .status()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn app_version() -> String {
    synchrome_types::user_agent()
}

#[tauri::command]
async fn cloud_status() -> CloudStatus {
    match synchrome_config::AppConfig::load() {
        Err(e) => CloudStatus::unconfigured(e.to_string()),
        Ok(cfg) => match synchrome_supabase::cloud_status(&cfg.supabase).await {
            Ok(status) => status,
            Err(e) => CloudStatus::unconfigured(format!("http client error: {e}")),
        },
    }
}

// ---------- auth ----------

#[tauri::command]
fn account_status(state: State<'_, AppState>) -> AccountStatus {
    match &state.auth {
        Some(m) => m.account_status(),
        None => AccountStatus::signed_out().with_error(
            state
                .config_error
                .clone()
                .unwrap_or_else(|| "auth unavailable".into()),
        ),
    }
}

#[tauri::command]
async fn sign_in(
    state: State<'_, AppState>,
    email: String,
    password: String,
) -> Result<AccountStatus, String> {
    let st = state
        .auth()?
        .sign_in(email.trim(), &password)
        .await
        .map_err(|e| e.to_string())?;
    // the Realtime supervisor follows the signed-in account
    realtime::set_account(
        state
            .auth()?
            .stored_session()
            .ok()
            .flatten()
            .map(|s| s.user_id),
    );
    Ok(st)
}

#[tauri::command]
async fn refresh_session(state: State<'_, AppState>) -> Result<AccountStatus, String> {
    match state.auth()?.ensure_valid_session().await {
        Ok(Some(s)) => Ok(AccountStatus::from_session(&s)),
        Ok(None) => Ok(AccountStatus::signed_out()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
async fn sign_out(state: State<'_, AppState>) -> Result<AccountStatus, String> {
    realtime::set_account(None);
    match state.auth()?.sign_out().await {
        Ok(()) => Ok(AccountStatus::signed_out()),
        Err(e) => Ok(AccountStatus::signed_out().with_error(format!("signed out locally; {e}"))),
    }
}

// ---------- device identity + account metadata ----------

/// This installation's identity (not a secret; safe for the UI).
#[tauri::command]
fn device_identity(state: State<'_, AppState>) -> Result<DeviceIdentity, String> {
    state.device().cloned()
}

/// Upsert this installation into `devices` and bump `last_seen_at`. Requires a session.
#[tauri::command]
async fn register_device(state: State<'_, AppState>) -> Result<DeviceSummary, String> {
    let session = state.session().await?;
    let me = state.device()?;
    state
        .cloud()?
        .upsert_device(&session, me)
        .await
        .map_err(|e| e.to_string())
}

/// All devices on the account; the current one is flagged.
#[tauri::command]
async fn list_devices(state: State<'_, AppState>) -> Result<Vec<DeviceSummary>, String> {
    let session = state.session().await?;
    let me = state.device()?;
    state
        .cloud()?
        .list_devices(&session, me.device_id)
        .await
        .map_err(|e| e.to_string())
}

/// All Synchrome profiles on the account (empty for a fresh account).
#[tauri::command]
async fn list_profiles(state: State<'_, AppState>) -> Result<Vec<ProfileSummary>, String> {
    let session = state.session().await?;
    state
        .cloud()?
        .list_profiles(&session)
        .await
        .map_err(|e| e.to_string())
}

// ---------- cloud profiles + local <-> cloud mappings ----------

/// Make sure this installation exists in `devices` (FK target of a mapping); returns its id.
async fn ensure_device(state: &AppState, session: &Session) -> Result<Uuid, String> {
    let me = state.device()?;
    state
        .cloud()?
        .upsert_device(session, me)
        .await
        .map_err(|e| e.to_string())?;
    Ok(me.device_id)
}

/// Only profiles Synchrome actually detected on this machine may be mapped; the UI cannot
/// make up a browser/profile-directory pair.
fn ensure_detected_local_profile(
    kind: BrowserKind,
    dir: &str,
    udd: Option<&str>,
) -> Result<(), String> {
    let found = discovered().iter().any(|b| {
        b.kind == kind
            && udd.map_or(true, |u| b.user_data_dir == u)
            && b.profiles.iter().any(|p| p.dir_name == dir)
    });
    if found {
        Ok(())
    } else {
        Err(format!(
            "{} profile {dir:?} was not detected on this device",
            kind.display_name()
        ))
    }
}

/// The user-data directory of a mapping's local profile: the one the UI named, else the
/// one the local registry recorded, else the discovered folder holding that profile
/// (standard location first), else the standard location.
fn udd_for(m: &Mapping, named: Option<&str>) -> Option<std::path::PathBuf> {
    if let Some(u) = named.filter(|u| !u.is_empty()) {
        return Some(std::path::PathBuf::from(u));
    }
    if let Some(r) = local_token::registry().find_by_mapping(m.id) {
        if !r.user_data_dir.is_empty() {
            return Some(std::path::PathBuf::from(r.user_data_dir));
        }
    }
    discovered()
        .into_iter()
        .find(|b| {
            b.kind == m.browser_kind
                && b.profiles
                    .iter()
                    .any(|p| p.dir_name == m.browser_profile_dir)
        })
        .map(|b| std::path::PathBuf::from(b.user_data_dir))
        .or_else(|| synchrome_browser::user_data_dir(m.browser_kind))
}

/// Turn internal mappings into safe summaries, resolving profile and device names.
async fn summarize(
    state: &AppState,
    session: &Session,
    mappings: Vec<Mapping>,
) -> Result<Vec<MappingSummary>, String> {
    let cloud = state.cloud()?;
    let me = state.device()?.device_id;
    let profiles = cloud
        .list_profiles(session)
        .await
        .map_err(|e| e.to_string())?;
    let devices = cloud
        .list_devices(session, me)
        .await
        .map_err(|e| e.to_string())?;
    Ok(mappings
        .iter()
        .map(|m| {
            let pname = profiles
                .iter()
                .find(|p| p.id == m.profile_id)
                .map(|p| p.name.clone());
            let dname = devices
                .iter()
                .find(|d| d.device_id == m.device_id)
                .map(|d| d.name.clone());
            m.summary(pname, dname, me)
        })
        .collect())
}

#[tauri::command]
async fn create_profile(
    state: State<'_, AppState>,
    name: String,
) -> Result<ProfileSummary, String> {
    let session = state.session().await?;
    state
        .cloud()?
        .create_profile(&session, &name)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn rename_profile(
    state: State<'_, AppState>,
    profile_id: Uuid,
    name: String,
) -> Result<ProfileSummary, String> {
    let session = state.session().await?;
    state
        .cloud()?
        .rename_profile(&session, profile_id, &name)
        .await
        .map_err(|e| e.to_string())
}

/// Rejected by the server while the profile is mapped on any device (FK RESTRICT).
#[tauri::command]
async fn delete_profile(state: State<'_, AppState>, profile_id: Uuid) -> Result<(), String> {
    let session = state.session().await?;
    // objects the server allows → the row (cascades events/snapshots/cursors) → the rest
    let r = bookmarks::delete_profile_everywhere(&state, &session, profile_id).await?;
    match r.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Summaries + local write state, for the UI. Never contains a token.
pub(crate) async fn mapping_views(
    state: &AppState,
    session: &Session,
    mappings: Vec<Mapping>,
) -> Result<Vec<MappingView>, String> {
    let registry = local_token::registry();
    Ok(summarize(state, session, mappings)
        .await?
        .into_iter()
        .map(|s| local_token::view(s, &registry))
        .collect())
}

/// Write a mapping's token into the Synchrome extension store of its local profile.
/// `udd_override` targets a specific user-data dir (dev validation with disposable profiles);
/// otherwise the browser's default user-data dir on this machine is used.
pub(crate) async fn write_token_for_mapping(
    state: &AppState,
    mapping_id: Uuid,
    udd_override: Option<std::path::PathBuf>,
) -> Result<(MappingView, synchrome_browser::WriteReport), String> {
    let session = state.session().await?;
    let me = state.device()?.device_id;
    let m = state
        .cloud()?
        .list_mappings(&session)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|m| m.id == mapping_id)
        .ok_or_else(|| "mapping not found in this account".to_string())?;
    if m.device_id != me {
        return Err("this mapping belongs to another device".into());
    }
    let udd = match udd_override {
        Some(p) => p,
        None => udd_for(&m, None).ok_or_else(|| "unsupported platform".to_string())?,
    };
    let written = local_token::write_token_local(&m, &session.user_id, &udd);
    let view = mapping_views(state, &session, vec![m]).await?.remove(0);
    written.map(|r| (view, r))
}

/// Create the mapping's local half right after the cloud mapping exists. Failures (typically
/// "browser is open") are recorded and shown as Error with a Retry action; they do not fail
/// the mapping itself.
async fn view_after_mapping(
    state: &AppState,
    session: &Session,
    m: Mapping,
    udd: Option<&str>,
) -> Result<MappingView, String> {
    if let Some(udd) = udd_for(&m, udd) {
        let _ = local_token::write_token_local(&m, &session.user_id, &udd);
    }
    Ok(mapping_views(state, session, vec![m]).await?.remove(0))
}

/// Every mapping of the account (all devices), with local write state.
#[tauri::command]
async fn list_mappings(state: State<'_, AppState>) -> Result<Vec<MappingView>, String> {
    let session = state.session().await?;
    let mappings = state
        .cloud()?
        .list_mappings(&session)
        .await
        .map_err(|e| e.to_string())?;
    mapping_views(&state, &session, mappings).await
}

/// Map a detected local profile on THIS device to an existing cloud profile, then try to
/// write its token locally.
#[tauri::command]
async fn map_local_profile(
    state: State<'_, AppState>,
    profile_id: Uuid,
    browser_kind: BrowserKind,
    browser_profile_dir: String,
    user_data_dir: Option<String>,
) -> Result<MappingView, String> {
    ensure_detected_local_profile(browser_kind, &browser_profile_dir, user_data_dir.as_deref())?;
    let session = state.session().await?;
    let device_id = ensure_device(&state, &session).await?;
    let m = state
        .cloud()?
        .map_local_profile(
            &session,
            profile_id,
            device_id,
            browser_kind,
            &browser_profile_dir,
        )
        .await
        .map_err(|e| e.to_string())?;
    view_after_mapping(&state, &session, m, user_data_dir.as_deref()).await
}

/// "Create Synchrome profile" from a local profile: create the cloud profile, map it, then try
/// the local token write. If mapping fails, the just-created cloud profile is deleted again.
#[tauri::command]
async fn create_and_map_local_profile(
    state: State<'_, AppState>,
    name: String,
    browser_kind: BrowserKind,
    browser_profile_dir: String,
    user_data_dir: Option<String>,
) -> Result<MappingView, String> {
    ensure_detected_local_profile(browser_kind, &browser_profile_dir, user_data_dir.as_deref())?;
    let session = state.session().await?;
    let device_id = ensure_device(&state, &session).await?;
    let cloud = state.cloud()?;
    let profile = cloud
        .create_profile(&session, &name)
        .await
        .map_err(|e| e.to_string())?;
    match cloud
        .map_local_profile(
            &session,
            profile.id,
            device_id,
            browser_kind,
            &browser_profile_dir,
        )
        .await
    {
        Ok(m) => view_after_mapping(&state, &session, m, user_data_dir.as_deref()).await,
        Err(map_err) => {
            let rollback = cloud.delete_profile(&session, profile.id).await;
            Err(match rollback {
                Ok(()) => {
                    format!("could not map the new profile ({map_err}); it was removed again")
                }
                Err(e) => format!(
                    "could not map the new profile ({map_err}); removing it also failed ({e})"
                ),
            })
        }
    }
}

/// Unmap in the cloud. The local registry entry is kept on purpose: if the extension later
/// presents that token, the handshake can report "mapping deleted" instead of "unknown".
#[tauri::command]
async fn unmap_local_profile(state: State<'_, AppState>, mapping_id: Uuid) -> Result<(), String> {
    let session = state.session().await?;
    state
        .cloud()?
        .unmap_local_profile(&session, mapping_id)
        .await
        .map_err(|e| e.to_string())?;
    // The local profile stops being the bookmarks source; captured ops are kept.
    if let Ok(mut db) = bookmarks::open_db() {
        let _ = db.release_source(mapping_id);
    }
    Ok(())
}

/// Sync this mapping's bookmarks now: download + materialise remote changes, then upload
/// pending local ones. Returns the new bookmarks status.
#[tauri::command]
async fn publish_bookmarks(
    state: State<'_, AppState>,
    mapping_id: Uuid,
) -> Result<Option<synchrome_state::BookmarkSyncStatus>, String> {
    let db = bookmarks::open_db()?;
    let Some(profile_id) = db
        .profile_for_mapping(mapping_id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    drop(db);
    // pull before push (remote changes are materialised; the extension applies them)
    bookmarks::sync_profile(&state, profile_id).await?;
    Ok(bookmarks::status_for_mapping(mapping_id, profile_id))
}

/// Conflicts of a mapping of THIS device (open ones; `include_closed` adds the history).
/// Titles/URLs are the user's own data and are shown; no token or internal id beyond the
/// conflict's handle and the remote device id (mapped to a device name by the UI).
#[tauri::command]
fn list_conflicts(
    state: State<'_, AppState>,
    mapping_id: Uuid,
    include_closed: bool,
) -> Result<Vec<synchrome_state::ConflictView>, String> {
    bookmarks::conflicts_for_mapping(&state, mapping_id, include_closed)
}

#[derive(serde::Deserialize)]
struct ResolveItem {
    conflict_id: String,
    resolution: synchrome_state::Resolution,
}

/// Resolve conflicts of a mapping of THIS device (only the current account's own profile;
/// a conflict id of another profile or account is rejected as not found).
#[tauri::command]
async fn resolve_conflicts(
    state: State<'_, AppState>,
    mapping_id: Uuid,
    items: Vec<ResolveItem>,
) -> Result<Vec<bookmarks::ResolveResult>, String> {
    let items = items
        .into_iter()
        .map(|i| (i.conflict_id, i.resolution))
        .collect();
    bookmarks::resolve_for_mapping(&state, mapping_id, items).await
}

/// Extensions of a mapping of THIS device: this device vs the synchronised state.
#[tauri::command]
fn list_extensions(
    state: State<'_, AppState>,
    mapping_id: Uuid,
) -> Result<Vec<synchrome_state::ExtView>, String> {
    bookmarks::extensions_for_mapping(&state, mapping_id)
}

/// Install (opens the official store page) / Enable / Disable / Remove (confirmed in the UI)
/// / Keep / Retry for one extension of a mapping of THIS device.
#[tauri::command]
async fn extension_action(
    state: State<'_, AppState>,
    mapping_id: Uuid,
    ext_id: String,
    action: synchrome_state::UserExtAction,
) -> Result<(), String> {
    bookmarks::extension_action(&state, mapping_id, &ext_id, action).await
}

/// Settings sync state of the extensions of a mapping of THIS device (counts only).
#[tauri::command]
fn list_extension_settings(
    state: State<'_, AppState>,
    mapping_id: Uuid,
) -> Result<Vec<synchrome_state::SettingsView>, String> {
    ext_settings::settings_for_mapping(&state, mapping_id)
}

/// Off / Local only / Local + Sync for one extension's settings, on THIS device (opt-in).
#[tauri::command]
async fn set_extension_settings_mode(
    state: State<'_, AppState>,
    mapping_id: Uuid,
    ext_id: String,
    mode: synchrome_sync::extension_settings::Mode,
) -> Result<(), String> {
    ext_settings::set_settings_mode(&state, mapping_id, &ext_id, mode, None).await
}

/// Run the extension settings cycle now (reads/writes only if the browser is closed).
#[tauri::command]
async fn extension_settings_sync_now(
    state: State<'_, AppState>,
    mapping_id: Uuid,
) -> Result<ext_settings::CycleReport, String> {
    ext_settings::settings_sync_now(&state, mapping_id).await
}

/// Browser preferences state of a mapping of THIS device (counts only, never values).
#[tauri::command]
fn list_browser_preferences(
    state: State<'_, AppState>,
    mapping_id: Uuid,
) -> Result<synchrome_state::PrefsView, String> {
    browser_prefs::prefs_for_mapping(&state, mapping_id)
}

/// Off / Sync recommended preferences, on THIS device (opt-in).
#[tauri::command]
async fn set_browser_preferences_mode(
    state: State<'_, AppState>,
    mapping_id: Uuid,
    mode: synchrome_sync::browser_prefs::Mode,
) -> Result<(), String> {
    browser_prefs::set_prefs_mode(&state, mapping_id, mode).await
}

/// Run the browser preferences cycle now (reads/writes only if the browser is closed).
#[tauri::command]
async fn browser_preferences_sync_now(
    state: State<'_, AppState>,
    mapping_id: Uuid,
) -> Result<browser_prefs::CycleReport, String> {
    browser_prefs::prefs_sync_now(&state, mapping_id).await
}

/// Realtime + compaction diagnostics of a mapping of THIS device (counts and generations).
#[tauri::command]
async fn sync_diagnostics(
    state: State<'_, AppState>,
    mapping_id: Uuid,
) -> Result<compaction::Diagnostics, String> {
    compaction::diagnostics(&state, mapping_id).await
}

/// Explicit user action: remove a device from a profile's compaction (reversible).
#[tauri::command]
async fn retire_device(
    state: State<'_, AppState>,
    profile_id: Uuid,
    device_id: Uuid,
) -> Result<(), String> {
    compaction::retire_device(&state, profile_id, device_id).await
}

/// Explicit user action: a retired device takes part in the compaction again.
#[tauri::command]
async fn reactivate_device(
    state: State<'_, AppState>,
    profile_id: Uuid,
    device_id: Uuid,
) -> Result<(), String> {
    compaction::reactivate_device(&state, profile_id, device_id).await
}

/// Retry writing the mapping token into the local extension store (browser must be closed).
#[tauri::command]
async fn write_mapping_token_local(
    state: State<'_, AppState>,
    mapping_id: Uuid,
) -> Result<MappingView, String> {
    write_token_for_mapping(&state, mapping_id, None)
        .await
        .map(|(view, _)| view)
}

/// Register the native messaging host for a browser (writes its NativeMessagingHosts manifest,
/// which is not a browser profile and is not TCC-protected). Returns the manifest path.
#[tauri::command]
fn install_native_host(
    browser_kind: BrowserKind,
    user_data_dir: Option<String>,
) -> Result<String, String> {
    let udd = match user_data_dir.filter(|u| !u.is_empty()) {
        Some(u) => std::path::PathBuf::from(u),
        None => synchrome_browser::user_data_dir(browser_kind)
            .ok_or_else(|| "unsupported platform".to_string())?,
    };
    // the manifest must point at a path that will still exist when the browser starts
    let loc = app_location();
    if !loc.stable {
        return Err(loc.reason.unwrap_or_default());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    synchrome_browser::install_native_host(&udd, &exe)
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())
}

/// Mapping of a local profile on THIS device, if any.
#[tauri::command]
async fn mapping_for_local_profile(
    state: State<'_, AppState>,
    browser_kind: BrowserKind,
    browser_profile_dir: String,
) -> Result<Option<MappingView>, String> {
    let session = state.session().await?;
    let me = state.device()?.device_id;
    let found = state
        .cloud()?
        .mapping_for_local_profile(&session, me, browser_kind, &browser_profile_dir)
        .await
        .map_err(|e| e.to_string())?;
    match found {
        Some(m) => Ok(mapping_views(&state, &session, vec![m]).await?.pop()),
        None => Ok(None),
    }
}

/// All mappings (across devices) of one cloud profile.
#[tauri::command]
async fn mapping_for_cloud_profile(
    state: State<'_, AppState>,
    profile_id: Uuid,
) -> Result<Vec<MappingView>, String> {
    let session = state.session().await?;
    let mappings = state
        .cloud()?
        .mapping_for_cloud_profile(&session, profile_id)
        .await
        .map_err(|e| e.to_string())?;
    mapping_views(&state, &session, mappings).await
}

/// Development-only UI driver (debug builds, `SYNCHROME_DEV_UI_DRIVER=<dir>`): the app
/// evaluates `<dir>/in/*.js` in its own (embedded-frontend) window, and the page reports the
/// result through this command into `<dir>/out/<id>.json`. Lets an E2E harness drive the
/// REAL UI (WKWebView offers no automation protocol on macOS). Absent from release builds.
#[tauri::command]
fn dev_ui_report(id: String, value: serde_json::Value) -> Result<(), String> {
    #[cfg(debug_assertions)]
    {
        let dir = std::env::var_os("SYNCHROME_DEV_UI_DRIVER").ok_or("ui driver off")?;
        let safe: String = id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let out = std::path::Path::new(&dir).join("out");
        std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        std::fs::write(out.join(format!("{safe}.json")), value.to_string())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (id, value);
        Err("not available".into())
    }
}

#[cfg(debug_assertions)]
fn start_dev_ui_driver(app: &tauri::App) {
    use tauri::Manager;
    let Some(dir) = std::env::var_os("SYNCHROME_DEV_UI_DRIVER") else {
        return;
    };
    let input = std::path::PathBuf::from(dir).join("in");
    let _ = std::fs::create_dir_all(&input);
    let handle = app.handle().clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(250));
        let Ok(rd) = std::fs::read_dir(&input) else {
            continue;
        };
        let mut files: Vec<_> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "js"))
            .collect();
        files.sort();
        for f in files {
            let js = std::fs::read_to_string(&f).unwrap_or_default();
            let _ = std::fs::remove_file(&f);
            if let Some(w) = handle.webview_windows().values().next() {
                let _ = w.eval(&js);
            }
        }
    });
}

/// Native messaging host mode (see [`host`]).
pub fn run_native_host(origin: &str) -> i32 {
    host::run(origin)
}

/// Development-only CLI (see [`devcli`]).
#[cfg(debug_assertions)]
pub fn run_dev_cli(args: &[String]) -> i32 {
    devcli::run(args)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::new())
        .setup(|_app| {
            #[cfg(debug_assertions)]
            start_dev_ui_driver(_app);
            // Realtime wake-up: one connection for the account signed in at startup (if any)
            realtime::start();
            {
                let s = AppState::new();
                realtime::set_account(
                    s.auth()
                        .ok()
                        .and_then(|a| a.stored_session().ok().flatten())
                        .map(|x| x.user_id),
                );
            }
            // Background publisher: retries the bookmarks outbox (offline, app restarts,
            // a host killed mid-upload). The per-profile lease in state.sqlite keeps it from
            // racing a native host that is publishing at the same moment.
            tauri::async_runtime::spawn(async {
                let state = AppState::new();
                loop {
                    // pull (download + materialise) then publish; the extension applies
                    // materialised remote changes on its next connection
                    let _ = bookmarks::sync_all(&state).await;
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_browsers,
            app_version,
            cloud_status,
            account_status,
            sign_in,
            sign_up,
            refresh_session,
            sign_out,
            device_identity,
            register_device,
            list_devices,
            list_profiles,
            create_profile,
            rename_profile,
            delete_profile,
            list_mappings,
            map_local_profile,
            create_and_map_local_profile,
            unmap_local_profile,
            write_mapping_token_local,
            install_native_host,
            install_location,
            extension_folder,
            reveal_extension_folder,
            open_full_disk_access_settings,
            mapping_for_local_profile,
            mapping_for_cloud_profile,
            publish_bookmarks,
            list_conflicts,
            resolve_conflicts,
            list_extensions,
            extension_action,
            list_extension_settings,
            set_extension_settings_mode,
            extension_settings_sync_now,
            list_browser_preferences,
            set_browser_preferences_mode,
            browser_preferences_sync_now,
            sync_diagnostics,
            retire_device,
            reactivate_device,
            dev_ui_report
        ])
        .run(tauri::generate_context!())
        .expect("error while running Synchrome");
}
