//! Wire messages between the Synchrome extension and the native host (JSON per frame).

use serde::{Deserialize, Serialize};
use std::fmt;
pub use synchrome_state::{
    ApplyOp, ApplyResult, BookmarkSyncStatus, ExtApplyOp, ExtApplyResult, ExtStatus,
};
pub use synchrome_sync::bookmarks::{BookmarkEvent, ChromiumNode};
pub use synchrome_sync::extensions::{ExtEvent, ExtInfo};
use uuid::Uuid;

/// The mapping token as received from the extension. Redacted in `Debug`/`Display`; there is
/// no `Serialize`, so it can never be echoed back over the wire.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct WireToken(String);

impl WireToken {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Same rule as the server check `^[0-9a-f]{64}$`.
    pub fn is_well_formed(&self) -> bool {
        self.0.len() == 64
            && self
                .0
                .bytes()
                .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    }
    /// Short, safe hint for logs.
    pub fn hint(&self) -> String {
        format!("{}…", self.0.chars().take(4).collect::<String>())
    }
}
impl fmt::Debug for WireToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WireToken(<redacted>)")
    }
}

/// Extension -> app.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtToApp {
    /// First frame. `mapping_token` is whatever the extension found in `chrome.storage.local`.
    Hello {
        ext_version: String,
        #[serde(default)]
        browser: Option<String>,
        #[serde(default)]
        mapping_token: Option<WireToken>,
    },
    Ping,
    /// One part of a full `chrome.bookmarks.getTree()` capture (flattened nodes).
    BookmarksSnapshot {
        snapshot_id: String,
        epoch: String,
        /// Highest event number of `epoch` already reflected in this tree.
        #[serde(default)]
        covers_through: u64,
        part: u32,
        parts: u32,
        nodes: Vec<ChromiumNode>,
    },
    /// Incremental events, numbered within the extension's current `epoch`.
    BookmarksEvents {
        epoch: String,
        batch_id: String,
        events: Vec<NumberedEvent>,
    },
    /// Publish pending ops now (alarm / popup).
    SyncNow,
    /// Download remote changes and return the next apply plan (sent after the extension
    /// flushed its own events, so local changes always reach the app before an apply).
    BookmarksPull {
        pull_id: String,
    },
    /// Results of executing a `bookmarks_apply`.
    BookmarksApplied {
        apply_id: String,
        results: Vec<ApplyResult>,
    },
    /// One part of `chrome.management.getAll()` (reduced fields; never paths/permissions).
    ExtensionsSnapshot {
        snapshot_id: String,
        epoch: String,
        #[serde(default)]
        covers_through: u64,
        part: u32,
        parts: u32,
        items: Vec<ExtInfo>,
    },
    /// Incremental chrome.management events, numbered within the extension's epoch.
    ExtensionsEvents {
        epoch: String,
        batch_id: String,
        events: Vec<NumberedExtEvent>,
    },
    /// Download remote changes and return the next extensions apply plan.
    ExtensionsPull {
        pull_id: String,
    },
    /// Results of executing an `extensions_apply`.
    ExtensionsApplied {
        apply_id: String,
        results: Vec<ExtApplyResult>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct NumberedExtEvent {
    pub n: u64,
    #[serde(flatten)]
    pub event: ExtEvent,
}

/// Extensions limits (inventories are small; these are generous bounds).
pub const MAX_EXT_SNAPSHOT_ITEMS: usize = 1000;
pub const MAX_EXT_SNAPSHOT_PARTS: u32 = 16;
pub const MAX_EXT_EVENTS_PER_BATCH: usize = 500;
pub const MAX_EXT_APPLY_RESULTS: usize = synchrome_state::MAX_EXT_PLAN_OPS;
const MAX_EXT_TEXT: usize = 1024;

fn ext_info_ok(i: &ExtInfo) -> bool {
    synchrome_sync::extensions::valid_id(&i.id)
        && i.name.len() <= MAX_EXT_TEXT
        && i.version.len() <= 64
        && i.kind.len() <= 32
        && i.install_type.len() <= 32
        && i.update_url
            .as_ref()
            .map_or(true, |u| u.len() <= MAX_EXT_TEXT)
        && i.disabled_reason.as_ref().map_or(true, |r| r.len() <= 64)
}

/// An event with its delivery number.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct NumberedEvent {
    pub n: u64,
    #[serde(flatten)]
    pub event: BookmarkEvent,
}

