//! GoTrue client: password login, refresh, logout. Transport is injected so tests mock HTTP.

use crate::session::{now_unix, Secret, Session};
use serde::Deserialize;
use std::future::Future;
use std::pin::Pin;
use synchrome_config::SupabaseConfig;
use synchrome_http::{HttpClient, HttpError};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthError {
    /// Wrong password — or unknown account: GoTrue deliberately returns the same error for
    /// both so account existence is not disclosed.
    #[error("invalid email or password")]
    InvalidCredentials,
    /// Refresh token rejected/revoked: the session is definitively gone.
    #[error("session expired or revoked; please sign in again")]
    SessionInvalid,
    #[error("network error: {0}")]
    Network(String),
    #[error("server error (HTTP {0})")]
    Server(u16),
    #[error("invalid response from auth service: {0}")]
    InvalidResponse(String),
    #[error("request rejected (HTTP {status}): {message}")]
    Rejected { status: u16, message: String },
}

/// Boxed future returned by [`AuthTransport::post_json`].
pub type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), HttpError>> + Send + 'a>>;

/// Raw HTTP transport the client speaks through. The production impl wraps
/// [`synchrome_http::HttpClient`]; tests provide a scripted mock.
pub trait AuthTransport: Send + Sync {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(&'a str, &'a str)],
        body: &'a serde_json::Value,
    ) -> TransportFuture<'a>;
}

pub struct ReqwestTransport(pub HttpClient);

impl AuthTransport for ReqwestTransport {
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(&'a str, &'a str)],
        body: &'a serde_json::Value,
    ) -> TransportFuture<'a> {
        Box::pin(self.0.post_json(url, headers, body))
    }
}

/// GoTrue `/token` success payload (fields we use).
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    token_type: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    expires_at: Option<i64>,
    user: TokenUser,
}
#[derive(Deserialize)]
struct TokenUser {
    id: String,
    #[serde(default)]
    email: Option<String>,
}

/// GoTrue error payload — both the legacy and the current shapes.
#[derive(Deserialize, Default)]
struct ErrorBody {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    msg: Option<String>,
}

/// Parse a `/token` response body into a [`Session`]. Pure, unit-testable.
pub fn parse_token_response(bytes: &[u8], now: i64) -> Result<Session, AuthError> {
    let t: TokenResponse = serde_json::from_slice(bytes)
        .map_err(|e| AuthError::InvalidResponse(format!("token payload: {e}")))?;
    if t.access_token.is_empty() || t.refresh_token.is_empty() {
        return Err(AuthError::InvalidResponse("empty token".into()));
    }
    let expires_at = t
        .expires_at
        .or_else(|| t.expires_in.map(|s| now + s))
        .ok_or_else(|| AuthError::InvalidResponse("no expiry".into()))?;
    Ok(Session {
        user_id: t.user.id,
        email: t.user.email,
        access_token: Secret::new(t.access_token),
        refresh_token: Secret::new(t.refresh_token),
        expires_at,
        token_type: t.token_type,
    })
}

/// Map a non-2xx `/token` response to a typed error. `is_refresh` selects the invalid-grant
/// meaning: bad password for login, dead session for refresh.
pub fn classify_error(status: u16, bytes: &[u8], is_refresh: bool) -> AuthError {
    if (500..=599).contains(&status) {
        return AuthError::Server(status);
    }
    let body: ErrorBody = serde_json::from_slice(bytes).unwrap_or_default();
    let code = body
        .error_code
        .clone()
        .or(body.error.clone())
        .unwrap_or_default();
    let msg = body
        .msg
        .clone()
        .or(body.error_description.clone())
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).chars().take(200).collect());
    let lower = format!("{code} {msg}").to_lowercase();
    let invalid_grant = status == 400 || status == 401 || lower.contains("invalid");
    if invalid_grant
        && (lower.contains("invalid_grant")
            || lower.contains("invalid_credentials")
            || lower.contains("invalid login")
            || lower.contains("refresh_token")
            || lower.contains("invalid refresh")
            || lower.contains("email not confirmed"))
    {
        return if is_refresh {
            AuthError::SessionInvalid
        } else {
            AuthError::InvalidCredentials
        };
    }
    if is_refresh && (status == 400 || status == 401 || status == 403) {
        return AuthError::SessionInvalid;
    }
    if !is_refresh && (status == 400 || status == 401) {
        return AuthError::InvalidCredentials;
    }
    AuthError::Rejected {
        status,
        message: msg,
    }
}

pub struct AuthClient<T: AuthTransport> {
    cfg: SupabaseConfig,
    transport: T,
}

impl AuthClient<ReqwestTransport> {
    /// Production client over the branded Synchrome HTTP client.
    pub fn new(cfg: SupabaseConfig) -> Result<Self, HttpError> {
        Ok(Self {
            cfg,
            transport: ReqwestTransport(HttpClient::new()?),
        })
    }
}

