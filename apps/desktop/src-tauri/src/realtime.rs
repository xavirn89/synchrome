//! Realtime wake-up supervisor (M1 slice 13, `docs/03` §3.7 A). The app process keeps at most
//! ONE Supabase Realtime connection for the signed-in account, subscribed to store_events
//! INSERTs (RLS-filtered with the user's JWT). An event is only a hint: it marks the profile
//! dirty and starts the normal cursor-based sync earlier (coalesced by
//! [`synchrome_sync::wake::Wake`]); the payload is never applied. The 30 s loop stays the
//! fallback, and every (re)connection triggers a catch-up sync of every local profile.
//!
//! Lifecycle: exponential backoff 1–60 s with jitter; heartbeats; token refresh pushed to the
//! channel; a new account (or logout) ends the connection and drops all wake state. Nothing
//! here logs tokens, payloads or ids.

use crate::{bookmarks, AppState};
use futures_util::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use synchrome_cloud::realtime::{self as rt, Inbound};
use synchrome_sync::wake::Wake;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue, Message};
use uuid::Uuid;

/// Counters and state for diagnostics (no ids, no payloads).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RtStatus {
    pub enabled: bool,
    pub account_set: bool,
    pub connected: bool,
    pub connects: u64,
    pub reconnects: u64,
    /// store_events INSERTs received.
    pub events: u64,
    /// ... of which published by this device (ignored).
    pub own_events: u64,
    /// ... of which for profiles this device does not keep (ignored).
    pub ignored_events: u64,
    /// Hints merged into a pending/running sync (duplicate pulls avoided).
    pub coalesced: u64,
    /// Syncs started by wake-ups (including catch-ups after (re)connecting).
    pub pulls: u64,
    pub last_event_ms: Option<u64>,
    pub last_pull_started_ms: Option<u64>,
    pub last_error: Option<String>,
    /// Wake-up syncs running right now / the most ever running at once (all profiles).
    pub inflight: u64,
    pub max_inflight: u64,
}

static STATUS: OnceLock<Mutex<RtStatus>> = OnceLock::new();
static ACCOUNT: OnceLock<watch::Sender<Option<String>>> = OnceLock::new();

fn st() -> &'static Mutex<RtStatus> {
    STATUS.get_or_init(|| Mutex::new(RtStatus::default()))
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn status() -> RtStatus {
    st().lock().map(|s| s.clone()).unwrap_or_default()
}

fn update(f: impl FnOnce(&mut RtStatus)) {
    if let Ok(mut s) = st().lock() {
        f(&mut s);
        // a status file next to state.sqlite (counters only) for diagnostics / the harness
        if let Ok(dir) = crate::local_token::app_data_dir() {
            if let Ok(text) = serde_json::to_vec(&*s) {
                let tmp = dir.join("realtime-status.json.tmp");
                if std::fs::write(&tmp, text).is_ok() {
                    let _ = std::fs::rename(&tmp, dir.join("realtime-status.json"));
                }
            }
        }
    }
}

fn enabled() -> bool {
    #[cfg(debug_assertions)]
    if std::env::var("SYNCHROME_DEV_REALTIME").is_ok_and(|v| v == "off") {
        return false;
    }
    true
}

/// Debug harness only: drop every event (lost Realtime) / deliver each twice (duplicates).
fn fault() -> (bool, bool) {
    #[cfg(debug_assertions)]
    {
        let on = |k: &str| std::env::var(k).is_ok_and(|v| v == "1");
        return (
            on("SYNCHROME_DEV_REALTIME_DROP"),
            on("SYNCHROME_DEV_REALTIME_DUPLICATE"),
        );
    }
    #[allow(unreachable_code)]
    (false, false)
}

/// Debug harness only: while `<data dir>/realtime-disconnect` exists the connection is dropped
/// and not re-established (a network loss), so the 30 s polling alone must keep up.
fn dev_disconnected() -> bool {
    #[cfg(debug_assertions)]
    if let Ok(dir) = crate::local_token::app_data_dir() {
        return dir.join("realtime-disconnect").exists();
    }
    false
}

