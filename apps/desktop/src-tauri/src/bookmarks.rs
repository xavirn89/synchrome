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
use synchrome_cloud::{
    CloudClient, CloudError, ReqwestRest, RestTransport, StoreEvent, JOURNAL_CONTENT_TYPE,
};
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

/// Like `with_session!` but returns the raw `CloudError` (callers map rejections).
macro_rules! with_session_raw {
    ($sink:expr, |$s:ident, $c:ident| $call:expr) => {{
        let $c = $sink.cloud()?;
        let first = {
            let $s = $sink.session().await?;
            $call.await
        };
        match first {
            Err(CloudError::Unauthorized) => {
                let $s = $sink.refreshed().await?;
                $call.await
            }
            r => r,
        }
    }};
}

/// A database refusal (check / trigger / privilege / lost race) is `Rejected`; anything else
/// keeps the usual mapping. Messages are short and carry no values.
fn map_rej(e: CloudError) -> SinkError {
    if synchrome_cloud::compaction::is_rejection(&e) {
        SinkError::Rejected(e.to_string().chars().take(160).collect())
    } else {
        map_cloud(e)
    }
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
    #[cfg_attr(not(debug_assertions), allow(dead_code))] // dev CLI only
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
        #[allow(unused_mut)]
        let mut got = with_session!(self, |s, c| c.get_object(&s, path))?;
        // debug builds only: simulate a corrupted snapshot generation (E2E fallback test)
        #[cfg(debug_assertions)]
        if let (Ok(gens), Some(b)) = (
            std::env::var("SYNCHROME_DEV_CORRUPT_SNAPSHOT_GEN"),
            got.as_mut(),
        ) {
            // one generation or a comma-separated list
            if gens
                .split(',')
                .any(|g| path.contains(&format!("/snapshots/{:0>10}/", g.trim())))
                && path.ends_with(".jsonl")
                && !b.is_empty()
            {
                b[0] ^= 1;
            }
        }
        Ok(got)
    }

    async fn ready_snapshots(
        &self,
        profile_id: Uuid,
    ) -> Result<Vec<synchrome_state::compaction::SnapshotRow>, SinkError> {
        let rows = match with_session_raw!(self, |s, c| c.list_snapshot_rows(&s, profile_id)) {
            Ok(r) => r,
            // an instance without 0005: no snapshots
            Err(CloudError::SchemaMissing(_)) => return Ok(Vec::new()),
            Err(e) => return Err(map_cloud(e)),
        };
        let mut v: Vec<_> = rows
            .into_iter()
            .filter(|r| r.state == "ready")
            .map(|r| crate::compaction::snapshot_row(r, false))
            .collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.generation));
        v.truncate(2);
        Ok(v)
    }

    fn prefer_snapshot_bootstrap(&self) -> bool {
        #[cfg(debug_assertions)]
        if std::env::var("SYNCHROME_DEV_FULL_REPLAY").is_ok_and(|v| v == "1") {
            return false;
        }
        true
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
        // extension settings: read/written only while that profile's browser is closed
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            crate::ext_settings::settings_cycle(state, p),
        )
        .await;
        // browser preferences: likewise, only while that profile's browser is closed
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            crate::browser_prefs::prefs_cycle(state, p),
        )
        .await;
        // compaction (slice 13): cursor report; at most every 10 min the database is asked
        // whether a snapshot / journal GC is due
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            crate::compaction::after_sync(state, p),
        )
        .await;
    }
    // connections made with the pre-store companion: hand their token to the store companion
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        reseed_store_companions(state),
    )
    .await;
    out
}

/// Connections whose store companion was already seen holding its token (no further reads).
static COMPANION_TOKEN_OK: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<Uuid>>> =
    std::sync::OnceLock::new();

