//! Cloud journal compaction and snapshot bootstrap (M1 slice 13, `docs/03` §3.7, migration
//! 0005). The state side, generic over a [`CompactionSink`]:
//!
//! - **report**: this device's applied cursors + snapshot protocol → `sync_cursors` (only when
//!   they changed; the server stamps activity);
//! - **compact**: thresholds → watermarks (contiguous publication chains ∩ what every active,
//!   snapshot-capable reader applied) → claim → fold *cloud* data (previous ready snapshot +
//!   journals) → upload parts + manifest → re-download and verify → mark ready (the database
//!   re-validates the watermarks; a refusal aborts and keeps every journal);
//! - **gc**: one generation behind, only when `synchrome_compaction_status` (database clock)
//!   allows it: chunk objects, then the `synchrome_gc_journal_events` RPC per source device,
//!   then a re-list; generation cleanup only when the server allows superseding;
//! - **import**: verify manifest + parts, feed synthetic ops through the normal merge, move the
//!   cursors to the watermarks (used by the puller for new / far-behind devices).
//!
//! Device-local data (unpublished ops, conflict records, pending applies, indexes) never enters
//! a snapshot: the fold reads nothing from `state.sqlite`.

use crate::publish::{JournalSink, SinkError};
use crate::pull::RemoteEvent;
use crate::{StateDb, StateError};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use synchrome_sync::snapshot::{self as sn, Fold, Manifest, Record};
use synchrome_sync::HlcClock;
use uuid::Uuid;

/// A `profile_snapshots` row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRow {
    pub generation: u64,
    pub state: String,
    pub created_by: String,
    pub manifest_path: Option<String>,
    pub manifest_sha256: Option<String>,
    pub watermarks: BTreeMap<String, u64>,
    pub bytes: u64,
    /// Server times (RFC 3339), informational for the client.
    pub claimed_at: Option<String>,
    pub expires_at: Option<String>,
    pub ready_at: Option<String>,
    /// Computed by the server sink: the claim expired (database clock).
    #[serde(default)]
    pub expired: bool,
}

/// `synchrome_compaction_status(profile)`: every time-based decision, database clock.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionStatus {
    pub current_generation: Option<u64>,
    pub previous_generation: Option<u64>,
    pub journal_gc_watermarks: Option<BTreeMap<String, u64>>,
    pub gc_blockers: u32,
    pub journal_gc_ok: bool,
    pub generation_cleanup_ok: bool,
    pub claim_allowed: bool,
}

/// A `sync_cursors` row as the compactor needs it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorRow {
    pub device_id: String,
    pub applied: BTreeMap<String, u64>,
    pub snapshot_protocol: i32,
    pub retired: bool,
    /// Last report within the active window (30 days, database clock).
    pub active: bool,
}

/// What this device reports (`sync_cursors`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorReport {
    pub applied: BTreeMap<String, u64>,
    pub published_through: u64,
    pub snapshot_generation: Option<u64>,
    pub snapshot_protocol: i32,
}

/// What `building → ready` sends (the database re-validates the watermarks).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyInfo {
    pub manifest_path: String,
    pub manifest_sha256: String,
    pub watermarks: BTreeMap<String, u64>,
    /// collection → (entities, tombstones)
    pub counts: BTreeMap<String, (u64, u64)>,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub path: String,
    pub bytes: u64,
}

