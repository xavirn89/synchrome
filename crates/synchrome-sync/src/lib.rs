//! Sync engine core (pure, no I/O), specified in `docs/03`.
//!
//! Implemented so far: hybrid logical clocks ([`hlc`]), fractional sibling keys
//! ([`fractional`]), the journal op encoding ([`op`]) and the bookmarks normaliser
//! ([`bookmarks`]) used by the upload-only capture slice. Merge, conflicts and compaction
//! come with the download slice.

pub mod bookmarks;
pub mod extensions;
pub mod fractional;
pub mod hlc;
pub mod op;

pub use hlc::{Hlc, HlcClock, HlcError};
pub use op::{Fields, Op, OpKind};

use serde::{Deserialize, Serialize};

/// Data categories Synchrome can sync, each independently switchable per profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Bookmarks,
    Extensions,
    ExtensionSettings,
    History,
    Preferences,
}

impl Scope {
    pub fn all() -> &'static [Scope] {
        &[
            Scope::Bookmarks,
            Scope::Extensions,
            Scope::ExtensionSettings,
            Scope::History,
            Scope::Preferences,
        ]
    }
}
