//! Common HTTP client for every Synchrome network call.
//!
//! Guarantees (Milestone 0, E9): a branded `User-Agent: Synchrome/<version>` on every request
//! (the Cloudflare front returns `403 error code 1010` for default library agents), a bounded
//! timeout, and typed errors so callers can distinguish "unreachable" from "service answered
//! with an error". No authentication logic lives here.

use serde::Serialize;
use std::time::{Duration, Instant};
use thiserror::Error;

pub use reqwest::StatusCode;

/// Default per-request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Error)]
pub enum HttpError {
    /// DNS, TCP, TLS or timeout failure — the service was never reached.
    #[error("unreachable: {0}")]
    Unreachable(String),
    /// The service answered, but with a non-success status.
    #[error("http {status}: {body_preview}")]
    Status { status: u16, body_preview: String },
    /// Could not build the client/request (programming or config error).
    #[error("client error: {0}")]
    Client(String),
}

/// Coarse classification of an HTTP outcome, used by health probes and the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// 2xx.
    Ok,
    /// 4xx: the service is up but refused/rejected this request (auth, RLS, bad input).
    ClientError,
    /// 5xx: the service is up but failing.
    ServerError,
    /// Never reached (network/TLS/timeout).
    Unreachable,
}

/// Classify a status code into an [`Outcome`].
pub fn classify_status(status: u16) -> Outcome {
    match status {
        200..=299 => Outcome::Ok,
        400..=499 => Outcome::ClientError,
        500..=599 => Outcome::ServerError,
        // 1xx/3xx are not expected from our APIs; treat as a client-side anomaly.
        _ => Outcome::ClientError,
    }
}

/// Result of a single probe: outcome, status (if reached), and round-trip latency.
#[derive(Debug, Clone, Serialize)]
pub struct Probe {
    pub outcome: Outcome,
    pub status: Option<u16>,
    pub latency_ms: u64,
    pub error: Option<String>,
}

/// Thin wrapper over `reqwest::Client` that enforces Synchrome's defaults.
#[derive(Clone)]
pub struct HttpClient {
    inner: reqwest::Client,
}

impl HttpClient {
    /// Build a client with the branded User-Agent and the default timeout.
    pub fn new() -> Result<Self, HttpError> {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(timeout: Duration) -> Result<Self, HttpError> {
        let inner = reqwest::Client::builder()
            .user_agent(synchrome_types::user_agent())
            .timeout(timeout)
            .build()
            .map_err(|e| HttpError::Client(e.to_string()))?;
        Ok(Self { inner })
    }

    /// The `User-Agent` this client sends (exposed for tests/UI).
    pub fn user_agent(&self) -> String {
        synchrome_types::user_agent()
    }

    /// POST a JSON body and return `(status, body_bytes)`. Network/TLS/timeout failures are
    /// `HttpError::Unreachable`; any HTTP status (including 4xx/5xx) is returned to the caller
    /// so it can parse the service's own error payload. No retries, no auth logic.
    pub async fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: &serde_json::Value,
    ) -> Result<(u16, Vec<u8>), HttpError> {
        let mut req = self.inner.post(url).json(body);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| HttpError::Unreachable(e.to_string()))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| HttpError::Unreachable(e.to_string()))?;
        Ok((status, bytes.to_vec()))
    }

    /// Perform a GET with optional extra headers and return a [`Probe`]. Never panics; network
    /// failures become `Outcome::Unreachable`. The response body is discarded (read-only probe).
    pub async fn probe_get(&self, url: &str, headers: &[(&str, &str)]) -> Probe {
        let started = Instant::now();
        let mut req = self.inner.get(url);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                // Drain a small body preview for diagnostics; ignore failures.
                let _ = resp.bytes().await;
                Probe {
                    outcome: classify_status(status),
                    status: Some(status),
                    latency_ms: started.elapsed().as_millis() as u64,
                    error: None,
                }
            }
            Err(e) => Probe {
                outcome: Outcome::Unreachable,
                status: None,
                latency_ms: started.elapsed().as_millis() as u64,
                error: Some(e.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_is_branded() {
        let c = HttpClient::new().unwrap();
        assert!(c.user_agent().starts_with("Synchrome/"));
    }

    #[test]
    fn status_classification() {
        assert_eq!(classify_status(200), Outcome::Ok);
        assert_eq!(classify_status(204), Outcome::Ok);
        assert_eq!(classify_status(401), Outcome::ClientError);
        assert_eq!(classify_status(403), Outcome::ClientError);
        assert_eq!(classify_status(404), Outcome::ClientError);
        assert_eq!(classify_status(500), Outcome::ServerError);
        assert_eq!(classify_status(503), Outcome::ServerError);
    }

    #[tokio::test]
    async fn unreachable_host_is_classified_not_panicked() {
        let c = HttpClient::with_timeout(Duration::from_secs(3)).unwrap();
        // .invalid is reserved and never resolves.
        let p = c
            .probe_get("https://synchrome-nonexistent.invalid/", &[])
            .await;
        assert_eq!(p.outcome, Outcome::Unreachable);
        assert!(p.status.is_none());
        assert!(p.error.is_some());
    }
}
