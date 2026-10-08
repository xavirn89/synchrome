//! Native messaging host mode.
//!
//! Chromium launches this same executable with the calling extension's origin as the first
//! argument (`chrome-extension://<id>/`). In that mode no window is created: the process
//! speaks the framed JSON protocol on stdin/stdout, resolves the handshake with the user's
//! session (same binary => same Keychain item, no access prompt), and exits when the extension
//! disconnects or after an idle timeout. stdout carries ONLY protocol frames; diagnostics go
//! to stderr and to `<app data>/logs/native-host.log`, never with a full token, a title or a
//! URL (counts and outcomes only).
//!
//! After a `connected` / `connected_offline` hello, the connection carries bookmark capture
//! messages: each is written durably to `state.sqlite` before its ack, then the profile's
//! outbox is published (bounded by a timeout) and a `sync_status` is sent.

use crate::local_token;
use crate::{bookmarks, AppState};
use std::io::Write as _;
use std::time::Duration;
use synchrome_bridge::protocol::MAX_APPLY_PLAN_BYTES;
use synchrome_bridge::{
    decode, read_frame, resolve, write_frame, AckResult, AppToExt, AssemblyError, CloudBackend,
    ExtToApp, FrameError, HandshakeContext, HandshakeReply, HandshakeStatus, SnapshotAssembly,
    WireToken,
};
use synchrome_cloud::ReqwestRest;
use synchrome_state::{ApplyPlan, CaptureError, CaptureOutcome, ExtPlan, Source};
use synchrome_types::synchrome_extension_origins;
use uuid::Uuid;

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Upper bound for the publication attempt that follows an ack; the outbox survives a
/// timeout and the app's background publisher retries.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(20);

fn log(line: &str) {
    eprintln!("synchrome-host: {line}");
    if let Ok(dir) = local_token::app_data_dir() {
        let logs = dir.join("logs");
        let _ = std::fs::create_dir_all(&logs);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(logs.join("native-host.log"))
        {
            let _ = writeln!(f, "{} {line}", synchrome_bridge::registry::now_unix());
        }
    }
}

fn parent_pid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getppid has no preconditions.
        unsafe { libc::getppid() as u32 }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Entry point for host mode. Returns the process exit code.
pub fn run(origin: &str) -> i32 {
    if !synchrome_extension_origins().iter().any(|o| o == origin) {
        log(&format!("rejected origin {origin:?}"));
        return 2;
    }
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            log(&format!("runtime error: {e}"));
            return 5;
        }
    };
    rt.block_on(serve())
}

async fn handle_hello(
    state: &AppState,
    token: Option<&WireToken>,
) -> (HandshakeReply, Option<Source>) {
    let t0 = std::time::Instant::now();
    let registry = local_token::registry();
    let this_device = state.device().map(|d| d.device_id).unwrap_or(Uuid::nil());
    // Offline (refresh impossible because the cloud is unreachable): the stored session still
    // says WHO is signed in, which is all the offline handshake needs; the cloud lookup then
    // fails with a network error and only an already-confirmed token gets `connected_offline`.
    let session = match state.auth() {
        Ok(a) => match a.ensure_valid_session().await {
            Ok(s) => s,
            Err(synchrome_auth::AuthError::Network(_) | synchrome_auth::AuthError::Server(_)) => {
                a.stored_session().ok().flatten()
            }
            Err(_) => None,
        },
        Err(_) => None,
    };
    log(&format!(
        "stage session_loaded signed_in={} after {} ms",
        session.is_some(),
        t0.elapsed().as_millis()
    ));
    let ctx = HandshakeContext {
        account_id: session.as_ref().map(|s| s.user_id.clone()),
        this_device,
    };
    let reply = match (session.as_ref(), state.cloud()) {
        (Some(s), Ok(cloud)) => {
            let backend = CloudBackend { cloud, session: s };
            resolve(token, &ctx, Some(&backend), &registry).await
        }
        _ => resolve::<CloudBackend<'_, ReqwestRest>>(token, &ctx, None, &registry).await,
    };
    // Capture is allowed only for a confirmed mapping of this account on this device.
    let source = match (reply.status, token, ctx.account_id) {
        (HandshakeStatus::Connected | HandshakeStatus::ConnectedOffline, Some(t), Some(acct)) => {
            registry
                .find_by_token(t.expose())
                .filter(|r| r.account_id == acct)
                .map(|r| Source {
                    mapping_id: r.mapping_id,
                    account_id: acct,
                    device_id: this_device,
                    profile_id: r.profile_id,
                    identity: format!(
                        "{}|{}|{}",
                        serde_json::to_value(r.browser_kind)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default(),
                        r.user_data_dir,
                        r.profile_dir
                    ),
                })
        }
        _ => None,
    };
    (reply, source)
}

