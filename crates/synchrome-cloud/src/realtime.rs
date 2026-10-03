//! Supabase Realtime wake-up protocol (Phoenix v1 JSON over WebSocket), pure: URLs, outbound
//! frames and inbound parsing. The socket, timers and reconnection live in the app.
//!
//! One channel per signed-in account subscribes to `postgres_changes` INSERT on
//! `public.store_events` with the user's access token; Realtime filters rows with the table's
//! RLS (`account_id = auth.uid()`). An inbound change is reduced to `(profile_id, device_id)`:
//! a hint that something was published — the payload is never applied.

use crate::client::{CloudClient, RestTransport};
use serde_json::{json, Value};
use synchrome_auth::Session;
use uuid::Uuid;

impl<T: RestTransport> CloudClient<T> {
    /// The Realtime WebSocket URL of this instance.
    pub fn realtime_url(&self) -> String {
        websocket_url(&self.cfg.url, &self.cfg.anon_key)
    }

    /// The join frame for the account's channel (the access token stays in this crate).
    pub fn realtime_join(&self, session: &Session, r: u64) -> String {
        join(
            &topic(&session.user_id),
            &session.user_id,
            session.access_token.expose(),
            r,
        )
    }

    /// A refreshed token for the account's channel.
    pub fn realtime_token(&self, session: &Session, r: u64) -> String {
        access_token(&topic(&session.user_id), session.access_token.expose(), r)
    }
}

/// `wss://<host>/realtime/v1/websocket?apikey=<anon>&vsn=1.0.0` (the anon key is public).
pub fn websocket_url(supabase_url: &str, anon_key: &str) -> String {
    let base = supabase_url.trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{ws}/realtime/v1/websocket?apikey={anon_key}&vsn=1.0.0")
}

/// The account's channel topic (opaque to the server; RLS does the scoping).
pub fn topic(account_id: &str) -> String {
    format!("realtime:synchrome-store-events-{account_id}")
}

pub fn join(topic: &str, account_id: &str, access_token: &str, r: u64) -> String {
    json!({
        "topic": topic,
        "event": "phx_join",
        "payload": {
            "config": {
                "broadcast": { "self": false },
                "presence": { "key": "" },
                "postgres_changes": [{
                    "event": "INSERT", "schema": "public", "table": "store_events",
                    "filter": format!("account_id=eq.{account_id}"),
                }],
            },
            "access_token": access_token,
        },
        "ref": r.to_string(),
        "join_ref": r.to_string(),
    })
    .to_string()
}

pub fn heartbeat(r: u64) -> String {
    json!({ "topic": "phoenix", "event": "heartbeat", "payload": {}, "ref": r.to_string() })
        .to_string()
}

/// A refreshed access token for an existing channel.
pub fn access_token(topic: &str, token: &str, r: u64) -> String {
    json!({ "topic": topic, "event": "access_token", "payload": { "access_token": token }, "ref": r.to_string() })
        .to_string()
}

pub fn leave(topic: &str, r: u64) -> String {
    json!({ "topic": topic, "event": "phx_leave", "payload": {}, "ref": r.to_string() }).to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    /// The join (or another request) was acknowledged.
    Ok {
        r: Option<String>,
    },
    /// The server refused a request (bad token, unknown table, …): reconnect with backoff.
    Error {
        r: Option<String>,
        reason: String,
    },
    /// A store_events INSERT: something was published for this profile by this device.
    Published {
        profile_id: Uuid,
        device_id: Uuid,
    },
    /// Subscription status from the server (`system` event); `ok = false` means not subscribed.
    System {
        ok: bool,
    },
    /// The channel was closed by the server.
    Closed,
    Other,
}

pub fn parse(text: &str) -> Inbound {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Inbound::Other;
    };
    let event = v.get("event").and_then(Value::as_str).unwrap_or_default();
    let r = v.get("ref").and_then(Value::as_str).map(str::to_string);
    let payload = v.get("payload").cloned().unwrap_or(Value::Null);
    match event {
        "phx_reply" => {
            let status = payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if status == "ok" {
                Inbound::Ok { r }
            } else {
                let reason = payload
                    .get("response")
                    .and_then(|x| x.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or(status)
                    .chars()
                    .take(120)
                    .collect();
                Inbound::Error { r, reason }
            }
        }
        "phx_error" => Inbound::Error {
            r,
            reason: "channel error".into(),
        },
        "phx_close" => Inbound::Closed,
        "system" => Inbound::System {
            ok: payload.get("status").and_then(Value::as_str) == Some("ok"),
        },
        "postgres_changes" => {
            let rec = payload.get("data").and_then(|d| d.get("record"));
            let get = |k: &str| {
                rec.and_then(|r| r.get(k))
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok())
            };
            match (get("profile_id"), get("device_id")) {
                (Some(profile_id), Some(device_id)) => Inbound::Published {
                    profile_id,
                    device_id,
                },
                _ => Inbound::Other,
            }
        }
        _ => Inbound::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_frames() {
        assert_eq!(
            websocket_url("https://sb.example/", "anon"),
            "wss://sb.example/realtime/v1/websocket?apikey=anon&vsn=1.0.0"
        );
        let j: Value = serde_json::from_str(&join("realtime:t", "acc", "tok", 1)).unwrap();
        assert_eq!(j["event"], "phx_join");
        assert_eq!(j["payload"]["access_token"], "tok");
        assert_eq!(
            j["payload"]["config"]["postgres_changes"][0]["table"],
            "store_events"
        );
        assert_eq!(
            j["payload"]["config"]["postgres_changes"][0]["filter"],
            "account_id=eq.acc"
        );
        let h: Value = serde_json::from_str(&heartbeat(7)).unwrap();
        assert_eq!(
            (h["topic"].as_str(), h["ref"].as_str()),
            (Some("phoenix"), Some("7"))
        );
        let a: Value = serde_json::from_str(&access_token("realtime:t", "new", 8)).unwrap();
        assert_eq!(a["payload"]["access_token"], "new");
    }

    #[test]
    fn inbound_messages() {
        let p = Uuid::new_v4();
        let d = Uuid::new_v4();
        let change = json!({
            "topic": "realtime:t", "event": "postgres_changes", "ref": null,
            "payload": {"data": {"type": "INSERT", "table": "store_events", "schema": "public",
                "record": {"profile_id": p, "device_id": d, "path": "x", "seq_from": 1, "seq_to": 2}}, "ids": [1]}
        });
        assert_eq!(
            parse(&change.to_string()),
            Inbound::Published {
                profile_id: p,
                device_id: d
            }
        );
        assert_eq!(
            parse(r#"{"event":"phx_reply","ref":"1","payload":{"status":"ok","response":{}}}"#),
            Inbound::Ok {
                r: Some("1".into())
            }
        );
        assert!(matches!(
            parse(
                r#"{"event":"phx_reply","ref":"1","payload":{"status":"error","response":{"reason":"Invalid JWT"}}}"#
            ),
            Inbound::Error { .. }
        ));
        assert_eq!(
            parse(r#"{"event":"system","payload":{"status":"ok"}}"#),
            Inbound::System { ok: true }
        );
        assert_eq!(
            parse(r#"{"event":"phx_close","payload":{}}"#),
            Inbound::Closed
        );
        assert_eq!(parse("not json"), Inbound::Other);
        let bad = json!({"event": "postgres_changes", "payload": {"data": {"record": {"profile_id": "x"}}}});
        assert_eq!(parse(&bad.to_string()), Inbound::Other);
    }
}
