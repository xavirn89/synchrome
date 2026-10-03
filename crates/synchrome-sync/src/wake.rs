//! Wake-up coalescing for Realtime hints (`docs/03` §3.7 A). Pure and deterministic: the
//! caller passes the time. Per profile: a debounce window after the first hint, at most one
//! sync in flight plus ONE trailing re-run, and a minimum gap between syncs of the same
//! profile — so N hints in a burst cause at most two syncs, never a storm.

use std::collections::BTreeMap;
use uuid::Uuid;

pub const DEBOUNCE_MS: u64 = 500;
pub const MIN_GAP_MS: u64 = 2_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct P {
    dirty_since: Option<u64>,
    running: bool,
    trailing: bool,
    last_start: Option<u64>,
}

/// Counters (no ids).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct WakeStats {
    pub hints: u64,
    /// Hints that did not cause a sync of their own (merged into a pending/running one).
    pub coalesced: u64,
    pub syncs_started: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Wake {
    profiles: BTreeMap<Uuid, P>,
    pub stats: WakeStats,
}

impl Wake {
    pub fn new() -> Self {
        Self::default()
    }

    /// A hint for `profile` arrived at `now`.
    pub fn hint(&mut self, profile: Uuid, now: u64) {
        self.stats.hints += 1;
        let p = self.profiles.entry(profile).or_default();
        if p.running {
            if p.trailing {
                self.stats.coalesced += 1;
            }
            p.trailing = true;
            return;
        }
        if p.dirty_since.is_some() {
            self.stats.coalesced += 1;
            return;
        }
        p.dirty_since = Some(now);
    }

    fn ready_at(p: &P) -> Option<u64> {
        let d = p.dirty_since?;
        if p.running {
            return None;
        }
        let gap = p.last_start.map_or(0, |l| l + MIN_GAP_MS);
        Some((d + DEBOUNCE_MS).max(gap))
    }

    /// Profiles whose sync should start now (they are marked running).
    pub fn due(&mut self, now: u64) -> Vec<Uuid> {
        let mut out = Vec::new();
        for (id, p) in self.profiles.iter_mut() {
            if Self::ready_at(p).is_some_and(|t| t <= now) {
                p.running = true;
                p.dirty_since = None;
                p.last_start = Some(now);
                out.push(*id);
            }
        }
        self.stats.syncs_started += out.len() as u64;
        out
    }

    /// When the next sync becomes due (to arm a timer), if any.
    pub fn next_deadline(&self) -> Option<u64> {
        self.profiles.values().filter_map(Self::ready_at).min()
    }

    /// The sync of `profile` finished; a hint that arrived meanwhile schedules one re-run.
    pub fn finished(&mut self, profile: Uuid, now: u64) {
        if let Some(p) = self.profiles.get_mut(&profile) {
            p.running = false;
            if p.trailing {
                p.trailing = false;
                p.dirty_since = Some(now);
            }
        }
    }

    /// Catch-up after a (re)connection: every mapped profile becomes dirty once.
    pub fn all_dirty(&mut self, profiles: &[Uuid], now: u64) {
        for id in profiles {
            let p = self.profiles.entry(*id).or_default();
            if p.running {
                p.trailing = true;
            } else if p.dirty_since.is_none() {
                p.dirty_since = Some(now);
            }
        }
    }

    /// A profile no longer exists / is not mapped here.
    pub fn forget(&mut self, profile: Uuid) {
        self.profiles.remove(&profile);
    }

    /// Logout / account switch: nothing pending survives.
    pub fn reset(&mut self) {
        self.profiles.clear();
    }

    pub fn running(&self, profile: Uuid) -> bool {
        self.profiles.get(&profile).is_some_and(|p| p.running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_is_one_sync_after_the_debounce() {
        let p = Uuid::new_v4();
        let mut w = Wake::new();
        for t in 0..10 {
            w.hint(p, 1000 + t * 10); // ten hints within 100 ms (incl. duplicates)
        }
        assert!(w.due(1400).is_empty(), "still debouncing");
        assert_eq!(w.next_deadline(), Some(1500));
        assert_eq!(w.due(1500), vec![p]);
        assert!(w.due(1600).is_empty());
        assert_eq!(
            w.stats,
            WakeStats {
                hints: 10,
                coalesced: 9,
                syncs_started: 1
            }
        );
    }

    #[test]
    fn hints_during_a_sync_give_exactly_one_trailing_run_after_the_min_gap() {
        let p = Uuid::new_v4();
        let mut w = Wake::new();
        w.hint(p, 0);
        assert_eq!(w.due(500), vec![p]);
        for t in 0..5 {
            w.hint(p, 600 + t); // while running
        }
        assert!(w.due(700).is_empty(), "max one in flight");
        w.finished(p, 800);
        assert!(
            w.due(1300).is_empty(),
            "min gap of 2 s since the last start"
        );
        assert_eq!(w.next_deadline(), Some(2500));
        assert_eq!(w.due(2500), vec![p]);
        w.finished(p, 2600);
        assert!(w.due(10_000).is_empty(), "nothing more without new hints");
        assert_eq!(w.stats.syncs_started, 2);
        assert_eq!(w.stats.coalesced, 4);
    }

    #[test]
    fn profiles_are_independent_and_catch_up_marks_all_once() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut w = Wake::new();
        w.hint(a, 0);
        w.all_dirty(&[a, b], 100);
        let mut due = w.due(600);
        due.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(due, want);
        w.forget(b);
        w.finished(a, 700);
        w.reset();
        assert!(w.due(100_000).is_empty());
        assert!(!w.running(a));
    }

    #[test]
    fn lost_or_out_of_order_hints_are_harmless() {
        // the scheduler never looks at payloads or order: a late hint just asks for one more
        // (cursor-based, idempotent) sync; a lost one changes nothing (polling covers it)
        let p = Uuid::new_v4();
        let mut w = Wake::new();
        w.hint(p, 5_000);
        w.hint(p, 4_000); // "older" hint arriving later
        assert_eq!(w.due(5_500), vec![p]);
    }
}