/// Per-connection state.
struct Conn {
    source: Option<Source>,
    assembly: SnapshotAssembly,
    /// The hello resolved as `connected_offline`: capture only, no download/apply.
    offline: bool,
    /// The apply plan last sent to the extension (results are matched against it).
    plan: Option<ApplyPlan>,
    /// Extensions snapshot parts being assembled: (snapshot id, epoch, covers, parts, items, received).
    ext_parts: Option<ExtParts>,
    /// The extensions apply plan last sent.
    ext_plan: Option<ExtPlan>,
}

struct ExtParts {
    snapshot_id: String,
    epoch: String,
    covers_through: u64,
    parts: u32,
    received: std::collections::BTreeSet<u32>,
    items: Vec<synchrome_sync::extensions::ExtInfo>,
}

/// Total extensions per assembled snapshot.
const MAX_EXT_TOTAL: usize = 5000;

fn reject(code: &'static str, message: &str, batch_id: String) -> Vec<AppToExt> {
    vec![
        AppToExt::Error {
            code,
            message: message.to_string(),
        },
        AppToExt::BookmarksAck {
            batch_id,
            result: AckResult::Rejected,
            ops: 0,
            pending: 0,
        },
    ]
}

fn capture_error(e: CaptureError, batch_id: String) -> Vec<AppToExt> {
    log(&format!("bookmarks capture refused: {e}"));
    match e {
        CaptureError::SourceConflict => reject("source_conflict", &e.to_string(), batch_id),
        CaptureError::OtherAccount => reject("other_account", &e.to_string(), batch_id),
        CaptureError::Clock(_) => reject("clock_skew", &e.to_string(), batch_id),
        CaptureError::State(_) => reject("local_state", "local state unavailable", batch_id),
    }
}

/// Publish this source's profile (bounded), then report its status.
/// Pull, then publish (never the other way round: a local op must still be "pending" when a
/// remote op on the same entity arrives, or the conflict would go unnoticed), then report.
async fn publish_and_status(state: &AppState, src: &Source) -> AppToExt {
    match tokio::time::timeout(
        PUBLISH_TIMEOUT,
        bookmarks::sync_profile(state, src.profile_id),
    )
    .await
    {
        Ok(Ok((pulled, published))) => log(&format!(
            "bookmarks pull -> {pulled:?}; publish -> {published:?}"
        )),
        Ok(Err(e)) => log(&format!("bookmarks sync error: {e}")),
        Err(_) => log("bookmarks sync timed out; will be retried"),
    }
    let status =
        bookmarks::status_for_mapping(src.mapping_id, src.profile_id).unwrap_or_else(|| {
            synchrome_state::BookmarkSyncStatus {
                state: synchrome_state::BookmarksSyncState::Error,
                bookmarks: 0,
                folders: 0,
                pending: 0,
                last_upload_at: None,
                error: Some("local state unavailable".into()),
                download: None,
            }
        });
    AppToExt::SyncStatus {
        bookmarks: status,
        extensions: bookmarks::ext_status(src.profile_id),
    }
}

fn status_of(src: &Source) -> AppToExt {
    match bookmarks::status_for_mapping(src.mapping_id, src.profile_id) {
        Some(st) => AppToExt::SyncStatus {
            bookmarks: st,
            extensions: bookmarks::ext_status(src.profile_id),
        },
        None => AppToExt::Error {
            code: "local_state",
            message: "local state unavailable".into(),
        },
    }
}

