//! PostgREST client over an injectable transport (mockable in tests).

use crate::models::{DeviceSummary, ProfileSummary};
use serde::Deserialize;
use std::future::Future;
use std::pin::Pin;
use synchrome_auth::Session;
use synchrome_config::SupabaseConfig;
use synchrome_device::DeviceIdentity;
use synchrome_http::{HttpClient, HttpError};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CloudError {
    #[error("not signed in or session rejected (HTTP 401)")]
    Unauthorized,
    #[error("forbidden by server policy (HTTP 403)")]
    Forbidden,
    /// The table does not exist yet: the `0002` migration has not been applied.
    #[error("cloud schema not available (migration not applied): {0}")]
    SchemaMissing(String),
    #[error("postgrest error {code}: {message}")]
    Postgrest { code: String, message: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("server error (HTTP {0})")]
    Server(u16),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    /// Unique constraint violated (Postgres 23505), e.g. local profile already mapped,
    /// duplicate profile name, cloud profile already mapped on this device.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Foreign key violated (Postgres 23503) on insert/update: the referenced profile or
    /// device does not exist **in this account**.
    #[error("unknown or foreign reference: {0}")]
    UnknownReference(String),
    /// Foreign key RESTRICT blocked a delete: the profile is still mapped on some device.
    #[error("profile is still mapped on at least one device; unmap it first")]
    ProfileInUse,
    /// Check constraint violated (Postgres 23514): invalid field value.
    #[error("invalid value: {0}")]
    InvalidValue(String),
    /// The target row does not exist in this account (0 rows affected under RLS).
    #[error("not found")]
    NotFound,
}

pub type RestFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), HttpError>> + Send + 'a>>;

/// Raw transport. Production wraps [`HttpClient`]; tests script responses.
pub trait RestTransport: Send + Sync {
    fn request<'a>(
        &'a self,
        method: &'a str,
        url: &'a str,
        headers: &'a [(&'a str, &'a str)],
        body: Option<&'a serde_json::Value>,
    ) -> RestFuture<'a>;
}

pub struct ReqwestRest(pub HttpClient);

impl RestTransport for ReqwestRest {
    fn request<'a>(
        &'a self,
        method: &'a str,
        url: &'a str,
        headers: &'a [(&'a str, &'a str)],
        body: Option<&'a serde_json::Value>,
    ) -> RestFuture<'a> {
        Box::pin(self.0.request_json(method, url, headers, body))
    }
}

/// The ONLY place the access token is read. Returns owned header values.
pub fn auth_headers(cfg: &SupabaseConfig, session: &Session) -> Vec<(String, String)> {
    vec![
        ("apikey".into(), cfg.anon_key.clone()),
        (
            "Authorization".into(),
            format!("Bearer {}", session.access_token.expose()),
        ),
        ("Accept".into(), "application/json".into()),
    ]
}

