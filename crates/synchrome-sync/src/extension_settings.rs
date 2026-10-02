//! Extension settings (`chrome.storage.local` / `chrome.storage.sync` of other extensions,
//! `docs/03` §3.5): identity, value validation, size limits and the sensitive-key denylist.
//! Pure; no I/O.
//!
//! Entity id = `<extension id>/<area>/<key>` (the key may itself contain `/`). Values are the
//! JSON Chromium stores (key → JSON text in LevelDB); only JSON-serialisable values exist
//! there, so anything that does not parse as JSON is rejected (reported, never synced).

use serde::{Deserialize, Serialize};

pub const COLLECTION: &str = "extension_settings";
/// Longest key accepted (bytes).
pub const MAX_KEY_BYTES: usize = 512;
/// Largest value accepted (bytes of canonical JSON).
pub const MAX_VALUE_BYTES: usize = 64 * 1024;
/// Largest synchronised dataset per extension and area (sum of key + value bytes).
pub const MAX_AREA_BYTES: usize = 5 * 1024 * 1024;

/// Name fragments treated as sensitive by default (case-insensitive substring match). This is
/// a heuristic: it cannot recognise every secret (a token stored under an innocent name, or
/// inside a nested object, is not detected) and it excludes some harmless keys ("author").
pub const SENSITIVE_FRAGMENTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "passphrase",
    "api_key",
    "apikey",
    "api-key",
    "auth",
    "session",
    "cookie",
    "credential",
    "private_key",
    "privatekey",
    "bearer",
    "jwt",
    "otp",
    "seed",
    "mnemonic",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    Local,
    Sync,
}

impl Area {
    pub fn as_str(self) -> &'static str {
        match self {
            Area::Local => "local",
            Area::Sync => "sync",
        }
    }
    pub fn parse(s: &str) -> Option<Area> {
        match s {
            "local" => Some(Area::Local),
            "sync" => Some(Area::Sync),
            _ => None,
        }
    }
}

/// Per-extension choice (default Off: nothing is read, published or written).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Off,
    /// `chrome.storage.local` only.
    Local,
    /// `chrome.storage.local` and `chrome.storage.sync`.
    LocalSync,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Local => "local",
            Mode::LocalSync => "local_sync",
        }
    }
    pub fn parse(s: &str) -> Mode {
        match s {
            "local" => Mode::Local,
            "local_sync" => Mode::LocalSync,
            _ => Mode::Off,
        }
    }
    pub fn areas(self) -> &'static [Area] {
        match self {
            Mode::Off => &[],
            Mode::Local => &[Area::Local],
            Mode::LocalSync => &[Area::Local, Area::Sync],
        }
    }
}

pub fn entity_id(ext_id: &str, area: Area, key: &str) -> String {
    format!("{ext_id}/{}/{key}", area.as_str())
}

/// `(extension id, area, key)` of an entity id.
pub fn parse_id(id: &str) -> Option<(&str, Area, &str)> {
    let (ext, rest) = id.split_once('/')?;
    let (area, key) = rest.split_once('/')?;
    if !crate::extensions::valid_id(ext) || key.is_empty() {
        return None;
    }
    Some((ext, Area::parse(area)?, key))
}

/// Why a key is not synchronised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Excluded {
    Sensitive,
    Oversized,
    /// Not valid JSON (corrupt or written by something else than chrome.storage).
    Invalid,
}

/// Default + per-extension denylist. `extra` entries are exact keys, or prefixes ending in `*`.
pub fn is_sensitive(key: &str, extra: &[String]) -> bool {
    let k = key.to_ascii_lowercase();
    SENSITIVE_FRAGMENTS.iter().any(|f| k.contains(f))
        || extra.iter().any(|e| match e.strip_suffix('*') {
            Some(prefix) => key.starts_with(prefix),
            None => key == e,
        })
}

/// Validate one stored entry: the canonical JSON of its value, or why it is excluded.
pub fn check(key: &str, raw: &str, extra: &[String]) -> Result<serde_json::Value, Excluded> {
    if is_sensitive(key, extra) {
        return Err(Excluded::Sensitive);
    }
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        return Err(Excluded::Oversized);
    }
    let v: serde_json::Value = serde_json::from_str(raw).map_err(|_| Excluded::Invalid)?;
    if canonical(&v).len() > MAX_VALUE_BYTES {
        return Err(Excluded::Oversized);
    }
    Ok(v)
}

/// Canonical JSON text (object keys sorted): equal values compare equal as text.
pub fn canonical(v: &serde_json::Value) -> String {
    // serde_json's Map is a BTreeMap without the preserve_order feature: keys are sorted
    serde_json::to_string(v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    #[test]
    fn ids_round_trip_with_slashes_in_keys() {
        let id = entity_id(ID, Area::Sync, "a/b c");
        assert_eq!(id, format!("{ID}/sync/a/b c"));
        assert_eq!(parse_id(&id), Some((ID, Area::Sync, "a/b c")));
        assert_eq!(parse_id("bad/local/x"), None);
        assert_eq!(parse_id(&format!("{ID}/session/x")), None);
    }

    #[test]
    fn values_of_every_json_type_and_exclusions() {
        for raw in [
            "null",
            "true",
            "1.5",
            "\"s\"",
            "[1,\"a\",null]",
            "{\"b\":{\"c\":[true]}}",
        ] {
            assert!(check("k", raw, &[]).is_ok(), "{raw}");
        }
        assert_eq!(check("accessToken", "\"x\"", &[]), Err(Excluded::Sensitive));
        assert_eq!(
            check("refresh_token", "\"x\"", &[]),
            Err(Excluded::Sensitive)
        );
        assert_eq!(check("my_api_key", "1", &[]), Err(Excluded::Sensitive));
        assert_eq!(
            check("plain", "1", &["plain".into()]),
            Err(Excluded::Sensitive)
        );
        assert_eq!(
            check("cache.v1", "1", &["cache.*".into()]),
            Err(Excluded::Sensitive)
        );
        assert_eq!(check("k", "not json", &[]), Err(Excluded::Invalid));
        let big = format!("\"{}\"", "x".repeat(MAX_VALUE_BYTES));
        assert_eq!(check("k", &big, &[]), Err(Excluded::Oversized));
        assert_eq!(
            check(&"k".repeat(MAX_KEY_BYTES + 1), "1", &[]),
            Err(Excluded::Oversized)
        );
    }

    #[test]
    fn canonical_json_ignores_key_order() {
        let a: serde_json::Value = serde_json::from_str("{\"b\":1,\"a\":2}").unwrap();
        let b: serde_json::Value = serde_json::from_str("{\"a\":2,\"b\":1}").unwrap();
        assert_eq!(canonical(&a), canonical(&b));
    }
}