/// Give the Chrome Web Store companion the connection token where a confirmed connection was
/// made with the pre-store companion (browser closed only; nothing for absent companions).
pub async fn reseed_store_companions(state: &AppState) {
    let ok = COMPANION_TOKEN_OK.get_or_init(Default::default);
    let Ok(account) = current_account(state) else {
        return;
    };
    let pending: Vec<synchrome_bridge::LocalTokenRecord> = crate::local_token::registry()
        .load()
        .into_iter()
        .filter(|r| r.account_id == account && r.state != synchrome_bridge::LocalTokenState::Failed)
        .filter(|r| {
            !ok.lock()
                .map(|s| s.contains(&r.mapping_id))
                .unwrap_or(false)
        })
        .collect();
    if pending.is_empty() {
        return;
    }
    let mut mappings: Option<Vec<synchrome_cloud::Mapping>> = None;
    for r in pending {
        let udd = std::path::PathBuf::from(&r.user_data_dir);
        let store = synchrome_browser::extension_store_path(
            &udd,
            &r.profile_dir,
            synchrome_types::SYNCHROME_EXTENSION_ID,
        );
        if !store.exists() || synchrome_browser::is_browser_running(&udd) {
            continue; // not installed there / not now
        }
        if !crate::local_token::store_companion_missing_token(&udd, &r.profile_dir) {
            if let Ok(mut s) = ok.lock() {
                s.insert(r.mapping_id);
            }
            continue;
        }
        if mappings.is_none() {
            let (Ok(session), Ok(cloud)) = (state.session().await, state.cloud()) else {
                return;
            };
            mappings = cloud.list_mappings(&session).await.ok();
        }
        let me = state.device().map(|d| d.device_id).ok();
        if let Some(m) = mappings.as_ref().and_then(|all| {
            all.iter()
                .find(|m| m.id == r.mapping_id && Some(m.device_id) == me)
        }) {
            if crate::local_token::write_token_local(m, &account, &udd).is_ok() {
                if let Ok(mut s) = ok.lock() {
                    s.insert(r.mapping_id);
                }
            }
        }
    }
}

/// Extensions counts of a profile on this device (None when unknown).
pub fn ext_status(profile_id: Uuid) -> Option<synchrome_state::ExtStatus> {
    open_db().ok()?.ext_status(profile_id).ok()
}

/// Bookmarks status of a mapping of THIS device (None when it never captured).
pub fn status_for_mapping(mapping_id: Uuid, profile_id: Uuid) -> Option<BookmarkSyncStatus> {
    let db = open_db().ok()?;
    db.status(profile_id, mapping_id).ok()
}

/// Outcome of deleting a cloud profile with everything it owns.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ProfileDeletion {
    pub deleted: bool,
    pub objects_deleted: usize,
    /// Phase 1 (before the row): objects the server allowed while the profile existed.
    pub objects_before_row: usize,
    /// Phase 3 (after the row and its cascades): the remaining objects.
    pub objects_after_row: usize,
    /// Objects still under the profile's prefix afterwards (expected 0).
    pub leftovers: usize,
    pub error: Option<String>,
}

/// Delete a cloud profile under the 0005 contract: (1) the objects the server lets us delete
/// now (journal chunks already garbage-collectable, non-ready snapshot objects), (2) the
/// profile row — its `store_events`, snapshot generations and cursors go by FK cascade (it is
/// refused while the profile is still mapped somewhere), (3) every remaining object under the
/// profile's prefix (allowed once the row is gone), (4) verify nothing is left. Local sync
/// state is forgotten only after the cloud row was deleted. Browser profiles are untouched.
pub async fn delete_profile_everywhere(
    state: &AppState,
    session: &Session,
    profile_id: Uuid,
) -> Result<ProfileDeletion, String> {
    let cloud = state.cloud()?;
    delete_profile_with(cloud, session, profile_id, |id| {
        open_db()?.forget_profile(id).map_err(|e| e.to_string())
    })
    .await
}

