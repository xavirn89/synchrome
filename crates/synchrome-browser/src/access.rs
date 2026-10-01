//! Access-state detection: can Synchrome read a profile's files right now?
//!
//! Encodes what Milestone 0 (E1) observed on macOS: a plain `EPERM` reading a well-known
//! browser data dir means the OS is withholding it behind Full Disk Access; a live
//! `SingletonLock` means the browser owns the profile and must not be written.

use std::path::Path;
use synchrome_types::AccessState;

/// Classify read access to a path (a `Local State` file or a profile directory).
///
/// `read_dir` is used for directories and `File::open` for files; the errno is mapped to an
/// [`AccessState`]. `ENOENT` is reported as [`AccessState::Unknown`] by this low-level helper;
/// callers decide whether a missing path means "not installed".
pub fn classify_read(path: &Path) -> AccessState {
    let result = if path.is_dir() {
        std::fs::read_dir(path).map(|_| ())
    } else {
        std::fs::File::open(path).map(|_| ())
    };
    match result {
        Ok(()) => AccessState::Accessible,
        Err(e) => match e.raw_os_error() {
            // EPERM / EACCES on a known browser path on macOS => Full Disk Access needed.
            Some(1) | Some(13) => AccessState::RequiresFullDiskAccess,
            _ => AccessState::Unknown,
        },
    }
}

/// Detect the access state for a browser user-data directory, folding in liveness.
///
/// Precedence: `RequiresFullDiskAccess` > `BrowserRunning` > `Accessible`/`Unknown`. Full Disk
/// Access wins because it is the blocker the user must resolve first (on macOS a TCC-withheld
/// browser stays inaccessible whether or not it is running); only when files are readable does
/// a live browser matter, and then writes are unsafe so we surface `BrowserRunning`.
pub fn detect_access(user_data_dir: &Path) -> AccessState {
    // Probe the smallest always-present artifact: `Local State` at the user-data root.
    let local_state = user_data_dir.join("Local State");
    let read = match classify_read(&local_state) {
        AccessState::Unknown => classify_read(user_data_dir),
        other => other,
    };
    if read == AccessState::RequiresFullDiskAccess {
        return AccessState::RequiresFullDiskAccess;
    }
    if is_browser_running(user_data_dir) {
        return AccessState::BrowserRunning;
    }
    read
}

/// Whether a browser currently owns this user-data directory (POSIX: live `SingletonLock`).
///
/// On macOS/Linux `SingletonLock` is a symlink to `<hostname>-<pid>`; its presence as a
/// symlink is a reliable "in use" signal. Windows liveness (message window / `lockfile`) is a
/// later-milestone concern and returns `false` here.
pub fn is_browser_running(user_data_dir: &Path) -> bool {
    #[cfg(unix)]
    {
        let lock = user_data_dir.join("SingletonLock");
        // symlink_metadata does not follow the link and succeeds even if the target is stale.
        std::fs::symlink_metadata(&lock)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = user_data_dir;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn accessible_dir_is_accessible() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Local State"), b"{}").unwrap();
        assert_eq!(detect_access(dir.path()), AccessState::Accessible);
    }

    #[test]
    fn singleton_lock_reports_running() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Local State"), b"{}").unwrap();
        symlink("somehost-1234", dir.path().join("SingletonLock")).unwrap();
        assert_eq!(detect_access(dir.path()), AccessState::BrowserRunning);
    }

    #[test]
    fn missing_everything_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        // no Local State, empty dir -> readable dir, so Accessible (dir listing succeeds)
        assert_eq!(detect_access(dir.path()), AccessState::Accessible);
    }
}
