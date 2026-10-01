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
