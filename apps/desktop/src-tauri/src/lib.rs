//! Synchrome desktop (Tauri 2) application entry point.
//!
//! Commands are read-only with respect to browser profiles. Auth commands manage the user's
//! session, which lives exclusively in the OS credential store (macOS Keychain); cloud
//! commands use that session inside Rust only. The webview receives token-free models:
//! [`AccountStatus`], [`DeviceIdentity`], [`DeviceSummary`], [`ProfileSummary`].

use std::sync::Arc;
use synchrome_auth::{
    AccountStatus, AuthClient, AuthError, KeychainStore, ReqwestTransport, Session, SessionManager,
};
use synchrome_cloud::{CloudClient, DeviceSummary, ProfileSummary, ReqwestRest};
use synchrome_config::SupabaseConfig;
use synchrome_device::DeviceIdentity;
use synchrome_supabase::CloudStatus;
use synchrome_types::DiscoveredBrowser;
use tauri::State;

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
            list_profiles
        ])
        .run(tauri::generate_context!())
        .expect("error while running Synchrome");
}
