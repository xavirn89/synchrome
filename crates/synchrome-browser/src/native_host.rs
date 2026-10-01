//! Native messaging host registration.
//!
//! Chromium looks for `<user-data-dir>/NativeMessagingHosts/<host-name>.json` (user level;
//! Milestone 0 E1: this directory is NOT TCC-protected even for Chrome). The manifest only
//! allows the Synchrome extension's origin.

use std::path::{Path, PathBuf};
use synchrome_types::{synchrome_extension_origin, NATIVE_HOST_NAME};

/// Manifest JSON for the Synchrome host. `host_path` must be absolute.
pub fn host_manifest(host_path: &Path) -> serde_json::Value {
    serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "Synchrome native messaging host",
        "path": host_path.display().to_string(),
        "type": "stdio",
        "allowed_origins": [synchrome_extension_origin()],
    })
}

/// Write (atomically) the host manifest into `<user_data_dir>/NativeMessagingHosts/`.
pub fn install_native_host(user_data_dir: &Path, host_path: &Path) -> std::io::Result<PathBuf> {
    if !host_path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "native host path must be absolute",
        ));
    }
    let dir = user_data_dir.join("NativeMessagingHosts");
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(format!("{NATIVE_HOST_NAME}.json"));
    let tmp = dir.join(format!(".{NATIVE_HOST_NAME}.json.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&host_manifest(host_path))?)?;
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_allows_only_the_synchrome_extension() {
        let m = host_manifest(Path::new(
            "/Applications/Synchrome.app/Contents/MacOS/synchrome",
        ));
        assert_eq!(m["name"], NATIVE_HOST_NAME);
        assert_eq!(m["type"], "stdio");
        let origins = m["allowed_origins"].as_array().unwrap();
        assert_eq!(origins.len(), 1);
        assert_eq!(
            origins[0],
            "chrome-extension://gmpmcnknoaogpjdoonlddhpdpgkebbog/"
        );
    }

    #[test]
    fn install_writes_manifest_and_rejects_relative_paths() {
        let udd = tempfile::tempdir().unwrap();
        assert!(install_native_host(udd.path(), Path::new("relative/host")).is_err());
        let p = install_native_host(udd.path(), Path::new("/abs/host")).unwrap();
        assert!(p.ends_with("NativeMessagingHosts/com.synchrome.host.json"));
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["path"], "/abs/host");
    }
}