/// The signed-in account changed (sign-in, sign-out, startup).
pub fn set_account(account: Option<String>) {
    let tx = ACCOUNT.get_or_init(|| watch::channel(None).0);
    let _ = tx.send_replace(account.clone());
    update(|s| s.account_set = account.is_some());
}

/// Start the supervisor once (app process only).
pub fn start() {
    let tx = ACCOUNT.get_or_init(|| watch::channel(None).0);
    let rx = tx.subscribe();
    update(|s| s.enabled = enabled());
    if !enabled() {
        return;
    }
    tauri::async_runtime::spawn(supervise(rx));
}

enum Ended {
    AccountChanged,
    Failed(&'static str),
}

async fn supervise(mut acct: watch::Receiver<Option<String>>) {
    let state = Arc::new(AppState::new());
    let mut backoff: u64 = 1;
    loop {
        let account = acct.borrow_and_update().clone();
        let Some(account) = account else {
            update(|s| s.connected = false);
            if acct.changed().await.is_err() {
                return;
            }
            continue;
        };
        if dev_disconnected() {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                r = acct.changed() => if r.is_err() { return },
            }
            continue;
        }
        let (joined, ended) = run(&state, &account, &mut acct).await;
        update(|s| s.connected = false);
        if joined {
            backoff = 1;
        }
        match ended {
            Ended::AccountChanged => continue,
            Ended::Failed(why) => {
                update(|s| {
                    s.reconnects += 1;
                    s.last_error = Some(why.into());
                });
                let jitter = unix_ms() % (backoff * 500 + 1);
                let wait = Duration::from_millis(backoff * 1000 + jitter);
                backoff = (backoff * 2).min(60);
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    r = acct.changed() => if r.is_err() { return },
                }
            }
        }
    }
}

fn local_profiles() -> Vec<Uuid> {
    bookmarks::open_db()
        .ok()
        .and_then(|db| db.profile_ids().ok())
        .unwrap_or_default()
}

/// The sync a wake-up runs: the same steps as the background loop for one profile.
async fn wake_sync(state: Arc<AppState>, pid: Uuid) {
    let _ = bookmarks::sync_profile(&state, pid).await;
    let _ = tokio::time::timeout(
        Duration::from_secs(60),
        crate::ext_settings::settings_cycle(&state, pid),
    )
    .await;
    let _ = tokio::time::timeout(
        Duration::from_secs(60),
        crate::browser_prefs::prefs_cycle(&state, pid),
    )
    .await;
    crate::compaction::after_sync(&state, pid).await;
}

