//! Read-only discovery of installed Chromium-family browsers and their profiles.
//!
//! Milestone 1 scope: detect browsers, enumerate profiles from `Local State`, and report an
//! [`AccessState`] per browser/profile. Nothing here writes to a profile. Writing adapters
//! [`ext_storage`] performs the one write this milestone needs (the mapping token, E8) with
//! browser-closed, LOCK, backup and atomic-swap guarantees; [`native_host`] registers the host.

mod access;
mod discovery;
pub mod ext_storage;
pub mod native_host;
mod paths;

pub use access::{detect_access, is_browser_running};
pub use discovery::discover_browsers;
pub use ext_storage::{
    extension_store_path, is_store_locked, read_extension_settings, write_extension_setting,
    ExtStorageError, WriteReport,
};
pub use native_host::{host_manifest, install_native_host};
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
