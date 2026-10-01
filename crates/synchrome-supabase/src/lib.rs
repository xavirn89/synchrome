//! Supabase Cloud storage adapter (default store) — skeleton only.
//!
//! Milestone 0 (E9) validated the real instance: Auth, PostgREST, Storage and Realtime work;
//! Storage has no native `If-Match`, so CAS is emulated with a Postgres `synchrome_objects`
//! version row; every request must send a `Synchrome/<version>` User-Agent; the service-role
//! key must never reach the client. This crate will implement `StorageAdapter` over those
//! facts. No network code is present in M1.

use synchrome_types::user_agent;

/// Client-safe configuration. The anon key is publishable; the service-role key is never here.
#[derive(Debug, Clone)]
pub struct SupabaseConfig {
    pub url: String,
    pub anon_key: String,
}

impl SupabaseConfig {
    /// The `User-Agent` every Supabase request from Synchrome must carry (Cloudflare 1010).
    pub fn user_agent(&self) -> String {
        user_agent()
    }
}

/// Placeholder adapter. Methods and `StorageAdapter` impl land in a later milestone.
pub struct SupabaseStore {
    #[allow(dead_code)]
    config: SupabaseConfig,
}

impl SupabaseStore {
    pub fn new(config: SupabaseConfig) -> Self {
        Self { config }
    }
}