impl<T: AuthTransport> AuthClient<T> {
    pub fn with_transport(cfg: SupabaseConfig, transport: T) -> Self {
        Self { cfg, transport }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.url.trim_end_matches('/'), path)
    }

    /// `POST /auth/v1/token?grant_type=password`
    pub async fn login_password(&self, email: &str, password: &str) -> Result<Session, AuthError> {
        let body = serde_json::json!({ "email": email, "password": password });
        let headers = [
            ("apikey", self.cfg.anon_key.as_str()),
            ("Content-Type", "application/json"),
        ];
        let (status, bytes) = self
            .transport
            .post_json(
                &self.url("/auth/v1/token?grant_type=password"),
                &headers,
                &body,
            )
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        if (200..=299).contains(&status) {
            parse_token_response(&bytes, now_unix())
        } else {
            Err(classify_error(status, &bytes, false))
        }
    }

    /// `POST /auth/v1/token?grant_type=refresh_token`
    pub async fn refresh(&self, refresh_token: &Secret) -> Result<Session, AuthError> {
        let body = serde_json::json!({ "refresh_token": refresh_token.expose() });
        let headers = [
            ("apikey", self.cfg.anon_key.as_str()),
            ("Content-Type", "application/json"),
        ];
        let (status, bytes) = self
            .transport
            .post_json(
                &self.url("/auth/v1/token?grant_type=refresh_token"),
                &headers,
                &body,
            )
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        if (200..=299).contains(&status) {
            parse_token_response(&bytes, now_unix())
        } else {
            Err(classify_error(status, &bytes, true))
        }
    }

    /// `POST /auth/v1/logout` (best effort; the caller clears local state regardless).
    pub async fn logout(&self, access_token: &Secret) -> Result<(), AuthError> {
        let bearer = format!("Bearer {}", access_token.expose());
        let headers = [
            ("apikey", self.cfg.anon_key.as_str()),
            ("Authorization", bearer.as_str()),
        ];
        let (status, bytes) = self
            .transport
            .post_json(
                &self.url("/auth/v1/logout"),
                &headers,
                &serde_json::json!({}),
            )
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        if (200..=299).contains(&status) || status == 401 || status == 403 || status == 404 {
            // Already-invalid tokens are fine for a logout.
            Ok(())
        } else {
            Err(classify_error(status, &bytes, true))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"{"access_token":"AT1","token_type":"bearer","expires_in":3600,
        "refresh_token":"RT1","user":{"id":"user-1","email":"x@y.z"}}"#;

    #[test]
    fn parses_success_with_expires_in() {
        let s = parse_token_response(OK.as_bytes(), 1000).unwrap();
        assert_eq!(s.user_id, "user-1");
        assert_eq!(s.email.as_deref(), Some("x@y.z"));
        assert_eq!(s.expires_at, 4600);
        assert_eq!(s.access_token.expose(), "AT1");
        assert_eq!(s.token_type, "bearer");
    }

    #[test]
    fn prefers_absolute_expires_at() {
        let body = r#"{"access_token":"a","token_type":"bearer","expires_in":10,"expires_at":99999,
            "refresh_token":"r","user":{"id":"u"}}"#;
        assert_eq!(
            parse_token_response(body.as_bytes(), 0).unwrap().expires_at,
            99999
        );
    }

    #[test]
    fn rejects_garbage_and_empty_tokens() {
        assert!(matches!(
            parse_token_response(b"not json", 0),
            Err(AuthError::InvalidResponse(_))
        ));
        let empty = r#"{"access_token":"","token_type":"bearer","expires_in":1,"refresh_token":"r","user":{"id":"u"}}"#;
        assert!(matches!(
            parse_token_response(empty.as_bytes(), 0),
            Err(AuthError::InvalidResponse(_))
        ));
    }

    #[test]
    fn classifies_login_errors() {
        let legacy =
            br#"{"error":"invalid_grant","error_description":"Invalid login credentials"}"#;
        assert_eq!(
            classify_error(400, legacy, false),
            AuthError::InvalidCredentials
        );
        let modern =
            br#"{"code":400,"error_code":"invalid_credentials","msg":"Invalid login credentials"}"#;
        assert_eq!(
            classify_error(400, modern, false),
            AuthError::InvalidCredentials
        );
        assert_eq!(classify_error(503, b"", false), AuthError::Server(503));
        assert!(matches!(
            classify_error(429, br#"{"msg":"rate limited"}"#, false),
            AuthError::Rejected { status: 429, .. }
        ));
    }

    #[test]
    fn classifies_refresh_errors_as_session_invalid() {
        let b = br#"{"error":"invalid_grant","error_description":"Invalid Refresh Token"}"#;
        assert_eq!(classify_error(400, b, true), AuthError::SessionInvalid);
        assert_eq!(classify_error(401, b"{}", true), AuthError::SessionInvalid);
        assert_eq!(classify_error(500, b"", true), AuthError::Server(500));
    }
}