/// The cloud as the compactor sees it (migration 0005 contract). The puller only needs
/// [`JournalSink::ready_snapshots`].
pub trait CompactionSink: JournalSink {
    fn compaction_status(
        &self,
        profile_id: Uuid,
    ) -> impl Future<Output = Result<CompactionStatus, SinkError>> + Send;
    fn list_snapshots(
        &self,
        profile_id: Uuid,
    ) -> impl Future<Output = Result<Vec<SnapshotRow>, SinkError>> + Send;
    fn list_cursors(
        &self,
        profile_id: Uuid,
    ) -> impl Future<Output = Result<Vec<CursorRow>, SinkError>> + Send;
    fn report_cursors(
        &self,
        profile_id: Uuid,
        report: &CursorReport,
    ) -> impl Future<Output = Result<(), SinkError>> + Send;
    /// Insert the `building` claim; `Rejected` when another compactor won.
    fn claim_snapshot(
        &self,
        profile_id: Uuid,
        generation: u64,
    ) -> impl Future<Output = Result<(), SinkError>> + Send;
    fn put_snapshot_object(
        &self,
        path: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> impl Future<Output = Result<(), SinkError>> + Send;
    fn mark_ready(
        &self,
        profile_id: Uuid,
        generation: u64,
        info: &ReadyInfo,
    ) -> impl Future<Output = Result<(), SinkError>> + Send;
    fn mark_superseded(
        &self,
        profile_id: Uuid,
        generation: u64,
    ) -> impl Future<Output = Result<(), SinkError>> + Send;
    fn delete_snapshot_row(
        &self,
        profile_id: Uuid,
        generation: u64,
    ) -> impl Future<Output = Result<bool, SinkError>> + Send;
    fn list_objects(
        &self,
        prefix: &str,
    ) -> impl Future<Output = Result<Vec<ObjectInfo>, SinkError>> + Send;
    /// Returns how many objects were actually deleted (the server skips protected ones).
    fn delete_objects(
        &self,
        paths: &[String],
    ) -> impl Future<Output = Result<usize, SinkError>> + Send;
    fn gc_events(
        &self,
        profile_id: Uuid,
        device_id: &str,
        through: u64,
    ) -> impl Future<Output = Result<u64, SinkError>> + Send;
}

// ------------------------------------------------------------------ local bookkeeping

/// Compaction / bootstrap diagnostics of a profile on this device (no ids beyond counts).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CompactionInfo {
    pub snapshot_generation: Option<u64>,
    pub bootstrapped_at: Option<i64>,
    pub bootstrap_ms: Option<i64>,
    pub bootstrap_fallback: bool,
    pub cloud_generation: Option<u64>,
    pub last_compaction_at: Option<i64>,
    pub last_compaction_generation: Option<u64>,
    pub last_compaction_ms: Option<i64>,
    pub last_compaction_error: Option<String>,
    pub last_gc_at: Option<i64>,
    pub last_gc_events: Option<i64>,
    pub last_gc_objects: Option<i64>,
    pub gc_blockers: Option<i64>,
}

fn touch(
    db: &StateDb,
    pid: &str,
    sets: &str,
    vals: &[&dyn rusqlite::ToSql],
) -> Result<(), StateError> {
    db.conn.execute(
        "INSERT INTO compaction_state (profile_id) VALUES (?1) ON CONFLICT(profile_id) DO NOTHING",
        [pid],
    )?;
    let mut all: Vec<&dyn rusqlite::ToSql> = vec![&pid];
    all.extend_from_slice(vals);
    db.conn.execute(
        &format!("UPDATE compaction_state SET {sets} WHERE profile_id = ?1"),
        all.as_slice(),
    )?;
    Ok(())
}