/// Pull (download + materialise), publish pending local ops, then hand the extension the
/// next apply plan (possibly empty). Offline connections get an empty plan.
async fn pull_and_plan(
    state: &AppState,
    conn: &mut Conn,
    src: &Source,
    pull_id: String,
) -> Vec<AppToExt> {
    let empty = |pull_id: String| AppToExt::BookmarksApply {
        pull_id,
        apply_id: String::new(),
        ops: vec![],
        more: false,
    };
    if conn.offline {
        return vec![empty(pull_id), status_of(src)];
    }
    match tokio::time::timeout(
        PUBLISH_TIMEOUT,
        bookmarks::sync_profile(state, src.profile_id),
    )
    .await
    {
        Ok(Ok((pulled, published))) => log(&format!(
            "bookmarks pull -> {pulled:?}; publish -> {published:?}"
        )),
        Ok(Err(e)) => log(&format!("bookmarks sync error: {e}")),
        Err(_) => log("bookmarks sync timed out; will be retried"),
    }
    let plan = match bookmarks::open_db().and_then(|mut db| {
        db.plan_apply(src.profile_id, MAX_APPLY_PLAN_BYTES)
            .map_err(|e| e.to_string())
    }) {
        Ok(p) => p,
        Err(e) => {
            log(&format!("bookmarks apply plan error: {e}"));
            return vec![empty(pull_id), status_of(src)];
        }
    };
    let mut kinds: std::collections::BTreeMap<&str, usize> = Default::default();
    for op in &plan.ops {
        *kinds.entry(apply_kind(&op.action)).or_default() += 1;
    }
    // order as short codes (cf/cb create folder/bookmark, rn, url, mv, ro, d/D delete
    // bookmark/folder): shows e.g. children-before-parent deletes without any content
    let order: Vec<&str> = plan.ops.iter().map(|o| short_kind(&o.action)).collect();
    log(&format!(
        "bookmarks apply plan ops={} {kinds:?} more={} order={}",
        plan.ops.len(),
        plan.more,
        order.join(",")
    ));
    let msg = AppToExt::BookmarksApply {
        pull_id,
        apply_id: plan.apply_id.clone(),
        ops: plan.ops.clone(),
        more: plan.more,
    };
    conn.plan = if plan.ops.is_empty() {
        None
    } else {
        Some(plan)
    };
    vec![msg, status_of(src)]
}

fn short_kind(a: &synchrome_state::ApplyAction) -> &'static str {
    use synchrome_state::ApplyAction::*;
    match a {
        CreateFolder { .. } => "cf",
        CreateBookmark { .. } => "cb",
        Rename { .. } => "rn",
        SetUrl { .. } => "url",
        Move { .. } => "mv",
        Reorder { .. } => "ro",
        Delete { folder: false, .. } => "d",
        Delete { folder: true, .. } => "D",
    }
}

fn apply_kind(a: &synchrome_state::ApplyAction) -> &'static str {
    use synchrome_state::ApplyAction::*;
    match a {
        CreateFolder { .. } => "create_folder",
        CreateBookmark { .. } => "create_bookmark",
        Rename { .. } => "rename",
        SetUrl { .. } => "set_url",
        Move { .. } => "move",
        Reorder { .. } => "reorder",
        Delete { .. } => "delete",
    }
}

fn ext_reject(code: &'static str, message: &str, batch_id: String) -> Vec<AppToExt> {
    vec![
        AppToExt::Error {
            code,
            message: message.to_string(),
        },
        AppToExt::ExtensionsAck {
            batch_id,
            result: AckResult::Rejected,
            ops: 0,
            pending: 0,
        },
    ]
}

/// Ack an extensions capture, then publish (pull first) and report the status.
async fn ext_ack_and_status(
    state: &AppState,
    src: &Source,
    batch_id: String,
    r: Result<CaptureOutcome, CaptureError>,
) -> Vec<AppToExt> {
    let (result, ops, pending) = match r {
        Ok(CaptureOutcome::Applied { ops, pending }) => (AckResult::Applied, ops, pending),
        Ok(CaptureOutcome::Duplicate { pending }) => (AckResult::Duplicate, 0, pending),
        Ok(CaptureOutcome::ResyncRequired(_)) => (AckResult::ResyncRequired, 0, 0),
        Err(e) => {
            log(&format!("extensions capture refused: {e}"));
            let code = match e {
                CaptureError::SourceConflict => "source_conflict",
                CaptureError::OtherAccount => "other_account",
                CaptureError::Clock(_) => "clock_skew",
                CaptureError::State(_) => "local_state",
            };
            return ext_reject(code, "extensions capture refused", batch_id);
        }
    };
    let mut out = vec![AppToExt::ExtensionsAck {
        batch_id,
        result,
        ops,
        pending,
    }];
    if pending > 0 {
        out.push(publish_and_status(state, src).await);
    } else {
        out.push(status_of(src));
    }
    out
}

