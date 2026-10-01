//! Synchrome desktop (Tauri 2) application entry point.
//!
//! Milestone 1 commands are all read-only: [`list_browsers`] discovers browsers/profiles,
//! [`cloud_status`] probes Synchrome Cloud connectivity, [`app_version`] reports the build.
//! No profile is modified, no user is authenticated, nothing is written anywhere.

use synchrome_supabase::CloudStatus;
use synchrome_types::DiscoveredBrowser;

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
///
/// Loads the client configuration from the environment / `.env`; if it is missing the result
/// explains which variable is absent instead of failing the command.
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            list_browsers,
            app_version,
            cloud_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running Synchrome");
}
