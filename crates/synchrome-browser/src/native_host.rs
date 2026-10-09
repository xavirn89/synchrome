//! Native messaging host registration.
//!
//! Chromium looks for `<user-data-dir>/NativeMessagingHosts/<host-name>.json` (user level;
//! Milestone 0 E1: this directory is NOT TCC-protected even for Chrome). The manifest only
//! allows the Synchrome companion's origins (its store id and the pre-store development id).

use std::path::{Path, PathBuf};
use synchrome_types::{synchrome_extension_origins, NATIVE_HOST_NAME};

/// Manifest JSON for the Synchrome host. `host_path` must be absolute.
pub fn host_manifest(host_path: &Path) -> serde_json::Value {
    serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "Synchrome native messaging host",
        "path": host_path.display().to_string(),
        "type": "stdio",
        "allowed_origins": synchrome_extension_origins(),
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

/// Bring an EXISTING Synchrome host manifest up to date (path of this app, current allowed
/// origins). A browser without the manifest is left alone (installing the connector stays a
/// user action). Returns whether the file was rewritten.
pub fn refresh_native_host(user_data_dir: &Path, host_path: &Path) -> std::io::Result<bool> {
    let target = user_data_dir
        .join("NativeMessagingHosts")
        .join(format!("{NATIVE_HOST_NAME}.json"));
    let Ok(bytes) = std::fs::read(&target) else {
        return Ok(false);
    };
    let current: Option<serde_json::Value> = serde_json::from_slice(&bytes).ok();
    if current.as_ref() == Some(&host_manifest(host_path)) {
        return Ok(false);
    }
    install_native_host(user_data_dir, host_path).map(|_| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_updates_only_an_existing_outdated_manifest() {
        let udd = tempfile::tempdir().unwrap();
        let host = Path::new("/Applications/Synchrome.app/Contents/MacOS/synchrome-desktop");
        // no connector installed: nothing is created
        assert!(!refresh_native_host(udd.path(), host).unwrap());
        assert!(!udd.path().join("NativeMessagingHosts").exists());
        // a 1.0.2 manifest (pre-store origin only) is rewritten with the current origins
        let dir = udd.path().join("NativeMessagingHosts");
        std::fs::create_dir_all(&dir).unwrap();
        let old = serde_json::json!({
            "name": NATIVE_HOST_NAME, "description": "Synchrome native messaging host",
            "path": host.display().to_string(), "type": "stdio",
            "allowed_origins": ["chrome-extension://gmpmcnknoaogpjdoonlddhpdpgkebbog/"],
        });
        let file = dir.join(format!("{NATIVE_HOST_NAME}.json"));
        std::fs::write(&file, serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(refresh_native_host(udd.path(), host).unwrap());
        let now: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(now, host_manifest(host));
        // already current: untouched
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        assert!(!refresh_native_host(udd.path(), host).unwrap());
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), mtime);
        // a manifest pointing at another copy of the app is repointed
        std::fs::write(
            &file,
            serde_json::to_vec(&host_manifest(Path::new("/Volumes/Synchrome/x"))).unwrap(),
        )
        .unwrap();
        assert!(refresh_native_host(udd.path(), host).unwrap());
    }

    #[test]
    fn manifest_allows_only_the_synchrome_extension() {
        let m = host_manifest(Path::new(
            "/Applications/Synchrome.app/Contents/MacOS/synchrome",
        ));
        assert_eq!(m["name"], NATIVE_HOST_NAME);
        assert_eq!(m["type"], "stdio");
        let origins: Vec<&str> = m["allowed_origins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_str().unwrap())
            .collect();
        assert_eq!(
            origins,
            [
                "chrome-extension://ceflmoojogokmklgcmcihdgakbefgiee/",
                "chrome-extension://gmpmcnknoaogpjdoonlddhpdpgkebbog/"
            ]
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
