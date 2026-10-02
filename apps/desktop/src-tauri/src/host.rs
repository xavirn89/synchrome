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
use synchrome_bridge::{
    decode, read_frame, resolve, write_frame, AckResult, AppToExt, AssemblyError, CloudBackend,
    ExtToApp, FrameError, HandshakeContext, HandshakeReply, HandshakeStatus, SnapshotAssembly,
    WireToken,
};
use synchrome_cloud::ReqwestRest;
use synchrome_state::{CaptureError, CaptureOutcome, Source};
use synchrome_types::synchrome_extension_origin;
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
    if origin != synchrome_extension_origin() {
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
}

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
async fn publish_and_status(state: &AppState, src: &Source) -> AppToExt {
    match tokio::time::timeout(
        PUBLISH_TIMEOUT,
        bookmarks::publish_profile(state, src.profile_id),
    )
    .await
    {
        Ok(Ok(outcome)) => log(&format!("bookmarks publish -> {outcome:?}")),
        Ok(Err(e)) => log(&format!("bookmarks publish error: {e}")),
        Err(_) => log("bookmarks publish timed out; will be retried"),
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
            }
        });
    AppToExt::SyncStatus { bookmarks: status }
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
        _ => String::new(),
    };
    let Some(src) = conn.source.clone() else {
        return reject(
            "not_confirmed",
            "this extension instance has no confirmed mapping",
            batch_id,
        );
    };
    if let Err(code) = msg.validate() {
        return reject(code, "message exceeds bookmark limits", batch_id);
    }
    let mut db = match bookmarks::open_db() {
        Ok(db) => db,
        Err(_) => return reject("local_state", "local state unavailable", batch_id),
    };
    let now = synchrome_sync::hlc::now_ms();
    let outcome = match msg {
        ExtToApp::SyncNow => return vec![publish_and_status(state, &src).await],
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
            out.push(AppToExt::SyncStatus { bookmarks: st });
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