impl StateDb {
    pub fn compaction_info(&self, profile_id: Uuid) -> Result<CompactionInfo, StateError> {
        Ok(self
            .conn
            .query_row(
                "SELECT snapshot_generation, bootstrapped_at, bootstrap_ms, bootstrap_fallback,
                        cloud_generation, last_compaction_at, last_compaction_generation,
                        last_compaction_ms, last_compaction_error, last_gc_at, last_gc_events,
                        last_gc_objects, gc_blockers
                 FROM compaction_state WHERE profile_id = ?1",
                [profile_id.to_string()],
                |r| {
                    Ok(CompactionInfo {
                        snapshot_generation: r.get::<_, Option<i64>>(0)?.map(|v| v as u64),
                        bootstrapped_at: r.get(1)?,
                        bootstrap_ms: r.get(2)?,
                        bootstrap_fallback: r.get::<_, i64>(3)? != 0,
                        cloud_generation: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                        last_compaction_at: r.get(5)?,
                        last_compaction_generation: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                        last_compaction_ms: r.get(7)?,
                        last_compaction_error: r.get(8)?,
                        last_gc_at: r.get(9)?,
                        last_gc_events: r.get(10)?,
                        last_gc_objects: r.get(11)?,
                        gc_blockers: r.get(12)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    /// When the background loop last evaluated compaction for this profile (unix seconds).
    pub fn compaction_last_check(&self, profile_id: Uuid) -> Result<Option<i64>, StateError> {
        Ok(self
            .conn
            .query_row(
                "SELECT last_status_at FROM compaction_state WHERE profile_id = ?1",
                [profile_id.to_string()],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    pub fn compaction_checked(&self, profile_id: Uuid, now: i64) -> Result<(), StateError> {
        touch(
            self,
            &profile_id.to_string(),
            "last_status_at = ?2",
            &[&now],
        )
    }

    /// Own ops not yet published (compaction waits until they are in the cloud).
    pub fn unpublished_ops(&self, profile_id: Uuid) -> Result<u64, StateError> {
        let pid = profile_id.to_string();
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM ops WHERE profile_id = ?1
               AND seq > (SELECT published_through FROM profiles WHERE profile_id = ?1)",
            [&pid],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    /// This device's cursor report for a profile (remote cursors + own published seq).
    pub fn cursor_report(&self, profile_id: Uuid) -> Result<CursorReport, StateError> {
        let pid = profile_id.to_string();
        let mut st = self.conn.prepare(
            "SELECT device_id, applied_through FROM remote_cursors WHERE profile_id = ?1",
        )?;
        let applied: BTreeMap<String, u64> = st
            .query_map([&pid], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })?
            .collect::<Result<_, _>>()?;
        let published: i64 = self
            .conn
            .query_row(
                "SELECT published_through FROM profiles WHERE profile_id = ?1",
                [&pid],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(CursorReport {
            applied,
            published_through: published.max(0) as u64,
            snapshot_generation: self.compaction_info(profile_id)?.snapshot_generation,
            snapshot_protocol: sn::PROTOCOL,
        })
    }
}

/// Send this device's cursors when they changed since the last report. Returns whether a
/// report was sent.
pub async fn report_cursors<S: CompactionSink>(
    db: &mut StateDb,
    sink: &S,
    profile_id: Uuid,
) -> Result<bool, SinkError> {
    let pid = profile_id.to_string();
    if crate::load_profile(&db.conn, &pid)
        .map_err(|e| SinkError::Other(e.to_string()))?
        .is_none()
    {
        return Ok(false);
    }
    let rep = db
        .cursor_report(profile_id)
        .map_err(|e| SinkError::Other(e.to_string()))?;
    let text = serde_json::to_string(&rep).unwrap_or_default();
    let last: Option<String> = db
        .conn
        .query_row(
            "SELECT last_report FROM compaction_state WHERE profile_id = ?1",
            [&pid],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| SinkError::Other(e.to_string()))?
        .flatten();
    if last.as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    sink.report_cursors(profile_id, &rep).await?;
    touch(db, &pid, "last_report = ?2", &[&text]).map_err(|e| SinkError::Other(e.to_string()))?;
    Ok(true)
}

// ------------------------------------------------------------------ watermarks / thresholds

/// Thresholds (`docs/03` §3.7); debug builds may lower them for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub min_ops: u64,
    pub min_chunks: u64,
    pub min_bytes: u64,
    /// Ignore the thresholds and the client-side 6 h rule (debug harness only; every rule the
    /// database enforces still applies).
    pub force: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            min_ops: 5000,
            min_chunks: 300,
            min_bytes: 16 * 1024 * 1024,
            force: false,
        }
    }
}

fn group(events: &[RemoteEvent]) -> BTreeMap<String, Vec<RemoteEvent>> {
    let mut m: BTreeMap<String, Vec<RemoteEvent>> = BTreeMap::new();
    for e in events {
        m.entry(e.device_id.clone()).or_default().push(e.clone());
    }
    for v in m.values_mut() {
        v.sort_by_key(|e| (e.seq_to, e.seq_from));
        v.dedup_by_key(|e| e.seq_to);
    }
    m
}

/// End of the contiguous publication chain of a device starting after `lo` (= `lo` if the
/// next publication is missing).
pub fn chain_end(events: &[RemoteEvent], lo: u64) -> u64 {
    let mut end = lo;
    for e in events.iter().filter(|e| e.seq_to > lo) {
        if e.seq_from != end + 1 {
            break;
        }
        end = e.seq_to;
    }
    end
}

/// Watermarks of a new generation: per device, the contiguous chain end limited by what every
/// active, snapshot-capable reader applied, never below the current generation's watermark.
pub fn watermarks(
    events: &[RemoteEvent],
    current: &BTreeMap<String, u64>,
    cursors: &[CursorRow],
) -> BTreeMap<String, u64> {
    let by_dev = group(events);
    let mut devices: Vec<String> = by_dev.keys().cloned().collect();
    devices.extend(current.keys().cloned());
    devices.sort();
    devices.dedup();
    let readers: Vec<&CursorRow> = cursors
        .iter()
        .filter(|c| c.active && !c.retired && c.snapshot_protocol >= sn::PROTOCOL)
        .collect();
    let mut out = BTreeMap::new();
    for d in devices {
        let lo = current.get(&d).copied().unwrap_or(0);
        let p = chain_end(by_dev.get(&d).map(Vec::as_slice).unwrap_or(&[]), lo);
        let a = readers
            .iter()
            .filter(|r| r.device_id != d)
            .map(|r| r.applied.get(&d).copied().unwrap_or(0))
            .min();
        let w = a.map_or(p, |a| p.min(a)).max(lo);
        if w > 0 || current.contains_key(&d) {
            out.insert(d, w);
        }
    }
    out
}

/// History above the current generation (what a new generation would fold).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Pressure {
    pub ops: u64,
    pub chunks: u64,
    pub bytes: u64,
}

pub fn pressure(
    events: &[RemoteEvent],
    current: &BTreeMap<String, u64>,
    sizes: &BTreeMap<String, u64>,
) -> Pressure {
    let mut p = Pressure::default();
    for e in events {
        if e.seq_to > current.get(&e.device_id).copied().unwrap_or(0) {
            p.ops += e.seq_to - e.seq_from + 1;
            p.chunks += 1;
            p.bytes += sizes.get(&e.path).copied().unwrap_or(0);
        }
    }
    p
}

// ------------------------------------------------------------------ snapshot download

/// Download and verify a ready snapshot: manifest hash against the row, every part against the
/// manifest. Returns the manifest and its records per collection.
pub async fn fetch_snapshot<S: JournalSink>(
    sink: &S,
    row: &SnapshotRow,
) -> Result<(Manifest, BTreeMap<String, Vec<Record>>), String> {
    let (Some(path), Some(sha)) = (&row.manifest_path, &row.manifest_sha256) else {
        return Err("snapshot row incomplete".into());
    };
    let bytes = sink
        .get_chunk(path)
        .await
        .map_err(|e| format!("manifest download: {}", e.kind()))?
        .ok_or("manifest missing")?;
    if sn::sha256_hex(&bytes) != *sha {
        return Err("manifest does not match its hash".into());
    }
    let m = Manifest::parse(&bytes).map_err(|e| e.to_string())?;
    if m.generation != row.generation || m.watermarks != row.watermarks {
        return Err("manifest does not match its row".into());
    }
    let dir = path
        .rsplit_once('/')
        .map(|(d, _)| d.to_string())
        .unwrap_or_default();
    let mut out: BTreeMap<String, Vec<Record>> = BTreeMap::new();
    for (col, info) in m.part_names() {
        let b = sink
            .get_chunk(&format!("{dir}/{}", info.name))
            .await
            .map_err(|e| format!("part download: {}", e.kind()))?
            .ok_or("part missing")?;
        let recs = sn::read_part(&info, &b).map_err(|e| e.to_string())?;
        out.entry(col).or_default().extend(recs);
    }
    Ok((m, out))
}

// ------------------------------------------------------------------ compaction

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum CompactionOutcome {
    /// Below every threshold, or nothing new to fold.
    NotNeeded { pressure: Pressure },
    /// The server's policy says not now (6 h rule / a live claim).
    NotAllowed { reason: String },
    /// Another compactor holds the claim.
    Busy,
    /// Built, verified and marked ready.
    Ready {
        generation: u64,
        entities: u64,
        tombstones: u64,
        bytes: u64,
        parts: usize,
        build_ms: u64,
    },
    /// The database refused the generation (e.g. a watermark): journals untouched.
    Rejected { reason: String },
    /// Building failed before ready (journals untouched; the claim expires).
    Failed { reason: String },
}

/// Build and publish a new snapshot generation of a profile when the policy says so.
pub async fn compact<S: CompactionSink>(
    db: &mut StateDb,
    sink: &S,
    profile_id: Uuid,
    device_id: &str,
    policy: Policy,
    now: i64,
) -> Result<CompactionOutcome, SinkError> {
    let t0 = std::time::Instant::now();
    let status = sink.compaction_status(profile_id).await?;
    let mut rows = sink.list_snapshots(profile_id).await?;
    rows.sort_by_key(|r| r.generation);
    // claims that expired (database clock) are removed, objects first
    for r in rows.iter().filter(|r| r.state == "building" && r.expired) {
        let objs = sink
            .list_objects(&sn::generation_dir(&profile_id.to_string(), r.generation))
            .await?;
        sink.delete_objects(&objs.into_iter().map(|o| o.path).collect::<Vec<_>>())
            .await?;
        sink.delete_snapshot_row(profile_id, r.generation).await?;
    }
    rows.retain(|r| !(r.state == "building" && r.expired));
    if rows.iter().any(|r| r.state == "building") {
        return Ok(CompactionOutcome::Busy);
    }
    let current = rows
        .iter()
        .filter(|r| r.state == "ready")
        .max_by_key(|r| r.generation)
        .cloned();
    let lo = current
        .as_ref()
        .map(|c| c.watermarks.clone())
        .unwrap_or_default();
    let events = sink.list_events(profile_id).await?;
    let sizes: BTreeMap<String, u64> = sink
        .list_objects(&format!("profiles/{profile_id}/journal"))
        .await?
        .into_iter()
        .map(|o| (o.path, o.bytes))
        .collect();
    let pr = pressure(&events, &lo, &sizes);
    let cursors = sink.list_cursors(profile_id).await?;
    let wm = watermarks(&events, &lo, &cursors);
    if wm == lo {
        return Ok(CompactionOutcome::NotNeeded { pressure: pr });
    }
    if !policy.force {
        if pr.ops < policy.min_ops && pr.chunks < policy.min_chunks && pr.bytes < policy.min_bytes {
            return Ok(CompactionOutcome::NotNeeded { pressure: pr });
        }
        if !status.claim_allowed {
            return Ok(CompactionOutcome::NotAllowed {
                reason: "claim_interval".into(),
            });
        }
    }
    let generation = rows.iter().map(|r| r.generation).max().unwrap_or(0) + 1;
    match sink.claim_snapshot(profile_id, generation).await {
        Ok(()) => {}
        Err(SinkError::Rejected(_)) => return Ok(CompactionOutcome::Busy),
        Err(e) => return Err(e),
    }
    let pid = profile_id.to_string();
    let fail = |db: &StateDb, reason: String| -> Result<CompactionOutcome, SinkError> {
        let _ = touch(db, &pid, "last_compaction_error = ?2", &[&reason]);
        Ok(CompactionOutcome::Failed { reason })
    };
    // fold: the current ready snapshot + every published op above it, up to the watermarks
    let mut fold = Fold::new();
    if let Some(c) = &current {
        match fetch_snapshot(sink, c).await {
            Ok((_, cols)) => {
                for (col, recs) in cols {
                    if let Err(e) = fold.load(&col, recs) {
                        return fail(db, e.to_string());
                    }
                }
            }
            Err(e) => return fail(db, format!("base snapshot unusable: {e}")),
        }
    }
    for e in &events {
        let low = lo.get(&e.device_id).copied().unwrap_or(0);
        let high = wm.get(&e.device_id).copied().unwrap_or(0);
        if e.seq_to <= low || e.seq_to > high {
            continue;
        }
        let bytes = match sink.get_chunk(&e.path).await? {
            Some(b) => b,
            None => return fail(db, "a journal chunk below the watermark is missing".into()),
        };
        let ops = match crate::pull::verify_chunk(&bytes, e) {
            Ok(o) => o,
            Err(why) => return fail(db, format!("journal rejected: {why}")),
        };
        for op in &ops {
            if let Err(err) = fold.apply(op) {
                return fail(db, err.to_string());
            }
        }
    }
    let created = format!("{now}");
    let built = match sn::build(
        fold,
        &pid,
        generation,
        current.as_ref().map(|c| c.generation),
        &created,
        device_id,
        wm.clone(),
    ) {
        Ok(b) => b,
        Err(e) => return fail(db, e.to_string()),
    };
    let dir = sn::generation_dir(&pid, generation);
    for (name, bytes) in &built.parts {
        sink.put_snapshot_object(&format!("{dir}/{name}"), bytes, "application/x-ndjson")
            .await?;
    }
    let manifest_path = format!("{dir}/manifest.json");
    sink.put_snapshot_object(&manifest_path, &built.manifest_bytes, "application/json")
        .await?;
    // verify what the cloud holds before declaring it ready
    let check = SnapshotRow {
        generation,
        manifest_path: Some(manifest_path.clone()),
        manifest_sha256: Some(built.manifest_sha256.clone()),
        watermarks: wm.clone(),
        ..Default::default()
    };
    if let Err(e) = fetch_snapshot(sink, &check).await {
        return fail(db, format!("uploaded snapshot does not verify: {e}"));
    }
    let counts: BTreeMap<String, (u64, u64)> = built
        .manifest
        .collections
        .iter()
        .map(|(c, i)| (c.clone(), (i.entities, i.tombstones)))
        .collect();
    let total = built.manifest.total_bytes + built.manifest_bytes.len() as u64;
    let info = ReadyInfo {
        manifest_path: manifest_path.clone(),
        manifest_sha256: built.manifest_sha256.clone(),
        watermarks: wm.clone(),
        counts: counts.clone(),
        bytes: total,
    };
    match sink.mark_ready(profile_id, generation, &info).await {
        Ok(()) => {}
        Err(SinkError::Rejected(reason)) => {
            let _ = touch(db, &pid, "last_compaction_error = ?2", &[&reason]);
            return Ok(CompactionOutcome::Rejected { reason });
        }
        Err(e) => return Err(e),
    }
    let ms = t0.elapsed().as_millis() as i64;
    let g = generation as i64;
    let _ = touch(
        db,
        &pid,
        "last_compaction_at = ?2, last_compaction_generation = ?3, last_compaction_ms = ?4,
         last_compaction_error = NULL, cloud_generation = ?3",
        &[&now, &g, &ms],
    );
    Ok(CompactionOutcome::Ready {
        generation,
        entities: counts.values().map(|c| c.0).sum(),
        tombstones: counts.values().map(|c| c.1).sum(),
        bytes: total,
        parts: built.parts.len(),
        build_ms: ms as u64,
    })
}

// ------------------------------------------------------------------ GC + generation cleanup

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GcReport {
    pub allowed: bool,
    pub blockers: u32,
    pub objects_deleted: usize,
    pub events_deleted: u64,
    pub generations_removed: Vec<u64>,
    /// Events still listed at or below the GC watermarks after the run (expected 0).
    pub leftover_events: usize,
}

/// Journal GC one generation behind + cleanup of generations older than the fallback.
pub async fn gc<S: CompactionSink>(
    db: &mut StateDb,
    sink: &S,
    profile_id: Uuid,
    now: i64,
) -> Result<GcReport, SinkError> {
    let pid = profile_id.to_string();
    let status = sink.compaction_status(profile_id).await?;
    let mut rep = GcReport {
        blockers: status.gc_blockers,
        ..Default::default()
    };
    let blockers = status.gc_blockers as i64;
    let _ = touch(
        db,
        &pid,
        "gc_blockers = ?2, cloud_generation = ?3, last_status_at = ?4",
        &[
            &blockers,
            &status.current_generation.map(|g| g as i64),
            &now,
        ],
    );
    if status.journal_gc_ok {
        if let Some(w) = &status.journal_gc_watermarks {
            rep.allowed = true;
            let events = sink.list_events(profile_id).await?;
            let by_dev = group(&events);
            // 1. chunk objects of eligible publications (Storage applies the same rule)
            let mut paths: Vec<String> = Vec::new();
            for (d, wd) in w {
                for e in by_dev
                    .get(d)
                    .into_iter()
                    .flatten()
                    .filter(|e| e.seq_to <= *wd)
                {
                    paths.push(e.path.clone());
                }
            }
            // orphan chunks left by an interrupted run (no event row, first seq ≤ watermark)
            let listed: std::collections::BTreeSet<&str> =
                events.iter().map(|e| e.path.as_str()).collect();
            for o in sink
                .list_objects(&format!("profiles/{profile_id}/journal"))
                .await?
            {
                let mut seg = o.path.split('/');
                let dev = seg.nth(3).unwrap_or_default().to_string();
                let first = o
                    .path
                    .rsplit('/')
                    .next()
                    .and_then(|f| f.strip_suffix(".jsonl"))
                    .and_then(|f| f.parse::<u64>().ok());
                if let (Some(wd), Some(first)) = (w.get(&dev), first) {
                    if first <= *wd && !listed.contains(o.path.as_str()) {
                        paths.push(o.path);
                    }
                }
            }
            paths.sort();
            paths.dedup();
            rep.objects_deleted = sink.delete_objects(&paths).await?;
            // 2. the publication rows, per source device, through the RPC
            for (d, wd) in w {
                if by_dev
                    .get(d)
                    .is_some_and(|v| v.iter().any(|e| e.seq_to <= *wd))
                {
                    rep.events_deleted += sink.gc_events(profile_id, d, *wd).await?;
                }
            }
            // 3. verify: nothing at or below the GC watermarks is still listed
            let after = sink.list_events(profile_id).await?;
            rep.leftover_events = after
                .iter()
                .filter(|e| w.get(&e.device_id).is_some_and(|wd| e.seq_to <= *wd))
                .count();
            let ev = rep.events_deleted as i64;
            let ob = rep.objects_deleted as i64;
            let _ = touch(
                db,
                &pid,
                "last_gc_at = ?2, last_gc_events = ?3, last_gc_objects = ?4",
                &[&now, &ev, &ob],
            );
        }
    }
    // generations older than the fallback, once the server says the current one is stable
    if status.generation_cleanup_ok {
        if let Some(cur) = status.current_generation {
            let rows = sink.list_snapshots(profile_id).await?;
            for r in rows.iter().filter(|r| r.generation + 2 <= cur) {
                if r.state == "ready" {
                    match sink.mark_superseded(profile_id, r.generation).await {
                        Ok(()) => {}
                        Err(SinkError::Rejected(_)) => continue, // the server says: not yet
                        Err(e) => return Err(e),
                    }
                }
                if r.state == "ready" || r.state == "superseded" {
                    let objs = sink
                        .list_objects(&sn::generation_dir(&pid, r.generation))
                        .await?;
                    sink.delete_objects(&objs.into_iter().map(|o| o.path).collect::<Vec<_>>())
                        .await?;
                    if sink.delete_snapshot_row(profile_id, r.generation).await? {
                        rep.generations_removed.push(r.generation);
                    }
                }
            }
        }
    }
    Ok(rep)
}

// ------------------------------------------------------------------ import (bootstrap)

/// Outcome of a snapshot import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub generation: u64,
    pub fallback: bool,
    pub ops: usize,
    pub conflicts: usize,
}

/// Import a verified snapshot through the normal merge and move the cursors to its watermarks.
/// Tries the current generation, then the previous one; nothing changes if neither verifies.
pub(crate) async fn import<S: JournalSink>(
    db: &mut StateDb,
    sink: &S,
    profile_id: Uuid,
    own_device: &str,
    snaps: &[SnapshotRow],
    now: i64,
) -> Result<Result<Imported, String>, StateError> {
    let t0 = std::time::Instant::now();
    let mut last_err = String::from("no ready snapshot");
    for (i, row) in snaps.iter().take(2).enumerate() {
        let (m, cols) = match fetch_snapshot(sink, row).await {
            Ok(x) => x,
            Err(e) => {
                last_err = e;
                continue;
            }
        };
        let pid = profile_id.to_string();
        let mut done = Imported {
            generation: m.generation,
            fallback: i > 0,
            ops: 0,
            conflicts: 0,
        };
        for (col, recs) in &cols {
            let ops = sn::collection_ops(col, recs);
            for batch in ops.chunks(500) {
                let tx = db
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                let published: i64 = tx.query_row(
                    "SELECT published_through FROM profiles WHERE profile_id = ?1",
                    [&pid],
                    |r| r.get(0),
                )?;
                let last: Option<String> = tx
                    .query_row("SELECT value FROM meta WHERE key = 'hlc_last'", [], |r| {
                        r.get(0)
                    })
                    .optional()?;
                let mut clock =
                    HlcClock::new(own_device.to_string(), last.and_then(|s| s.parse().ok()));
                for op in batch {
                    if crate::pull::apply_remote_op(&tx, &pid, published, &op.by, op, now)? {
                        done.conflicts += 1;
                    }
                    if let Ok(h) = op.hlc.parse() {
                        clock.observe(&h);
                    }
                    done.ops += 1;
                }
                if let Some(l) = &clock.last {
                    tx.execute(
                        "INSERT INTO meta (key, value) VALUES ('hlc_last', ?1)
                         ON CONFLICT(key) DO UPDATE SET value = MAX(value, excluded.value)",
                        [l.to_string()],
                    )?;
                }
                tx.commit()?;
            }
        }
        let tx = db
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (d, w) in &m.watermarks {
            if d == own_device {
                continue;
            }
            tx.execute(
                "INSERT INTO remote_cursors (profile_id, device_id, applied_through, gap_at, updated_at)
                 VALUES (?1, ?2, ?3, NULL, ?4)
                 ON CONFLICT(profile_id, device_id) DO UPDATE SET
                    applied_through = MAX(applied_through, excluded.applied_through),
                    gap_at = NULL, updated_at = excluded.updated_at",
                params![pid, d, *w as i64, now],
            )?;
        }
        tx.execute(
            "INSERT INTO compaction_state (profile_id) VALUES (?1) ON CONFLICT(profile_id) DO NOTHING",
            [&pid],
        )?;
        tx.execute(
            "UPDATE compaction_state SET snapshot_generation = ?2, bootstrapped_at = ?3,
                    bootstrap_ms = ?4, bootstrap_fallback = ?5, cloud_generation = ?6
             WHERE profile_id = ?1",
            params![
                pid,
                m.generation as i64,
                now,
                t0.elapsed().as_millis() as i64,
                (i > 0) as i64,
                snaps.first().map(|s| s.generation as i64)
            ],
        )?;
        tx.commit()?;
        return Ok(Ok(done));
    }
    Ok(Err(last_err))
}

// ------------------------------------------------------------------ shared-state digest

impl StateDb {
    /// The SHARED state of a profile (published columns only, every collection), one canonical
    /// line per entity. Two devices that converged have equal outputs; device-local columns
    /// (pending applies, conflicts, observed values, indexes) are excluded.
    pub fn shared_state(
        &self,
        profile_id: Uuid,
    ) -> Result<BTreeMap<String, Vec<String>>, StateError> {
        let pid = profile_id.to_string();
        let mut out = BTreeMap::new();
        let q: [(&str, &str); 4] = [
            (
                "bookmarks",
                "SELECT sid, kind, root, parent, pos, title, IFNULL(url, ''), deleted, field_hlc,
                        IFNULL(deleted_hlc, '') FROM entities WHERE profile_id = ?1 ORDER BY sid",
            ),
            (
                "extensions",
                "SELECT ext_id, name, version, source, enabled, deleted, field_hlc,
                        IFNULL(deleted_hlc, '') FROM ext_entities WHERE profile_id = ?1 ORDER BY ext_id",
            ),
            (
                "extension_settings",
                "SELECT ext_id || '/' || area || '/' || key, IFNULL(value, ''), deleted, field_hlc,
                        IFNULL(deleted_hlc, '') FROM ext_settings_entities WHERE profile_id = ?1
                 ORDER BY ext_id, area, key",
            ),
            (
                "browser_preferences",
                "SELECT path, IFNULL(value, ''), deleted, field_hlc, IFNULL(deleted_hlc, '')
                 FROM browser_preferences_entities WHERE profile_id = ?1 ORDER BY path",
            ),
        ];
        for (col, sql) in q {
            let mut st = self.conn.prepare(sql)?;
            let n = st.column_count();
            let rows: Vec<String> = st
                .query_map([&pid], |r| {
                    let mut parts = Vec::with_capacity(n);
                    for i in 0..n {
                        let v: rusqlite::types::Value = r.get(i)?;
                        parts.push(match v {
                            rusqlite::types::Value::Null => String::new(),
                            rusqlite::types::Value::Integer(i) => i.to_string(),
                            rusqlite::types::Value::Real(f) => f.to_string(),
                            rusqlite::types::Value::Text(t) => t,
                            rusqlite::types::Value::Blob(_) => "<blob>".into(),
                        });
                    }
                    Ok(parts.join("\u{1f}"))
                })?
                .collect::<Result<_, _>>()?;
            out.insert(col.to_string(), rows);
        }
        Ok(out)
    }

    /// SHA-256 of [`StateDb::shared_state`] per collection (diagnostics / E2E convergence).
    pub fn shared_state_digest(
        &self,
        profile_id: Uuid,
    ) -> Result<BTreeMap<String, (usize, String)>, StateError> {
        Ok(self
            .shared_state(profile_id)?
            .into_iter()
            .map(|(c, rows)| {
                let n = rows.len();
                (
                    c,
                    (
                        n,
                        sn::sha256_hex(rows.join("\n").as_bytes())[..16].to_string(),
                    ),
                )
            })
            .collect())
    }
}
