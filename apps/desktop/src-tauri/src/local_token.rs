//! Local half of a mapping: writing its token into the Synchrome extension's store, and the
//! combined (cloud + local) view the UI shows.

use serde::Serialize;
use std::path::{Path, PathBuf};
use synchrome_bridge::{
    registry::now_unix, token_sha256, LocalTokenRecord, LocalTokenState, TokenRegistry,
};
use synchrome_browser::{write_extension_setting, ExtStorageError, WriteReport};
use synchrome_cloud::{Mapping, MappingStatus, MappingSummary};
use synchrome_types::{LEGACY_EXTENSION_IDS, MAPPING_TOKEN_STORAGE_KEY, SYNCHROME_EXTENSION_ID};

/// What the UI shows for a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayStatus {
    PendingLocalWrite,
    TokenWritten,
    ExtensionConfirmed,
    Error,
}

/// Safe mapping view for the webview: cloud summary + local write state. No token.
#[derive(Debug, Clone, Serialize)]
pub struct MappingView {
    #[serde(flatten)]
    pub summary: MappingSummary,
    pub display_status: DisplayStatus,
    pub local_error: Option<String>,
    /// Bookmarks capture/upload state (this device's mappings only). No titles, no URLs.
    pub bookmarks: Option<synchrome_state::BookmarkSyncStatus>,
    /// Extensions counts (this device's mappings only). No names.
    pub extensions: Option<synchrome_state::ExtStatus>,
}

pub fn app_data_dir() -> Result<PathBuf, String> {
    synchrome_device::app_data_dir().ok_or_else(|| "no application data directory".to_string())
}

pub fn registry() -> TokenRegistry {
    match app_data_dir() {
        Ok(d) => TokenRegistry::in_dir(&d),
        Err(_) => TokenRegistry::in_dir(&std::env::temp_dir().join("synchrome")),
    }
}

pub fn view(summary: MappingSummary, registry: &TokenRegistry) -> MappingView {
    let rec = if summary.is_this_device {
        registry.find_by_mapping(summary.id)
    } else {
        None
    };
    let (display_status, local_error) = if summary.status == MappingStatus::TokenConfirmed {
        (DisplayStatus::ExtensionConfirmed, None)
    } else {
        match rec {
            Some(r) if r.state == LocalTokenState::Failed => (DisplayStatus::Error, r.last_error),
            Some(r) => (DisplayStatus::TokenWritten, r.last_error),
            None => (DisplayStatus::PendingLocalWrite, None),
        }
    };
    let bookmarks = if summary.is_this_device {
        crate::bookmarks::status_for_mapping(summary.id, summary.profile_id)
    } else {
        None
    };
    let extensions = if summary.is_this_device && bookmarks.is_some() {
        crate::bookmarks::ext_status(summary.profile_id)
    } else {
        None
    };
    MappingView {
        summary,
        display_status,
        local_error,
        bookmarks,
        extensions,
    }
}

fn reason(e: &ExtStorageError) -> String {
    match e {
        ExtStorageError::BrowserRunning => {
            "browser is open — close it, then Retry local write".to_string()
        }
        ExtStorageError::Locked => "extension store is locked — close the browser and retry".into(),
        ExtStorageError::Invalid(m) => format!("invalid target: {m}"),
        ExtStorageError::Io { path, source } => format!("could not write {path}: {source}"),
        ExtStorageError::LevelDb(m) | ExtStorageError::Verify(m) => {
            format!("local write failed: {m}")
        }
    }
}

/// Whether the Chrome Web Store companion of this browser profile still lacks the connection
/// token: its store exists (the store companion is installed there), the browser is closed and
/// no token is stored yet. This happens when a profile connected through the pre-store companion
/// switches to the store one (the old companion's storage goes away with it).
pub fn store_companion_missing_token(user_data_dir: &Path, profile_dir: &str) -> bool {
    let store =
        synchrome_browser::extension_store_path(user_data_dir, profile_dir, SYNCHROME_EXTENSION_ID);
    if !store.exists() || synchrome_browser::is_browser_running(user_data_dir) {
        return false;
    }
    synchrome_browser::read_extension_settings(user_data_dir, profile_dir, SYNCHROME_EXTENSION_ID)
        .map(|kv| !kv.contains_key(MAPPING_TOKEN_STORAGE_KEY))
        .unwrap_or(false)
}

/// Pre-store companion ids that already have a local store in this browser profile (an old
/// unpacked companion is, or was, installed there): they get the token as well.
pub fn legacy_token_targets(user_data_dir: &Path, profile_dir: &str) -> Vec<&'static str> {
    LEGACY_EXTENSION_IDS
        .iter()
        .copied()
        .filter(|id| {
            synchrome_browser::extension_store_path(user_data_dir, profile_dir, id).exists()
        })
        .collect()
}

