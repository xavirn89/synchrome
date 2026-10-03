//! Migration 0005 contract (compaction metadata, guarded journal GC): `profile_snapshots`,
//! `sync_cursors`, the `synchrome_compaction_status` / `synchrome_gc_journal_events` RPCs and
//! sized Storage listings. Every call runs as the signed-in user (RLS); the database decides
//! what is allowed and when (its own clock) — nothing here relaxes a rule.

use crate::client::{CloudClient, CloudError, RestTransport};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use synchrome_auth::Session;
use uuid::Uuid;

/// `synchrome_compaction_status(profile)`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CompactionStatusRow {
    pub server_now: String,
    pub current_generation: Option<i64>,
    pub current_ready_at: Option<String>,
    pub previous_generation: Option<i64>,
    pub journal_gc_watermarks: Option<BTreeMap<String, i64>>,
    pub gc_blockers: i32,
    pub journal_gc_ok: bool,
    pub generation_cleanup_ok: bool,
    pub last_claimed_at: Option<String>,
    pub claim_allowed: bool,
}

/// A `profile_snapshots` row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct SnapshotRowDto {
    pub generation: i64,
    pub state: String,
    pub created_by: Uuid,
    pub manifest_path: Option<String>,
    pub manifest_sha256: Option<String>,
    pub watermarks: BTreeMap<String, i64>,
    pub bytes: i64,
    pub claimed_at: Option<String>,
    pub expires_at: Option<String>,
    pub ready_at: Option<String>,
}

/// A `sync_cursors` row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CursorRowDto {
    pub device_id: Uuid,
    pub applied: BTreeMap<String, i64>,
    pub published_through: i64,
    pub snapshot_generation: Option<i64>,
    pub snapshot_protocol: i32,
    pub updated_at: String,
    pub retired_at: Option<String>,
}

/// What a device reports about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CursorReportDto {
    pub profile_id: Uuid,
    pub device_id: Uuid,
    pub applied: BTreeMap<String, i64>,
    pub published_through: i64,
    pub snapshot_generation: Option<i64>,
    pub snapshot_protocol: i32,
}

/// Microseconds since the epoch of an RFC 3339 timestamp as PostgREST renders `timestamptz`
/// (`2026-10-03T10:20:30.123456+00:00`, `…Z`, any offset). No chrono dependency.
pub fn parse_ts_micros(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once(['T', ' '])?;
    let mut d = date.split('-');
    let (y, m, dd): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let (time, off_secs) = if let Some(t) = rest.strip_suffix('Z') {
        (t, 0)
    } else if let Some(i) = rest.rfind(['+', '-']) {
        let (t, o) = rest.split_at(i);
        let sign = if o.starts_with('-') { -1 } else { 1 };
        let o = &o[1..];
        let (oh, om) = o
            .split_once(':')
            .unwrap_or((&o[..o.len().min(2)], &o[o.len().min(2)..]));
        let oh: i64 = oh.parse().ok()?;
        let om: i64 = if om.is_empty() { 0 } else { om.parse().ok()? };
        (t, sign * (oh * 3600 + om * 60))
    } else {
        (rest, 0)
    };
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = hms.split(':');
    let (hh, mi, ss): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
    );
    let mut micros: i64 = 0;
    for (i, c) in frac.chars().take(6).enumerate() {
        micros += c.to_digit(10)? as i64 * 10i64.pow(5 - i as u32);
    }
    // days from civil (Howard Hinnant)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mi * 60 + ss - off_secs;
    Some(secs * 1_000_000 + micros)
}

/// The database refused a write on purpose (a check / trigger / privilege / lost race).
pub fn is_rejection(e: &CloudError) -> bool {
    match e {
        CloudError::InvalidValue(_) | CloudError::Conflict(_) | CloudError::Forbidden => true,
        CloudError::Postgrest { code, .. } => {
            code == "42501" || code == "P0001" || code.starts_with("23")
        }
        _ => false,
    }
}

impl<T: RestTransport> CloudClient<T> {
    pub async fn compaction_status(
        &self,
        session: &Session,
        profile_id: Uuid,
    ) -> Result<CompactionStatusRow, CloudError> {
        let body = serde_json::json!({ "p_profile": profile_id });
        let bytes = self
            .call(
                session,
                "POST",
                "/rpc/synchrome_compaction_status",
                &[("Content-Type", "application/json")],
                Some(&body),
            )
            .await?;
        Self::one(&bytes)
    }

