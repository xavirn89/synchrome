//! User authentication against Synchrome Cloud (Supabase Auth / GoTrue).
//!
//! Scope of this slice: email/password login, explicit refresh, logout, and a token-free
//! [`AccountStatus`] for the UI. Sessions persist only in the OS credential store
//! ([`KeychainStore`] on macOS). No signup, magic link, OAuth or background refresh loops.
//!
//! Invariants enforced by types:
//! - [`Session`] is never `Serialize`d to the frontend; only [`AccountStatus`] is.
//! - Tokens are [`Secret`]s that redact in `Debug`/`Display`.
//! - The only persistent home for a session is a [`CredentialStore`].

mod client;
mod manager;
mod session;
mod store;

pub use client::{
    classify_error, parse_token_response, AuthClient, AuthError, AuthTransport, ReqwestTransport,
    TransportFuture,
};
pub use manager::SessionManager;
pub use session::{now_unix, AccountStatus, Secret, Session, REFRESH_SKEW_SECS};
pub use store::{
    CredentialStore, KeychainStore, MemoryStore, StoreError, KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE,
};
