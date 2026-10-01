//! Client configuration for Synchrome.
//!
//! Loads the two publishable Supabase values the desktop app needs — the project URL and
//! the anon (publishable) key — from the process environment, with an optional `.env` file for
//! development. The struct deliberately has **no field** for any administrative secret: the
//! service-role key, JWT secret and database password never exist in the client, by
//! construction. Only names are documented in `.env.example`.

use serde::Serialize;
use std::fmt;
use thiserror::Error;

/// Environment variable holding the Supabase project URL (publishable).
pub const ENV_SUPABASE_URL: &str = "SUPABASE_URL";
/// Environment variable holding the Supabase anon/publishable key (safe to ship).
pub const ENV_SUPABASE_ANON_KEY: &str = "SUPABASE_ANON_KEY";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("missing required configuration: {0}")]
    Missing(&'static str),
    #[error("invalid value for {name}: {reason}")]
    Invalid { name: &'static str, reason: String },
}

/// Supabase client settings. Both values are publishable; RLS protects the data.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SupabaseConfig {
    pub url: String,
    pub anon_key: String,
}

impl fmt::Debug for SupabaseConfig {
    /// Redact the key in logs; the URL is fine to show.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SupabaseConfig")
            .field("url", &self.url)
            .field("anon_key", &"<redacted>")
            .finish()
    }
}

/// The whole client configuration (grows with later milestones).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppConfig {
    pub supabase: SupabaseConfig,
}

impl AppConfig {
    /// Load from the process environment, after loading a `.env` file if one is found in the
    /// current directory or any parent (development convenience; silently ignored if absent).
    pub fn load() -> Result<Self, ConfigError> {
        let _ = dotenvy::dotenv();
        // A process spawned by the browser (native messaging host) has an arbitrary cwd, so
        // also look for `.env` next to the executable's ancestors (development builds live in
        // `<repo>/target/...`). Already-set variables are never overridden.
        if std::env::var(ENV_SUPABASE_URL).is_err() || std::env::var(ENV_SUPABASE_ANON_KEY).is_err()
        {
            if let Ok(exe) = std::env::current_exe() {
                if let Some(env_file) = exe
                    .ancestors()
                    .map(|d| d.join(".env"))
                    .find(|p| p.is_file())
                {
                    let _ = dotenvy::from_path(env_file);
                }
            }
        }
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Build from an arbitrary lookup function (testable without touching the real env).
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let url = required(&lookup, ENV_SUPABASE_URL)?;
        let anon_key = required(&lookup, ENV_SUPABASE_ANON_KEY)?;
        if !(url.starts_with("https://") || url.starts_with("http://localhost")) {
            return Err(ConfigError::Invalid {
                name: ENV_SUPABASE_URL,
                reason: "must start with https:// (or http://localhost for local dev)".into(),
            });
        }
        Ok(Self {
            supabase: SupabaseConfig {
                url: url.trim_end_matches('/').to_string(),
                anon_key,
            },
        })
    }
}

fn required(
    lookup: &impl Fn(&str) -> Option<String>,
    name: &'static str,
) -> Result<String, ConfigError> {
    match lookup(name) {
        Some(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(ConfigError::Missing(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn valid_config_loads_and_trims_trailing_slash() {
        let cfg = AppConfig::from_lookup(env(&[
            (ENV_SUPABASE_URL, "https://example.supabase.test/"),
            (ENV_SUPABASE_ANON_KEY, "anon-123"),
        ]))
        .unwrap();
        assert_eq!(cfg.supabase.url, "https://example.supabase.test");
        assert_eq!(cfg.supabase.anon_key, "anon-123");
    }

    #[test]
    fn missing_url_is_a_clear_error() {
        let err = AppConfig::from_lookup(env(&[(ENV_SUPABASE_ANON_KEY, "x")])).unwrap_err();
        assert_eq!(err, ConfigError::Missing(ENV_SUPABASE_URL));
    }

    #[test]
    fn missing_key_is_a_clear_error() {
        let err = AppConfig::from_lookup(env(&[(ENV_SUPABASE_URL, "https://x.test")])).unwrap_err();
        assert_eq!(err, ConfigError::Missing(ENV_SUPABASE_ANON_KEY));
    }

    #[test]
    fn non_https_url_is_rejected() {
        let err = AppConfig::from_lookup(env(&[
            (ENV_SUPABASE_URL, "ftp://x.test"),
            (ENV_SUPABASE_ANON_KEY, "k"),
        ]))
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn config_exposes_only_publishable_fields() {
        // The serialized shape is the whole truth about what the client holds.
        let cfg = AppConfig::from_lookup(env(&[
            (ENV_SUPABASE_URL, "https://x.test"),
            (ENV_SUPABASE_ANON_KEY, "k"),
        ]))
        .unwrap();
        let v = serde_json::to_value(&cfg).unwrap();
        let keys: Vec<&str> = v["supabase"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["anon_key", "url"]);
    }

    #[test]
    fn debug_redacts_the_key() {
        let cfg = AppConfig::from_lookup(env(&[
            (ENV_SUPABASE_URL, "https://x.test"),
            (ENV_SUPABASE_ANON_KEY, "super-secret-looking"),
        ]))
        .unwrap();
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("super-secret-looking"));
        assert!(dbg.contains("<redacted>"));
    }
}
