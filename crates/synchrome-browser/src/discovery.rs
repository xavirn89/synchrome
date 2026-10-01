//! Orchestrates read-only discovery of installed browsers and their profiles.

use crate::access::detect_access;
use crate::paths::user_data_dir;
use std::path::Path;
use synchrome_types::{AccessState, BrowserKind, DiscoveredBrowser, DiscoveredProfile};

/// Discover every installed Chromium-family browser and its profiles on this machine.
///
/// A browser is included when its user-data directory exists or is at least stat-able (TCC can
/// withhold reads while still allowing metadata). Nothing is written. Browsers that are not
/// installed are omitted.
pub fn discover_browsers() -> Vec<DiscoveredBrowser> {
    let mut out = Vec::new();
    for &kind in BrowserKind::all() {
        let Some(udd) = user_data_dir(kind) else {
            continue;
        };
        if !is_present(kind, &udd) {
            continue;
        }
        let access = detect_access(&udd);
        let version = read_version(&udd);
        let profiles = enumerate_profiles(&udd, access);
        out.push(DiscoveredBrowser {
            kind,
            display_name: kind.display_name().to_string(),
            user_data_dir: udd.to_string_lossy().into_owned(),
            version,
            access,
            profiles,
        });
    }
    out
}

/// Is this browser installed? The user-data dir being stat-able is the primary signal; the
/// macOS app bundle is a fallback when the dir itself is TCC-withheld.
fn is_present(kind: BrowserKind, udd: &Path) -> bool {
    if std::fs::symlink_metadata(udd).is_ok() {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(app) = crate::paths::app_bundle_path(kind) {
            return app.exists();
        }
    }
    let _ = kind;
    false
}

/// Read the browser's reported version from the `Last Version` file if accessible.
fn read_version(udd: &Path) -> Option<String> {
    std::fs::read_to_string(udd.join("Last Version"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Enumerate profiles from `Local State → profile.info_cache`.
///
/// When `Local State` cannot be read (e.g. Full Disk Access needed), the profile list is empty
/// and the browser's access state already tells the UI why.
fn enumerate_profiles(udd: &Path, browser_access: AccessState) -> Vec<DiscoveredProfile> {
    let Ok(raw) = std::fs::read_to_string(udd.join("Local State")) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(cache) = value
        .get("profile")
        .and_then(|p| p.get("info_cache"))
        .and_then(|c| c.as_object())
    else {
        return Vec::new();
    };

    // A running browser locks the whole user-data dir, so every profile inherits that state.
    let per_profile_access = if browser_access == AccessState::BrowserRunning {
        AccessState::BrowserRunning
    } else {
        AccessState::Accessible
    };

    let mut profiles: Vec<DiscoveredProfile> = cache
        .iter()
        .map(|(dir_name, entry)| DiscoveredProfile {
            dir_name: dir_name.clone(),
            display_name: entry
                .get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string()),
            access: per_profile_access,
        })
        .collect();
    // Stable, human-friendly ordering by directory name.
    profiles.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
    profiles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_reads_info_cache() {
        let dir = tempfile::tempdir().unwrap();
        let local_state = r#"{
            "profile": { "info_cache": {
                "Profile 2": { "name": "Work" },
                "Default":   { "name": "Personal" }
            }}
        }"#;
        std::fs::write(dir.path().join("Local State"), local_state).unwrap();
        let profiles = enumerate_profiles(dir.path(), AccessState::Accessible);
        assert_eq!(profiles.len(), 2);
        // sorted by dir_name: "Default" before "Profile 2"
        assert_eq!(profiles[0].dir_name, "Default");
        assert_eq!(profiles[0].display_name.as_deref(), Some("Personal"));
        assert_eq!(profiles[1].display_name.as_deref(), Some("Work"));
    }

    #[test]
    fn missing_local_state_yields_no_profiles() {
        let dir = tempfile::tempdir().unwrap();
        assert!(enumerate_profiles(dir.path(), AccessState::Accessible).is_empty());
    }
}
