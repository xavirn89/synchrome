//! Local ↔ cloud profile mapping model.
//!
//! - [`MappingToken`]: 256-bit random identifier (64 lowercase hex). Not an account
//!   credential, but treated as sensitive: redacted in `Debug`/`Display`, never serialised.
//! - [`Mapping`]: internal row as returned by PostgREST. Deliberately **not** `Serialize`, so
//!   it cannot be handed to the webview by accident.
//! - [`MappingSummary`]: the only mapping type the frontend receives; carries a 4-char
//!   `token_hint`, never the token.

use serde::{Deserialize, Serialize};
use std::fmt;
use synchrome_types::BrowserKind;
use uuid::Uuid;

/// Random mapping token seeded (later) into the extension's `storage.local` (E8).
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct MappingToken(String);

impl MappingToken {
    /// 32 bytes from the OS CSPRNG, hex-encoded.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }

    /// Raw value. Only the cloud insert and the future local seeding read it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Short, non-reversible hint safe for the UI (first 4 hex chars).
    pub fn hint(&self) -> String {
        let head: String = self.0.chars().take(4).collect();
        format!("{head}…")
    }

    /// Matches the server-side check `^[0-9a-f]{64}$`.
    pub fn is_well_formed(&self) -> bool {
        self.0.len() == 64
            && self
                .0
                .bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    }
}

impl fmt::Debug for MappingToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MappingToken(<redacted>)")
    }
}
impl fmt::Display for MappingToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Server-side token lifecycle (`profile_mappings.token_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenState {
    /// Mapping exists in the cloud; the token is not yet written into the local extension store.
    PendingLocalWrite,
    /// The extension read the seeded token back (future slice).
    Confirmed,
}

/// Internal mapping row. Not `Serialize` on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Mapping {
    pub id: Uuid,
    pub profile_id: Uuid,
    pub device_id: Uuid,
    pub browser_kind: BrowserKind,
    pub browser_profile_dir: String,
    pub mapping_token: MappingToken,
    pub token_state: TokenState,
    #[serde(default)]
    pub token_confirmed_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// UI-facing mapping status. A summary only exists for a mapped profile, so "mapped" is
/// implied; the status tells where the token is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingStatus {
    TokenPendingLocalWrite,
    TokenConfirmed,
}

/// Safe mapping view for the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MappingSummary {
    pub id: Uuid,
    pub mapped: bool,
    pub status: MappingStatus,
    pub profile_id: Uuid,
    pub profile_name: Option<String>,
    pub device_id: Uuid,
    pub device_name: Option<String>,
    pub is_this_device: bool,
    pub browser_kind: BrowserKind,
    pub browser_profile_dir: String,
    pub token_hint: String,
    /// When the companion extension proved it holds the token (handshake), if it has.
    pub token_confirmed_at: Option<String>,
    pub created_at: Option<String>,
}

impl Mapping {
    pub fn summary(
        &self,
        profile_name: Option<String>,
        device_name: Option<String>,
        this_device: Uuid,
    ) -> MappingSummary {
        MappingSummary {
            id: self.id,
            mapped: true,
            status: match self.token_state {
                TokenState::PendingLocalWrite => MappingStatus::TokenPendingLocalWrite,
                TokenState::Confirmed => MappingStatus::TokenConfirmed,
            },
            profile_id: self.profile_id,
            profile_name,
            device_id: self.device_id,
            device_name,
            is_this_device: self.device_id == this_device,
            browser_kind: self.browser_kind,
            browser_profile_dir: self.browser_profile_dir.clone(),
            token_hint: self.mapping_token.hint(),
            token_confirmed_at: self.token_confirmed_at.clone(),
            created_at: self.created_at.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn row(token: &str) -> Mapping {
        serde_json::from_value(serde_json::json!({
            "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "profile_id": "33333333-3333-4333-8333-333333333333",
            "device_id": "11111111-1111-4111-8111-111111111111",
            "browser_kind": "helium",
            "browser_profile_dir": "Profile 2",
            "mapping_token": token,
            "token_state": "pending_local_write",
            "token_confirmed_at": null,
            "created_at": "2026-10-01T00:00:00+00:00",
            "updated_at": "2026-10-01T00:00:00+00:00"
        }))
        .unwrap()
    }

    #[test]
    fn tokens_are_well_formed_and_unique() {
        let mut seen = HashSet::new();
        for _ in 0..2000 {
            let t = MappingToken::generate().unwrap();
            assert!(t.is_well_formed(), "malformed token");
            assert!(seen.insert(t.expose().to_string()), "duplicate token");
        }
    }

    #[test]
    fn token_never_prints() {
        let t = MappingToken::generate().unwrap();
        assert!(!format!("{t:?}").contains(t.expose()));
        assert_eq!(format!("{t}"), "<redacted>");
        let m = row(t.expose());
        assert!(
            !format!("{m:?}").contains(t.expose()),
            "Debug of Mapping leaked the token"
        );
    }

    #[test]
    fn summary_is_safe_and_flags_this_device() {
        let token = "ab".repeat(32);
        let m = row(&token);
        let me = m.device_id;
        let s = m.summary(Some("Personal".into()), Some("Core".into()), me);
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains(&token), "full token reached the summary");
        assert!(!json.contains("mapping_token"));
        assert!(!json.contains("account_id"));
        assert_eq!(s.token_hint, "abab…");
        assert!(s.mapped && s.is_this_device);
        assert!(s.token_confirmed_at.is_none());
        assert_eq!(s.status, MappingStatus::TokenPendingLocalWrite);
        assert!(json.contains("\"browser_kind\":\"helium\""));
        let other = m.summary(None, None, Uuid::nil());
        assert!(!other.is_this_device);
    }
}