/// Per-message limits on top of the frame limit (`framing::MAX_INBOUND`).
pub const MAX_EVENTS_PER_BATCH: usize = 1000;
pub const MAX_SNAPSHOT_PARTS: u32 = 128;
pub const MAX_SNAPSHOT_NODES: usize = 100_000;
pub const MAX_ID_LEN: usize = 64;

impl ExtToApp {
    /// Structural limits of bookmark messages (sizes, counts, ordering). `Err(code)`.
    pub fn validate(&self) -> Result<(), &'static str> {
        let id_ok = |s: &str| !s.is_empty() && s.len() <= MAX_ID_LEN;
        match self {
            ExtToApp::BookmarksSnapshot {
                snapshot_id,
                epoch,
                part,
                parts,
                nodes,
                ..
            } => {
                if !id_ok(snapshot_id) || !id_ok(epoch) {
                    return Err("invalid_message");
                }
                if *parts == 0 || *parts > MAX_SNAPSHOT_PARTS || part >= parts {
                    return Err("too_large");
                }
                if nodes.len() > MAX_SNAPSHOT_NODES {
                    return Err("too_large");
                }
                Ok(())
            }
            ExtToApp::BookmarksEvents {
                epoch,
                batch_id,
                events,
            } => {
                if !id_ok(epoch) || !id_ok(batch_id) || events.is_empty() {
                    return Err("invalid_message");
                }
                if events.len() > MAX_EVENTS_PER_BATCH {
                    return Err("too_large");
                }
                if !events.windows(2).all(|w| w[0].n < w[1].n) || events[0].n == 0 {
                    return Err("invalid_message");
                }
                Ok(())
            }
            ExtToApp::BookmarksPull { pull_id } => {
                if id_ok(pull_id) {
                    Ok(())
                } else {
                    Err("invalid_message")
                }
            }
            ExtToApp::BookmarksApplied { apply_id, results } => {
                if !id_ok(apply_id) {
                    return Err("invalid_message");
                }
                if results.len() > MAX_APPLY_RESULTS {
                    return Err("too_large");
                }
                Ok(())
            }
            ExtToApp::ExtensionsSnapshot {
                snapshot_id,
                epoch,
                part,
                parts,
                items,
                ..
            } => {
                if !id_ok(snapshot_id) || !id_ok(epoch) {
                    return Err("invalid_message");
                }
                if *parts == 0 || *parts > MAX_EXT_SNAPSHOT_PARTS || part >= parts {
                    return Err("too_large");
                }
                if items.len() > MAX_EXT_SNAPSHOT_ITEMS {
                    return Err("too_large");
                }
                if !items.iter().all(ext_info_ok) {
                    return Err("invalid_message");
                }
                Ok(())
            }
            ExtToApp::ExtensionsEvents {
                epoch,
                batch_id,
                events,
            } => {
                if !id_ok(epoch) || !id_ok(batch_id) || events.is_empty() {
                    return Err("invalid_message");
                }
                if events.len() > MAX_EXT_EVENTS_PER_BATCH {
                    return Err("too_large");
                }
                if !events.windows(2).all(|w| w[0].n < w[1].n) || events[0].n == 0 {
                    return Err("invalid_message");
                }
                let ok = events.iter().all(|e| match &e.event {
                    ExtEvent::Uninstalled { id } => synchrome_sync::extensions::valid_id(id),
                    ExtEvent::Installed { info }
                    | ExtEvent::Enabled { info }
                    | ExtEvent::Disabled { info } => ext_info_ok(info),
                });
                if !ok {
                    return Err("invalid_message");
                }
                Ok(())
            }
            ExtToApp::ExtensionsPull { pull_id } => {
                if id_ok(pull_id) {
                    Ok(())
                } else {
                    Err("invalid_message")
                }
            }
            ExtToApp::ExtensionsApplied { apply_id, results } => {
                if !id_ok(apply_id) {
                    return Err("invalid_message");
                }
                if results.len() > MAX_EXT_APPLY_RESULTS {
                    return Err("too_large");
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Apply plans sent to the extension are capped well below the 1 MB host→extension limit.
pub const MAX_APPLY_PLAN_BYTES: usize = 512 * 1024;
pub const MAX_APPLY_RESULTS: usize = 5000;

/// Every handshake outcome the app distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandshakeStatus {
    /// Token matched a mapping of the signed-in account for this device; confirmed.
    Connected,
    /// Synchrome Cloud is unreachable, but this device already confirmed this token for the
    /// signed-in account (local registry): capture continues into the durable outbox.
    ConnectedOffline,
    /// The extension store has no token (profile not mapped / not written yet).
    TokenMissing,
    /// No mapping with this token in the account and no local record of it.
    TokenUnknown,
    /// This device wrote the token while signed in to a different account.
    TokenOtherAccount,
    /// The token was written for a mapping that no longer exists.
    MappingDeleted,
    /// The mapping is gone and so is its cloud profile.
    CloudProfileDeleted,
    /// The mapping belongs to another device (token copied between machines?).
    WrongDevice,
    /// The app has no valid session.
    NotSignedIn,
    /// Synchrome Cloud could not be reached / answered with an error.
    CloudUnavailable,
}

impl HandshakeStatus {
    /// The four states the extension UI shows.
    pub fn ui_state(self) -> &'static str {
        match self {
            HandshakeStatus::Connected | HandshakeStatus::ConnectedOffline => "connected",
            HandshakeStatus::TokenMissing => "unmapped",
            HandshakeStatus::TokenUnknown
            | HandshakeStatus::TokenOtherAccount
            | HandshakeStatus::MappingDeleted
            | HandshakeStatus::CloudProfileDeleted
            | HandshakeStatus::WrongDevice => "token_mismatch",
            HandshakeStatus::NotSignedIn | HandshakeStatus::CloudUnavailable => "app_unavailable",
        }
    }
}

/// The only mapping information ever sent back to the extension. Never contains the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HandshakeReply {
    pub status: HandshakeStatus,
    pub ui_state: &'static str,
    pub mapped: bool,
    pub profile_id: Option<Uuid>,
    pub profile_name: Option<String>,
    pub device_id: Option<Uuid>,
    pub token_state: Option<&'static str>,
    pub detail: Option<String>,
}

impl HandshakeReply {
    pub fn status(status: HandshakeStatus) -> Self {
        Self {
            status,
            ui_state: status.ui_state(),
            mapped: false,
            profile_id: None,
            profile_name: None,
            device_id: None,
            token_state: None,
            detail: None,
        }
    }
    pub fn with_detail(mut self, d: impl Into<String>) -> Self {
        self.detail = Some(d.into());
        self
    }
}

/// App -> extension.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppToExt {
    HelloResult(HandshakeReply),
    Pong,
    Error {
        code: &'static str,
        message: String,
    },
    /// Sent after the ops of a bookmarks message are durable (or why not).
    BookmarksAck {
        batch_id: String,
        result: AckResult,
        ops: usize,
        pending: u64,
    },
    /// Publication state after an ack or `sync_now`.
    SyncStatus {
        bookmarks: BookmarkSyncStatus,
        /// Extensions counts (absent when unknown).
        #[serde(skip_serializing_if = "Option::is_none")]
        extensions: Option<ExtStatus>,
    },
    /// Sent after the ops of an extensions message are durable (or why not).
    ExtensionsAck {
        batch_id: String,
        result: AckResult,
        ops: usize,
        pending: u64,
    },
    /// Explicit chrome.management operations (may be empty). `user_actions` lists what needs
    /// a click in the popup (Chromium refused without a user gesture): `(id, action)`.
    ExtensionsApply {
        pull_id: String,
        apply_id: String,
        ops: Vec<ExtApplyOp>,
        user_actions: Vec<(String, String)>,
    },
    /// Explicit chrome.bookmarks operations to execute, in order (may be empty).
    BookmarksApply {
        pull_id: String,
        apply_id: String,
        ops: Vec<ApplyOp>,
        /// More remote changes remain: pull again after reporting these results.
        more: bool,
    },
}