/// The deletion ordering is shared by production and tests with an injected cloud store and
/// local cleanup. A failure before cloud row deletion must preserve the local outbox; after
/// row deletion, cleanup failures must report that the profile is already gone.
async fn delete_profile_with<T: RestTransport>(
    cloud: &CloudClient<T>,
    session: &Session,
    profile_id: Uuid,
    forget_local: impl FnOnce(Uuid) -> Result<(), String>,
) -> Result<ProfileDeletion, String> {
    let tolerate = |e: CloudError| match e {
        // bucket not there yet = nothing to delete
        CloudError::SchemaMissing(_) | CloudError::NotFound => Ok(0),
        e => Err(e.to_string()),
    };
    let first = cloud
        .delete_profile_objects(session, profile_id)
        .await
        .or_else(tolerate)?;
    let mut rep = ProfileDeletion {
        objects_deleted: first,
        objects_before_row: first,
        ..Default::default()
    };
    match cloud.delete_profile(session, profile_id).await {
        Ok(()) => rep.deleted = true,
        Err(e) => {
            rep.error = Some(e.to_string());
            return Ok(rep);
        }
    }
    let mut problems = Vec::new();
    if let Err(e) = forget_local(profile_id) {
        problems.push(format!("The local sync cache could not be cleared: {e}"));
    }
    match cloud
        .delete_profile_objects(session, profile_id)
        .await
        .or_else(tolerate)
    {
        Ok(n) => rep.objects_after_row = n,
        Err(e) => problems.push(format!("Cloud files could not be fully cleaned up: {e}")),
    }
    rep.objects_deleted += rep.objects_after_row;
    match cloud
        .list_tree(session, &format!("profiles/{profile_id}"))
        .await
        .map(|v| v.len())
        .or_else(tolerate)
    {
        Ok(n) => {
            rep.leftovers = n;
            if n > 0 {
                problems.push(format!("{n} cloud file(s) still need cleanup."));
            }
        }
        Err(e) => problems.push(format!("Cloud file cleanup could not be verified: {e}")),
    }
    if !problems.is_empty() {
        rep.error = Some(format!(
            "The Synchrome profile was deleted, but cleanup is incomplete. Your local browser profiles and data were not deleted. {}",
            problems.join(" ")
        ));
    }
    Ok(rep)
}

