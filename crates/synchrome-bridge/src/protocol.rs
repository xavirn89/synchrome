//! Wire messages between the Synchrome extension and the native host (JSON per frame).

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// The mapping token as received from the extension. Redacted in `Debug`/`Display`; there is
/// no `Serialize`, so it can never be echoed back over the wire.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct WireToken(String);

impl WireToken {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Same rule as the server check `^[0-9a-f]{64}$`.
    pub fn is_well_formed(&self) -> bool {
        self.0.len() == 64
            && self
                .0
                .bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    }
    /// Short, safe hint for logs.
    pub fn hint(&self) -> String {
        format!("{}…", self.0.chars().take(4).collect::<String>())
    }
}
impl fmt::Debug for WireToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WireToken(<redacted>)")
    }
}

/// Extension -> app.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtToApp {
    /// First frame. `mapping_token` is whatever the extension found in `chrome.storage.local`.
    Hello {
        ext_version: String,
        #[serde(default)]
        browser: Option<String>,
        #[serde(default)]
        mapping_token: Option<WireToken>,
    },
    Ping,
}

/// Every handshake outcome the app distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeStatus {
    /// Token matched a mapping of the signed-in account for this device; confirmed.
    Connected,
    /// The extension store has no token (profile not mapped / not written yet).
    TokenMissing,
    /// No mapping with this token in the account and no local record of it.
    TokenUnknown,
    /// This device wrote the token while signed in to a different account.
    TokenOtherAccount,
    /// The token was written for a mapping that no longer exists.
    MappingDeleted,
    /// The mapping is gone and so is its cloud profile.
    CloudProfileDeleted,
    /// The mapping belongs to another device (token copied between machines?).
    WrongDevice,
    /// The app has no valid session.
    NotSignedIn,
    /// Synchrome Cloud could not be reached / answered with an error.
    CloudUnavailable,
}

impl HandshakeStatus {
    /// The four states the extension UI shows.
    pub fn ui_state(self) -> &'static str {
        match self {
            HandshakeStatus::Connected => "connected",
            HandshakeStatus::TokenMissing => "unmapped",
            HandshakeStatus::TokenUnknown
            | HandshakeStatus::TokenOtherAccount
            | HandshakeStatus::MappingDeleted
            | HandshakeStatus::CloudProfileDeleted
            | HandshakeStatus::WrongDevice => "token_mismatch",
            HandshakeStatus::NotSignedIn | HandshakeStatus::CloudUnavailable => "app_unavailable",
        }
    }
}

/// The only mapping information ever sent back to the extension. Never contains the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandshakeReply {
    pub status: HandshakeStatus,
    pub ui_state: &'static str,
    pub mapped: bool,
    pub profile_id: Option<Uuid>,
    pub profile_name: Option<String>,
    pub device_id: Option<Uuid>,
    pub token_state: Option<&'static str>,
    pub detail: Option<String>,
}

impl HandshakeReply {
    pub fn status(status: HandshakeStatus) -> Self {
        Self {
            status,
            ui_state: status.ui_state(),
            mapped: false,
            profile_id: None,
            profile_name: None,
            device_id: None,
            token_state: None,
            detail: None,
        }
    }
    pub fn with_detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }
}

/// App -> extension.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppToExt {
    HelloResult(HandshakeReply),
    Pong,
    Error { code: &'static str, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_parses_and_redacts_token() {
        let t = "ab".repeat(32);
        let m: ExtToApp = serde_json::from_str(&format!(
            r#"{{"type":"hello","ext_version":"0.1.0","browser":"helium","mapping_token":"{t}"}}"#
        ))
        .unwrap();
        let dbg = format!("{m:?}");
        assert!(!dbg.contains(&t), "Debug leaked the token");
        match m {
            ExtToApp::Hello {
                mapping_token: Some(tok),
                ..
            } => assert!(tok.is_well_formed()),
            _ => panic!("expected hello"),
        }
    }

    #[test]
    fn hello_without_token_is_valid() {
        let m: ExtToApp =
            serde_json::from_str(r#"{"type":"hello","ext_version":"0.1.0"}"#).unwrap();
        assert!(matches!(
            m,
            ExtToApp::Hello {
                mapping_token: None,
                ..
            }
        ));
    }

    #[test]
    fn reply_serializes_flat_with_ui_state_and_no_token() {
        let mut r = HandshakeReply::status(HandshakeStatus::Connected);
        r.mapped = true;
        r.profile_name = Some("Personal".into());
        let v = serde_json::to_value(AppToExt::HelloResult(r)).unwrap();
        assert_eq!(v["type"], "hello_result");
        assert_eq!(v["status"], "connected");
        assert_eq!(v["ui_state"], "connected");
        assert!(v.get("mapping_token").is_none());
    }

    #[test]
    fn ui_state_mapping_covers_the_four_states() {
        use HandshakeStatus::*;
        assert_eq!(Connected.ui_state(), "connected");
        assert_eq!(TokenMissing.ui_state(), "unmapped");
        for s in [
            TokenUnknown,
            TokenOtherAccount,
            MappingDeleted,
            CloudProfileDeleted,
            WrongDevice,
        ] {
            assert_eq!(s.ui_state(), "token_mismatch");
        }
        for s in [NotSignedIn, CloudUnavailable] {
            assert_eq!(s.ui_state(), "app_unavailable");
        }
    }
}
