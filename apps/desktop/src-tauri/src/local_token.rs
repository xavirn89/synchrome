//! Local half of a mapping: writing its token into the Synchrome extension's store, and the
//! combined (cloud + local) view the UI shows.

use serde::Serialize;
use std::path::{Path, PathBuf};
use synchrome_bridge::{
    registry::now_unix, token_sha256, LocalTokenRecord, LocalTokenState, TokenRegistry,
};
use synchrome_browser::{write_extension_setting, ExtStorageError, WriteReport};
use synchrome_cloud::{Mapping, MappingStatus, MappingSummary};
use synchrome_types::{MAPPING_TOKEN_STORAGE_KEY, SYNCHROME_EXTENSION_ID};

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
    MappingView {
        summary,
        display_status,
        local_error,
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
