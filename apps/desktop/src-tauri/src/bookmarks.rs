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
    BookmarkSyncStatus, JournalSink, MappingCheck, PublishOutcome, PublishedEvent, PullOutcome,
    RemoteEvent, SinkError, StateDb,
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
    refreshes: std::sync::atomic::AtomicUsize,
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
            refreshes: Default::default(),
        }
    }

    /// Start from a given session (dev harness: a deliberately rejected access token proves
    /// the 401 → force-refresh → retry path).
    #[cfg(debug_assertions)]
    pub fn with_session(state: &'a AppState, session: Session) -> Self {
        Self {
            state,
            session: Mutex::new(Some(session)),
            refreshes: Default::default(),
        }
    }

    /// How many forced refreshes this sink performed.
    pub fn refreshes(&self) -> usize {
        self.refreshes.load(std::sync::atomic::Ordering::Relaxed)
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
        self.refreshes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
    async fn list_events(&self, profile_id: Uuid) -> Result<Vec<RemoteEvent>, SinkError> {
        let rows = with_session!(self, |s, c| c.list_store_events(&s, profile_id))?;
        Ok(rows
            .into_iter()
            .map(|e| RemoteEvent {
                device_id: e.device_id.to_string(),
                path: e.path,
                seq_from: e.seq_from.max(0) as u64,
                seq_to: e.seq_to.max(0) as u64,
            })
            .collect())
    }

    async fn get_chunk(&self, path: &str) -> Result<Option<Vec<u8>>, SinkError> {
        with_session!(self, |s, c| c.get_object(&s, path))
    }

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
    publish_with(&CloudSink::new(state), profile_id).await
}

/// Publish through any sink (production: [`CloudSink`]; dev harness: fault-injecting
/// wrappers around it).
pub async fn publish_with<S: JournalSink>(
    sink: &S,
    profile_id: Uuid,
) -> Result<PublishOutcome, String> {
    let mut db = open_db()?;
    let holder = format!("{}-{}", std::process::id(), Uuid::new_v4());
    synchrome_state::publish(&mut db, sink, profile_id, &holder, now_unix())
        .await
        .map_err(|e| e.to_string())
}

/// Pull (download + materialise remote changes) then publish, for one profile.
pub async fn sync_profile(
    state: &AppState,
    profile_id: Uuid,
) -> Result<(PullOutcome, PublishOutcome), String> {
    let mut db = open_db()?;
    let sink = CloudSink::new(state);
    let holder = format!("{}-{}", std::process::id(), Uuid::new_v4());
    synchrome_state::sync(&mut db, &sink, profile_id, &holder, now_unix())
        .await
        .map_err(|e| e.to_string())
}

/// Pull only (before a joining device's first snapshot).
pub async fn pull_profile(state: &AppState, profile_id: Uuid) -> Result<PullOutcome, String> {
    let mut db = open_db()?;
    let sink = CloudSink::new(state);
    let holder = format!("{}-{}", std::process::id(), Uuid::new_v4());
    synchrome_state::pull(&mut db, &sink, profile_id, &holder, now_unix())
        .await
        .map_err(|e| e.to_string())
}

