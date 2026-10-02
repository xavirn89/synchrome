//! Bookmarks capture → publication glue (M1 slice 7, upload only).
//!
//! The native host feeds extension observations into `state.sqlite` (see
//! [`synchrome_state`]); this module publishes the outbox to Synchrome Cloud through a
//! [`JournalSink`] built on the user's session, which stays in Rust: the access token is read
//! by `synchrome-cloud` only, refreshed when near expiry, and force-refreshed once when the
//! server answers 401. Nothing here logs titles, URLs or tokens.

use crate::{local_token, AppState};
use std::path::PathBuf;
use synchrome_auth::{AuthError, Session};
use synchrome_cloud::TokenState;
use synchrome_cloud::{CloudClient, CloudError, ReqwestRest, StoreEvent, JOURNAL_CONTENT_TYPE};
use synchrome_state::{
    BookmarkSyncStatus, JournalSink, MappingCheck, PublishOutcome, PublishedEvent, SinkError,
    StateDb,
};
use tokio::sync::Mutex;
use uuid::Uuid;

pub fn state_db_path() -> Result<PathBuf, String> {
    Ok(local_token::app_data_dir()?.join("state.sqlite"))
}

pub fn open_db() -> Result<StateDb, String> {
    StateDb::open(&state_db_path()?).map_err(|e| e.to_string())
}

pub fn now_unix() -> i64 {
    synchrome_bridge::registry::now_unix()
}

fn map_cloud(e: CloudError) -> SinkError {
    match e {
        CloudError::Network(m) => SinkError::Offline(m),
        CloudError::Server(s) => SinkError::Offline(format!("HTTP {s}")),
        CloudError::Unauthorized => SinkError::NotSignedIn,
        CloudError::Forbidden => SinkError::Forbidden("policy".into()),
        CloudError::SchemaMissing(m) => SinkError::SchemaMissing(m),
        other => SinkError::Other(other.to_string()),
    }
}

fn map_auth(e: AuthError) -> SinkError {
    match e {
        AuthError::Network(m) => SinkError::Offline(m),
        AuthError::Server(s) => SinkError::Offline(format!("HTTP {s}")),
        AuthError::SessionInvalid | AuthError::InvalidCredentials => SinkError::NotSignedIn,
        other => SinkError::Other(other.to_string()),
    }
}

/// The store as seen through the signed-in session.
pub struct CloudSink<'a> {
    state: &'a AppState,
    session: Mutex<Option<Session>>,
}

/// Run a cloud call; on 401 force-refresh the session once and retry.
macro_rules! with_session {
    ($sink:expr, |$s:ident, $c:ident| $call:expr) => {{
        let $c = $sink.cloud()?;
        let first = {
            let $s = $sink.session().await?;
            $call.await
        };
        match first {
            Err(CloudError::Unauthorized) => {
                let $s = $sink.refreshed().await?;
                $call.await.map_err(map_cloud)
            }
            r => r.map_err(map_cloud),
        }
    }};
}

impl<'a> CloudSink<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self {
            state,
            session: Mutex::new(None),
        }
    }

    fn cloud(&self) -> Result<&'a CloudClient<ReqwestRest>, SinkError> {
        self.state.cloud().map_err(SinkError::Other)
    }

    async fn session(&self) -> Result<Session, SinkError> {
        let mut g = self.session.lock().await;
        if let Some(s) = g.as_ref() {
            return Ok(s.clone());
        }
        let auth = self.state.auth().map_err(SinkError::Other)?;
        match auth.ensure_valid_session().await.map_err(map_auth)? {
            Some(s) => {
                *g = Some(s.clone());
                Ok(s)
            }
            None => Err(SinkError::NotSignedIn),
        }
    }

    async fn refreshed(&self) -> Result<Session, SinkError> {
        let auth = self.state.auth().map_err(SinkError::Other)?;
        match auth.force_refresh().await.map_err(map_auth)? {
            Some(s) => {
                *self.session.lock().await = Some(s.clone());
                Ok(s)
            }
            None => Err(SinkError::NotSignedIn),
        }
    }
}

impl JournalSink for CloudSink<'_> {
    async fn account_id(&self) -> Result<String, SinkError> {
        Ok(self.session().await?.user_id)
    }

    async fn mapping(&self, mapping_id: Uuid) -> Result<MappingCheck, SinkError> {
        let me = self.state.device().map_err(SinkError::Other)?.device_id;
        let all = with_session!(self, |s, c| c.list_mappings(&s))?;
        Ok(match all.into_iter().find(|m| m.id == mapping_id) {
            Some(m) if m.device_id == me && m.token_state == TokenState::Confirmed => {
                MappingCheck::Active
            }
            _ => MappingCheck::Missing,
        })
    }

    async fn journal_devices(&self, profile_id: Uuid) -> Result<Vec<String>, SinkError> {
        with_session!(self, |s, c| c.journal_devices(&s, profile_id))
    }

    async fn put_chunk(&self, path: &str, body: &[u8]) -> Result<(), SinkError> {
        with_session!(self, |s, c| c.put_object(
            &s,
            path,
            body.to_vec(),
            JOURNAL_CONTENT_TYPE
        ))
    }

    async fn record_event(&self, ev: &PublishedEvent) -> Result<(), SinkError> {
        let row = StoreEvent {
            profile_id: ev.profile_id,
            device_id: ev.device_id,
            path: ev.path.clone(),
            seq_from: ev.seq_from as i64,
            seq_to: ev.seq_to as i64,
            hlc: ev.hlc.clone(),
            created_at: None,
        };
        with_session!(self, |s, c| c.record_store_event(&s, &row))
    }
}

/// Publish one profile's outbox now.
pub async fn publish_profile(state: &AppState, profile_id: Uuid) -> Result<PublishOutcome, String> {
    let mut db = open_db()?;
    let sink = CloudSink::new(state);
    let holder = format!("{}-{}", std::process::id(), Uuid::new_v4());
    synchrome_state::publish(&mut db, &sink, profile_id, &holder, now_unix())
        .await
        .map_err(|e| e.to_string())
}

/// Publish every profile with pending ops (background loop, `sync_now`).
pub async fn publish_all_pending(state: &AppState) -> Vec<(Uuid, Result<PublishOutcome, String>)> {
    let pending =
        match open_db().and_then(|db| db.profiles_with_pending().map_err(|e| e.to_string())) {
            Ok(p) => p,
            Err(e) => return vec![(Uuid::nil(), Err(e))],
        };
    let mut out = Vec::new();
    for p in pending {
        out.push((p, publish_profile(state, p).await));
    }
    out
}

/// Bookmarks status of a mapping of THIS device (None when it never captured).
pub fn status_for_mapping(mapping_id: Uuid, profile_id: Uuid) -> Option<BookmarkSyncStatus> {
    let db = open_db().ok()?;
    db.status(profile_id, mapping_id).ok()
}

/// Remove a profile's store objects and local state (before deleting the cloud profile).
pub async fn purge_profile(
    state: &AppState,
    session: &Session,
    profile_id: Uuid,
) -> Result<usize, String> {
    let n = state
        .cloud()?
        .delete_profile_objects(session, profile_id)
        .await
        .or_else(|e| match e {
            // bucket not there yet (0004 not applied) = nothing to delete
            CloudError::SchemaMissing(_) | CloudError::NotFound => Ok(0),
            e => Err(e),
        })
        .map_err(|e| e.to_string())?;
    if let Ok(mut db) = open_db() {
        let _ = db.forget_profile(profile_id);
    }
    Ok(n)
}
