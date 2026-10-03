//! Compacted snapshots (`docs/03` §3.7): the deterministic fold of published journal ops into
//! per-entity records, their NDJSON parts and manifest, integrity hashes, and the synthetic ops
//! that import a snapshot through the normal merge. Pure; no I/O.
//!
//! The fold is the shared state every collection's merge converges to: per field, the value
//! with the highest HLC; per entity, the highest tombstone HLC; an entity is deleted iff its
//! tombstone is newer than every field. It never contains device-local data (unpublished ops,
//! conflict records, pending applies): it is built from cloud journals only.

use crate::op::{Fields, Op, OpKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const FORMAT: &str = "synchrome-snapshot/1";
/// Snapshot protocol this build implements (`sync_cursors.snapshot_protocol`).
pub const PROTOCOL: i32 = 1;
/// The bucket accepts objects ≤ 4 MiB; parts stay below 3.5 MiB.
pub const MAX_PART_BYTES: usize = 3 * 1024 * 1024 + 512 * 1024;
pub const MAX_PARTS: usize = 2000;
pub const MAX_ENTITIES_PER_COLLECTION: usize = 250_000;
pub const MAX_COLLECTION_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SnapshotError {
    #[error("collection {0} exceeds the snapshot limits")]
    TooLarge(String),
    #[error("snapshot integrity check failed: {0}")]
    Integrity(String),
    #[error("snapshot format not supported: {0}")]
    Format(String),
}

/// One entity of the shared state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    /// field → value
    pub f: BTreeMap<String, Value>,
    /// field → HLC of that value
    pub h: BTreeMap<String, String>,
    /// deleted (tombstone newer than every field)
    pub d: bool,
    /// highest tombstone HLC (kept even when a later field resurrected the entity)
    pub dh: Option<String>,
}

impl Record {
    fn newest_field(&self) -> Option<&String> {
        self.h.values().max()
    }

    fn recompute(&mut self) {
        self.d = match (&self.dh, self.newest_field()) {
            (Some(dh), Some(f)) => dh > f,
            (Some(_), None) => true,
            _ => false,
        };
    }

    fn apply(&mut self, op: &Op) {
        match op.op {
            OpKind::Set => {
                for (k, v) in op.fields.iter().flatten() {
                    if self.h.get(k).map_or(true, |h| h.as_str() < op.hlc.as_str()) {
                        self.f.insert(k.clone(), v.clone());
                        self.h.insert(k.clone(), op.hlc.clone());
                    }
                }
            }
            OpKind::Delete => {
                if self.dh.as_deref().map_or(true, |d| d < op.hlc.as_str()) {
                    self.dh = Some(op.hlc.clone());
                }
            }
        }
        self.recompute();
    }

    fn line(&self) -> String {
        serde_json::to_string(self).expect("record serialises")
    }
}

/// The fold of one profile's shared state, per collection.
#[derive(Debug, Default)]
pub struct Fold {
    cols: BTreeMap<String, BTreeMap<String, Record>>,
    bytes: BTreeMap<String, usize>,
}

impl Fold {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start from the records of a previous snapshot.
    pub fn load(&mut self, col: &str, records: Vec<Record>) -> Result<(), SnapshotError> {
        for r in records {
            let n = r.line().len();
            *self.bytes.entry(col.to_string()).or_default() += n;
            self.cols
                .entry(col.to_string())
                .or_default()
                .insert(r.id.clone(), r);
        }
        self.check(col)
    }

    fn check(&self, col: &str) -> Result<(), SnapshotError> {
        let n = self.cols.get(col).map_or(0, |m| m.len());
        let b = self.bytes.get(col).copied().unwrap_or(0);
        if n > MAX_ENTITIES_PER_COLLECTION || b > MAX_COLLECTION_BYTES {
            return Err(SnapshotError::TooLarge(col.to_string()));
        }
        Ok(())
    }