/// Pull (download + materialise), publish, then the next extensions apply plan (possibly
/// empty) and what needs the user in the popup. Offline connections get an empty plan.
async fn ext_pull_and_plan(
    state: &AppState,
    conn: &mut Conn,
    src: &Source,
    pull_id: String,
) -> Vec<AppToExt> {
    let empty = |pull_id: String, user_actions: Vec<(String, String)>| AppToExt::ExtensionsApply {
        pull_id,
        apply_id: String::new(),
        ops: vec![],
        user_actions,
    };
    if conn.offline {
        return vec![empty(pull_id, vec![]), status_of(src)];
    }
    match tokio::time::timeout(
        PUBLISH_TIMEOUT,
        bookmarks::sync_profile(state, src.profile_id),
    )
    .await
    {
        Ok(Ok((pulled, published))) => log(&format!(
            "extensions pull -> {pulled:?}; publish -> {published:?}"
        )),
        Ok(Err(e)) => log(&format!("extensions sync error: {e}")),
        Err(_) => log("extensions sync timed out; will be retried"),
    }
    let mut db = match bookmarks::open_db() {
        Ok(db) => db,
        Err(e) => {
            log(&format!("extensions plan error: {e}"));
            return vec![empty(pull_id, vec![]), status_of(src)];
        }
    };
    let plan = match db.plan_ext_apply(src.profile_id) {
        Ok(p) => p,
        Err(e) => {
            log(&format!("extensions plan error: {e}"));
            return vec![empty(pull_id, vec![]), status_of(src)];
        }
    };
    let user_actions = db.ext_user_actions(src.profile_id).unwrap_or_default();
    drop(db);
    let order: Vec<&str> = plan
        .ops
        .iter()
        .map(|o| match &o.action {
            synchrome_state::ExtAction::SetEnabled { enabled: true, .. } => "on",
            synchrome_state::ExtAction::SetEnabled { enabled: false, .. } => "off",
            synchrome_state::ExtAction::Uninstall { .. } => "rm",
            synchrome_state::ExtAction::OpenInstall { .. } => "store",
        })
        .collect();
    log(&format!(
        "extensions apply plan ops={} order={} user_actions={}",
        plan.ops.len(),
        order.join(","),
        user_actions.len()
    ));
    let msg = AppToExt::ExtensionsApply {
        pull_id,
        apply_id: plan.apply_id.clone(),
        ops: plan.ops.clone(),
        user_actions,
    };
    conn.ext_plan = if plan.ops.is_empty() {
        None
    } else {
        Some(plan)
    };
    vec![msg, status_of(src)]
}

