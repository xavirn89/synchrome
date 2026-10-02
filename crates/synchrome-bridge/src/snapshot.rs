//! Reassembly of a multi-part bookmarks snapshot within one native-messaging connection.
//!
//! Parts may arrive in any order; a part of a different `snapshot_id` discards the one in
//! progress (the extension restarted it). Nothing is persisted until the last part arrives,
//! so a connection that dies mid-snapshot leaves no trace and the extension simply resends.

use crate::protocol::{ChromiumNode, MAX_SNAPSHOT_NODES};

/// Total bytes of all parts of one snapshot (sum of frame sizes).
pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblyError {
    TooLarge,
    Inconsistent,
}

#[derive(Debug, PartialEq)]
pub struct CompleteSnapshot {
    pub snapshot_id: String,
    pub epoch: String,
    pub covers_through: u64,
    pub nodes: Vec<ChromiumNode>,
}

#[derive(Debug, Default)]
pub struct SnapshotAssembly {
    id: Option<String>,
    epoch: String,
    covers_through: u64,
    parts: Vec<Option<Vec<ChromiumNode>>>,
    bytes: usize,
    nodes: usize,
}

impl SnapshotAssembly {
    /// Add one part (`frame_len` = its frame size). Returns the full snapshot when complete.
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        snapshot_id: &str,
        epoch: &str,
        covers_through: u64,
        part: u32,
        parts: u32,
        nodes: Vec<ChromiumNode>,
        frame_len: usize,
    ) -> Result<Option<CompleteSnapshot>, AssemblyError> {
        if self.id.as_deref() != Some(snapshot_id) {
            *self = SnapshotAssembly {
                id: Some(snapshot_id.to_string()),
                epoch: epoch.to_string(),
                covers_through,
                parts: vec![None; parts as usize],
                ..Default::default()
            };
        }
        if self.parts.len() != parts as usize || self.epoch != epoch || part >= parts {
            *self = SnapshotAssembly::default();
            return Err(AssemblyError::Inconsistent);
        }
        let slot = &mut self.parts[part as usize];
        if slot.is_none() {
            self.bytes += frame_len;
            self.nodes += nodes.len();
            *slot = Some(nodes);
        }
        if self.bytes > MAX_SNAPSHOT_BYTES || self.nodes > MAX_SNAPSHOT_NODES {
            *self = SnapshotAssembly::default();
            return Err(AssemblyError::TooLarge);
        }
        self.covers_through = self.covers_through.max(covers_through);
        if self.parts.iter().all(Option::is_some) {
            let done = std::mem::take(self);
            return Ok(Some(CompleteSnapshot {
                snapshot_id: done.id.unwrap_or_default(),
                epoch: done.epoch,
                covers_through: done.covers_through,
                nodes: done.parts.into_iter().flatten().flatten().collect(),
            }));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str) -> ChromiumNode {
        ChromiumNode {
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn parts_in_any_order_and_duplicates() {
        let mut a = SnapshotAssembly::default();
        assert!(a
            .add("s", "e", 0, 2, 3, vec![n("5")], 10)
            .unwrap()
            .is_none());
        assert!(a
            .add("s", "e", 0, 0, 3, vec![n("1")], 10)
            .unwrap()
            .is_none());
        assert!(a
            .add("s", "e", 0, 0, 3, vec![n("1")], 10)
            .unwrap()
            .is_none()); // dup
        let done = a
            .add("s", "e", 4, 1, 3, vec![n("2"), n("3")], 10)
            .unwrap()
            .unwrap();
        let ids: Vec<&str> = done.nodes.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, vec!["1", "2", "3", "5"]);
        assert_eq!(done.covers_through, 4);
    }

    #[test]
    fn new_snapshot_id_restarts_and_limits_apply() {
        let mut a = SnapshotAssembly::default();
        a.add("s1", "e", 0, 0, 2, vec![n("1")], 10).unwrap();
        // restarted by the extension: s1 dropped
        assert!(a
            .add("s2", "e", 0, 0, 1, vec![n("9")], 10)
            .unwrap()
            .is_some());
        assert_eq!(
            a.add("s3", "e", 0, 0, 2, vec![], MAX_SNAPSHOT_BYTES + 1),
            Err(AssemblyError::TooLarge)
        );
        a.add("s4", "e", 0, 0, 2, vec![], 1).unwrap();
        assert_eq!(
            a.add("s4", "e", 0, 0, 3, vec![], 1).unwrap_err(),
            AssemblyError::Inconsistent
        );
    }
}
