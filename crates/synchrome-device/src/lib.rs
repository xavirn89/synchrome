//! Stable device identity for this Synchrome installation.
//!
//! A random UUID v4 generated once and persisted in the app's data directory
//! (`~/Library/Application Support/Synchrome/device.json` on macOS). It is **not** derived from
//! hardware serials, MAC addresses or any invasive system identifier, and it is not a secret.
//! A reinstall that wipes the data directory yields a new device.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

pub const DEVICE_FILE: &str = "device.json";
pub const APP_DIR_NAME: &str = "Synchrome";

#[derive(Debug, Error)]
pub enum DeviceError {
    #[error("no application data directory available on this platform")]
    NoDataDir,
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("device file is corrupt: {0}")]
    Corrupt(String),
}

/// What identifies this installation. Safe to show in the UI and to send to the cloud.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub device_id: Uuid,
    pub device_name: String,
    pub platform: String,
    pub os_version: Option<String>,
    pub app_version: String,
}

/// Only the immutable part is persisted; name/platform/versions are recomputed each start so
/// renames and OS/app upgrades are reflected without touching the id.
#[derive(Debug, Serialize, Deserialize)]
struct Persisted {
    device_id: Uuid,
    created_at_unix: i64,
}

/// The platform's application data directory for Synchrome.
pub fn app_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(APP_DIR_NAME))
}

/// Load the identity from `dir/device.json`, creating it (and the dir) on first run.
pub fn load_or_create_in(dir: &Path) -> Result<DeviceIdentity, DeviceError> {
    let path = dir.join(DEVICE_FILE);
    let persisted = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Persisted>(&text)
            .map_err(|e| DeviceError::Corrupt(e.to_string()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir).map_err(|source| DeviceError::Io {
                path: dir.display().to_string(),
                source,
            })?;
            let p = Persisted {
                device_id: Uuid::new_v4(),
                created_at_unix: now_unix(),
            };
            // Write to a temp file then rename so a crash never leaves a half-written id.
            let tmp = dir.join(format!("{DEVICE_FILE}.tmp"));
            std::fs::write(&tmp, serde_json::to_vec_pretty(&p).unwrap()).map_err(|source| {
                DeviceError::Io {
                    path: tmp.display().to_string(),
                    source,
                }
            })?;
            std::fs::rename(&tmp, &path).map_err(|source| DeviceError::Io {
                path: path.display().to_string(),
                source,
            })?;
            p
        }
        Err(source) => {
            return Err(DeviceError::Io {
                path: path.display().to_string(),
                source,
            })
        }
    };
    Ok(DeviceIdentity {
        device_id: persisted.device_id,
        device_name: host_name(),
        platform: platform(),
        os_version: os_version(),
        app_version: synchrome_types::VERSION.to_string(),
    })
}

/// Load/create in the default app data directory.
pub fn load_or_create() -> Result<DeviceIdentity, DeviceError> {
    let dir = app_data_dir().ok_or(DeviceError::NoDataDir)?;
    load_or_create_in(&dir)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn platform() -> String {
    std::env::consts::OS.to_string()
}

/// Human-friendly machine name (e.g. "MacBook Pro de Xavi"), falling back to the hostname.
fn host_name() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "this device".to_string())
}

fn os_version() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!v.is_empty()).then_some(format!("macOS {v}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_once_and_is_stable_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let a = load_or_create_in(dir.path()).unwrap(); // "first launch"
        let b = load_or_create_in(dir.path()).unwrap(); // "restart"
        assert_eq!(a.device_id, b.device_id);
        assert!(dir.path().join(DEVICE_FILE).exists());
        assert!(!dir.path().join(format!("{DEVICE_FILE}.tmp")).exists());
    }

    #[test]
    fn different_dirs_are_different_devices() {
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        assert_ne!(
            load_or_create_in(d1.path()).unwrap().device_id,
            load_or_create_in(d2.path()).unwrap().device_id
        );
    }

    #[test]
    fn corrupt_file_is_reported_not_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(DEVICE_FILE), b"{not json").unwrap();
        assert!(matches!(
            load_or_create_in(dir.path()),
            Err(DeviceError::Corrupt(_))
        ));
    }

    #[test]
    fn identity_has_platform_and_app_version() {
        let dir = tempfile::tempdir().unwrap();
        let id = load_or_create_in(dir.path()).unwrap();
        assert_eq!(id.platform, std::env::consts::OS);
        assert_eq!(id.app_version, synchrome_types::VERSION);
        assert!(!id.device_name.is_empty());
    }
}
