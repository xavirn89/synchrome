//! Local registry of mapping tokens this installation wrote into browser profiles.
//!
//! Stores only the SHA-256 of each token (never the token) plus where and for which account /
//! mapping it was written. It lets the app (a) show "Token written" vs "Extension confirmed",
//! and (b) tell apart handshake failures that look identical from the cloud (0 rows under
//! RLS): a token written under another account, a mapping deleted after the write, or its
//! cloud profile deleted. Lives in the Synchrome app data dir; not a secret.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use synchrome_types::BrowserKind;
use uuid::Uuid;

pub const REGISTRY_FILE: &str = "local_tokens.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalTokenState {
    /// Token written into the extension store; extension has not confirmed yet.
    Written,
    /// The extension presented this token and the app confirmed the mapping.
    Confirmed,
    /// The last write attempt failed (`last_error` says why). No token on disk from it.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalTokenRecord {
    /// SHA-256 hex of the token; empty for a failed write.
    pub token_sha256: String,
    pub account_id: String,
    pub mapping_id: Uuid,
    pub profile_id: Uuid,
    pub browser_kind: BrowserKind,
    pub user_data_dir: String,
    pub profile_dir: String,
    pub state: LocalTokenState,
    pub updated_at: i64,
    #[serde(default)]
    pub last_error: Option<String>,
}

pub fn token_sha256(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Constant-time equality for token strings (no early exit on the first differing byte).
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
pub struct TokenRegistry {
    path: PathBuf,
}

impl TokenRegistry {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    /// Registry inside an app data directory.
    pub fn in_dir(dir: &Path) -> Self {
        Self::at(dir.join(REGISTRY_FILE))
    }

    pub fn load(&self) -> Vec<LocalTokenRecord> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, records: &[LocalTokenRecord]) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(records)?)?;
        std::fs::rename(&tmp, &self.path)
    }

    /// Insert or replace the record for `rec.mapping_id`.
    pub fn upsert(&self, rec: LocalTokenRecord) -> std::io::Result<()> {
        let mut all = self.load();
        all.retain(|r| r.mapping_id != rec.mapping_id);
        all.push(rec);
        self.save(&all)
    }

    pub fn find_by_token(&self, token: &str) -> Option<LocalTokenRecord> {
        let h = token_sha256(token);
        self.load()
            .into_iter()
            .find(|r| !r.token_sha256.is_empty() && ct_eq(&r.token_sha256, &h))
    }

    pub fn find_by_mapping(&self, mapping_id: Uuid) -> Option<LocalTokenRecord> {
        self.load().into_iter().find(|r| r.mapping_id == mapping_id)
    }

    pub fn mark_confirmed(&self, mapping_id: Uuid) -> std::io::Result<()> {
        let mut all = self.load();
        for r in all.iter_mut().filter(|r| r.mapping_id == mapping_id) {
            r.state = LocalTokenState::Confirmed;
            r.last_error = None;
            r.updated_at = now_unix();
        }
        self.save(&all)
    }

    pub fn remove_mapping(&self, mapping_id: Uuid) -> std::io::Result<()> {
        let mut all = self.load();
        all.retain(|r| r.mapping_id != mapping_id);
        self.save(&all)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(token: &str, account: &str, mapping: Uuid) -> LocalTokenRecord {
        LocalTokenRecord {
            token_sha256: token_sha256(token),
            account_id: account.into(),
            mapping_id: mapping,
            profile_id: Uuid::new_v4(),
            browser_kind: BrowserKind::Helium,
            user_data_dir: "/tmp/udd".into(),
            profile_dir: "Default".into(),
            state: LocalTokenState::Written,
            updated_at: 0,
            last_error: None,
        }
    }

    #[test]
    fn stores_hash_only_and_finds_by_token() {
        let dir = tempfile::tempdir().unwrap();
        let reg = TokenRegistry::in_dir(dir.path());
        let tok = "cd".repeat(32);
        let m = Uuid::new_v4();
        reg.upsert(rec(&tok, "acct-a", m)).unwrap();
        let raw = std::fs::read_to_string(dir.path().join(REGISTRY_FILE)).unwrap();
        assert!(
            !raw.contains(&tok),
            "registry must never contain the raw token"
        );
        assert_eq!(reg.find_by_token(&tok).unwrap().mapping_id, m);
        assert!(reg.find_by_token(&"00".repeat(32)).is_none());
        reg.mark_confirmed(m).unwrap();
        assert_eq!(
            reg.find_by_mapping(m).unwrap().state,
            LocalTokenState::Confirmed
        );
        reg.remove_mapping(m).unwrap();
        assert!(reg.find_by_mapping(m).is_none());
    }

    #[test]
    fn upsert_replaces_per_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let reg = TokenRegistry::in_dir(dir.path());
        let m = Uuid::new_v4();
        reg.upsert(rec(&"aa".repeat(32), "a", m)).unwrap();
        reg.upsert(rec(&"bb".repeat(32), "a", m)).unwrap();
        assert_eq!(reg.load().len(), 1);
        assert!(reg.find_by_token(&"bb".repeat(32)).is_some());
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
    }
}