    /// Merge one published op (any order: the result does not depend on it).
    pub fn apply(&mut self, op: &Op) -> Result<(), SnapshotError> {
        let m = self.cols.entry(op.col.clone()).or_default();
        let r = m.entry(op.id.clone()).or_insert_with(|| Record {
            id: op.id.clone(),
            ..Default::default()
        });
        let before = r.line().len();
        r.apply(op);
        let after = r.line().len();
        let b = self.bytes.entry(op.col.clone()).or_default();
        *b = (*b + after).saturating_sub(before);
        self.check(&op.col)
    }

    pub fn collections(&self) -> impl Iterator<Item = (&String, &BTreeMap<String, Record>)> {
        self.cols.iter()
    }

    pub fn into_records(self) -> BTreeMap<String, Vec<Record>> {
        self.cols
            .into_iter()
            .map(|(c, m)| (c, m.into_values().collect()))
            .collect()
    }
}

/// SHA-256, lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartInfo {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub entities: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionInfo {
    pub entities: u64,
    pub tombstones: u64,
    pub bytes: u64,
    pub parts: Vec<PartInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub profile_id: String,
    pub generation: u64,
    pub base_generation: Option<u64>,
    /// Builder clock, informational (the database stamps claimed_at / ready_at).
    pub created_at: String,
    pub created_by: String,
    pub watermarks: BTreeMap<String, u64>,
    pub collections: BTreeMap<String, CollectionInfo>,
    pub total_bytes: u64,
}

impl Manifest {
    /// Canonical bytes (BTreeMaps: stable key order).
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("manifest serialises")
    }

    pub fn parse(bytes: &[u8]) -> Result<Manifest, SnapshotError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(SnapshotError::Integrity("manifest too large".into()));
        }
        let m: Manifest =
            serde_json::from_slice(bytes).map_err(|e| SnapshotError::Integrity(e.to_string()))?;
        if m.format != FORMAT {
            return Err(SnapshotError::Format(m.format));
        }
        let mut seen = 0usize;
        for (col, info) in &m.collections {
            seen += info.parts.len();
            for (i, p) in info.parts.iter().enumerate() {
                if p.name != part_name(col, i + 1) || p.sha256.len() != 64 {
                    return Err(SnapshotError::Integrity("unexpected part entry".into()));
                }
            }
        }
        if seen > MAX_PARTS {
            return Err(SnapshotError::Integrity("too many parts".into()));
        }
        Ok(m)
    }

    pub fn part_names(&self) -> Vec<(String, PartInfo)> {
        self.collections
            .iter()
            .flat_map(|(c, i)| i.parts.iter().map(move |p| (c.clone(), p.clone())))
            .collect()
    }
}

/// `<collection>-<nnnn>.jsonl`
pub fn part_name(col: &str, n: usize) -> String {
    format!("{col}-{n:04}.jsonl")
}

/// `profiles/<profile>/snapshots/<generation:10>`
pub fn generation_dir(profile_id: &str, generation: u64) -> String {
    format!("profiles/{profile_id}/snapshots/{generation:010}")
}

/// A built snapshot: manifest + parts (name → bytes).
#[derive(Debug, Clone)]
pub struct Built {
    pub manifest: Manifest,
    pub manifest_bytes: Vec<u8>,
    pub manifest_sha256: String,
    pub parts: Vec<(String, Vec<u8>)>,
}