async fn handle_bookmarks(
    state: &AppState,
    conn: &mut Conn,
    msg: ExtToApp,
    frame_len: usize,
) -> Vec<AppToExt> {
    let batch_id = match &msg {
        ExtToApp::BookmarksSnapshot {
            snapshot_id, part, ..
        } => format!("{snapshot_id}:{part}"),
        ExtToApp::BookmarksEvents { batch_id, .. } => batch_id.clone(),
        ExtToApp::BookmarksPull { pull_id } => pull_id.clone(),
        ExtToApp::BookmarksApplied { apply_id, .. } => apply_id.clone(),
        ExtToApp::ExtensionsSnapshot {
            snapshot_id, part, ..
        } => format!("{snapshot_id}:{part}"),
        ExtToApp::ExtensionsEvents { batch_id, .. } => batch_id.clone(),
        ExtToApp::ExtensionsPull { pull_id } => pull_id.clone(),
        ExtToApp::ExtensionsApplied { apply_id, .. } => apply_id.clone(),
        _ => String::new(),
    };
    let is_ext = matches!(
        msg,
        ExtToApp::ExtensionsSnapshot { .. }
            | ExtToApp::ExtensionsEvents { .. }
            | ExtToApp::ExtensionsPull { .. }
            | ExtToApp::ExtensionsApplied { .. }
    );
    let refuse = |code: &'static str, m: &str, b: String| {
        if is_ext {
            ext_reject(code, m, b)
        } else {
            reject(code, m, b)
        }
    };
    let Some(src) = conn.source.clone() else {
        return refuse(
            "not_confirmed",
            "this extension instance has no confirmed mapping",
            batch_id,
        );
    };
    if let Err(code) = msg.validate() {
        return refuse(code, "message exceeds limits", batch_id);
    }
    let mut db = match bookmarks::open_db() {
        Ok(db) => db,
        Err(_) => return refuse("local_state", "local state unavailable", batch_id),
    };
    let now = synchrome_sync::hlc::now_ms();
    let outcome = match msg {
        ExtToApp::SyncNow => return vec![publish_and_status(state, &src).await],
        ExtToApp::BookmarksPull { pull_id } => {
            drop(db);
            return pull_and_plan(state, conn, &src, pull_id).await;
        }
        ExtToApp::BookmarksApplied { apply_id, results } => {
            let plan = match conn.plan.take() {
                Some(p) if p.apply_id == apply_id => p,
                other => {
                    conn.plan = other;
                    return reject(
                        "unknown_apply",
                        "no such apply plan on this connection",
                        batch_id,
                    );
                }
            };
            let rec = db.record_apply(src.profile_id, &plan, &results, bookmarks::now_unix());
            log(&format!(
                "bookmarks applied results={} -> {:?}",
                results.len(),
                rec.as_ref().map_err(|e| e.to_string())
            ));
            let pending = db.pending(src.profile_id).unwrap_or(0);
            drop(db);
            return vec![
                AppToExt::BookmarksAck {
                    batch_id,
                    result: if rec.is_ok() {
                        AckResult::Applied
                    } else {
                        AckResult::Rejected
                    },
                    ops: rec.map(|r| r.applied).unwrap_or(0),
                    pending,
                },
                status_of(&src),
            ];
        }
        ExtToApp::ExtensionsPull { pull_id } => {
            drop(db);
            return ext_pull_and_plan(state, conn, &src, pull_id).await;
        }
        ExtToApp::ExtensionsApplied { apply_id, results } => {
            let plan = match conn.ext_plan.take() {
                Some(p) if p.apply_id == apply_id => p,
                other => {
                    conn.ext_plan = other;
                    return ext_reject(
                        "unknown_apply",
                        "no such extensions plan on this connection",
                        batch_id,
                    );
                }
            };
            let rec = db.record_ext_apply(src.profile_id, &plan, &results, bookmarks::now_unix());
            log(&format!(
                "extensions applied results={} -> {:?}",
                results.len(),
                rec.as_ref().map_err(|e| e.to_string())
            ));
            let pending = db.pending(src.profile_id).unwrap_or(0);
            drop(db);
            return vec![
                AppToExt::ExtensionsAck {
                    batch_id,
                    result: if rec.is_ok() {
                        AckResult::Applied
                    } else {
                        AckResult::Rejected
                    },
                    ops: rec.unwrap_or(0),
                    pending,
                },
                status_of(&src),
            ];
        }
        ExtToApp::ExtensionsSnapshot {
            snapshot_id,
            epoch,
            covers_through,
            part,
            parts,
            items,
        } => {
            let same = conn.ext_parts.as_ref().is_some_and(|p| {
                p.snapshot_id == snapshot_id && p.epoch == epoch && p.parts == parts
            });
            if !same {
                conn.ext_parts = Some(ExtParts {
                    snapshot_id: snapshot_id.clone(),
                    epoch: epoch.clone(),
                    covers_through,
                    parts,
                    received: Default::default(),
                    items: vec![],
                });
            }
            let asm = conn.ext_parts.as_mut().expect("set above");
            if asm.received.insert(part) {
                asm.items.extend(items);
            }
            if asm.items.len() > MAX_EXT_TOTAL {
                conn.ext_parts = None;
                return ext_reject("too_large", "extensions snapshot exceeds limits", batch_id);
            }
            if (asm.received.len() as u32) < parts {
                return vec![AppToExt::ExtensionsAck {
                    batch_id,
                    result: AckResult::Partial,
                    ops: 0,
                    pending: db.pending(src.profile_id).unwrap_or(0),
                }];
            }
            let full = conn.ext_parts.take().expect("assembled");
            // a joining device pulls first, so its extensions meet the existing inventory
            if !conn.offline
                && !db.has_pulled(src.profile_id).unwrap_or(true)
                && db.ensure_claimed(&src).is_ok()
            {
                drop(db);
                let r = tokio::time::timeout(
                    PUBLISH_TIMEOUT,
                    bookmarks::pull_profile(state, src.profile_id),
                )
                .await;
                log(&format!("extensions bootstrap pull -> {r:?}"));
                db = match bookmarks::open_db() {
                    Ok(db) => db,
                    Err(_) => {
                        return ext_reject("local_state", "local state unavailable", batch_id)
                    }
                };
            }
            let n = full.items.len();
            let r =
                db.capture_ext_snapshot(&src, &full.epoch, full.covers_through, &full.items, now);
            log(&format!(
                "extensions snapshot parts={parts} items={n} -> {r:?}"
            ));
            drop(db);
            return ext_ack_and_status(state, &src, batch_id, r).await;
        }
        ExtToApp::ExtensionsEvents { epoch, events, .. } => {
            let evs: Vec<(u64, synchrome_sync::extensions::ExtEvent)> =
                events.into_iter().map(|e| (e.n, e.event)).collect();
            let r = db.capture_ext_events(&src, &epoch, &evs, now);
            log(&format!("extensions events n={} -> {r:?}", evs.len()));
            drop(db);
            return ext_ack_and_status(state, &src, batch_id, r).await;
        }
        ExtToApp::BookmarksSnapshot {
            snapshot_id,
            epoch,
            covers_through,
            part,
            parts,
            nodes,
        } => {
            match conn.assembly.add(
                &snapshot_id,
                &epoch,
                covers_through,
                part,
                parts,
                nodes,
                frame_len,
            ) {
                Err(AssemblyError::TooLarge) => {
                    return reject("too_large", "snapshot exceeds limits", batch_id)
                }
                Err(AssemblyError::Inconsistent) => {
                    return reject(
                        "snapshot_incomplete",
                        "inconsistent snapshot parts",
                        batch_id,
                    )
                }
                Ok(None) => {
                    return vec![AppToExt::BookmarksAck {
                        batch_id,
                        result: AckResult::Partial,
                        ops: 0,
                        pending: db.pending(src.profile_id).unwrap_or(0),
                    }]
                }
                Ok(Some(full)) => {
                    // A joining device pulls the remote state BEFORE its first snapshot, so
                    // its local nodes pair with existing entities instead of duplicating.
                    if !conn.offline
                        && !db.has_pulled(src.profile_id).unwrap_or(true)
                        && db.ensure_claimed(&src).is_ok()
                    {
                        drop(db);
                        let r = tokio::time::timeout(
                            PUBLISH_TIMEOUT,
                            bookmarks::pull_profile(state, src.profile_id),
                        )
                        .await;
                        log(&format!("bookmarks bootstrap pull -> {r:?}"));
                        db = match bookmarks::open_db() {
                            Ok(db) => db,
                            Err(_) => {
                                return reject("local_state", "local state unavailable", batch_id)
                            }
                        };
                    }
                    let n = full.nodes.len();
                    let r = db.capture_snapshot(
                        &src,
                        &full.epoch,
                        full.covers_through,
                        &full.nodes,
                        now,
                    );
                    log(&format!(
                        "bookmarks snapshot parts={parts} nodes={n} -> {r:?}"
                    ));
                    r
                }
            }
        }
        ExtToApp::BookmarksEvents { epoch, events, .. } => {
            let evs: Vec<(u64, synchrome_sync::bookmarks::BookmarkEvent)> =
                events.into_iter().map(|e| (e.n, e.event)).collect();
            let r = db.capture_events(&src, &epoch, &evs, now);
            log(&format!("bookmarks events n={} -> {r:?}", evs.len()));
            r
        }
        _ => return vec![],
    };
    drop(db);
    let (result, ops, pending) = match outcome {
        Ok(CaptureOutcome::Applied { ops, pending }) => (AckResult::Applied, ops, pending),
        Ok(CaptureOutcome::Duplicate { pending }) => (AckResult::Duplicate, 0, pending),
        Ok(CaptureOutcome::ResyncRequired(_)) => (AckResult::ResyncRequired, 0, 0),
        Err(e) => return capture_error(e, batch_id),
    };
    let mut out = vec![AppToExt::BookmarksAck {
        batch_id,
        result,
        ops,
        pending,
    }];
    if pending > 0 {
        out.push(publish_and_status(state, &src).await);
    } else if result != AckResult::ResyncRequired {
        if let Some(st) = bookmarks::status_for_mapping(src.mapping_id, src.profile_id) {
            out.push(AppToExt::SyncStatus {
                bookmarks: st,
                extensions: bookmarks::ext_status(src.profile_id),
            });
        }
    }
    out
}

