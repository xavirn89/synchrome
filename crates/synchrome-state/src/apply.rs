//! Apply planning: materialised remote changes → explicit browser operations for the
//! extension (chrome.bookmarks), and recording of their results.
//!
//! Order of a plan: creates top-down (a created folder can be the parent of later creates
//! through an op reference), then renames / URL changes / moves, then one `reorder` per
//! affected folder (exact target order), then deletes bottom-up. Folder deletes are
//! non-recursive in the extension: a folder that still holds anything Synchrome did not
//! delete (e.g. a local bookmark) fails and becomes a conflict instead of losing data.

use crate::{StateDb, StateError};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use synchrome_sync::bookmarks::Root;
use uuid::Uuid;

/// A browser node: an existing Chromium id, or the node created by an earlier op of the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NodeRef {
    Id { id: String },
    Op { op: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ApplyAction {
    CreateFolder {
        parent: NodeRef,
        title: String,
    },
    CreateBookmark {
        parent: NodeRef,
        title: String,
        url: String,
    },
    Rename {
        id: String,
        title: String,
    },
    SetUrl {
        id: String,
        url: String,
    },
    Move {
        id: String,
        parent: NodeRef,
    },
    /// Put these children of `parent` in exactly this order (others keep their place after).
    Reorder {
        parent: NodeRef,
        children: Vec<NodeRef>,
    },
    /// Non-recursive for folders.
    Delete {
        id: String,
        folder: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyOp {
    pub op_id: u32,
    #[serde(flatten)]
    pub action: ApplyAction,
}

/// What the extension reports per op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyResult {
    pub op_id: u32,
    pub ok: bool,
    /// Chromium id of a created node.
    #[serde(default)]
    pub id: Option<String>,
    /// `not_found`, `not_empty`, or a short reason.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ApplyPlan {
    pub apply_id: String,
    pub ops: Vec<ApplyOp>,
    /// More pending changes than fit in this plan.
    pub more: bool,
    sid_ops: HashMap<String, Vec<u32>>,
    creates: HashMap<u32, String>,
    deletes: HashMap<u32, String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyRecord {
    pub applied: usize,
    pub failed: usize,
    pub incomplete: usize,
}

struct E {
    sid: String,
    kind: String,
    parent: String,
    pos: String,
    title: String,
    url: Option<String>,
    deleted: bool,
    pending: bool,
    fields: BTreeSet<String>,
    conflict: bool,
}

impl StateDb {
    /// Build the next apply plan (at most `max_bytes` of encoded ops).
    pub fn plan_apply(
        &mut self,
        profile_id: Uuid,
        max_bytes: usize,
    ) -> Result<ApplyPlan, StateError> {
        let pid = profile_id.to_string();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        {
            // orphans whose folder exists again (or that were deleted) need no decision
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            crate::conflict::reevaluate_orphans(&tx, &pid, now)?;
            tx.commit()?;
        }
        let mut st = self.conn.prepare(
            "SELECT sid, kind, parent, pos, title, url, deleted, pending_apply, pending_fields, conflict
             FROM entities WHERE profile_id = ?1",
        )?;
        let ents: Vec<E> = st
            .query_map([&pid], |r| {
                let pf: String = r.get(8)?;
                Ok(E {
                    sid: r.get(0)?,
                    kind: r.get(1)?,
                    parent: r.get(2)?,
                    pos: r.get(3)?,
                    title: r.get(4)?,
                    url: r.get(5)?,
                    deleted: r.get::<_, i64>(6)? != 0,
                    pending: r.get::<_, i64>(7)? != 0,
                    fields: serde_json::from_str(&pf).unwrap_or_default(),
                    conflict: r.get::<_, i64>(9)? != 0,
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(st);
        let mut st = self
            .conn
            .prepare("SELECT chromium_id, sid FROM chromium_index WHERE profile_id = ?1")?;
        let mut cid_of: HashMap<String, String> = HashMap::new();
        for r in st.query_map([&pid], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (c, s) = r?;
            cid_of.insert(s, c);
        }
        drop(st);
        let by_sid: HashMap<&str, &E> = ents.iter().map(|e| (e.sid.as_str(), e)).collect();
        let depth = |sid: &str| -> usize {
            let mut d = 0;
            let mut cur = sid;
            while let Some(e) = by_sid.get(cur) {
                if Root::from_sentinel(&e.parent).is_some() || d > 64 {
                    break;
                }
                d += 1;
                cur = &e.parent;
            }
            d
        };

        let mut plan = ApplyPlan {
            apply_id: Uuid::new_v4().to_string(),
            ..Default::default()
        };
        let mut bytes = 0usize;
        let mut next = 1u32;
        let mut push = |plan: &mut ApplyPlan, action: ApplyAction, sids: &[&str]| -> bool {
            let op = ApplyOp {
                op_id: next,
                action,
            };
            let sz = serde_json::to_string(&op).map(|s| s.len()).unwrap_or(0) + 1;
            if bytes + sz > max_bytes && !plan.ops.is_empty() {
                plan.more = true;
                return false;
            }
            bytes += sz;
            for s in sids {
                plan.sid_ops.entry(s.to_string()).or_default().push(next);
            }
            plan.ops.push(op);
            next += 1;
            true
        };
        let mut created: HashMap<String, u32> = HashMap::new();
        let parent_ref = |p: &str, created: &HashMap<String, u32>| -> Option<NodeRef> {
            if let Some(c) = cid_of.get(p) {
                return Some(NodeRef::Id { id: c.clone() });
            }
            created.get(p).map(|op| NodeRef::Op { op: *op })
        };
        let pending: Vec<&E> = ents.iter().filter(|e| e.pending && !e.conflict).collect();
        let mut clear_now: Vec<String> = Vec::new();

        // 1. creates, top-down
        let mut creates: Vec<&E> = pending
            .iter()
            .copied()
            .filter(|e| !e.deleted && !cid_of.contains_key(&e.sid))
            .collect();
        creates
            .sort_by(|a, b| (depth(&a.sid), &a.pos, &a.sid).cmp(&(depth(&b.sid), &b.pos, &b.sid)));
        let mut orphans: Vec<String> = Vec::new();
        for e in creates {
            let Some(parent) = parent_ref(&e.parent, &created) else {
                // a parent deleted on this device: never "later" — an explicit orphan
                // conflict the user resolves (relocate / restore the folder / accept)
                if by_sid.get(e.parent.as_str()).is_some_and(|p| p.deleted) {
                    orphans.push(e.sid.clone());
                }
                continue; // otherwise the parent is not in the browser yet: next plan
            };
            let action = match (&e.kind[..], &e.url) {
                ("url", Some(u)) => ApplyAction::CreateBookmark {
                    parent,
                    title: e.title.clone(),
                    url: u.clone(),
                },
                _ => ApplyAction::CreateFolder {
                    parent,
                    title: e.title.clone(),
                },
            };
            if !push(&mut plan, action, &[&e.sid]) {
                break;
            }
            let op = plan.ops.last().unwrap().op_id;
            created.insert(e.sid.clone(), op);
            plan.creates.insert(op, e.sid.clone());
        }
        // 2. renames / URL changes / moves of nodes already in the browser
        let mut affected: BTreeSet<String> = created
            .keys()
            .filter_map(|s| by_sid.get(s.as_str()).map(|e| e.parent.clone()))
            .collect();
        for e in pending
            .iter()
            .filter(|e| !e.deleted && cid_of.contains_key(&e.sid))
        {
            if plan.more {
                break;
            }
            let id = cid_of[&e.sid].clone();
            let mut any = false;
            if e.fields.contains("title") || e.fields.contains("created") {
                any = true;
                if !push(
                    &mut plan,
                    ApplyAction::Rename {
                        id: id.clone(),
                        title: e.title.clone(),
                    },
                    &[&e.sid],
                ) {
                    break;
                }
            }
            if e.kind == "url" && (e.fields.contains("url") || e.fields.contains("created")) {
                if let Some(u) = &e.url {
                    any = true;
                    if !push(
                        &mut plan,
                        ApplyAction::SetUrl {
                            id: id.clone(),
                            url: u.clone(),
                        },
                        &[&e.sid],
                    ) {
                        break;
                    }
                }
            }
            if (e.fields.contains("parentId") || e.fields.contains("created"))
                && by_sid.get(e.parent.as_str()).is_some_and(|p| p.deleted)
            {
                orphans.push(e.sid.clone());
                continue;
            }
            if e.fields.contains("parentId") || e.fields.contains("created") {
                if let Some(parent) = parent_ref(&e.parent, &created) {
                    any = true;
                    affected.insert(e.parent.clone());
                    if !push(&mut plan, ApplyAction::Move { id, parent }, &[&e.sid]) {
                        break;
                    }
                }
            }
            if e.fields.contains("pos") {
                any = true;
                affected.insert(e.parent.clone());
            }
            if !any {
                clear_now.push(e.sid.clone()); // only bookkeeping fields changed
            }
        }
        // 3. exact order of every affected folder
        if !plan.more {
            for p in affected {
                let Some(parent) = parent_ref(&p, &created) else {
                    continue;
                };
                let mut kids: Vec<&E> = ents
                    .iter()
                    .filter(|e| !e.deleted && e.parent == p)
                    .collect();
                kids.sort_by(|a, b| (&a.pos, &a.sid).cmp(&(&b.pos, &b.sid)));
                let children: Vec<NodeRef> = kids
                    .iter()
                    .filter_map(|k| parent_ref(&k.sid, &created))
                    .collect();
                let tied: Vec<&str> = kids
                    .iter()
                    .filter(|k| {
                        k.pending
                            && !k.conflict
                            && (k.fields.contains("pos")
                                || created.contains_key(&k.sid)
                                || k.fields.contains("parentId"))
                    })
                    .map(|k| k.sid.as_str())
                    .collect();
                if children.len() < 2 {
                    continue;
                }
                if !push(&mut plan, ApplyAction::Reorder { parent, children }, &tied) {
                    break;
                }
            }
        }
        // 4. deletes, bottom-up
        let mut dels: Vec<&E> = pending.iter().copied().filter(|e| e.deleted).collect();
        dels.sort_by_key(|e| std::cmp::Reverse(depth(&e.sid)));
        for e in dels {
            match cid_of.get(&e.sid) {
                None => clear_now.push(e.sid.clone()), // never reached the browser
                Some(c) => {
                    if plan.more {
                        break;
                    }
                    let a = ApplyAction::Delete {
                        id: c.clone(),
                        folder: e.kind == "folder",
                    };
                    if !push(&mut plan, a, &[&e.sid]) {
                        break;
                    }
                    let op = plan.ops.last().unwrap().op_id;
                    plan.deletes.insert(op, e.sid.clone());
                }
            }
        }
        if !plan.more {
            // pending entities already in the browser that needed no operation (e.g. only the
            // key of a lone child changed): nothing to wait for
            for e in pending
                .iter()
                .filter(|e| !e.deleted && cid_of.contains_key(&e.sid))
            {
                if !plan.sid_ops.contains_key(&e.sid) && !orphans.contains(&e.sid) {
                    clear_now.push(e.sid.clone());
                }
            }
        }
        let tx = self.conn.transaction()?;
        for sid in &orphans {
            crate::conflict::create_orphan(&tx, &pid, sid, now)?;
        }
        for sid in clear_now {
            tx.execute(
                "UPDATE entities SET pending_apply = 0, pending_fields = '[]' WHERE profile_id = ?1 AND sid = ?2",
                params![pid, sid],
            )?;
            crate::conflict::finish_resolving(&tx, &pid, &sid, now)?;
        }
        if !plan.ops.is_empty() {
            tx.execute(
                "UPDATE profiles SET applying_since = strftime('%s','now') WHERE profile_id = ?1",
                [&pid],
            )?;
        }
        tx.commit()?;
        Ok(plan)
    }

    /// Record what the extension did. An entity leaves `pending_apply` only when every op
    /// tied to it succeeded; a failed op turns it into an `apply_failed` conflict; ops
    /// without a result (worker stopped) stay pending and are planned again.
    pub fn record_apply(
        &mut self,
        profile_id: Uuid,
        plan: &ApplyPlan,
        results: &[ApplyResult],
        now: i64,
    ) -> Result<ApplyRecord, StateError> {
        let pid = profile_id.to_string();
        let res: HashMap<u32, &ApplyResult> = results.iter().map(|r| (r.op_id, r)).collect();
        let ok = |op: u32| {
            res.get(&op).is_some_and(|r| {
                r.ok || (plan.deletes.contains_key(&op) && r.error.as_deref() == Some("not_found"))
            })
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (op, sid) in &plan.creates {
            if let Some(Some(cid)) = res.get(op).filter(|r| r.ok).map(|r| r.id.clone()) {
                tx.execute(
                    "INSERT INTO chromium_index (profile_id, chromium_id, sid) VALUES (?1, ?2, ?3)
                     ON CONFLICT(profile_id, chromium_id) DO UPDATE SET sid = excluded.sid",
                    params![pid, cid, sid],
                )?;
            }
        }
        for (op, sid) in &plan.deletes {
            if ok(*op) {
                tx.execute(
                    "DELETE FROM chromium_index WHERE profile_id = ?1 AND sid = ?2",
                    params![pid, sid],
                )?;
            }
        }
        let mut rec = ApplyRecord::default();
        let mut failed_sids: HashSet<&str> = HashSet::new();
        for (sid, ops) in &plan.sid_ops {
            if ops.iter().all(|o| ok(*o)) {
                tx.execute(
                    "UPDATE entities SET pending_apply = 0, pending_fields = '[]' WHERE profile_id = ?1 AND sid = ?2",
                    params![pid, sid],
                )?;
                crate::conflict::finish_resolving(&tx, &pid, sid, now)?;
                rec.applied += 1;
            } else if let Some(r) = ops.iter().filter_map(|o| res.get(o)).find(|r| !ok(r.op_id)) {
                failed_sids.insert(sid);
                let was_delete = plan.deletes.values().any(|d| d == sid);
                let error = r.error.clone().unwrap_or_else(|| "failed".into());
                crate::conflict::on_apply_failed(&tx, &pid, sid, &error, was_delete, now)?;
                if was_delete {
                    // A failed remote delete (e.g. folder not empty) leaves the node in the
                    // browser: restore the entity so the browser and the materialised state
                    // agree again (otherwise the next reconcile would re-create it under a new
                    // id); the remote delete is held in the conflict record.
                    tx.execute(
                        "UPDATE entities SET conflict = 1, pending_apply = 0, pending_fields = '[]',
                                deleted = 0
                         WHERE profile_id = ?1 AND sid = ?2",
                        params![pid, sid],
                    )?;
                } else {
                    // The browser still shows the old values: the entity stays frozen (its
                    // pending fields are kept for a retry) so the next reconcile never turns
                    // that stale view into local ops.
                    tx.execute(
                        "UPDATE entities SET conflict = 1 WHERE profile_id = ?1 AND sid = ?2",
                        params![pid, sid],
                    )?;
                }
                rec.failed += 1;
            } else {
                rec.incomplete += 1;
            }
        }
        tx.execute(
            "UPDATE profiles SET applying_since = NULL WHERE profile_id = ?1",
            [&pid],
        )?;
        tx.commit()?;
        Ok(rec)
    }

    /// Chromium id ↔ Synchrome id pairs of a profile (diagnostics/tests).
    pub fn index_of(&self, profile_id: Uuid, sid: &str) -> Result<Option<String>, StateError> {
        Ok(self
            .conn
            .query_row(
                "SELECT chromium_id FROM chromium_index WHERE profile_id = ?1 AND sid = ?2",
                params![profile_id.to_string(), sid],
                |r| r.get(0),
            )
            .optional()?)
    }
}
