//! `SessionManager`: ties the GoTrue client to the credential store and owns the rules
//! "reuse while valid, refresh when near expiry, clear on definitive failure, always clear
//! locally on logout". No background loops: every call is explicit.

use crate::client::{AuthClient, AuthError, AuthTransport};
use crate::session::{now_unix, AccountStatus, Session};
use crate::store::{CredentialStore, StoreError};
use std::sync::Arc;

pub struct SessionManager<T: AuthTransport> {
    client: AuthClient<T>,
    store: Arc<dyn CredentialStore>,
}

impl<T: AuthTransport> SessionManager<T> {
    pub fn new(client: AuthClient<T>, store: Arc<dyn CredentialStore>) -> Self {
        Self { client, store }
    }

    /// Sign in and persist the session in the credential store.
    pub async fn sign_in(&self, email: &str, password: &str) -> Result<AccountStatus, AuthError> {
        let session = self.client.login_password(email, password).await?;
        self.store
            .save_session(&session)
            .map_err(|e| AuthError::InvalidResponse(format!("could not persist session: {e}")))?;
        Ok(AccountStatus::from_session(&session))
    }

    /// Create an account; signed in right away when the server auto-confirms e-mails,
    /// otherwise signed out with a hint to confirm the e-mail first.
    pub async fn sign_up(&self, email: &str, password: &str) -> Result<AccountStatus, AuthError> {
        match self.client.sign_up_password(email, password).await? {
            Some(session) => {
                self.store.save_session(&session).map_err(|e| {
                    AuthError::InvalidResponse(format!("could not persist session: {e}"))
                })?;
                Ok(AccountStatus::from_session(&session))
            }
            None => Ok(AccountStatus::signed_out()
                .with_error("Account created. Confirm the e-mail we sent you, then sign in.")),
        }
    }

    /// Return a session whose access token is valid now, refreshing if needed.
    ///
    /// - valid and not near expiry → returned as-is (no network)
    /// - near expiry/expired → refreshed and the new tokens saved atomically
    /// - refresh definitively rejected → local session deleted, `SessionInvalid`
    /// - transient network error → the old session is kept and the error returned
    pub async fn ensure_valid_session(&self) -> Result<Option<Session>, AuthError> {
        let Some(session) = self.load()? else {
            return Ok(None);
        };
        if !session.needs_refresh_at(now_unix()) {
            return Ok(Some(session));
        }
        self.refresh_stored(session).await.map(Some)
    }

    /// Refresh now even if the access token looks valid (the server rejected it with 401,
    /// e.g. revoked or clock skew). Same persistence/clearing rules as above.
    pub async fn force_refresh(&self) -> Result<Option<Session>, AuthError> {
        let Some(session) = self.load()? else {
            return Ok(None);
        };
        self.refresh_stored(session).await.map(Some)
    }

    async fn refresh_stored(&self, session: Session) -> Result<Session, AuthError> {
        match self.client.refresh(&session.refresh_token).await {
            Ok(fresh) => {
                self.store.save_session(&fresh).map_err(|e| {
                    AuthError::InvalidResponse(format!("could not persist refreshed session: {e}"))
                })?;
                Ok(fresh)
            }
            Err(AuthError::SessionInvalid) => {
                let _ = self.store.delete_session();
                Err(AuthError::SessionInvalid)
            }
            Err(e) => Err(e),
        }
    }

    /// The stored session as-is, without any network (it may be expired). Used only to know
    /// *who* is signed in while Synchrome Cloud is unreachable (offline capture); never to
    /// authorise a request.
    pub fn stored_session(&self) -> Result<Option<Session>, AuthError> {
        self.load()
    }

    /// Token-free status for the UI. Never performs a refresh (pure read).
    pub fn account_status(&self) -> AccountStatus {
        match self.load() {
            Ok(Some(s)) => AccountStatus::from_session(&s),
            Ok(None) => AccountStatus::signed_out(),
            Err(e) => AccountStatus::signed_out().with_error(e.to_string()),
        }
    }

