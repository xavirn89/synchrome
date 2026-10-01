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
/// On macOS/Linux `SingletonLock` is a symlink to `<hostname>-<pid>`. The directory is in use
/// only if that hostname is this machine AND the pid is alive. A browser killed abruptly
/// leaves a stale lock behind (observed in the E2E handshake run), so mere presence is not
/// enough. A lock owned by another hostname (shared/network profile dir) or one that cannot be
/// parsed is treated as in use — the conservative answer for a writer. Windows liveness
/// (message window / `lockfile`) is a later-milestone concern and returns `false` here.
pub fn is_browser_running(user_data_dir: &Path) -> bool {
    #[cfg(unix)]
    {
        let lock = user_data_dir.join("SingletonLock");
        let Ok(meta) = std::fs::symlink_metadata(&lock) else {
            return false; // no lock at all
        };
        if !meta.file_type().is_symlink() {
            return true; // unexpected shape: be conservative
        }
        let Ok(target) = std::fs::read_link(&lock) else {
            return true;
        };
        let target = target.to_string_lossy();
        let Some((host, pid)) = target.rsplit_once('-') else {
            return true;
        };
        let Ok(pid) = pid.parse::<i32>() else {
            return true;
        };
        if pid <= 0 || host != local_hostname() {
            return true;
        }
        pid_alive(pid)
    }
    #[cfg(not(unix))]
    {
        let _ = user_data_dir;
        false
    }
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: kill with signal 0 performs only the existence/permission check.
    let r = unsafe { libc::kill(pid, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn local_hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buf is valid for buf.len() bytes; gethostname NUL-terminates on success.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if r != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
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
    fn stale_lock_from_a_dead_local_pid_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Local State"), b"{}").unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        symlink(
            format!("{}-{dead}", local_hostname()),
            dir.path().join("SingletonLock"),
        )
        .unwrap();
        assert!(
            !is_browser_running(dir.path()),
            "stale lock must not count as running"
        );
        assert_eq!(detect_access(dir.path()), AccessState::Accessible);
    }

    #[test]
    fn live_local_lock_is_running_and_foreign_host_is_conservative() {
        let dir = tempfile::tempdir().unwrap();
        symlink(
            format!("{}-{}", local_hostname(), std::process::id()),
            dir.path().join("SingletonLock"),
        )
        .unwrap();
        assert!(is_browser_running(dir.path()));
        let other = tempfile::tempdir().unwrap();
        symlink(
            "some-other-machine.local-1",
            other.path().join("SingletonLock"),
        )
        .unwrap();
        assert!(
            is_browser_running(other.path()),
            "foreign host lock is treated as in use"
        );
        let junk = tempfile::tempdir().unwrap();
        symlink("not-a-lock", junk.path().join("SingletonLock")).unwrap();
        assert!(
            is_browser_running(junk.path()),
            "unparseable lock is treated as in use"
        );
    }

    #[test]
    fn missing_everything_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        // no Local State, empty dir -> readable dir, so Accessible (dir listing succeeds)
        assert_eq!(detect_access(dir.path()), AccessState::Accessible);
    }
}
