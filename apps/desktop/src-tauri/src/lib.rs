//! Synchrome desktop (Tauri 2) application entry point.
//!
//! Milestone 1 exposes a single read-only command, [`list_browsers`], that returns the
//! discovered browsers and profiles with their access state. No profile is modified.

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener_noop())
        .invoke_handler(tauri::generate_handler![list_browsers, app_version])
        .run(tauri::generate_context!())
        .expect("error while running Synchrome");
}

/// Placeholder no-op "plugin" slot kept so the builder chain is easy to extend later.
/// Returns an identity plugin; real plugins (updater, autostart) arrive in later milestones.
fn tauri_plugin_opener_noop() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("synchrome-noop").build()
}
