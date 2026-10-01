//! Credential store: the single persistent home for a session.
//!
//! `CredentialStore` is the portable abstraction; `KeychainStore` is the macOS backend built on
//! the `keyring` crate with the `apple-native` feature, which uses Security.framework and stores
//! the item in the user's login Keychain. A Windows Credential Manager backend will implement
//! the same trait later. `MemoryStore` is for tests only.

use crate::session::Session;
use std::sync::Mutex;
use thiserror::Error;

/// Stable identity of the Keychain item: service = bundle id, account = fixed slot.
pub const KEYCHAIN_SERVICE: &str = "com.synchrome.app";
pub const KEYCHAIN_ACCOUNT: &str = "session";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("credential store unavailable: {0}")]
    Unavailable(String),
    #[error("stored session is corrupt: {0}")]
    Corrupt(String),
}

/// Portable session persistence. Implementations must be safe to call from any thread.
pub trait CredentialStore: Send + Sync {
    fn save_session(&self, session: &Session) -> Result<(), StoreError>;
    /// `Ok(None)` when no session is stored.
    fn load_session(&self) -> Result<Option<Session>, StoreError>;
    /// Idempotent: deleting a missing session is `Ok`.
    fn delete_session(&self) -> Result<(), StoreError>;
}

/// macOS Keychain backend (login keychain, via `keyring` + Security.framework).
pub struct KeychainStore {
    service: String,
    account: String,
}

impl KeychainStore {
    pub fn new() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.into(),
            account: KEYCHAIN_ACCOUNT.into(),
        }
    }
    fn entry(&self) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(&self.service, &self.account)
            .map_err(|e| StoreError::Unavailable(e.to_string()))
    }
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore for KeychainStore {
    fn save_session(&self, session: &Session) -> Result<(), StoreError> {
        let json =
            serde_json::to_string(session).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        // A single set_password call replaces the item atomically from the caller's view.
        self.entry()?
            .set_password(&json)
            .map_err(|e| StoreError::Unavailable(e.to_string()))
    }

    fn load_session(&self) -> Result<Option<Session>, StoreError> {
        match self.entry()?.get_password() {
            Ok(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| StoreError::Corrupt(e.to_string())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(StoreError::Unavailable(e.to_string())),
        }
    }

    fn delete_session(&self) -> Result<(), StoreError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(StoreError::Unavailable(e.to_string())),
        }
    }
}

/// In-memory store for tests. Never used in the app.
#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<Option<Session>>,
    pub fail_saves: Mutex<bool>,
}

impl CredentialStore for MemoryStore {
    fn save_session(&self, session: &Session) -> Result<(), StoreError> {
        if *self.fail_saves.lock().unwrap() {
            return Err(StoreError::Unavailable("simulated".into()));
        }
        *self.inner.lock().unwrap() = Some(session.clone());
        Ok(())
    }
    fn load_session(&self) -> Result<Option<Session>, StoreError> {
        Ok(self.inner.lock().unwrap().clone())
    }
    fn delete_session(&self) -> Result<(), StoreError> {
        *self.inner.lock().unwrap() = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Secret;

    fn s() -> Session {
        Session {
            user_id: "u".into(),
            email: None,
            access_token: Secret::new("a"),
            refresh_token: Secret::new("r"),
            expires_at: 1,
            token_type: "bearer".into(),
        }
    }

    #[test]
    fn memory_store_roundtrip_and_delete() {
        let st = MemoryStore::default();
        assert!(st.load_session().unwrap().is_none());
        st.save_session(&s()).unwrap();
        assert_eq!(st.load_session().unwrap().unwrap().user_id, "u");
        st.delete_session().unwrap();
        assert!(st.load_session().unwrap().is_none());
        st.delete_session().unwrap(); // idempotent
    }

    #[test]
    fn keychain_identity_is_stable() {
        let k = KeychainStore::new();
        assert_eq!(k.service, "com.synchrome.app");
        assert_eq!(k.account, "session");
    }
}
