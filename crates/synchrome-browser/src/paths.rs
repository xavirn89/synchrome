//! Per-browser user-data directory resolution.
//!
//! Paths were validated in Milestone 0 (`docs/01` §8.1). Only macOS is implemented for M1;
//! Windows and Linux return `None` for now so the rest of the code can stay platform-agnostic.

use std::path::PathBuf;
use synchrome_types::BrowserKind;

/// Absolute path to a browser's user-data directory on this OS, if the OS is supported.
///
/// Returns the path whether or not it exists; callers probe existence/access separately.
pub fn user_data_dir(kind: BrowserKind) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        let base = PathBuf::from(home).join("Library/Application Support");
        let rel = match kind {
            BrowserKind::Helium => "net.imput.helium",
            BrowserKind::Chrome => "Google/Chrome",
            BrowserKind::Brave => "BraveSoftware/Brave-Browser",
            BrowserKind::Edge => "Microsoft Edge",
            BrowserKind::Chromium => "Chromium",
        };
        Some(base.join(rel))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = kind;
        None
    }
}

/// Arguments for `/usr/bin/open` that open `url` in one specific profile of a browser: a new
/// launch request (`-n`) of its app bundle with `--profile-directory`, plus `--user-data-dir`
/// when the profile lives in a non-standard data folder. A running browser receives the request
/// through its own single-instance handling and opens the page in that profile.
#[cfg(target_os = "macos")]
pub fn open_in_profile_args(
    kind: BrowserKind,
    user_data_dir: &std::path::Path,
    profile_dir: &str,
    url: &str,
) -> Option<Vec<String>> {
    let app = app_bundle_path(kind)?;
    let mut args = vec![
        "-na".to_string(),
        app.display().to_string(),
        "--args".to_string(),
    ];
    if user_data_dir_differs(kind, user_data_dir) {
        args.push(format!("--user-data-dir={}", user_data_dir.display()));
    }
    args.push(format!("--profile-directory={profile_dir}"));
    args.push(url.to_string());
    Some(args)
}

#[cfg(target_os = "macos")]
fn user_data_dir_differs(kind: BrowserKind, udd: &std::path::Path) -> bool {
    user_data_dir(kind).map_or(true, |std| std != udd)
}

/// macOS application bundle path, used as a secondary "installed" signal when the user-data
/// directory is TCC-protected and cannot even be stat-ed.
#[cfg(target_os = "macos")]
pub fn app_bundle_path(kind: BrowserKind) -> Option<PathBuf> {
    let name = match kind {
        BrowserKind::Helium => "Helium.app",
        BrowserKind::Chrome => "Google Chrome.app",
        BrowserKind::Brave => "Brave Browser.app",
        BrowserKind::Edge => "Microsoft Edge.app",
        BrowserKind::Chromium => "Chromium.app",
    };
    Some(PathBuf::from("/Applications").join(name))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn opens_the_url_in_the_connected_profile_only() {
        let url = "https://chromewebstore.google.com/detail/synchrome/x";
        let std = user_data_dir(BrowserKind::Chrome).unwrap();
        let a = open_in_profile_args(BrowserKind::Chrome, &std, "Profile 2", url).unwrap();
        assert_eq!(
            a,
            [
                "-na",
                "/Applications/Google Chrome.app",
                "--args",
                "--profile-directory=Profile 2",
                url
            ],
            "standard data folder: no --user-data-dir"
        );
        let custom = std::path::Path::new("/tmp/My Chrome");
        let b = open_in_profile_args(BrowserKind::Helium, custom, "Default", url).unwrap();
        assert_eq!(b[1], "/Applications/Helium.app");
        assert!(b.contains(&"--user-data-dir=/tmp/My Chrome".to_string()));
        assert_eq!(
            b.last().unwrap(),
            url,
            "one argument each: no shell, spaces kept"
        );
    }
}
