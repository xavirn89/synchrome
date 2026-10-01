//! Authenticated PostgREST client for Synchrome Cloud account metadata.
//!
//! First consumer of the user session: every request carries `apikey: <anon>` and
//! `Authorization: Bearer <access_token>`; Row Level Security on the server scopes rows to
//! `auth.uid()`. The access token is read in exactly one place ([`auth_headers`]) and never
//! leaves this crate. The frontend only ever receives [`DeviceSummary`] / [`ProfileSummary`].
//!
//! Scope of this slice: upsert the current device, touch `last_seen_at`, list devices and
//! profiles. No profile creation, no Storage, no Realtime, no CAS.

mod client;
mod models;

pub use client::{
    auth_headers, classify_postgrest, CloudClient, CloudError, ReqwestRest, RestFuture,
    RestTransport,
};
pub use models::{DeviceSummary, ProfileSummary};