/// Serialise a fold into NDJSON parts (records sorted by id, ≤ MAX_PART_BYTES each).
pub fn build(
    fold: Fold,
    profile_id: &str,
    generation: u64,
    base_generation: Option<u64>,
    created_at: &str,
    created_by: &str,
    watermarks: BTreeMap<String, u64>,
) -> Result<Built, SnapshotError> {
    let mut collections = BTreeMap::new();
    let mut parts = Vec::new();
    let mut total = 0u64;
    for (col, recs) in fold.into_records() {
        let mut info = CollectionInfo::default();
        let mut cur: Vec<u8> = Vec::new();
        let mut cur_n = 0u64;
        let mut flush = |cur: &mut Vec<u8>, cur_n: &mut u64, info: &mut CollectionInfo| {
            if cur.is_empty() {
                return;
            }
            let name = part_name(&col, info.parts.len() + 1);
            info.parts.push(PartInfo {
                name: name.clone(),
                bytes: cur.len() as u64,
                sha256: sha256_hex(cur),
                entities: *cur_n,
            });
            info.bytes += cur.len() as u64;
            parts.push((name, std::mem::take(cur)));
            *cur_n = 0;
        };
        for r in recs {
            let mut line = r.line().into_bytes();
            line.push(b'\n');
            if line.len() > MAX_PART_BYTES {
                return Err(SnapshotError::TooLarge(col.clone()));
            }
            if cur.len() + line.len() > MAX_PART_BYTES {
                flush(&mut cur, &mut cur_n, &mut info);
            }
            info.entities += 1;
            info.tombstones += r.d as u64;
            cur.extend_from_slice(&line);
            cur_n += 1;
        }
        flush(&mut cur, &mut cur_n, &mut info);
        total += info.bytes;
        collections.insert(col, info);
    }
    if parts.len() > MAX_PARTS {
        return Err(SnapshotError::TooLarge("parts".into()));
    }
    let manifest = Manifest {
        format: FORMAT.into(),
        profile_id: profile_id.into(),
        generation,
        base_generation,
        created_at: created_at.into(),
        created_by: created_by.into(),
        watermarks,
        collections,
        total_bytes: total,
    };
    let manifest_bytes = manifest.to_bytes();
    let manifest_sha256 = sha256_hex(&manifest_bytes);
    Ok(Built {
        manifest,
        manifest_bytes,
        manifest_sha256,
        parts,
    })
}

/// Verify a downloaded part against its manifest entry and parse its records.
pub fn read_part(info: &PartInfo, bytes: &[u8]) -> Result<Vec<Record>, SnapshotError> {
    if bytes.len() as u64 != info.bytes || sha256_hex(bytes) != info.sha256 {
        return Err(SnapshotError::Integrity(format!(
            "part {} does not match its hash",
            info.name
        )));
    }
    let mut out = Vec::new();
    for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let r: Record =
            serde_json::from_slice(line).map_err(|e| SnapshotError::Integrity(e.to_string()))?;
        out.push(r);
    }
    if out.len() as u64 != info.entities {
        return Err(SnapshotError::Integrity(format!(
            "part {} entity count",
            info.name
        )));
    }
    Ok(out)
}

/// The device embedded in an HLC (`<ms>-<counter>-<device>`).
pub fn hlc_device(hlc: &str) -> String {
    hlc.splitn(3, '-').nth(2).unwrap_or_default().to_string()
}

/// Ops that rebuild a record through the normal merge: one `set` per distinct field HLC and
/// one `delete` at the tombstone HLC (synthetic: `seq` 0, author = the HLC's device).
pub fn record_ops(col: &str, r: &Record) -> Vec<Op> {
    let mut by_hlc: BTreeMap<&str, Fields> = BTreeMap::new();
    for (k, h) in &r.h {
        if let Some(v) = r.f.get(k) {
            by_hlc
                .entry(h.as_str())
                .or_default()
                .insert(k.clone(), v.clone());
        }
    }
    let mut ops: Vec<Op> = by_hlc
        .into_iter()
        .map(|(h, fields)| Op {
            seq: 0,
            hlc: h.to_string(),
            col: col.to_string(),
            id: r.id.clone(),
            op: OpKind::Set,
            fields: Some(fields),
            by: hlc_device(h),
        })
        .collect();
    if let Some(dh) = &r.dh {
        ops.push(Op {
            seq: 0,
            hlc: dh.clone(),
            col: col.to_string(),
            id: r.id.clone(),
            op: OpKind::Delete,
            fields: None,
            by: hlc_device(dh),
        });
    }
    ops
}

