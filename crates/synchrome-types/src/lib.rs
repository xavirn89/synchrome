//! Shared domain types for Synchrome.
//!
//! This crate holds the vocabulary used across the sync engine, browser adapters,
//! storage adapters and the Tauri UI. It is intentionally dependency-light (serde only)
//! so every other crate can depend on it without pulling in I/O.

use serde::{Deserialize, Serialize};

/// The Synchrome product version, surfaced in the HTTP `User-Agent` and the UI.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The `User-Agent` every Synchrome HTTP/WebSocket client must send.
///
/// Milestone 0 (E9) proved the Cloudflare front returns `403 error code 1010` for default
/// library agents, so no client may rely on reqwest's default UA.
pub fn user_agent() -> String {
    format!("Synchrome/{VERSION}")
}

/// Stable id of the Synchrome companion extension, pinned by the public `key` in
/// `apps/extension/manifest.json`. Native messaging only accepts this origin.
pub const SYNCHROME_EXTENSION_ID: &str = "gmpmcnknoaogpjdoonlddhpdpgkebbog";

/// Native messaging host name registered for the extension (`connectNative` target).
pub const NATIVE_HOST_NAME: &str = "com.synchrome.host";

/// The `chrome.storage.local` key under which the app seeds the mapping token (E8).
pub const MAPPING_TOKEN_STORAGE_KEY: &str = "mappingToken";

/// Origin string Chromium passes to the native host for the Synchrome extension.
pub fn synchrome_extension_origin() -> String {
    format!("chrome-extension://{SYNCHROME_EXTENSION_ID}/")
}

/// A Chromium-family browser family Synchrome knows how to discover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserKind {
    Helium,
    Chrome,
    Brave,
    Edge,
    Chromium,
}

impl BrowserKind {
    /// Stable identifier used in config and the UI.
    pub fn id(self) -> &'static str {
        match self {
            BrowserKind::Helium => "helium",
            BrowserKind::Chrome => "chrome",
            BrowserKind::Brave => "brave",
            BrowserKind::Edge => "edge",
            BrowserKind::Chromium => "chromium",
        }
    }

    /// Human-facing display name.
    pub fn display_name(self) -> &'static str {
        match self {
            BrowserKind::Helium => "Helium",
            BrowserKind::Chrome => "Google Chrome",
            BrowserKind::Brave => "Brave",
            BrowserKind::Edge => "Microsoft Edge",
            BrowserKind::Chromium => "Chromium",
        }
    }

    /// Every family Synchrome can attempt to discover.
    pub fn all() -> &'static [BrowserKind] {
        &[
            BrowserKind::Helium,
            BrowserKind::Chrome,
            BrowserKind::Brave,
            BrowserKind::Edge,
            BrowserKind::Chromium,
        ]
    }
}

/// Whether Synchrome can currently read a browser's profile files.
///
/// Mirrors what Milestone 0 (E1) observed on macOS: Helium is freely readable; Chrome/Brave/
/// Edge data is TCC-protected and needs Full Disk Access; a running browser may still be read
/// but must never be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessState {
    /// Profile files are readable now.
    Accessible,
    /// Readable only after the user grants Full Disk Access (macOS TCC, EPERM observed).
    RequiresFullDiskAccess,
    /// The browser owns this profile right now; reads are best-effort, writes are unsafe.
    BrowserRunning,
    /// Access could not be determined (unexpected error).
    Unknown,
}

/// One installed browser and its discovered profiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredBrowser {
    pub kind: BrowserKind,
    pub display_name: String,
    /// Absolute path to the browser's user-data directory.
    pub user_data_dir: String,
    /// Browser version if it could be read from `Last Version`, else `None`.
    pub version: Option<String>,
    /// Overall access state for this browser's data directory.
    pub access: AccessState,
    pub profiles: Vec<DiscoveredProfile>,
}

/// One local Chromium profile inside a browser's user-data directory.
///
/// The directory name (`Default`, `Profile 2`, …) is incidental; Synchrome maps profiles by
/// a stable UUID, never by folder name (see `docs/03` §1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredProfile {
    /// On-disk directory name, e.g. `Default` or `Profile 2`.
    pub dir_name: String,
    /// Display name from `Local State → profile.info_cache[dir].name`, if available.
    pub display_name: Option<String>,
    /// Per-profile access state (a running browser locks the whole user-data dir).
    pub access: AccessState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_is_branded() {
        let ua = user_agent();
        assert!(ua.starts_with("Synchrome/"), "ua was {ua}");
        assert!(!ua.contains("reqwest"));
    }

    #[test]
    fn browser_ids_are_stable_and_unique() {
        let ids: Vec<_> = BrowserKind::all().iter().map(|b| b.id()).collect();
        let mut dedup = ids.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(ids.len(), dedup.len(), "browser ids must be unique");
    }

    #[test]
    fn access_state_serializes_snake_case() {
        let j = serde_json::to_string(&AccessState::RequiresFullDiskAccess).unwrap();
        assert_eq!(j, "\"requires_full_disk_access\"");
    }
}
