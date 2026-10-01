//! Authenticated PostgREST client for Synchrome Cloud account metadata.
//!
//! Every request carries `apikey: <anon>` and `Authorization: Bearer <access_token>`; Row
//! Level Security on the server scopes rows to `auth.uid()`. The access token is read in
//! exactly one place ([`auth_headers`]) and never leaves this crate. The frontend only ever
//! receives [`DeviceSummary`], [`ProfileSummary`] and [`MappingSummary`].
//!
//! Scope so far: devices (upsert/touch/list), profiles (create/rename/delete/list) and local ↔
//! cloud profile mappings (map/unmap/list/lookup/confirm). No Storage, Realtime, CAS or sync.

mod client;
mod mapping;
mod models;
mod ops;

pub use client::{
    auth_headers, classify_postgrest, CloudClient, CloudError, ReqwestRest, RestFuture,
    RestTransport,
};
pub use mapping::{Mapping, MappingStatus, MappingSummary, MappingToken, TokenState};
pub use models::{DeviceSummary, ProfileSummary};
pub use ops::encode_filter_value;