    /// Sign out: the local session is ALWAYS removed, even if the network call fails.
    pub async fn sign_out(&self) -> Result<(), AuthError> {
        let session = self.load().ok().flatten();
        // Clear locally first so a crash mid-logout never leaves a stale session behind.
        let local = self.store.delete_session();
        let remote = match session {
            Some(s) => self.client.logout(&s.access_token).await,
            None => Ok(()),
        };
        local.map_err(|e| AuthError::InvalidResponse(format!("could not clear session: {e}")))?;
        // Remote failure is reported but does not undo the local clear.
        remote
    }

    fn load(&self) -> Result<Option<Session>, AuthError> {
        self.store.load_session().map_err(|e: StoreError| match e {
            StoreError::Corrupt(m) => AuthError::InvalidResponse(format!("stored session: {m}")),
            StoreError::Unavailable(m) => AuthError::Network(format!("credential store: {m}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::TransportFuture;
    use crate::session::{Secret, REFRESH_SKEW_SECS};
    use crate::store::MemoryStore;
    use std::sync::Mutex;
    use synchrome_config::SupabaseConfig;
    use synchrome_http::HttpError;

    /// Scripted transport: returns queued (status, body) responses in order, or a network error.
    #[derive(Default)]
    struct MockTransport {
        responses: Mutex<Vec<Result<(u16, &'static str), &'static str>>>,
        pub calls: Mutex<Vec<String>>,
    }
    impl MockTransport {
        fn with(r: Vec<Result<(u16, &'static str), &'static str>>) -> Self {
            Self {
                responses: Mutex::new(r),
                calls: Mutex::new(vec![]),
            }
        }
    }
    impl AuthTransport for MockTransport {
        fn post_json<'a>(
            &'a self,
            url: &'a str,
            _h: &'a [(&'a str, &'a str)],
            _b: &'a serde_json::Value,
        ) -> TransportFuture<'a> {
            self.calls.lock().unwrap().push(url.to_string());
            let next = self.responses.lock().unwrap().remove(0);
            Box::pin(async move {
                match next {
                    Ok((s, b)) => Ok((s, b.as_bytes().to_vec())),
                    Err(e) => Err(HttpError::Unreachable(e.to_string())),
                }
            })
        }
    }

    fn cfg() -> SupabaseConfig {
        SupabaseConfig {
            url: "https://auth.test".into(),
            anon_key: "anon".into(),
        }
    }
    const OK: &str = r#"{"access_token":"AT1","token_type":"bearer","expires_in":3600,
        "refresh_token":"RT1","user":{"id":"user-1","email":"x@y.z"}}"#;
    const OK2: &str = r#"{"access_token":"AT2","token_type":"bearer","expires_in":3600,
        "refresh_token":"RT2","user":{"id":"user-1","email":"x@y.z"}}"#;
    const BAD: &str =
        r#"{"error":"invalid_grant","error_description":"Invalid login credentials"}"#;

    fn mgr(
        r: Vec<Result<(u16, &'static str), &'static str>>,
    ) -> (SessionManager<MockTransport>, Arc<MemoryStore>) {
        let store = Arc::new(MemoryStore::default());
        let client = AuthClient::with_transport(cfg(), MockTransport::with(r));
        (SessionManager::new(client, store.clone()), store)
    }

    fn seeded(expires_at: i64) -> Session {
        Session {
            user_id: "user-1".into(),
            email: Some("x@y.z".into()),
            access_token: Secret::new("OLD-AT"),
            refresh_token: Secret::new("OLD-RT"),
            expires_at,
            token_type: "bearer".into(),
        }
    }

    #[tokio::test]
    async fn force_refresh_rotates_a_rejected_but_unexpired_token() {
        let (m, store) = mgr(vec![Ok((200, OK2))]);
        store.save_session(&seeded(now_unix() + 3600)).unwrap();
        let s = m.force_refresh().await.unwrap().unwrap();
        assert_eq!(s.access_token.expose(), "AT2");
        assert_eq!(
            store
                .load_session()
                .unwrap()
                .unwrap()
                .refresh_token
                .expose(),
            "RT2"
        );
        let (m, _) = mgr(vec![]);
        assert!(m.force_refresh().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn expired_session_offline_is_still_known_locally() {
        let (m, store) = mgr(vec![Err("offline")]);
        store.save_session(&seeded(now_unix() - 10)).unwrap();
        assert!(matches!(
            m.ensure_valid_session().await,
            Err(AuthError::Network(_))
        ));
        let s = m.stored_session().unwrap().unwrap();
        assert_eq!(s.user_id, "user-1");
        assert!(
            store.load_session().unwrap().is_some(),
            "network failure keeps the session"
        );
    }

    #[tokio::test]
    async fn login_valid_persists_session() {
        let (m, store) = mgr(vec![Ok((200, OK))]);
        let st = m.sign_in("x@y.z", "pw").await.unwrap();
        assert!(st.signed_in);
        assert_eq!(st.email.as_deref(), Some("x@y.z"));
        let saved = store.load_session().unwrap().unwrap();
        assert_eq!(saved.access_token.expose(), "AT1");
    }

    #[tokio::test]
    async fn login_invalid_is_typed_and_persists_nothing() {
        let (m, store) = mgr(vec![Ok((400, BAD))]);
        assert_eq!(
            m.sign_in("x@y.z", "wrong").await.unwrap_err(),
            AuthError::InvalidCredentials
        );
        assert!(store.load_session().unwrap().is_none());
    }

    #[tokio::test]
    async fn valid_session_is_reused_without_network() {
        let (m, store) = mgr(vec![]); // any network call would panic (empty queue)
        store.save_session(&seeded(now_unix() + 3600)).unwrap();
        let s = m.ensure_valid_session().await.unwrap().unwrap();
        assert_eq!(s.access_token.expose(), "OLD-AT");
    }

    #[tokio::test]
    async fn near_expiry_refreshes_and_saves_atomically() {
        let (m, store) = mgr(vec![Ok((200, OK2))]);
        store
            .save_session(&seeded(now_unix() + REFRESH_SKEW_SECS - 5))
            .unwrap();
        let s = m.ensure_valid_session().await.unwrap().unwrap();
        assert_eq!(s.access_token.expose(), "AT2");
        assert_eq!(
            store
                .load_session()
                .unwrap()
                .unwrap()
                .refresh_token
                .expose(),
            "RT2"
        );
    }

    #[tokio::test]
    async fn refresh_rejected_clears_session() {
        let (m, store) = mgr(vec![Ok((400, BAD))]);
        store.save_session(&seeded(0)).unwrap(); // expired
        assert_eq!(
            m.ensure_valid_session().await.unwrap_err(),
            AuthError::SessionInvalid
        );
        assert!(store.load_session().unwrap().is_none());
    }

    #[tokio::test]
    async fn transient_network_error_keeps_session() {
        let (m, store) = mgr(vec![Err("dns down")]);
        store.save_session(&seeded(0)).unwrap();
        assert!(matches!(
            m.ensure_valid_session().await.unwrap_err(),
            AuthError::Network(_)
        ));
        assert!(store.load_session().unwrap().is_some());
    }

    #[tokio::test]
    async fn logout_clears_locally_even_if_network_fails() {
        let (m, store) = mgr(vec![Err("offline")]);
        store.save_session(&seeded(now_unix() + 3600)).unwrap();
        let r = m.sign_out().await;
        assert!(matches!(r, Err(AuthError::Network(_))));
        assert!(
            store.load_session().unwrap().is_none(),
            "local session must be gone"
        );
    }

    #[tokio::test]
    async fn account_status_never_contains_tokens() {
        let (m, store) = mgr(vec![]);
        store.save_session(&seeded(now_unix() + 3600)).unwrap();
        let json = serde_json::to_string(&m.account_status()).unwrap();
        assert!(!json.contains("OLD-AT"));
        assert!(!json.contains("OLD-RT"));
        assert!(json.contains("\"signed_in\":true"));
    }
}
