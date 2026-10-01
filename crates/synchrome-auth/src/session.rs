//! Session model. Tokens are wrapped so they never print in `Debug` and the session type is
//! deliberately **not** `Serialize`: it must never cross to the frontend. Only
//! [`AccountStatus`] (token-free) is serialisable.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// A secret string that redacts itself in `Debug`/`Display`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    /// Access the raw value. Call sites are the only places a token is ever read.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// An authenticated session. Persisted ONLY through a [`crate::CredentialStore`].
///
/// `Serialize`/`Deserialize` exist solely so the credential store can encode it into the OS
/// keychain item; the Tauri layer never serialises this type to the webview.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub user_id: String,
    pub email: Option<String>,
    pub access_token: Secret,
    pub refresh_token: Secret,
    /// Unix seconds when the access token expires.
    pub expires_at: i64,
    pub token_type: String,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("user_id", &self.user_id)
            .field("email", &self.email)
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("token_type", &self.token_type)
            .finish()
    }
}

/// Seconds of remaining validity below which a session is considered "needs refresh".
pub const REFRESH_SKEW_SECS: i64 = 60;

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Session {
    /// Expired or within the refresh skew window at the given instant.
    pub fn needs_refresh_at(&self, now: i64) -> bool {
        self.expires_at - now <= REFRESH_SKEW_SECS
    }
    pub fn needs_refresh(&self) -> bool {
        self.needs_refresh_at(now_unix())
    }
    pub fn is_expired_at(&self, now: i64) -> bool {
        self.expires_at <= now
    }
}

/// Token-free view of the account for the UI. The ONLY auth type the frontend ever receives.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AccountStatus {
    pub signed_in: bool,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub expires_at: Option<i64>,
    pub needs_refresh: bool,
    pub error: Option<String>,
}

impl AccountStatus {
    pub fn signed_out() -> Self {
        Self::default()
    }
    pub fn from_session(s: &Session) -> Self {
        Self {
            signed_in: true,
            user_id: Some(s.user_id.clone()),
            email: s.email.clone(),
            expires_at: Some(s.expires_at),
            needs_refresh: s.needs_refresh(),
            error: None,
        }
    }
    pub fn with_error(mut self, e: impl Into<String>) -> Self {
        self.error = Some(e.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(expires_at: i64) -> Session {
        Session {
            user_id: "u1".into(),
            email: Some("a@b.c".into()),
            access_token: Secret::new("ACCESS-TOKEN-XYZ"),
            refresh_token: Secret::new("REFRESH-TOKEN-XYZ"),
            expires_at,
            token_type: "bearer".into(),
        }
    }

    #[test]
    fn debug_never_prints_tokens() {
        let d = format!("{:?}", s(0));
        assert!(!d.contains("ACCESS-TOKEN-XYZ"));
        assert!(!d.contains("REFRESH-TOKEN-XYZ"));
        assert!(d.contains("<redacted>"));
        assert_eq!(format!("{}", Secret::new("x")), "<redacted>");
    }

    #[test]
    fn expiry_logic() {
        let now = 1_000_000;
        assert!(s(now - 1).is_expired_at(now));
        assert!(s(now + 30).needs_refresh_at(now)); // inside skew
        assert!(!s(now + 3600).needs_refresh_at(now));
    }

    #[test]
    fn account_status_has_no_tokens() {
        let st = AccountStatus::from_session(&s(now_unix() + 3600));
        let v = serde_json::to_value(&st).unwrap();
        let text = v.to_string();
        assert!(!text.contains("TOKEN"));
        assert!(v.get("access_token").is_none());
        assert!(v.get("refresh_token").is_none());
        assert_eq!(v["signed_in"], true);
        assert_eq!(v["email"], "a@b.c");
    }
}
