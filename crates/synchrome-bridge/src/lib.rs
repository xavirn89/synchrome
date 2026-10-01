//! Native-messaging bridge between the Synchrome extension and the app.
//!
//! - [`protocol`]: wire messages (`Hello { mapping_token }` / `HelloResult`), token redacted.
//! - [`framing`]: 4-byte native-endian length + JSON, bounded sizes, timeouts, typed errors.
//! - [`registry`]: local record (SHA-256 only) of tokens this installation wrote.
//! - [`handshake`]: resolve a presented token against the signed-in account and confirm it.
//! - [`CloudBackend`]: production [`handshake::CloudLookup`] over `CloudClient` + `Session`.
//!
//! Nothing here logs or returns a full mapping token.

pub mod framing;
pub mod handshake;
pub mod protocol;
pub mod registry;

pub use framing::{decode, encode, read_frame, write_frame, FrameError, MAX_INBOUND, MAX_OUTBOUND};
pub use handshake::{resolve, CloudLookup, HandshakeContext};
pub use protocol::{AppToExt, ExtToApp, HandshakeReply, HandshakeStatus, WireToken};
pub use registry::{token_sha256, LocalTokenRecord, LocalTokenState, TokenRegistry};

use synchrome_auth::Session;
use synchrome_cloud::{CloudClient, CloudError, Mapping, RestTransport};
use uuid::Uuid;

/// [`CloudLookup`] over the real PostgREST client with the user's session (RLS-scoped).
pub struct CloudBackend<'a, T: RestTransport> {
    pub cloud: &'a CloudClient<T>,
    pub session: &'a Session,
}

impl<T: RestTransport> CloudLookup for CloudBackend<'_, T> {
    async fn list_mappings(&self) -> Result<Vec<Mapping>, CloudError> {
        self.cloud.list_mappings(self.session).await
    }
    async fn profile_name(&self, profile_id: Uuid) -> Result<Option<String>, CloudError> {
        Ok(self
            .cloud
            .get_profile(self.session, profile_id)
            .await?
            .map(|p| p.name))
    }
    async fn confirm(&self, mapping_id: Uuid) -> Result<Mapping, CloudError> {
        self.cloud
            .confirm_mapping_token(self.session, mapping_id)
            .await
    }
}
