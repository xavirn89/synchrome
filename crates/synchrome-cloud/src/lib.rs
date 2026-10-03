//! Authenticated PostgREST client for Synchrome Cloud account metadata.
//!
//! Every request carries `apikey: <anon>` and `Authorization: Bearer <access_token>`; Row
//! Level Security on the server scopes rows to `auth.uid()`. The access token is read in
//! exactly one place ([`auth_headers`]) and never leaves this crate. The frontend only ever
//! receives [`DeviceSummary`], [`ProfileSummary`] and [`MappingSummary`].
//!
//! Scope so far: devices (upsert/touch/list), profiles (create/rename/delete/list) and local ↔
//! cloud profile mappings (map/unmap/list/lookup/confirm), store objects in Storage (journal
//! chunks) and `store_events`, compaction metadata (migration 0005) and the Realtime wake-up
//! protocol (Phoenix over WebSocket; the socket itself lives in the app).

mod client;
pub mod compaction;
mod mapping;
mod models;
mod ops;
pub mod realtime;
mod storage;

pub use client::{
    auth_headers, classify_postgrest, CloudClient, CloudError, ReqwestRest, RestFuture,
    RestTransport,
};
pub use mapping::{Mapping, MappingStatus, MappingSummary, MappingToken, TokenState};
pub use models::{DeviceSummary, ProfileSummary};
pub use ops::encode_filter_value;
pub use storage::{
    classify_storage, valid_rel_path, StorageEntry, StoreEvent, JOURNAL_CONTENT_TYPE, STORE_BUCKET,
};