impl AppToExt {
    /// Wire `type` of the message (diagnostics: logged instead of any content).
    pub fn kind(&self) -> &'static str {
        match self {
            AppToExt::HelloResult(_) => "hello_result",
            AppToExt::Pong => "pong",
            AppToExt::Error { .. } => "error",
            AppToExt::BookmarksAck { .. } => "bookmarks_ack",
            AppToExt::SyncStatus { .. } => "sync_status",
            AppToExt::BookmarksApply { .. } => "bookmarks_apply",
            AppToExt::ExtensionsAck { .. } => "extensions_ack",
            AppToExt::ExtensionsApply { .. } => "extensions_apply",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AckResult {
    /// Ops written to the outbox (possibly zero).
    Applied,
    /// Already applied earlier (redelivery).
    Duplicate,
    /// A snapshot part was received; the snapshot is applied when the last part arrives.
    Partial,
    /// An event referenced an unknown node; nothing written; send a full snapshot.
    ResyncRequired,
    /// Refused (not confirmed, source conflict, …); see the preceding `error`.
    Rejected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_parses_and_redacts_token() {
        let t = "ab".repeat(32);
        let m: ExtToApp = serde_json::from_str(&format!(
            r#"{{"type":"hello","ext_version":"0.1.0","browser":"helium","mapping_token":"{t}"}}"#
        ))
        .unwrap();
        let dbg = format!("{m:?}");
        assert!(!dbg.contains(&t), "Debug leaked the token");
        match m {
            ExtToApp::Hello {
                mapping_token: Some(tok),
                ..
            } => assert!(tok.is_well_formed()),
            _ => panic!("expected hello"),
        }
    }

    #[test]
    fn hello_without_token_is_valid() {
        let m: ExtToApp =
            serde_json::from_str(r#"{"type":"hello","ext_version":"0.1.0"}"#).unwrap();
        assert!(matches!(
            m,
            ExtToApp::Hello {
                mapping_token: None,
                ..
            }
        ));
    }

    #[test]
    fn reply_serializes_flat_with_ui_state_and_no_token() {
        let mut r = HandshakeReply::status(HandshakeStatus::Connected);
        r.mapped = true;
        r.profile_name = Some("Personal".into());
        let v = serde_json::to_value(AppToExt::HelloResult(r)).unwrap();
        assert_eq!(v["type"], "hello_result");
        assert_eq!(v["status"], "connected");
        assert_eq!(v["ui_state"], "connected");
        assert!(v.get("mapping_token").is_none());
    }

    #[test]
    fn bookmark_messages_parse_and_validate() {
        let snap: ExtToApp = serde_json::from_str(
            r#"{"type":"bookmarks_snapshot","snapshot_id":"s1","epoch":"e1","covers_through":3,"part":0,"parts":2,
                "nodes":[{"id":"1","parent_id":"0","index":0,"title":"Bookmarks bar","folder_type":"bookmarks-bar"}]}"#,
        )
        .unwrap();
        assert!(snap.validate().is_ok());
        let ev: ExtToApp = serde_json::from_str(
            r#"{"type":"bookmarks_events","epoch":"e1","batch_id":"e1:1","events":[
                {"n":1,"kind":"created","id":"20","node":{"id":"20","parent_id":"1","index":0,"title":"x","url":"https://x/"}},
                {"n":2,"kind":"changed","id":"20","title":"y"},
                {"n":3,"kind":"moved","id":"20","parent_id":"2","index":0},
                {"n":4,"kind":"reordered","id":"2","child_ids":["20"]},
                {"n":5,"kind":"removed","id":"20"}]}"#,
        )
        .unwrap();
        assert!(ev.validate().is_ok());
        let bad_order: ExtToApp = serde_json::from_str(
            r#"{"type":"bookmarks_events","epoch":"e1","batch_id":"b","events":[
                {"n":2,"kind":"removed","id":"1"},{"n":2,"kind":"removed","id":"2"}]}"#,
        )
        .unwrap();
        assert_eq!(bad_order.validate(), Err("invalid_message"));
        let many = format!(
            r#"{{"type":"bookmarks_events","epoch":"e","batch_id":"b","events":[{}]}}"#,
            (1..=MAX_EVENTS_PER_BATCH as u64 + 1)
                .map(|n| format!(r#"{{"n":{n},"kind":"removed","id":"1"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let many: ExtToApp = serde_json::from_str(&many).unwrap();
        assert_eq!(many.validate(), Err("too_large"));
        let parts: ExtToApp = serde_json::from_str(
            r#"{"type":"bookmarks_snapshot","snapshot_id":"s","epoch":"e","part":0,"parts":129,"nodes":[]}"#,
        )
        .unwrap();
        assert_eq!(parts.validate(), Err("too_large"));
        assert!(matches!(
            serde_json::from_str::<ExtToApp>(r#"{"type":"sync_now"}"#).unwrap(),
            ExtToApp::SyncNow
        ));
    }

    #[test]
    fn apply_messages_round_trip_and_are_bounded() {
        let ops = vec![
            ApplyOp {
                op_id: 1,
                action: synchrome_state::ApplyAction::CreateFolder {
                    parent: synchrome_state::NodeRef::Id { id: "1".into() },
                    title: "F".into(),
                },
            },
            ApplyOp {
                op_id: 2,
                action: synchrome_state::ApplyAction::CreateBookmark {
                    parent: synchrome_state::NodeRef::Op { op: 1 },
                    title: "B".into(),
                    url: "https://x/".into(),
                },
            },
            ApplyOp {
                op_id: 3,
                action: synchrome_state::ApplyAction::Delete {
                    id: "9".into(),
                    folder: true,
                },
            },
        ];
        let v = serde_json::to_value(AppToExt::BookmarksApply {
            pull_id: "p".into(),
            apply_id: "a".into(),
            ops,
            more: false,
        })
        .unwrap();
        assert_eq!(v["type"], "bookmarks_apply");
        assert_eq!(v["ops"][0]["action"], "create_folder");
        assert_eq!(v["ops"][0]["parent"]["id"], "1");
        assert_eq!(v["ops"][1]["parent"]["op"], 1);
        let back: ExtToApp = serde_json::from_str(
            r#"{"type":"bookmarks_applied","apply_id":"a","results":[{"op_id":1,"ok":true,"id":"55"},{"op_id":3,"ok":false,"error":"not_empty"}]}"#,
        )
        .unwrap();
        assert!(back.validate().is_ok());
        let many = format!(
            r#"{{"type":"bookmarks_applied","apply_id":"a","results":[{}]}}"#,
            (0..=MAX_APPLY_RESULTS)
                .map(|i| format!(r#"{{"op_id":{i},"ok":true}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let many: ExtToApp = serde_json::from_str(&many).unwrap();
        assert_eq!(many.validate(), Err("too_large"));
        let pull: ExtToApp =
            serde_json::from_str(r#"{"type":"bookmarks_pull","pull_id":""}"#).unwrap();
        assert_eq!(pull.validate(), Err("invalid_message"));
    }

    #[test]
    fn ack_and_status_serialize() {
        let v = serde_json::to_value(AppToExt::BookmarksAck {
            batch_id: "e1:1".into(),
            result: AckResult::ResyncRequired,
            ops: 0,
            pending: 3,
        })
        .unwrap();
        assert_eq!(v["type"], "bookmarks_ack");
        assert_eq!(v["result"], "resync_required");
        assert_eq!(
            AppToExt::BookmarksAck {
                batch_id: String::new(),
                result: AckResult::Applied,
                ops: 0,
                pending: 0
            }
            .kind(),
            "bookmarks_ack"
        );
        assert_eq!(HandshakeStatus::ConnectedOffline.ui_state(), "connected");
    }

    #[test]
    fn extension_messages_parse_validate_and_are_bounded() {
        let id = "aaaabbbbccccddddeeeeffffgggghhhh";
        let item = format!(
            r#"{{"id":"{id}","name":"X","version":"1.0","enabled":true,"type":"extension","install_type":"normal","may_disable":true}}"#
        );
        let snap: ExtToApp = serde_json::from_str(&format!(
            r#"{{"type":"extensions_snapshot","snapshot_id":"s","epoch":"e","part":0,"parts":1,"items":[{item}]}}"#
        ))
        .unwrap();
        assert!(snap.validate().is_ok());
        let ev: ExtToApp = serde_json::from_str(&format!(
            r#"{{"type":"extensions_events","epoch":"e","batch_id":"b","events":[
                {{"n":1,"kind":"installed","info":{item}}},{{"n":2,"kind":"disabled","info":{item}}},
                {{"n":3,"kind":"enabled","info":{item}}},{{"n":4,"kind":"uninstalled","id":"{id}"}}]}}"#
        ))
        .unwrap();
        assert!(ev.validate().is_ok());
        let bad_id: ExtToApp = serde_json::from_str(
            r#"{"type":"extensions_events","epoch":"e","batch_id":"b","events":[{"n":1,"kind":"uninstalled","id":"../../x"}]}"#,
        )
        .unwrap();
        assert_eq!(bad_id.validate(), Err("invalid_message"));
        let many = format!(
            r#"{{"type":"extensions_snapshot","snapshot_id":"s","epoch":"e","part":0,"parts":1,"items":[{}]}}"#,
            vec![item.clone(); MAX_EXT_SNAPSHOT_ITEMS + 1].join(",")
        );
        let many: ExtToApp = serde_json::from_str(&many).unwrap();
        assert_eq!(many.validate(), Err("too_large"));
        let long_name = item.replace(
            r#""name":"X""#,
            &format!(r#""name":"{}""#, "n".repeat(2000)),
        );
        let long: ExtToApp = serde_json::from_str(&format!(
            r#"{{"type":"extensions_snapshot","snapshot_id":"s","epoch":"e","part":0,"parts":1,"items":[{long_name}]}}"#
        ))
        .unwrap();
        assert_eq!(long.validate(), Err("invalid_message"));
        let applied: ExtToApp = serde_json::from_str(
            r#"{"type":"extensions_applied","apply_id":"a","results":[{"op_id":1,"ok":false,"error":"gesture_required"}]}"#,
        )
        .unwrap();
        assert!(applied.validate().is_ok());
        let v = serde_json::to_value(AppToExt::ExtensionsApply {
            pull_id: "p".into(),
            apply_id: "a".into(),
            ops: vec![ExtApplyOp {
                op_id: 1,
                action: synchrome_state::ExtAction::SetEnabled {
                    id: id.into(),
                    enabled: false,
                },
            }],
            user_actions: vec![],
        })
        .unwrap();
        assert_eq!(v["type"], "extensions_apply");
        assert_eq!(v["ops"][0]["action"], "set_enabled");
        assert_eq!(v["ops"][0]["enabled"], false);
    }

    #[test]
    fn ui_state_mapping_covers_the_four_states() {
        use HandshakeStatus::*;
        assert_eq!(Connected.ui_state(), "connected");
        assert_eq!(TokenMissing.ui_state(), "unmapped");
        for s in [
            TokenUnknown,
            TokenOtherAccount,
            MappingDeleted,
            CloudProfileDeleted,
            WrongDevice,
        ] {
            assert_eq!(s.ui_state(), "token_mismatch");
        }
        for s in [NotSignedIn, CloudUnavailable] {
            assert_eq!(s.ui_state(), "app_unavailable");
        }
    }
}