async fn serve() -> i32 {
    let t0 = std::time::Instant::now();
    log(&format!(
        "stage start pid={} ppid={}",
        std::process::id(),
        parent_pid()
    ));
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let state = AppState::new();
    log(&format!(
        "stage state_ready after {} ms",
        t0.elapsed().as_millis()
    ));
    let mut conn = Conn {
        source: None,
        assembly: SnapshotAssembly::default(),
        offline: false,
        plan: None,
        ext_parts: None,
        ext_plan: None,
    };
    let mut timeout = FIRST_FRAME_TIMEOUT;
    let (mut frames_in, mut frames_out) = (0u32, 0u32);
    loop {
        let body = match read_frame(&mut stdin, timeout).await {
            Ok(b) => b,
            Err(FrameError::Eof) => {
                // one line per connection, so a test can tell connections apart
                log(&format!(
                    "stage end after {} ms frames_in={frames_in} frames_out={frames_out}",
                    t0.elapsed().as_millis()
                ));
                return 0;
            }
            Err(FrameError::Timeout) => {
                log("timeout waiting for the extension; exiting");
                return 0;
            }
            Err(e @ FrameError::TooLarge { .. }) => {
                log(&format!("rejected frame: {e}"));
                let _ = write_frame(
                    &mut stdout,
                    &AppToExt::Error {
                        code: "too_large",
                        message: e.to_string(),
                    },
                )
                .await;
                return 3; // the stream can no longer be trusted
            }
            Err(e) => {
                log(&format!("read error: {e}"));
                return 4;
            }
        };
        timeout = IDLE_TIMEOUT;
        frames_in += 1;
        let replies = match decode(&body) {
            Ok(ExtToApp::Ping) => vec![AppToExt::Pong],
            Ok(ExtToApp::Hello {
                ext_version,
                browser,
                mapping_token,
            }) => {
                let (r, source) = handle_hello(&state, mapping_token.as_ref()).await;
                log(&format!(
                    "hello ext={ext_version} browser={} token={} -> {:?}",
                    browser.as_deref().unwrap_or("?"),
                    mapping_token
                        .as_ref()
                        .map(|t| t.hint())
                        .unwrap_or_else(|| "none".into()),
                    r.status
                ));
                conn.source = source;
                conn.offline = r.status == HandshakeStatus::ConnectedOffline;
                conn.plan = None;
                conn.ext_plan = None;
                conn.ext_parts = None;
                vec![AppToExt::HelloResult(r)]
            }
            Ok(msg) => handle_bookmarks(&state, &mut conn, msg, body.len()).await,
            Err(e) => {
                log(&format!("invalid message: {e}"));
                vec![AppToExt::Error {
                    code: "invalid_message",
                    message: e.to_string(),
                }]
            }
        };
        let kinds: Vec<&str> = replies.iter().map(AppToExt::kind).collect();
        for reply in replies {
            if let Err(e) = write_frame(&mut stdout, &reply).await {
                log(&format!("write error: {e}"));
                return 4;
            }
            frames_out += 1;
        }
        log(&format!("sent {}", kinds.join(",")));
    }
}