/// Background cycle: every profile this device keeps state for is pulled and published.
/// Applying to the browser needs the extension's connection (it asks with bookmarks_pull).
pub async fn sync_all(
    state: &AppState,
) -> Vec<(Uuid, Result<(PullOutcome, PublishOutcome), String>)> {
    let ids = match open_db().and_then(|db| db.profile_ids().map_err(|e| e.to_string())) {
        Ok(p) => p,
        Err(e) => return vec![(Uuid::nil(), Err(e))],
    };
    let mut out = Vec::new();
    for p in ids {
        out.push((p, sync_profile(state, p).await));
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

// ------------------------------------------------------------------ conflicts (slice 9)

/// Outcome of one resolution request, for the webview (no ids beyond the conflict's own).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolveResult {
    pub conflict_id: String,
    /// `resolved` | `resolving` | `already_resolved` | `closed` | `error`
    pub outcome: &'static str,
    pub status: Option<synchrome_state::ConflictStatus>,
    pub ops: usize,
    pub error: Option<String>,
}

/// The signed-in account (from the stored session: resolving works offline; the ops are
/// uploaded when the cloud is reachable again).
fn current_account(state: &AppState) -> Result<String, String> {
    state
        .auth()?
        .stored_session()
        .map_err(|e| e.to_string())?
        .map(|s| s.user_id)
        .ok_or_else(|| "sign in to review conflicts".to_string())
}

/// The local profile behind a mapping of THIS device, checked against the current account.
fn profile_of_mapping(state: &AppState, mapping_id: Uuid) -> Result<(String, Uuid), String> {
    let account = current_account(state)?;
    let db = open_db()?;
    let pid = db
        .profile_for_mapping(mapping_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no local bookmarks state for this mapping".to_string())?;
    Ok((account, pid))
}

/// Conflicts of a mapping's profile (open ones; with `include_closed`, the history too).
pub fn conflicts_for_mapping(
    state: &AppState,
    mapping_id: Uuid,
    include_closed: bool,
) -> Result<Vec<synchrome_state::ConflictView>, String> {
    let (account, pid) = profile_of_mapping(state, mapping_id)?;
    let mut db = open_db()?;
    // a profile captured under another account is not shown to this one
    if !db
        .profile_account(pid)
        .map_err(|e| e.to_string())?
        .is_some_and(|a| a == account)
    {
        return Err("this profile belongs to another account".into());
    }
    db.conflict_views(pid, include_closed)
        .map_err(|e| e.to_string())
}

/// Resolve conflicts of a mapping's profile, then upload the resolution ops (bounded; the
/// background loop retries if this attempt fails). The browser is brought to the decision by
/// the extension's next apply plan.
pub async fn resolve_for_mapping(
    state: &AppState,
    mapping_id: Uuid,
    items: Vec<(String, synchrome_state::Resolution)>,
) -> Result<Vec<ResolveResult>, String> {
    use synchrome_state::{ResolveError, ResolveOutcome};
    let (account, pid) = profile_of_mapping(state, mapping_id)?;
    let mut db = open_db()?;
    // the profile must belong to the signed-in account (each record is checked again below);
    // this refuses the whole request without looking at any conflict id
    if !db
        .profile_account(pid)
        .map_err(|e| e.to_string())?
        .is_some_and(|a| a == account)
    {
        return Err("this profile belongs to another account".into());
    }
    let now = synchrome_sync::hlc::now_ms();
    let results = db.resolve_conflicts(&account, pid, &items, now);
    drop(db);
    let out: Vec<ResolveResult> = items
        .iter()
        .zip(results)
        .map(|((id, _), r)| {
            let (outcome, status, ops, error) = match r {
                Ok(ResolveOutcome::Resolved { ops }) => ("resolved", None, ops, None),
                Ok(ResolveOutcome::Resolving { ops }) => ("resolving", None, ops, None),
                Ok(ResolveOutcome::AlreadyResolved) => ("already_resolved", None, 0, None),
                Ok(ResolveOutcome::Closed { status }) => ("closed", Some(status), 0, None),
                Err(ResolveError::NotFound) => {
                    ("error", None, 0, Some("conflict not found".into()))
                }
                Err(e) => ("error", None, 0, Some(e.to_string())),
            };
            ResolveResult {
                conflict_id: id.clone(),
                outcome,
                status,
                ops,
                error,
            }
        })
        .collect();
    if out.iter().any(|r| r.ops > 0) {
        // publish only (no pull first): the decision was taken against the versions shown;
        // anything newer merges by LWW after it. On failure the ops stay in the outbox.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            publish_profile(state, pid),
        )
        .await;
    }
    Ok(out)
}