/// Write `mapping`'s token into `<user_data_dir>/<dir>/Local Extension Settings/<ext-id>` and
/// record the outcome in the local registry (hash only). Errors are user-facing strings.
pub fn write_token_local(
    mapping: &Mapping,
    account_id: &str,
    user_data_dir: &Path,
) -> Result<WriteReport, String> {
    let backups = app_data_dir()?.join("backups").join("ext-storage");
    let value = serde_json::to_string(mapping.mapping_token.expose()).map_err(|e| e.to_string())?;
    let reg = registry();
    let previous = reg.find_by_mapping(mapping.id);
    // a profile that still runs the pre-store companion keeps working: its store gets the
    // token too (only when that store already exists — nothing is created for an old id)
    for legacy in legacy_token_targets(user_data_dir, &mapping.browser_profile_dir) {
        let _ = write_extension_setting(
            user_data_dir,
            &mapping.browser_profile_dir,
            legacy,
            MAPPING_TOKEN_STORAGE_KEY,
            &value,
            &backups,
        );
    }
    match write_extension_setting(
        user_data_dir,
        &mapping.browser_profile_dir,
        SYNCHROME_EXTENSION_ID,
        MAPPING_TOKEN_STORAGE_KEY,
        &value,
        &backups,
    ) {
        Ok(report) => {
            let _ = reg.upsert(LocalTokenRecord {
                token_sha256: token_sha256(mapping.mapping_token.expose()),
                account_id: account_id.to_string(),
                mapping_id: mapping.id,
                profile_id: mapping.profile_id,
                browser_kind: mapping.browser_kind,
                user_data_dir: user_data_dir.display().to_string(),
                profile_dir: mapping.browser_profile_dir.clone(),
                state: LocalTokenState::Written,
                updated_at: now_unix(),
                last_error: None,
            });
            Ok(report)
        }
        Err(e) => {
            let msg = reason(&e);
            // Keep an earlier successful write's hash/state (that token is still on disk).
            let rec = match previous {
                Some(mut p) if p.state != LocalTokenState::Failed => {
                    p.last_error = Some(msg.clone());
                    p.updated_at = now_unix();
                    p
                }
                _ => LocalTokenRecord {
                    token_sha256: String::new(),
                    account_id: account_id.to_string(),
                    mapping_id: mapping.id,
                    profile_id: mapping.profile_id,
                    browser_kind: mapping.browser_kind,
                    user_data_dir: user_data_dir.display().to_string(),
                    profile_dir: mapping.browser_profile_dir.clone(),
                    state: LocalTokenState::Failed,
                    updated_at: now_unix(),
                    last_error: Some(msg.clone()),
                },
            };
            let _ = reg.upsert(rec);
            Err(msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_store_companion_without_the_connection_token_is_detected_only_when_installed() {
        let udd = std::env::temp_dir().join(format!("synchrome-reseed-{}", std::process::id()));
        let backups = udd.join("backups");
        let _ = std::fs::remove_dir_all(&udd);
        std::fs::create_dir_all(udd.join("Default")).unwrap();
        assert!(
            !store_companion_missing_token(&udd, "Default"),
            "store companion not installed"
        );
        // the store companion ran there (its storage exists) but holds no connection token
        write_extension_setting(
            &udd,
            "Default",
            SYNCHROME_EXTENSION_ID,
            "synchromeStatus",
            "{}",
            &backups,
        )
        .unwrap();
        assert!(store_companion_missing_token(&udd, "Default"));
        write_extension_setting(
            &udd,
            "Default",
            SYNCHROME_EXTENSION_ID,
            MAPPING_TOKEN_STORAGE_KEY,
            "\"t\"",
            &backups,
        )
        .unwrap();
        assert!(
            !store_companion_missing_token(&udd, "Default"),
            "token present"
        );
        let _ = std::fs::remove_dir_all(&udd);
    }

    #[test]
    fn legacy_store_gets_the_token_only_where_the_old_companion_already_has_one() {
        let udd = std::env::temp_dir().join(format!("synchrome-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&udd);
        std::fs::create_dir_all(udd.join("Default")).unwrap();
        assert!(
            legacy_token_targets(&udd, "Default").is_empty(),
            "nothing created for an old id"
        );
        let old = synchrome_browser::extension_store_path(&udd, "Default", LEGACY_EXTENSION_IDS[0]);
        std::fs::create_dir_all(&old).unwrap();
        assert_eq!(
            legacy_token_targets(&udd, "Default"),
            [LEGACY_EXTENSION_IDS[0]]
        );
        assert!(
            legacy_token_targets(&udd, "Profile 2").is_empty(),
            "per profile"
        );
        let _ = std::fs::remove_dir_all(&udd);
    }
}