#[derive(Deserialize, Default)]
struct PgError {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Map a non-2xx PostgREST response to a typed error.
pub fn classify_postgrest(status: u16, bytes: &[u8]) -> CloudError {
    if (500..=599).contains(&status) {
        return CloudError::Server(status);
    }
    let e: PgError = serde_json::from_slice(bytes).unwrap_or_default();
    let code = e.code.clone().unwrap_or_default();
    let message = e
        .message
        .clone()
        .unwrap_or_else(|| String::from_utf8_lossy(bytes).chars().take(200).collect());
    match code.as_str() {
        "23505" => return CloudError::Conflict(message),
        "23503" => return CloudError::UnknownReference(message),
        "23514" => return CloudError::InvalidValue(message),
        _ => {}
    }
    match status {
        401 => CloudError::Unauthorized,
        403 => CloudError::Forbidden,
        // 42P01 = undefined_table; PostgREST also answers 404 for unknown relations.
        _ if code == "42P01" || code == "PGRST205" || status == 404 => {
            CloudError::SchemaMissing(message)
        }
        _ => CloudError::Postgrest { code, message },
    }
}

pub(crate) fn now_rfc3339() -> String {
    // Avoid a chrono dependency: PostgREST accepts epoch-derived ISO via "now()" semantics
    // only in SQL, so we build a UTC RFC 3339 string by hand from seconds since the epoch.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Civil-from-days (Howard Hinnant's algorithm).
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

pub struct CloudClient<T: RestTransport> {
    cfg: SupabaseConfig,
    pub(crate) transport: T,
}

impl CloudClient<ReqwestRest> {
    pub fn new(cfg: SupabaseConfig) -> Result<Self, HttpError> {
        Ok(Self {
            cfg,
            transport: ReqwestRest(HttpClient::new()?),
        })
    }
}

impl<T: RestTransport> CloudClient<T> {
    pub fn with_transport(cfg: SupabaseConfig, transport: T) -> Self {
        Self { cfg, transport }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/rest/v1{}", self.cfg.url.trim_end_matches('/'), path)
    }

    pub(crate) async fn call(
        &self,
        session: &Session,
        method: &str,
        path: &str,
        extra: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> Result<Vec<u8>, CloudError> {
        let owned = auth_headers(&self.cfg, session);
        let mut headers: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        headers.extend_from_slice(extra);
        let url = self.url(path);
        let (status, bytes) = self
            .transport
            .request(method, &url, &headers, body)
            .await
            .map_err(|e| CloudError::Network(e.to_string()))?;
        if (200..=299).contains(&status) {
            Ok(bytes)
        } else {
            Err(classify_postgrest(status, &bytes))
        }
    }

    pub(crate) fn one<D: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<D, CloudError> {
        let mut rows: Vec<D> = serde_json::from_slice(bytes)
            .map_err(|e| CloudError::InvalidResponse(format!("expected row array: {e}")))?;
        rows.pop()
            .ok_or_else(|| CloudError::InvalidResponse("empty row array".into()))
    }

    /// Register (or refresh) this installation: `POST /devices?on_conflict=device_id` with
    /// merge-duplicates. Also bumps `last_seen_at`. RLS stamps `account_id = auth.uid()`.
    pub async fn upsert_device(
        &self,
        session: &Session,
        me: &DeviceIdentity,
    ) -> Result<DeviceSummary, CloudError> {
        let body = serde_json::json!({
            "device_id": me.device_id,
            "name": me.device_name,
            "platform": me.platform,
            "os_version": me.os_version,
            "app_version": me.app_version,
            "last_seen_at": now_rfc3339(),
        });
        let bytes = self
            .call(
                session,
                "POST",
                "/devices?on_conflict=device_id",
                &[
                    (
                        "Prefer",
                        "resolution=merge-duplicates,return=representation",
                    ),
                    ("Content-Type", "application/json"),
                ],
                Some(&body),
            )
            .await?;
        let mut d: DeviceSummary = Self::one(&bytes)?;
        d.is_current = true;
        Ok(d)
    }

    /// `PATCH /devices?device_id=eq.<id>` setting `last_seen_at = now`.
    pub async fn touch_device(
        &self,
        session: &Session,
        device_id: Uuid,
    ) -> Result<DeviceSummary, CloudError> {
        let body = serde_json::json!({ "last_seen_at": now_rfc3339() });
        let bytes = self
            .call(
                session,
                "PATCH",
                &format!("/devices?device_id=eq.{device_id}"),
                &[
                    ("Prefer", "return=representation"),
                    ("Content-Type", "application/json"),
                ],
                Some(&body),
            )
            .await?;
        let mut d: DeviceSummary = Self::one(&bytes)?;
        d.is_current = true;
        Ok(d)
    }

    /// All devices on the account (RLS-scoped), most recently seen first. Marks `current`.
    pub async fn list_devices(
        &self,
        session: &Session,
        current: Uuid,
    ) -> Result<Vec<DeviceSummary>, CloudError> {
        let bytes = self
            .call(
                session,
                "GET",
                "/devices?select=device_id,name,platform,os_version,app_version,created_at,last_seen_at&order=last_seen_at.desc",
                &[],
                None,
            )
            .await?;
        let mut rows: Vec<DeviceSummary> = serde_json::from_slice(&bytes)
            .map_err(|e| CloudError::InvalidResponse(format!("devices: {e}")))?;
        for r in &mut rows {
            r.is_current = r.device_id == current;
        }
        Ok(rows)
    }

    /// All profiles on the account (RLS-scoped), by name.
    pub async fn list_profiles(
        &self,
        session: &Session,
    ) -> Result<Vec<ProfileSummary>, CloudError> {
        let bytes = self
            .call(
                session,
                "GET",
                "/profiles?select=id,name,created_at,updated_at&order=name.asc",
                &[],
                None,
            )
            .await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| CloudError::InvalidResponse(format!("profiles: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use synchrome_auth::Secret;

    type SeenRequest = (String, String, Vec<(String, String)>);

    /// Scripted transport. Each queued entry: (expected Authorization bearer or "*", status, body).
    /// Returns rows depending on which user's token is presented — simulates RLS at the client
    /// boundary (real RLS is validated against the instance after the migration).
    struct Mock {
        queue: Mutex<Vec<(&'static str, u16, &'static str)>>,
        pub seen: Mutex<Vec<SeenRequest>>,
    }
    impl Mock {
        fn new(q: Vec<(&'static str, u16, &'static str)>) -> Self {
            Self {
                queue: Mutex::new(q),
                seen: Mutex::new(vec![]),
            }
        }
    }
    impl RestTransport for Mock {
        fn request<'a>(
            &'a self,
            method: &'a str,
            url: &'a str,
            headers: &'a [(&'a str, &'a str)],
            _body: Option<&'a serde_json::Value>,
        ) -> RestFuture<'a> {
            self.seen.lock().unwrap().push((
                method.into(),
                url.into(),
                headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ));
            let (want_bearer, status, body) = self.queue.lock().unwrap().remove(0);
            let auth = headers
                .iter()
                .find(|(k, _)| *k == "Authorization")
                .map(|(_, v)| v.to_string())
                .unwrap_or_default();
            Box::pin(async move {
                if want_bearer != "*" && auth != format!("Bearer {want_bearer}") {
                    // Wrong user's token: RLS would hide rows / GoTrue would 401.
                    return Ok((401u16, br#"{"message":"JWT invalid"}"#.to_vec()));
                }
                Ok((status, body.as_bytes().to_vec()))
            })
        }
    }

    fn cfg() -> SupabaseConfig {
        SupabaseConfig {
            url: "https://cloud.test".into(),
            anon_key: "anon-key".into(),
        }
    }
    fn session(tok: &str, uid: &str) -> Session {
        Session {
            user_id: uid.into(),
            email: None,
            access_token: Secret::new(tok),
            refresh_token: Secret::new("r"),
            expires_at: i64::MAX,
            token_type: "bearer".into(),
        }
    }
    fn me() -> DeviceIdentity {
        DeviceIdentity {
            device_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
            device_name: "MacBook".into(),
            platform: "macos".into(),
            os_version: Some("macOS 27.0.1".into()),
            app_version: "0.1.0".into(),
        }
    }
    const DEV_A: &str = r#"[{"device_id":"11111111-1111-4111-8111-111111111111","name":"MacBook","platform":"macos","os_version":"macOS 27.0.1","app_version":"0.1.0","created_at":"2026-10-01T00:00:00Z","last_seen_at":"2026-10-01T00:00:01Z"}]"#;
    const DEVS_A: &str = r#"[{"device_id":"11111111-1111-4111-8111-111111111111","name":"MacBook","platform":"macos","app_version":"0.1.0"},{"device_id":"22222222-2222-4222-8222-222222222222","name":"Desktop","platform":"linux","app_version":"0.1.0"}]"#;
    const PROFS: &str = r#"[{"id":"33333333-3333-4333-8333-333333333333","name":"Personal","created_at":"2026-10-01T00:00:00Z","updated_at":"2026-10-01T00:00:00Z"}]"#;

    #[test]
    fn authorization_header_is_bearer_and_only_place_token_is_read() {
        let h = auth_headers(&cfg(), &session("TOK-A", "a"));
        assert_eq!(h[0], ("apikey".to_string(), "anon-key".to_string()));
        assert_eq!(
            h[1],
            ("Authorization".to_string(), "Bearer TOK-A".to_string())
        );
        assert_eq!(h[2].0, "Accept");
    }

    #[tokio::test]
    async fn upsert_device_uses_merge_duplicates_and_marks_current() {
        let m = Mock::new(vec![("TOK-A", 201, DEV_A)]);
        let c = CloudClient::with_transport(cfg(), m);
        let d = c
            .upsert_device(&session("TOK-A", "a"), &me())
            .await
            .unwrap();
        assert!(d.is_current);
        assert_eq!(d.name, "MacBook");
        let seen = c.transport.seen.lock().unwrap();
        let (method, url, headers) = &seen[0];
        assert_eq!(method, "POST");
        assert!(url.ends_with("/rest/v1/devices?on_conflict=device_id"));
        assert!(headers
            .iter()
            .any(|(k, v)| k == "Prefer" && v.contains("merge-duplicates")));
    }

    #[tokio::test]
    async fn list_devices_marks_only_the_current_one() {
        let c = CloudClient::with_transport(cfg(), Mock::new(vec![("TOK-A", 200, DEVS_A)]));
        let rows = c
            .list_devices(&session("TOK-A", "a"), me().device_id)
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].is_current && !rows[1].is_current);
    }

    #[tokio::test]
    async fn user_b_token_cannot_read_user_a_rows() {
        // Queue expects A's token; presenting B's yields 401 → Unauthorized, no rows leak.
        let c = CloudClient::with_transport(cfg(), Mock::new(vec![("TOK-A", 200, DEVS_A)]));
        let err = c
            .list_devices(&session("TOK-B", "b"), Uuid::nil())
            .await
            .unwrap_err();
        assert_eq!(err, CloudError::Unauthorized);
    }

    #[tokio::test]
    async fn parses_profiles_and_empty_account() {
        let c = CloudClient::with_transport(
            cfg(),
            Mock::new(vec![("TOK-A", 200, PROFS), ("TOK-A", 200, "[]")]),
        );
        let p = c.list_profiles(&session("TOK-A", "a")).await.unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "Personal");
        let empty = c.list_profiles(&session("TOK-A", "a")).await.unwrap();
        assert!(
            empty.is_empty(),
            "account without profiles is a normal empty state"
        );
    }

    #[tokio::test]
    async fn postgrest_errors_are_typed() {
        let missing = r#"{"code":"42P01","message":"relation \"public.devices\" does not exist"}"#;
        assert!(matches!(
            classify_postgrest(404, missing.as_bytes()),
            CloudError::SchemaMissing(_)
        ));
        assert_eq!(classify_postgrest(401, b"{}"), CloudError::Unauthorized);
        assert_eq!(classify_postgrest(403, b"{}"), CloudError::Forbidden);
        assert_eq!(classify_postgrest(503, b""), CloudError::Server(503));
        let check = r#"{"code":"23514","message":"check constraint violated"}"#;
        assert!(matches!(
            classify_postgrest(400, check.as_bytes()),
            CloudError::InvalidValue(_)
        ));
        let dup = r#"{"code":"23505","message":"duplicate key"}"#;
        assert!(matches!(
            classify_postgrest(409, dup.as_bytes()),
            CloudError::Conflict(_)
        ));
        let fk = r#"{"code":"23503","message":"violates foreign key"}"#;
        assert!(matches!(
            classify_postgrest(409, fk.as_bytes()),
            CloudError::UnknownReference(_)
        ));
        let other = r#"{"code":"22P02","message":"invalid input syntax for type uuid"}"#;
        assert!(matches!(
            classify_postgrest(400, other.as_bytes()),
            CloudError::Postgrest { code, .. } if code == "22P02"
        ));
        // Network failure path through the client
        struct Down;
        impl RestTransport for Down {
            fn request<'a>(
                &'a self,
                _m: &'a str,
                _u: &'a str,
                _h: &'a [(&'a str, &'a str)],
                _b: Option<&'a serde_json::Value>,
            ) -> RestFuture<'a> {
                Box::pin(async { Err(HttpError::Unreachable("dns".into())) })
            }
        }
        let c = CloudClient::with_transport(cfg(), Down);
        assert!(matches!(
            c.list_profiles(&session("t", "u")).await.unwrap_err(),
            CloudError::Network(_)
        ));
    }

    #[test]
    fn rfc3339_now_is_well_formed() {
        let s = now_rfc3339();
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z') && s.as_bytes()[10] == b'T');
    }
}
