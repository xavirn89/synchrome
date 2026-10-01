//! Storage adapter abstraction.
//!
//! A `StorageAdapter` is the only thing the sync engine knows about a backend. Concrete
//! adapters (Supabase Cloud, GitHub, local folder) live in their own crates and implement this
//! trait. Compare-and-swap is expressed via an opaque `version` token; backends without native
//! object CAS (Supabase Storage, per E9) emulate it. No method is implemented in M1.

use serde::{Deserialize, Serialize};
use std::future::Future;
use thiserror::Error;

/// Opaque per-object version token (ETag, git blob sha, Storage `version` UUID, …).
pub type Version = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub path: String,
    pub version: Version,
    pub size: u64,
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("compare-and-swap version mismatch for {path}")]
    VersionMismatch { path: String },
    #[error("object not found: {path}")]
    NotFound { path: String },
    #[error("backend error: {0}")]
    Backend(String),
}

/// What a backend can do natively, so the engine can choose a strategy.
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub conditional_write: bool,
    pub atomic_batch: bool,
    pub push: bool,
}

/// The contract every Synchrome storage backend implements.
///
/// Async is expressed with `impl Future` so the trait stays object-safe-friendly without
/// pulling in `async-trait` at this stage. Signatures are frozen for M1; bodies arrive later.
pub trait StorageAdapter: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    fn get(
        &self,
        path: &str,
    ) -> impl Future<Output = Result<Option<(Vec<u8>, Version)>, StorageError>> + Send;

    fn list(
        &self,
        prefix: &str,
    ) -> impl Future<Output = Result<Vec<ObjectMeta>, StorageError>> + Send;

    /// Write an object. `if_version = Some(v)` requires the current version to equal `v`
    /// (compare-and-swap); `if_version = Some("")`-style "must not exist" is modelled by the
    /// concrete adapter. Returns the new version.
    fn put(
        &self,
        path: &str,
        data: &[u8],
        if_version: Option<&str>,
    ) -> impl Future<Output = Result<Version, StorageError>> + Send;

    fn delete(
        &self,
        path: &str,
        if_version: Option<&str>,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;
}
