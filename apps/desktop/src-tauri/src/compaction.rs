//! Compaction glue (M1 slice 13, `docs/03` §3.7): after every sync this device reports its
//! cursors; at most every 10 minutes per profile it asks the database whether a new snapshot
//! generation or journal GC is due, and runs them. Everything time-based or permission-based
//! is decided by the database (migration 0005); nothing here logs values or ids.

use crate::bookmarks::{self, open_db, CloudSink};
use crate::AppState;
use synchrome_state::compaction::{
    self as cp, CompactionOutcome, CompactionStatus, GcReport, Policy, SnapshotRow,
};
use uuid::Uuid;

/// How often (seconds) the background loop re-evaluates compaction for one profile.
const CHECK_EVERY_SECS: i64 = 600;

pub fn snapshot_row(r: synchrome_cloud::compaction::SnapshotRowDto, expired: bool) -> SnapshotRow {
    SnapshotRow {
        generation: r.generation.max(0) as u64,
        state: r.state,
        created_by: r.created_by.to_string(),
        manifest_path: r.manifest_path,
        manifest_sha256: r.manifest_sha256,
        watermarks: r
            .watermarks
            .into_iter()
            .map(|(k, v)| (k, v.max(0) as u64))
            .collect(),
        bytes: r.bytes.max(0) as u64,
        claimed_at: r.claimed_at,
        expires_at: r.expires_at,
        ready_at: r.ready_at,
        expired,
    }
}

pub fn status(r: synchrome_cloud::compaction::CompactionStatusRow) -> CompactionStatus {
    CompactionStatus {
        server_now: Some(r.server_now),
        current_generation: r.current_generation.map(|g| g.max(0) as u64),
        current_ready_at: r.current_ready_at,
        previous_generation: r.previous_generation.map(|g| g.max(0) as u64),
        journal_gc_watermarks: r
            .journal_gc_watermarks
            .map(|m| m.into_iter().map(|(k, v)| (k, v.max(0) as u64)).collect()),
        gc_blockers: r.gc_blockers.max(0) as u32,
        journal_gc_ok: r.journal_gc_ok,
        generation_cleanup_ok: r.generation_cleanup_ok,
        claim_allowed: r.claim_allowed,
    }
}

/// The thresholds of the spec; debug builds may lower them for tests
/// (`SYNCHROME_DEV_COMPACTION_THRESHOLDS=ops,chunks,bytes`).
pub fn policy(force: bool) -> Policy {
    #[allow(unused_mut)]
    let mut p = Policy {
        force,
        ..Policy::default()
    };
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("SYNCHROME_DEV_COMPACTION_THRESHOLDS") {
        let n: Vec<u64> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if n.len() == 3 {
            p.min_ops = n[0];
            p.min_chunks = n[1];
            p.min_bytes = n[2];
        }
    }
    p
}

fn device_id(state: &AppState) -> Result<String, String> {
    Ok(state.device()?.device_id.to_string())
}

/// After a sync of `profile_id`: report the cursors (cheap; only when they changed), and every
/// CHECK_EVERY_SECS evaluate compaction + GC. Errors are recorded, never fatal.
pub async fn after_sync(state: &AppState, profile_id: Uuid) {
    let Ok(mut db) = open_db() else { return };
    let sink = CloudSink::new(state);
    let _ = cp::report_cursors(&mut db, &sink, profile_id).await;
    #[cfg(debug_assertions)]
    if std::env::var("SYNCHROME_DEV_NO_AUTO_COMPACTION").is_ok_and(|v| v == "1") {
        return; // harness: compaction / GC only when it asks
    }
    let now = bookmarks::now_unix();
    let last_check: Option<i64> = db.compaction_last_check(profile_id).ok().flatten();
    if last_check.is_some_and(|t| now - t < CHECK_EVERY_SECS) {
        return;
    }
    let _ = db.compaction_checked(profile_id, now);
    // never fold while this device still has its own unpublished ops (they belong in the
    // cloud first)
    if db.unpublished_ops(profile_id).unwrap_or(1) > 0 {
        return;
    }
    let Ok(dev) = device_id(state) else { return };
    let _ = cp::compact(&mut db, &sink, profile_id, &dev, policy(false), now).await;
    let _ = cp::gc(&mut db, &sink, profile_id, now).await;
}

/// Compact a mapping's profile now (UI / dev harness). `force` ignores the thresholds and the
/// client-side 6 h rule in debug builds only; every database rule still applies.
pub async fn compact_now(
    state: &AppState,
    mapping_id: Uuid,
    force: bool,
) -> Result<CompactionOutcome, String> {
    let (_, pid) = bookmarks::own_profile_pub(state, mapping_id)?;
    let force = force && cfg!(debug_assertions);
    let mut db = open_db()?;
    let sink = CloudSink::new(state);
    let _ = cp::report_cursors(&mut db, &sink, pid).await;
    let dev = device_id(state)?;
    cp::compact(
        &mut db,
        &sink,
        pid,
        &dev,
        policy(force),
        bookmarks::now_unix(),
    )
    .await
    .map_err(|e| e.to_string())
}

pub async fn gc_now(state: &AppState, mapping_id: Uuid) -> Result<GcReport, String> {
    let (_, pid) = bookmarks::own_profile_pub(state, mapping_id)?;
    let mut db = open_db()?;
    let sink = CloudSink::new(state);
    cp::gc(&mut db, &sink, pid, bookmarks::now_unix())
        .await
        .map_err(|e| e.to_string())
}

/// Report this device's cursors now (also done after every sync).
pub async fn report_now(state: &AppState, mapping_id: Uuid) -> Result<bool, String> {
    let (_, pid) = bookmarks::own_profile_pub(state, mapping_id)?;
    let mut db = open_db()?;
    cp::report_cursors(&mut db, &CloudSink::new(state), pid)
        .await
        .map_err(|e| e.to_string())
}

/// Diagnostics of a mapping's profile (counts and generations only).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Diagnostics {
    pub realtime: crate::realtime::RtStatus,
    pub local: cp::CompactionInfo,
    pub cloud: Option<CompactionStatus>,
    pub gc_blocked: bool,
    pub snapshot_generations_kept: usize,
}

pub async fn diagnostics(state: &AppState, mapping_id: Uuid) -> Result<Diagnostics, String> {
    let (_, pid) = bookmarks::own_profile_pub(state, mapping_id)?;
    let db = open_db()?;
    let local = db.compaction_info(pid).map_err(|e| e.to_string())?;
    drop(db);
    let sink = CloudSink::new(state);
    use synchrome_state::compaction::CompactionSink;
    let cloud = sink.compaction_status(pid).await.ok();
    let kept = synchrome_state::JournalSink::ready_snapshots(&sink, pid)
        .await
        .map(|v| v.len())
        .unwrap_or(0);
    Ok(Diagnostics {
        realtime: crate::realtime::status(),
        gc_blocked: cloud.as_ref().is_some_and(|c| c.gc_blockers > 0),
        local,
        cloud,
        snapshot_generations_kept: kept,
    })
}