/// Synthetic ops of a whole collection, in HLC order (parents before children, as in the
/// original history).
pub fn collection_ops(col: &str, records: &[Record]) -> Vec<Op> {
    let mut ops: Vec<Op> = records.iter().flat_map(|r| record_ops(col, r)).collect();
    ops.sort_by(|a, b| a.hlc.cmp(&b.hlc).then_with(|| a.id.cmp(&b.id)));
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn op(hlc: &str, id: &str, kind: OpKind, fields: Option<Value>) -> Op {
        Op {
            seq: 1,
            hlc: hlc.into(),
            col: "bookmarks".into(),
            id: id.into(),
            op: kind,
            fields: fields.map(|f| serde_json::from_value(f).unwrap()),
            by: hlc_device(hlc),
        }
    }

    fn h(ms: u64, dev: &str) -> String {
        format!("{ms:013}-0000-{dev}")
    }

    fn fold(ops: &[Op]) -> BTreeMap<String, Vec<Record>> {
        let mut f = Fold::new();
        for o in ops {
            f.apply(o).unwrap();
        }
        f.into_records()
    }

    #[test]
    fn fold_is_order_independent_including_concurrent_deletes() {
        let ops = vec![
            op(
                &h(1, "a"),
                "x",
                OpKind::Set,
                Some(json!({"title": "one", "url": "u"})),
            ),
            op(&h(5, "b"), "x", OpKind::Delete, None),
            op(&h(6, "c"), "x", OpKind::Set, Some(json!({"title": "six"}))),
            op(&h(7, "a"), "x", OpKind::Delete, None),
            op(&h(2, "b"), "y", OpKind::Set, Some(json!({"title": "y"}))),
        ];
        let base = fold(&ops);
        // every permutation of these 5 ops folds to the same records
        let idx: Vec<usize> = (0..ops.len()).collect();
        let mut perms = vec![idx.clone()];
        let mut p = idx;
        for _ in 0..119 {
            // next lexicographic permutation
            let mut i = p.len() - 1;
            while i > 0 && p[i - 1] >= p[i] {
                i -= 1;
            }
            if i == 0 {
                break;
            }
            let mut j = p.len() - 1;
            while p[j] <= p[i - 1] {
                j -= 1;
            }
            p.swap(i - 1, j);
            p[i..].reverse();
            perms.push(p.clone());
        }
        assert_eq!(perms.len(), 120);
        for perm in perms {
            let shuffled: Vec<Op> = perm.iter().map(|i| ops[*i].clone()).collect();
            assert_eq!(fold(&shuffled), base);
        }
        let x = &base["bookmarks"][0];
        assert!(x.d, "the newest tombstone (7) beats the newest field (6)");
        assert_eq!(x.dh.as_deref(), Some(h(7, "a").as_str()));
        assert_eq!(x.f["title"], json!("six"));
    }

    #[test]
    fn tombstones_are_kept_and_resurrection_keeps_the_tombstone_hlc() {
        let r = &fold(&[
            op(&h(1, "a"), "x", OpKind::Set, Some(json!({"title": "t"}))),
            op(&h(2, "b"), "x", OpKind::Delete, None),
        ])["bookmarks"][0];
        assert!(r.d && r.dh.is_some() && r.f.contains_key("title"));
        let r = &fold(&[
            op(&h(2, "b"), "x", OpKind::Delete, None),
            op(&h(3, "a"), "x", OpKind::Set, Some(json!({"title": "back"}))),
        ])["bookmarks"][0];
        assert!(!r.d);
        assert_eq!(r.dh.as_deref(), Some(h(2, "b").as_str()));
        // a tombstone of an entity never seen alive is kept too
        let r = &fold(&[op(&h(4, "c"), "z", OpKind::Delete, None)])["bookmarks"][0];
        assert!(r.d && r.f.is_empty());
    }

    #[test]
    fn synthetic_ops_refold_to_the_same_records() {
        let ops = vec![
            op(
                &h(1, "a"),
                "x",
                OpKind::Set,
                Some(json!({"title": "one", "url": "u"})),
            ),
            op(
                &h(3, "b"),
                "x",
                OpKind::Set,
                Some(json!({"title": "three"})),
            ),
            op(&h(4, "c"), "x", OpKind::Delete, None),
            op(&h(5, "a"), "x", OpKind::Set, Some(json!({"url": "v"}))),
            op(&h(2, "b"), "y", OpKind::Delete, None),
        ];
        let recs = fold(&ops);
        let synth = collection_ops("bookmarks", &recs["bookmarks"]);
        assert!(synth.windows(2).all(|w| w[0].hlc <= w[1].hlc), "HLC order");
        assert!(synth
            .iter()
            .all(|o| o.seq == 0 && o.by == hlc_device(&o.hlc)));
        assert_eq!(fold(&synth), recs);
    }

    #[test]
    fn parts_hashes_manifest_and_corruption() {
        let mut f = Fold::new();
        for i in 0..2000 {
            f.apply(&op(
                &h(i, "a"),
                &format!("id{i:05}"),
                OpKind::Set,
                Some(json!({"title": "x".repeat(2000)})),
            ))
            .unwrap();
        }
        let wm = BTreeMap::from([("a".to_string(), 2000u64)]);
        let b = build(f, "p", 1, None, "t", "dev", wm.clone()).unwrap();
        assert!(b.parts.len() >= 2, "split into several parts");
        assert!(b.parts.iter().all(|(_, p)| p.len() <= MAX_PART_BYTES));
        let m = Manifest::parse(&b.manifest_bytes).unwrap();
        assert_eq!(m, b.manifest);
        assert_eq!(sha256_hex(&b.manifest_bytes), b.manifest_sha256);
        let total: u64 = m.collections["bookmarks"].entities;
        assert_eq!(total, 2000);
        let mut n = 0;
        for ((col, info), (name, bytes)) in m.part_names().into_iter().zip(&b.parts) {
            assert_eq!((&info.name, col.as_str()), (name, "bookmarks"));
            n += read_part(&info, bytes).unwrap().len();
            let mut bad = bytes.clone();
            bad[10] ^= 1;
            assert!(matches!(
                read_part(&info, &bad),
                Err(SnapshotError::Integrity(_))
            ));
        }
        assert_eq!(n, 2000);
        // deterministic: same input → same bytes
        let mut f2 = Fold::new();
        for i in (0..2000).rev() {
            f2.apply(&op(
                &h(i, "a"),
                &format!("id{i:05}"),
                OpKind::Set,
                Some(json!({"title": "x".repeat(2000)})),
            ))
            .unwrap();
        }
        let b2 = build(f2, "p", 1, None, "t", "dev", wm).unwrap();
        assert_eq!(b2.manifest_sha256, b.manifest_sha256);
        // a tampered manifest is refused
        let mut bad = b.manifest_bytes.clone();
        let s = String::from_utf8(bad.clone())
            .unwrap()
            .replace("synchrome-snapshot/1", "synchrome-snapshot/9");
        bad = s.into_bytes();
        assert!(matches!(
            Manifest::parse(&bad),
            Err(SnapshotError::Format(_))
        ));
        assert!(Manifest::parse(b"{not json").is_err());
    }

    #[test]
    fn limits_are_enforced() {
        let mut f = Fold::new();
        let big = "x".repeat(MAX_PART_BYTES);
        f.apply(&op(
            &h(1, "a"),
            "x",
            OpKind::Set,
            Some(json!({"title": big})),
        ))
        .unwrap();
        assert!(matches!(
            build(f, "p", 1, None, "t", "d", BTreeMap::new()),
            Err(SnapshotError::TooLarge(_))
        ));
    }
}
