//! Synchrome desktop (Tauri 2) application entry point.
//!
//! Commands are read-only with respect to browser profiles. Auth commands manage the user's
//! session, which lives exclusively in the OS credential store (macOS Keychain); the webview
//! only ever receives the token-free [`AccountStatus`].

use std::sync::Arc;
use synchrome_auth::{AccountStatus, AuthClient, KeychainStore, ReqwestTransport, SessionManager};
use synchrome_supabase::CloudStatus;
use synchrome_types::DiscoveredBrowser;
use tauri::State;

/// Shared app state. `auth` is `None` when the client configuration is missing.
pub struct AppState {
    auth: Option<SessionManager<ReqwestTransport>>,
    config_error: Option<String>,
}

impl AppState {
    fn new() -> Self {
        match synchrome_config::AppConfig::load() {
            Ok(cfg) => match AuthClient::new(cfg.supabase) {
                Ok(client) => Self {
                    auth: Some(SessionManager::new(client, Arc::new(KeychainStore::new()))),
                    config_error: None,
                },
                Err(e) => Self {
                    auth: None,
                    config_error: Some(format!("http client: {e}")),
                },
            },
            Err(e) => Self {
                auth: None,
                config_error: Some(e.to_string()),
            },
        }
    }
}

/// Tauri command: discover installed Chromium-family browsers and their profiles (read-only).
#[tauri::command]
fn list_browsers() -> Vec<DiscoveredBrowser> {
    synchrome_browser::discover_browsers()
}

/// Tauri command: the Synchrome client User-Agent, so the UI can display the build.
#[tauri::command]
fn app_version() -> String {
    synchrome_types::user_agent()
}

/// Tauri command: read-only connectivity probe of Synchrome Cloud (Auth, PostgREST, Storage).
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

/// Tauri command: token-free account status. Never returns tokens.
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

/// Tauri command: email/password sign-in. Persists the session in the Keychain.
#[tauri::command]
async fn sign_in(
    state: State<'_, AppState>,
    email: String,
    password: String,
) -> Result<AccountStatus, String> {
    let m = state
        .auth
        .as_ref()
        .ok_or_else(|| state.config_error.clone().unwrap_or_default())?;
    m.sign_in(email.trim(), &password)
        .await
        .map_err(|e| e.to_string())
}

/// Tauri command: refresh the session if it is near expiry; returns the resulting status.
#[tauri::command]
async fn refresh_session(state: State<'_, AppState>) -> Result<AccountStatus, String> {
    let m = state
        .auth
        .as_ref()
        .ok_or_else(|| state.config_error.clone().unwrap_or_default())?;
    match m.ensure_valid_session().await {
        Ok(Some(s)) => Ok(AccountStatus::from_session(&s)),
        Ok(None) => Ok(AccountStatus::signed_out()),
        Err(e) => Err(e.to_string()),
    }
}

/// Tauri command: sign out. Local session is always cleared; remote failure is reported.
#[tauri::command]
async fn sign_out(state: State<'_, AppState>) -> Result<AccountStatus, String> {
    let m = state
        .auth
        .as_ref()
        .ok_or_else(|| state.config_error.clone().unwrap_or_default())?;
    match m.sign_out().await {
        Ok(()) => Ok(AccountStatus::signed_out()),
        Err(e) => Ok(AccountStatus::signed_out().with_error(format!("signed out locally; {e}"))),
    }
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
            sign_out
        ])
        .run(tauri::generate_context!())
        .expect("error while running Synchrome");
}
