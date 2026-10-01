//! Sync engine — interfaces only in Milestone 1.
//!
//! The real engine (hybrid logical clocks, per-field LWW, tombstones, bookmark tree
//! invariants, conflict records, compaction) is specified in `docs/03`. Here we pin the
//! vocabulary so other crates can reference it; no merge logic is implemented yet.

use serde::{Deserialize, Serialize};

/// Hybrid logical clock timestamp (see `docs/03` §2). Ordering/merge impl comes later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hlc {
    pub wall_ms: u64,
    pub counter: u32,
    pub device_id: String,
}

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