    pub async fn list_snapshot_rows(
        &self,
        session: &Session,
        profile_id: Uuid,
    ) -> Result<Vec<SnapshotRowDto>, CloudError> {
        let bytes = self
            .call(
                session,
                "GET",
                &format!(
                    "/profile_snapshots?profile_id=eq.{profile_id}&select=generation,state,created_by,manifest_path,manifest_sha256,watermarks,bytes,claimed_at,expires_at,ready_at&order=generation.asc"
                ),
                &[],
                None,
            )
            .await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| CloudError::InvalidResponse(format!("profile_snapshots: {e}")))
    }

    /// Claim generation `generation` (the server sets state/times; 409/23514 = lost race).
    pub async fn claim_snapshot(
        &self,
        session: &Session,
        profile_id: Uuid,
        generation: i64,
        device_id: Uuid,
    ) -> Result<(), CloudError> {
        let body = serde_json::json!({ "profile_id": profile_id, "generation": generation, "created_by": device_id });
        self.call(
            session,
            "POST",
            "/profile_snapshots",
            &[
                ("Prefer", "return=minimal"),
                ("Content-Type", "application/json"),
            ],
            Some(&body),
        )
        .await
        .map(|_| ())
    }

    /// building → ready (the server validates the watermarks and stamps ready_at).
    #[allow(clippy::too_many_arguments)]
    pub async fn mark_snapshot_ready(
        &self,
        session: &Session,
        profile_id: Uuid,
        generation: i64,
        manifest_path: &str,
        manifest_sha256: &str,
        watermarks: &BTreeMap<String, i64>,
        counts: &BTreeMap<String, (u64, u64)>,
        bytes: i64,
    ) -> Result<(), CloudError> {
        let counts: serde_json::Map<String, serde_json::Value> = counts
            .iter()
            .map(|(c, (e, t))| {
                (
                    c.clone(),
                    serde_json::json!({ "entities": e, "tombstones": t }),
                )
            })
            .collect();
        let body = serde_json::json!({
            "state": "ready", "manifest_path": manifest_path, "manifest_sha256": manifest_sha256,
            "watermarks": watermarks, "counts": counts, "bytes": bytes,
        });
        let out = self
            .call(
                session,
                "PATCH",
                &format!("/profile_snapshots?profile_id=eq.{profile_id}&generation=eq.{generation}&state=eq.building&select=generation"),
                &[("Prefer", "return=representation"), ("Content-Type", "application/json")],
                Some(&body),
            )
            .await?;
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&out).unwrap_or_default();
        if rows.is_empty() {
            return Err(CloudError::InvalidValue("claim no longer building".into()));
        }
        Ok(())
    }

    pub async fn mark_snapshot_superseded(
        &self,
        session: &Session,
        profile_id: Uuid,
        generation: i64,
    ) -> Result<(), CloudError> {
        let body = serde_json::json!({ "state": "superseded" });
        self.call(
            session,
            "PATCH",
            &format!("/profile_snapshots?profile_id=eq.{profile_id}&generation=eq.{generation}&state=eq.ready"),
            &[("Prefer", "return=minimal"), ("Content-Type", "application/json")],
            Some(&body),
        )
        .await
        .map(|_| ())
    }

    /// Delete a superseded row or an expired claim (RLS refuses anything else: `false`).
    pub async fn delete_snapshot_row(
        &self,
        session: &Session,
        profile_id: Uuid,
        generation: i64,
    ) -> Result<bool, CloudError> {
        let out = self
            .call(
                session,
                "DELETE",
                &format!("/profile_snapshots?profile_id=eq.{profile_id}&generation=eq.{generation}&select=generation"),
                &[("Prefer", "return=representation")],
                None,
            )
            .await?;
        Ok(serde_json::from_slice::<Vec<serde_json::Value>>(&out)
            .map(|v| !v.is_empty())
            .unwrap_or(false))
    }

    pub async fn list_cursor_rows(
        &self,
        session: &Session,
        profile_id: Uuid,
    ) -> Result<Vec<CursorRowDto>, CloudError> {
        let bytes = self
            .call(
                session,
                "GET",
                &format!(
                    "/sync_cursors?profile_id=eq.{profile_id}&select=device_id,applied,published_through,snapshot_generation,snapshot_protocol,updated_at,retired_at"
                ),
                &[],
                None,
            )
            .await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| CloudError::InvalidResponse(format!("sync_cursors: {e}")))
    }

    /// PATCH the row of (profile, device) with `body`; when no row exists yet, INSERT it with
    /// the ids added. (Not a PostgREST upsert: 0005 grants UPDATE only on the client-owned
    /// columns, never on the ids.) A concurrent insert (409) is retried as a PATCH.
    async fn cursor_patch_or_insert(
        &self,
        session: &Session,
        profile_id: Uuid,
        device_id: Uuid,
        body: serde_json::Value,
    ) -> Result<(), CloudError> {
        let path = format!(
            "/sync_cursors?profile_id=eq.{profile_id}&device_id=eq.{device_id}&select=device_id"
        );
        let headers = [
            ("Prefer", "return=representation"),
            ("Content-Type", "application/json"),
        ];
        for _ in 0..2 {
            let out = self
                .call(session, "PATCH", &path, &headers, Some(&body))
                .await?;
            if !serde_json::from_slice::<Vec<serde_json::Value>>(&out)
                .unwrap_or_default()
                .is_empty()
            {
                return Ok(());
            }
            let mut ins = body.clone();
            ins["profile_id"] = serde_json::json!(profile_id);
            ins["device_id"] = serde_json::json!(device_id);
            match self
                .call(
                    session,
                    "POST",
                    "/sync_cursors",
                    &[
                        ("Prefer", "return=minimal"),
                        ("Content-Type", "application/json"),
                    ],
                    Some(&ins),
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(CloudError::Conflict(_)) => continue, // inserted concurrently: PATCH again
                Err(e) => return Err(e),
            }
        }
        Err(CloudError::Conflict("sync_cursors row".into()))
    }

    /// This device's cursors (a report also un-retires the device: it is in use).
    pub async fn report_cursor_row(
        &self,
        session: &Session,
        row: &CursorReportDto,
    ) -> Result<(), CloudError> {
        let body = serde_json::json!({
            "applied": row.applied, "published_through": row.published_through,
            "snapshot_generation": row.snapshot_generation, "snapshot_protocol": row.snapshot_protocol,
            "retired_at": null,
        });
        self.cursor_patch_or_insert(session, row.profile_id, row.device_id, body)
            .await
    }

    /// Explicit user action: retire a device from a profile (the server stamps the time).
    pub async fn retire_device(
        &self,
        session: &Session,
        profile_id: Uuid,
        device_id: Uuid,
    ) -> Result<(), CloudError> {
        // any non-null value: the database replaces it with its own clock
        let body = serde_json::json!({ "retired_at": crate::client::now_rfc3339() });
        self.cursor_patch_or_insert(session, profile_id, device_id, body)
            .await
    }

    /// `synchrome_gc_journal_events(profile, device, through)` → rows deleted.
    pub async fn gc_journal_events(
        &self,
        session: &Session,
        profile_id: Uuid,
        device_id: Uuid,
        through: i64,
    ) -> Result<i64, CloudError> {
        let body = serde_json::json!({ "p_profile": profile_id, "p_device": device_id, "p_through": through });
        let bytes = self
            .call(
                session,
                "POST",
                "/rpc/synchrome_gc_journal_events",
                &[("Content-Type", "application/json")],
                Some(&body),
            )
            .await?;
        serde_json::from_slice::<i64>(&bytes)
            .map_err(|e| CloudError::InvalidResponse(format!("gc: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_parse_like_postgrest_renders_them() {
        let a = parse_ts_micros("2026-10-03T10:20:30.123456+00:00").unwrap();
        let b = parse_ts_micros("2026-10-03T10:20:30.123456Z").unwrap();
        let c = parse_ts_micros("2026-10-03T12:20:30.123456+02:00").unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(parse_ts_micros("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_ts_micros("2000-03-01T00:00:00+00:00"),
            Some(951_868_800 * 1_000_000)
        );
        let d = parse_ts_micros("2026-10-03T10:20:31.1+00:00").unwrap();
        assert_eq!(d - a, 1_000_000 - 23_456);
        assert!(parse_ts_micros("garbage").is_none());
    }

    #[test]
    fn rejections_are_the_database_saying_no() {
        assert!(is_rejection(&CloudError::InvalidValue("check".into())));
        assert!(is_rejection(&CloudError::Conflict("dup".into())));
        assert!(is_rejection(&CloudError::Postgrest {
            code: "42501".into(),
            message: String::new()
        }));
        assert!(!is_rejection(&CloudError::Network("x".into())));
        assert!(!is_rejection(&CloudError::Server(503)));
    }
}