/// One connection: join, then hints → coalesced syncs until it fails or the account changes.
async fn run(
    state: &Arc<AppState>,
    account: &str,
    acct: &mut watch::Receiver<Option<String>>,
) -> (bool, Ended) {
    let (Ok(cloud), Ok(auth)) = (state.cloud(), state.auth()) else {
        return (false, Ended::Failed("not configured"));
    };
    let session = match auth.ensure_valid_session().await {
        Ok(Some(s)) if s.user_id == account => s,
        _ => return (false, Ended::Failed("no session")),
    };
    let Ok(mut req) = cloud.realtime_url().into_client_request() else {
        return (false, Ended::Failed("bad url"));
    };
    // Cloudflare refuses default library agents (E9)
    if let Ok(ua) = HeaderValue::from_str(&format!("Synchrome/{}", env!("CARGO_PKG_VERSION"))) {
        req.headers_mut().insert("User-Agent", ua);
    }
    let ws = match tokio::time::timeout(
        Duration::from_secs(15),
        tokio_tungstenite::connect_async(req),
    )
    .await
    {
        Ok(Ok((ws, _))) => ws,
        _ => return (false, Ended::Failed("connect")),
    };
    let (mut tx, mut rx) = ws.split();
    let mut r: u64 = 1;
    if tx
        .send(Message::Text(cloud.realtime_join(&session, r).into()))
        .await
        .is_err()
    {
        return (false, Ended::Failed("send"));
    }
    // wait for the join reply
    let joined = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(Ok(m)) = rx.next().await {
            if let Message::Text(t) = m {
                match rt::parse(&t) {
                    Inbound::Ok { r: Some(x) } if x == "1" => return true,
                    Inbound::Error { .. } | Inbound::Closed => return false,
                    _ => {}
                }
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    if !joined {
        return (false, Ended::Failed("join refused"));
    }
    update(|s| {
        s.connected = true;
        s.connects += 1;
        s.last_error = None;
    });
    let own = state.device().map(|d| d.device_id).ok();
    let t0 = Instant::now();
    let now = || t0.elapsed().as_millis() as u64;
    let mut wake = Wake::new();
    let mut known = local_profiles();
    wake.all_dirty(&known, now()); // catch-up: events missed while disconnected
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<Uuid>();
    let mut hb = tokio::time::interval(Duration::from_secs(25));
    let mut tok = tokio::time::interval(Duration::from_secs(240));
    hb.tick().await;
    tok.tick().await;
    let mut last_in = Instant::now();
    let (drop_all, duplicate) = fault();
    let mut probe = tokio::time::interval(Duration::from_secs(1));
    loop {
        let wait = wake
            .next_deadline()
            .map(|d| Duration::from_millis(d.saturating_sub(now())))
            .unwrap_or(Duration::from_secs(3600));
        tokio::select! {
            m = rx.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    last_in = Instant::now();
                    match rt::parse(&t) {
                        Inbound::Published { profile_id, device_id } => {
                            update(|s| { s.events += 1; s.last_event_ms = Some(unix_ms()); });
                            if Some(device_id) == own {
                                update(|s| s.own_events += 1);
                            } else {
                                if !known.contains(&profile_id) {
                                    known = local_profiles();
                                }
                                if !known.contains(&profile_id) {
                                    update(|s| s.ignored_events += 1);
                                } else if !drop_all {
                                    wake.hint(profile_id, now());
                                    if duplicate {
                                        wake.hint(profile_id, now());
                                    }
                                }
                            }
                        }
                        Inbound::Error { .. } | Inbound::Closed => return (true, Ended::Failed("channel closed")),
                        Inbound::System { ok: false } => return (true, Ended::Failed("subscription failed")),
                        _ => {}
                    }
                }
                Some(Ok(Message::Ping(p))) => {
                    last_in = Instant::now();
                    let _ = tx.send(Message::Pong(p)).await;
                }
                Some(Ok(Message::Close(_))) | None => return (true, Ended::Failed("closed")),
                Some(Err(_)) => return (true, Ended::Failed("socket error")),
                _ => {}
            },
            _ = hb.tick() => {
                if last_in.elapsed() > Duration::from_secs(75) {
                    return (true, Ended::Failed("heartbeat timeout"));
                }
                r += 1;
                if tx.send(Message::Text(rt::heartbeat(r).into())).await.is_err() {
                    return (true, Ended::Failed("send"));
                }
            },
            _ = tok.tick() => {
                if let Ok(Some(s)) = auth.ensure_valid_session().await {
                    r += 1;
                    let _ = tx.send(Message::Text(cloud.realtime_token(&s, r).into())).await;
                }
            },
            _ = tokio::time::sleep(wait) => {
                for pid in wake.due(now()) {
                    update(|s| {
                        s.pulls += 1;
                        s.last_pull_started_ms = Some(unix_ms());
                        s.inflight += 1;
                        s.max_inflight = s.max_inflight.max(s.inflight);
                    });
                    let st = state.clone();
                    let done = done_tx.clone();
                    tauri::async_runtime::spawn(async move {
                        wake_sync(st, pid).await;
                        update(|s| s.inflight = s.inflight.saturating_sub(1));
                        let _ = done.send(pid);
                    });
                }
            },
            Some(pid) = done_rx.recv() => wake.finished(pid, now()),
            _ = probe.tick() => {
                if dev_disconnected() {
                    let _ = tx.close().await;
                    return (true, Ended::Failed("disconnected (debug)"));
                }
            },
            c = acct.changed() => {
                let _ = c;
                r += 1;
                let _ = tx.send(Message::Text(rt::leave(&rt::topic(account), r).into())).await;
                let _ = tx.close().await;
                wake.reset();
                return (true, Ended::AccountChanged);
            },
        }
        let ws = wake.stats;
        update(|s| s.coalesced = ws.coalesced);
    }
}
