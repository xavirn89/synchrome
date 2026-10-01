//! Supabase Cloud adapter (default store).
//!
//! Milestone 0 (E9) validated the real instance: Auth, PostgREST, Storage and Realtime work;
//! Storage has no native `If-Match`, so CAS is emulated with a Postgres `synchrome_objects`
//! version row; every request must send a `Synchrome/<version>` User-Agent; the service-role
//! key must never reach the client.
//!
//! This slice implements only a **read-only connectivity probe** ([`cloud_status`]). No login,
//! no token refresh, no Storage/PostgREST reads or writes of application data.

use serde::Serialize;
use synchrome_config::SupabaseConfig;
use synchrome_http::{HttpClient, HttpError, Outcome, Probe};

/// Structured, UI-facing connectivity status for Synchrome Cloud.
#[derive(Debug, Clone, Serialize)]
pub struct CloudStatus {
    /// `SUPABASE_URL` and the anon key were both present and valid.
    pub configured: bool,
    /// The configured endpoint (never the key).
    pub endpoint: Option<String>,
    /// At least one service answered (any HTTP status) — the host is reachable over HTTPS.
    pub reachable: bool,
    /// `GET /auth/v1/health` returned 2xx.
    pub auth_ok: bool,
    /// `GET /rest/v1/` with the anon key returned 2xx.
    pub rest_ok: bool,
    /// `GET /storage/v1/bucket` with the anon key returned 2xx (anon sees an empty list under
    /// RLS). A 4xx means the service is up but the key was rejected — reported via `reachable`
    /// and the `storage` probe, not as ok.
    pub storage_ok: bool,
    /// Per-service probe details (latency, status, error).
    pub auth: Option<Probe>,
    pub rest: Option<Probe>,
    pub storage: Option<Probe>,
    /// Worst (max) latency among reached services, for a one-number UI summary.
    pub latency_ms: Option<u64>,
    /// Human-readable error when `configured` is false or nothing was reachable.
    pub error: Option<String>,
}

impl CloudStatus {
    /// Status for a missing/invalid configuration (no network touched).
    pub fn unconfigured(reason: impl Into<String>) -> Self {
        Self {
            configured: false,
            endpoint: None,
            reachable: false,
            auth_ok: false,
            rest_ok: false,
            storage_ok: false,
            auth: None,
            rest: None,
            storage: None,
            latency_ms: None,
            error: Some(reason.into()),
        }
    }
}

/// Headers Supabase expects on anon-authenticated requests (publishable key only).
fn anon_headers(cfg: &SupabaseConfig) -> Vec<(String, String)> {
    vec![
        ("apikey".to_string(), cfg.anon_key.clone()),
        (
            "Authorization".to_string(),
            format!("Bearer {}", cfg.anon_key),
        ),
    ]
}

/// Probe Synchrome Cloud read-only: Auth health, PostgREST root, Storage bucket list.
///
/// Writes nothing. Each probe is independent so partial outages are reported precisely.
pub async fn cloud_status(cfg: &SupabaseConfig) -> Result<CloudStatus, HttpError> {
    let client = HttpClient::new()?;
    let base = cfg.url.trim_end_matches('/');
    let hdrs = anon_headers(cfg);
    let hdr_refs: Vec<(&str, &str)> = hdrs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    // Auth health needs no key but we send the apikey anyway (Kong may require it).
    let auth = client
        .probe_get(&format!("{base}/auth/v1/health"), &hdr_refs)
        .await;
    let rest = client
        .probe_get(&format!("{base}/rest/v1/"), &hdr_refs)
        .await;
    let storage = client
        .probe_get(&format!("{base}/storage/v1/bucket"), &hdr_refs)
        .await;

    let reached = |p: &Probe| p.outcome != Outcome::Unreachable;
    let reachable = reached(&auth) || reached(&rest) || reached(&storage);
    let auth_ok = auth.outcome == Outcome::Ok;
    let rest_ok = rest.outcome == Outcome::Ok;
    let storage_ok = storage.outcome == Outcome::Ok;
    let latency_ms = [&auth, &rest, &storage]
        .iter()
        .filter(|p| reached(p))
        .map(|p| p.latency_ms)
        .max();
    let error = if reachable {
        None
    } else {
        Some(
            auth.error
                .clone()
                .or(rest.error.clone())
                .or(storage.error.clone())
                .unwrap_or_else(|| "no service reachable".into()),
        )
    };

    Ok(CloudStatus {
        configured: true,
        endpoint: Some(base.to_string()),
        reachable,
        auth_ok,
        rest_ok,
        storage_ok,
        auth: Some(auth),
        rest: Some(rest),
        storage: Some(storage),
        latency_ms,
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_status_serializes_with_error() {
        let s = CloudStatus::unconfigured("missing SUPABASE_URL");
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["configured"], false);
        assert_eq!(v["reachable"], false);
        assert_eq!(v["error"], "missing SUPABASE_URL");
        assert!(v["endpoint"].is_null());
    }

    #[test]
    fn cloud_status_has_the_agreed_fields() {
        let s = CloudStatus::unconfigured("x");
        let v = serde_json::to_value(&s).unwrap();
        for k in [
            "configured",
            "reachable",
            "auth_ok",
            "rest_ok",
            "storage_ok",
            "latency_ms",
            "error",
            "endpoint",
        ] {
            assert!(v.get(k).is_some(), "missing field {k}");
        }
    }

    #[test]
    fn anon_headers_use_publishable_key_only() {
        let cfg = SupabaseConfig {
            url: "https://x.test".into(),
            anon_key: "anon".into(),
        };
        let h = anon_headers(&cfg);
        assert_eq!(h[0], ("apikey".to_string(), "anon".to_string()));
        assert_eq!(h[1].1, "Bearer anon");
        assert_eq!(h.len(), 2);
    }
}