#[cfg(test)]
mod deletion_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex as StdMutex};
    use synchrome_auth::Secret;
    use synchrome_cloud::RestFuture;
    use synchrome_config::SupabaseConfig;
    use synchrome_state::Source;
    use synchrome_sync::bookmarks::ChromiumNode;

    type Trace = Arc<StdMutex<Vec<&'static str>>>;
    type Reply = (&'static str, u16, &'static str);

    struct MockStore {
        replies: StdMutex<VecDeque<Reply>>,
        trace: Trace,
    }

    impl RestTransport for MockStore {
        fn request<'a>(
            &'a self,
            method: &'a str,
            url: &'a str,
            _headers: &'a [(&'a str, &'a str)],
            _body: Option<&'a serde_json::Value>,
        ) -> RestFuture<'a> {
            let step = if method == "POST" && url.ends_with("/object/list/stores") {
                "list files"
            } else if method == "DELETE" && url.contains("/rest/v1/profiles?") {
                "delete row"
            } else if method == "DELETE" && url.ends_with("/object/stores") {
                "delete files"
            } else {
                panic!("unexpected deletion request: {method} {url}");
            };
            let (expected, status, body) = self
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra deletion request");
            assert_eq!(step, expected);
            self.trace.lock().unwrap().push(step);
            Box::pin(async move { Ok((status, body.as_bytes().to_vec())) })
        }
    }

    fn session() -> Session {
        Session {
            user_id: "11111111-1111-4111-8111-111111111111".into(),
            email: None,
            access_token: Secret::new("mock-token"),
            refresh_token: Secret::new("mock-refresh"),
            expires_at: i64::MAX,
            token_type: "bearer".into(),
        }
    }

    fn store(replies: Vec<Reply>) -> (CloudClient<MockStore>, Trace) {
        let trace = Trace::default();
        let cloud = CloudClient::with_transport(
            SupabaseConfig {
                url: "https://cloud.example.invalid".into(),
                anon_key: "mock-publishable-key".into(),
            },
            MockStore {
                replies: StdMutex::new(replies.into()),
                trace: trace.clone(),
            },
        );
        (cloud, trace)
    }

    /// Real in-memory sync state with an unpublished bookmark, so a rejected deletion must
    /// preserve actual user changes rather than just avoid calling a mock method.
    fn local_state(profile_id: Uuid) -> StateDb {
        let mut db = StateDb::open_in_memory().unwrap();
        let src = Source {
            mapping_id: Uuid::new_v4(),
            account_id: session().user_id,
            device_id: Uuid::new_v4(),
            profile_id,
            identity: "helium|/mock/browser|Default".into(),
        };
        let nodes: Vec<ChromiumNode> = serde_json::from_value(serde_json::json!([
            {"id": "1", "parent_id": "0", "title": "Bookmarks bar"},
            {"id": "2", "parent_id": "0", "title": "Other bookmarks"},
            {"id": "3", "parent_id": "0", "title": "Mobile bookmarks"},
            {"id": "4", "parent_id": "1", "title": "Pending bookmark", "url": "https://example.invalid"}
        ]))
        .unwrap();
        db.capture_snapshot(&src, "test-epoch", 0, &nodes, 1_790_900_000_000)
            .unwrap();
        assert_eq!(db.pending(profile_id).unwrap(), 1);
        db
    }

    #[tokio::test]
    async fn cloud_row_rejection_preserves_unpublished_local_changes() {
        for response in [
            (409, r#"{"code":"23503","message":"still mapped"}"#),
            (503, r#"{"message":"unavailable"}"#),
        ] {
            let profile = Uuid::new_v4();
            let mut db = local_state(profile);
            let (cloud, trace) = store(vec![
                ("list files", 200, "[]"),
                ("delete row", response.0, response.1),
            ]);
            let report = delete_profile_with(&cloud, &session(), profile, |id| {
                trace.lock().unwrap().push("forget local");
                db.forget_profile(id).map_err(|e| e.to_string())
            })
            .await
            .unwrap();
            assert!(!report.deleted);
            assert!(report.error.is_some());
            assert_eq!(db.pending(profile).unwrap(), 1);
            assert!(db.profile_account(profile).unwrap().is_some());
            assert_eq!(*trace.lock().unwrap(), ["list files", "delete row"]);
        }
    }

    #[tokio::test]
    async fn storage_failure_before_row_preserves_profile_and_local_changes() {
        let profile = Uuid::new_v4();
        let mut db = local_state(profile);
        let (cloud, trace) = store(vec![("list files", 503, "{}")]);
        let result = delete_profile_with(&cloud, &session(), profile, |id| {
            trace.lock().unwrap().push("forget local");
            db.forget_profile(id).map_err(|e| e.to_string())
        })
        .await;
        assert!(result.is_err());
        assert_eq!(db.pending(profile).unwrap(), 1);
        assert!(db.profile_account(profile).unwrap().is_some());
        assert_eq!(*trace.lock().unwrap(), ["list files"]);
    }

    #[tokio::test]
    async fn success_forgets_local_only_after_row_and_verifies_cloud_cleanup() {
        let profile = Uuid::new_v4();
        let mut db = local_state(profile);
        let (cloud, trace) = store(vec![
            ("list files", 200, "[]"),
            ("delete row", 200, r#"[{"id":"deleted"}]"#),
            (
                "list files",
                200,
                r#"[{"name":"journal.jsonl","id":"object"}]"#,
            ),
            ("delete files", 200, r#"[{"name":"journal.jsonl"}]"#),
            ("list files", 200, "[]"),
        ]);
        let report = delete_profile_with(&cloud, &session(), profile, |id| {
            trace.lock().unwrap().push("forget local");
            db.forget_profile(id).map_err(|e| e.to_string())
        })
        .await
        .unwrap();
        assert!(report.deleted);
        assert!(report.error.is_none());
        assert_eq!(report.objects_deleted, 1);
        assert_eq!(report.objects_after_row, 1);
        assert_eq!(report.leftovers, 0);
        assert!(db.profile_account(profile).unwrap().is_none());
        assert_eq!(
            *trace.lock().unwrap(),
            [
                "list files",
                "delete row",
                "forget local",
                "list files",
                "delete files",
                "list files"
            ]
        );
    }

    #[tokio::test]
    async fn failed_cloud_cleanup_reports_profile_already_deleted() {
        let profile = Uuid::new_v4();
        let mut db = local_state(profile);
        let (cloud, trace) = store(vec![
            ("list files", 200, "[]"),
            ("delete row", 200, r#"[{"id":"deleted"}]"#),
            (
                "list files",
                200,
                r#"[{"name":"journal.jsonl","id":"object"}]"#,
            ),
            ("delete files", 503, "{}"),
            (
                "list files",
                200,
                r#"[{"name":"journal.jsonl","id":"object"}]"#,
            ),
        ]);
        let report = delete_profile_with(&cloud, &session(), profile, |id| {
            trace.lock().unwrap().push("forget local");
            db.forget_profile(id).map_err(|e| e.to_string())
        })
        .await
        .unwrap();
        assert!(report.deleted);
        assert_eq!(report.leftovers, 1);
        let error = report.error.unwrap();
        assert!(error.contains("profile was deleted, but cleanup is incomplete"));
        assert!(error.contains("Cloud files could not be fully cleaned up"));
        assert!(db.profile_account(profile).unwrap().is_none());
    }

    #[tokio::test]
    async fn leftover_files_are_not_reported_as_complete_success() {
        let profile = Uuid::new_v4();
        let (cloud, _) = store(vec![
            ("list files", 200, "[]"),
            ("delete row", 200, r#"[{"id":"deleted"}]"#),
            ("list files", 200, "[]"),
            (
                "list files",
                200,
                r#"[{"name":"leftover.jsonl","id":"object"}]"#,
            ),
        ]);
        let report = delete_profile_with(&cloud, &session(), profile, |_| Ok(()))
            .await
            .unwrap();
        assert!(report.deleted);
        assert_eq!(report.leftovers, 1);
        assert!(report
            .error
            .unwrap()
            .contains("1 cloud file(s) still need cleanup"));
    }

    #[tokio::test]
    async fn failed_verification_does_not_claim_cloud_files_are_gone() {
        let profile = Uuid::new_v4();
        let (cloud, _) = store(vec![
            ("list files", 200, "[]"),
            ("delete row", 200, r#"[{"id":"deleted"}]"#),
            ("list files", 200, "[]"),
            ("list files", 503, "{}"),
        ]);
        let report = delete_profile_with(&cloud, &session(), profile, |_| Ok(()))
            .await
            .unwrap();
        assert!(report.deleted);
        assert!(report
            .error
            .unwrap()
            .contains("cleanup could not be verified"));
    }

    #[tokio::test]
    async fn local_cleanup_failure_is_reported_while_cloud_cleanup_still_finishes() {
        let profile = Uuid::new_v4();
        let mut db = local_state(profile);
        let (cloud, trace) = store(vec![
            ("list files", 200, "[]"),
            ("delete row", 200, r#"[{"id":"deleted"}]"#),
            ("list files", 200, "[]"),
            ("list files", 200, "[]"),
        ]);
        let report = delete_profile_with(&cloud, &session(), profile, |_| {
            trace.lock().unwrap().push("forget local");
            Err("local database unavailable".into())
        })
        .await
        .unwrap();
        assert!(report.deleted);
        assert!(report
            .error
            .unwrap()
            .contains("local sync cache could not be cleared"));
        assert_eq!(report.leftovers, 0);
        assert_eq!(db.pending(profile).unwrap(), 1);
        assert_eq!(
            *trace.lock().unwrap(),
            [
                "list files",
                "delete row",
                "forget local",
                "list files",
                "list files"
            ]
        );
        // The fixture is deliberately retained on failure; cleanup is still safe to retry.
        db.forget_profile(profile).unwrap();
    }
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

// ------------------------------------------------------------------ extensions (slice 10)

/// The current account and this device's profile of a mapping (refuses another account's).
pub fn own_profile_pub(state: &AppState, mapping_id: Uuid) -> Result<(String, Uuid), String> {
    own_profile(state, mapping_id)
}

fn own_profile(state: &AppState, mapping_id: Uuid) -> Result<(String, Uuid), String> {
    let (account, pid) = profile_of_mapping(state, mapping_id)?;
    let db = open_db()?;
    if !db
        .profile_account(pid)
        .map_err(|e| e.to_string())?
        .is_some_and(|a| a == account)
    {
        return Err("this profile belongs to another account".into());
    }
    Ok((account, pid))
}

/// Extensions of a mapping's profile: this device vs the synchronised state.
pub fn extensions_for_mapping(
    state: &AppState,
    mapping_id: Uuid,
) -> Result<Vec<synchrome_state::ExtView>, String> {
    let (_, pid) = own_profile(state, mapping_id)?;
    open_db()?.ext_views(pid).map_err(|e| e.to_string())
}

/// An explicit user action on one extension (Install opens the store page, Remove was
/// confirmed in the UI). The browser executes it on the extension's next pull; a changed
/// enabled state is published right away.
pub async fn extension_action(
    state: &AppState,
    mapping_id: Uuid,
    ext_id: &str,
    action: synchrome_state::UserExtAction,
) -> Result<(), String> {
    let (account, pid) = own_profile(state, mapping_id)?;
    let mut db = open_db()?;
    db.ext_user_action(&account, pid, ext_id, action, synchrome_sync::hlc::now_ms())
        .map_err(|e| e.to_string())?;
    drop(db);
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        publish_profile(state, pid),
    )
    .await;
    Ok(())
}

// ------------------------------------------------------------------ compaction (slice 13)

impl synchrome_state::compaction::CompactionSink for CloudSink<'_> {
    async fn compaction_status(
        &self,
        profile_id: Uuid,
    ) -> Result<synchrome_state::compaction::CompactionStatus, SinkError> {
        let r =
            with_session_raw!(self, |s, c| c.compaction_status(&s, profile_id)).map_err(map_rej)?;
        Ok(crate::compaction::status(r))
    }

    async fn list_snapshots(
        &self,
        profile_id: Uuid,
    ) -> Result<Vec<synchrome_state::compaction::SnapshotRow>, SinkError> {
        let st =
            with_session_raw!(self, |s, c| c.compaction_status(&s, profile_id)).map_err(map_rej)?;
        let now = synchrome_cloud::compaction::parse_ts_micros(&st.server_now).unwrap_or(i64::MAX);
        let rows = with_session_raw!(self, |s, c| c.list_snapshot_rows(&s, profile_id))
            .map_err(map_rej)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let expired = r.state == "building"
                    && r.expires_at
                        .as_deref()
                        .and_then(synchrome_cloud::compaction::parse_ts_micros)
                        .is_some_and(|e| e <= now);
                crate::compaction::snapshot_row(r, expired)
            })
            .collect())
    }

    async fn list_cursors(
        &self,
        profile_id: Uuid,
    ) -> Result<Vec<synchrome_state::compaction::CursorRow>, SinkError> {
        let st =
            with_session_raw!(self, |s, c| c.compaction_status(&s, profile_id)).map_err(map_rej)?;
        let now = synchrome_cloud::compaction::parse_ts_micros(&st.server_now).unwrap_or(0);
        let rows =
            with_session_raw!(self, |s, c| c.list_cursor_rows(&s, profile_id)).map_err(map_rej)?;
        const THIRTY_DAYS_US: i64 = 30 * 24 * 3600 * 1_000_000;
        Ok(rows
            .into_iter()
            .map(|r| synchrome_state::compaction::CursorRow {
                device_id: r.device_id.to_string(),
                applied: r
                    .applied
                    .into_iter()
                    .map(|(k, v)| (k, v.max(0) as u64))
                    .collect(),
                snapshot_protocol: r.snapshot_protocol,
                retired: r.retired_at.is_some(),
                active: synchrome_cloud::compaction::parse_ts_micros(&r.updated_at)
                    .is_some_and(|u| now - u <= THIRTY_DAYS_US),
            })
            .collect())
    }

    async fn report_cursors(
        &self,
        profile_id: Uuid,
        rep: &synchrome_state::compaction::CursorReport,
    ) -> Result<(), SinkError> {
        let me = self.state.device().map_err(SinkError::Other)?.device_id;
        let row = synchrome_cloud::compaction::CursorReportDto {
            profile_id,
            device_id: me,
            applied: rep
                .applied
                .iter()
                .map(|(k, v)| (k.clone(), *v as i64))
                .collect(),
            published_through: rep.published_through as i64,
            snapshot_generation: rep.snapshot_generation.map(|g| g as i64),
            snapshot_protocol: rep.snapshot_protocol,
        };
        with_session_raw!(self, |s, c| c.report_cursor_row(&s, &row)).map_err(map_rej)
    }

    async fn claim_snapshot(&self, profile_id: Uuid, generation: u64) -> Result<(), SinkError> {
        let me = self.state.device().map_err(SinkError::Other)?.device_id;
        with_session_raw!(self, |s, c| c.claim_snapshot(
            &s,
            profile_id,
            generation as i64,
            me
        ))
        .map_err(map_rej)
    }

    async fn put_snapshot_object(
        &self,
        path: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<(), SinkError> {
        with_session_raw!(self, |s, c| c.put_object(
            &s,
            path,
            bytes.to_vec(),
            content_type
        ))
        .map_err(map_rej)
    }

    async fn mark_ready(
        &self,
        profile_id: Uuid,
        generation: u64,
        info: &synchrome_state::compaction::ReadyInfo,
    ) -> Result<(), SinkError> {
        let (manifest_path, manifest_sha256, counts, bytes) = (
            info.manifest_path.as_str(),
            info.manifest_sha256.as_str(),
            &info.counts,
            info.bytes,
        );
        #[allow(unused_mut)]
        let mut wm: std::collections::BTreeMap<String, i64> = info
            .watermarks
            .iter()
            .map(|(k, v)| (k.clone(), *v as i64))
            .collect();
        // debug builds only: claim a watermark beyond the published chain (E2E: the database
        // must refuse it, the compactor must abort and leave the journals alone)
        #[cfg(debug_assertions)]
        if std::env::var("SYNCHROME_DEV_TAMPER_WATERMARK").is_ok_and(|v| v == "1") {
            if let Some(w) = wm.values_mut().next() {
                *w += 1_000_000;
            }
        }
        with_session_raw!(self, |s, c| c.mark_snapshot_ready(
            &s,
            profile_id,
            generation as i64,
            manifest_path,
            manifest_sha256,
            &wm,
            counts,
            bytes as i64
        ))
        .map_err(map_rej)
    }

    async fn mark_superseded(&self, profile_id: Uuid, generation: u64) -> Result<(), SinkError> {
        with_session_raw!(self, |s, c| c.mark_snapshot_superseded(
            &s,
            profile_id,
            generation as i64
        ))
        .map_err(map_rej)
    }

    async fn delete_snapshot_row(
        &self,
        profile_id: Uuid,
        generation: u64,
    ) -> Result<bool, SinkError> {
        with_session_raw!(self, |s, c| c.delete_snapshot_row(
            &s,
            profile_id,
            generation as i64
        ))
        .map_err(map_rej)
    }

    async fn list_objects(
        &self,
        prefix: &str,
    ) -> Result<Vec<synchrome_state::compaction::ObjectInfo>, SinkError> {
        let v = with_session_raw!(self, |s, c| c.list_tree_sized(&s, prefix)).map_err(map_rej)?;
        Ok(v.into_iter()
            .map(|(path, bytes)| synchrome_state::compaction::ObjectInfo { path, bytes })
            .collect())
    }

    async fn delete_objects(&self, paths: &[String]) -> Result<usize, SinkError> {
        if paths.is_empty() {
            return Ok(0);
        }
        with_session_raw!(self, |s, c| c.delete_objects(&s, paths)).map_err(map_rej)
    }

    async fn gc_events(
        &self,
        profile_id: Uuid,
        device_id: &str,
        through: u64,
    ) -> Result<u64, SinkError> {
        let d = Uuid::parse_str(device_id).map_err(|_| SinkError::Other("device id".into()))?;
        let n = with_session_raw!(self, |s, c| c.gc_journal_events(
            &s,
            profile_id,
            d,
            through as i64
        ))
        .map_err(map_rej)?;
        Ok(n.max(0) as u64)
    }
}
