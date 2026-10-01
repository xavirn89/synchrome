//! Synchrome desktop (Tauri 2) application entry point.
//!
//! Commands are read-only with respect to browser profiles. Auth commands manage the user's
//! session, which lives exclusively in the OS credential store (macOS Keychain); cloud
//! commands use that session inside Rust only. The webview receives token-free models:
//! [`AccountStatus`], [`DeviceIdentity`], [`DeviceSummary`], [`ProfileSummary`],
//! [`MappingView`]. The only write into a browser profile is the mapping token, placed in the
//! Synchrome extension's own `chrome.storage.local` store while the browser is closed
//! (see [`local_token`]); the same binary serves as native messaging host (see [`host`]).

#[cfg(debug_assertions)]
mod devcli;
mod host;
mod local_token;

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

#[tauri::command]
fn list_browsers() -> Vec<DiscoveredBrowser> {
    synchrome_browser::discover_browsers()
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
    state
        .auth()?
        .sign_in(email.trim(), &password)
        .await
        .map_err(|e| e.to_string())
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
fn ensure_detected_local_profile(kind: BrowserKind, dir: &str) -> Result<(), String> {
    let found = synchrome_browser::discover_browsers()
        .iter()
        .any(|b| b.kind == kind && b.profiles.iter().any(|p| p.dir_name == dir));
    if found {
        Ok(())
    } else {
        Err(format!(
            "{} profile {dir:?} was not detected on this device",
            kind.display_name()
        ))
    }
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
    state
        .cloud()?
        .delete_profile(&session, profile_id)
        .await
        .map_err(|e| e.to_string())
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
) -> Result<MappingView, String> {
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
        None => synchrome_browser::user_data_dir(m.browser_kind)
            .ok_or_else(|| "unsupported platform".to_string())?,
    };
    let written = local_token::write_token_local(&m, &session.user_id, &udd);
    let view = mapping_views(state, &session, vec![m]).await?.remove(0);
    written.map(|_| view)
}

/// Create the mapping's local half right after the cloud mapping exists. Failures (typically
/// "browser is open") are recorded and shown as Error with a Retry action; they do not fail
/// the mapping itself.
async fn view_after_mapping(
    state: &AppState,
    session: &Session,
    m: Mapping,
) -> Result<MappingView, String> {
    if let Some(udd) = synchrome_browser::user_data_dir(m.browser_kind) {
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
) -> Result<MappingView, String> {
    ensure_detected_local_profile(browser_kind, &browser_profile_dir)?;
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
    view_after_mapping(&state, &session, m).await
}

/// "Create Synchrome profile" from a local profile: create the cloud profile, map it, then try
/// the local token write. If mapping fails, the just-created cloud profile is deleted again.
#[tauri::command]
async fn create_and_map_local_profile(
    state: State<'_, AppState>,
    name: String,
    browser_kind: BrowserKind,
    browser_profile_dir: String,
) -> Result<MappingView, String> {
    ensure_detected_local_profile(browser_kind, &browser_profile_dir)?;
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
        Ok(m) => view_after_mapping(&state, &session, m).await,
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
        .map_err(|e| e.to_string())
}

/// Retry writing the mapping token into the local extension store (browser must be closed).
#[tauri::command]
async fn write_mapping_token_local(
    state: State<'_, AppState>,
    mapping_id: Uuid,
) -> Result<MappingView, String> {
    write_token_for_mapping(&state, mapping_id, None).await
}

/// Register the native messaging host for a browser (writes its NativeMessagingHosts manifest,
/// which is not a browser profile and is not TCC-protected). Returns the manifest path.
#[tauri::command]
fn install_native_host(browser_kind: BrowserKind) -> Result<String, String> {
    let udd = synchrome_browser::user_data_dir(browser_kind)
        .ok_or_else(|| "unsupported platform".to_string())?;
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
        .invoke_handler(tauri::generate_handler![
            list_browsers,
            app_version,
            cloud_status,
            account_status,
            sign_in,
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
            mapping_for_local_profile,
            mapping_for_cloud_profile
        ])
        .run(tauri::generate_context!())
        .expect("error while running Synchrome");
}
