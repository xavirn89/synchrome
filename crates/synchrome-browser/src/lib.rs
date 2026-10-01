//! Read-only discovery of installed Chromium-family browsers and their profiles.
//!
//! Milestone 1 scope: detect browsers, enumerate profiles from `Local State`, and report an
//! [`AccessState`] per browser/profile. Nothing here writes to a profile. Writing adapters
//! (Preferences, LevelDB, History) will live in sibling modules in a later milestone.

mod access;
mod discovery;
mod paths;

pub use access::{detect_access, is_browser_running};
pub use discovery::discover_browsers;
pub use paths::user_data_dir;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}
