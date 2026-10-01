//! Native messaging host mode.
//!
//! Chromium launches this same executable with the calling extension's origin as the first
//! argument (`chrome-extension://<id>/`). In that mode no window is created: the process
//! speaks the framed JSON protocol on stdin/stdout, resolves the handshake with the user's
//! session (same binary => same Keychain item, no access prompt), and exits when the extension
//! disconnects or after an idle timeout. stdout carries ONLY protocol frames; diagnostics go
//! to stderr and to `<app data>/logs/native-host.log`, never with a full token.

use crate::local_token;
use crate::AppState;
use std::io::Write as _;
use std::time::Duration;
use synchrome_bridge::{
    decode, read_frame, resolve, write_frame, AppToExt, CloudBackend, ExtToApp, FrameError,
    HandshakeContext, HandshakeReply, WireToken,
};
use synchrome_cloud::ReqwestRest;
use synchrome_types::synchrome_extension_origin;
use uuid::Uuid;

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

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

async fn handle_hello(state: &AppState, token: Option<&WireToken>) -> HandshakeReply {
    let t0 = std::time::Instant::now();
    let registry = local_token::registry();
    let this_device = state.device().map(|d| d.device_id).unwrap_or(Uuid::nil());
    let session = state.session().await.ok();
    log(&format!(
        "stage session_loaded signed_in={} after {} ms",
        session.is_some(),
        t0.elapsed().as_millis()
    ));
    let ctx = HandshakeContext {
        account_id: session.as_ref().map(|s| s.user_id.clone()),
        this_device,
    };
    match (session.as_ref(), state.cloud()) {
        (Some(s), Ok(cloud)) => {
            let backend = CloudBackend { cloud, session: s };
            resolve(token, &ctx, Some(&backend), &registry).await
        }
        _ => resolve::<CloudBackend<'_, ReqwestRest>>(token, &ctx, None, &registry).await,
    }
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
    let mut timeout = FIRST_FRAME_TIMEOUT;
    loop {
        let body = match read_frame(&mut stdin, timeout).await {
            Ok(b) => b,
            Err(FrameError::Eof) => return 0,
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
        let reply = match decode(&body) {
            Ok(ExtToApp::Ping) => AppToExt::Pong,
            Ok(ExtToApp::Hello {
                ext_version,
                browser,
                mapping_token,
            }) => {
                let r = handle_hello(&state, mapping_token.as_ref()).await;
                log(&format!(
                    "hello ext={ext_version} browser={} token={} -> {:?}",
                    browser.as_deref().unwrap_or("?"),
                    mapping_token
                        .as_ref()
                        .map(|t| t.hint())
                        .unwrap_or_else(|| "none".into()),
                    r.status
                ));
                AppToExt::HelloResult(r)
            }
            Err(e) => {
                log(&format!("invalid message: {e}"));
                AppToExt::Error {
                    code: "invalid_message",
                    message: e.to_string(),
                }
            }
        };
        if let Err(e) = write_frame(&mut stdout, &reply).await {
            log(&format!("write error: {e}"));
            return 4;
        }
    }
}
