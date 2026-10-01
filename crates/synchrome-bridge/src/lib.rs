//! Native-messaging bridge between the Chromium extension and the app — skeleton only.
//!
//! The wire protocol (4-byte length prefix + JSON, host->extension <= 1 MB) and the frame
//! vocabulary are specified in `docs/05` §4. M1 defines the frame enum so both sides can share
//! it; the stdio transport and session handling arrive with the live-sync milestone.

use serde::{Deserialize, Serialize};

/// A frame sent from the extension to the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtToApp {
    /// First frame: identify the browser and (optionally) a pre-seeded mapping token (E8).
    Hello {
        ext_version: String,
        browser: String,
        mapping_token: Option<String>,
    },
    Ping,
}

/// A frame sent from the app to the extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppToExt {
    /// Current sync status for the popup.
    Status {
        state: String,
    },
    Pong,
}
