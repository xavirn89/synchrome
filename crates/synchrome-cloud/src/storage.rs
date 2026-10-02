//! Synchrome Cloud store objects (Supabase Storage, bucket `stores`) and `store_events`.
//!
//! Object names are `<accountId>/<path relative to the store root>` (`docs/04` §3). The
//! account prefix is always the session's own user id, and the bucket's RLS policies
//! (migration 0004) reject anything else, so a client cannot read or write another account's
//! objects even if it tried.

use crate::client::{auth_headers, CloudClient, CloudError, RestTransport};
use serde::{Deserialize, Serialize};
use synchrome_auth::Session;
use uuid::Uuid;

pub const STORE_BUCKET: &str = "stores";
pub const JOURNAL_CONTENT_TYPE: &str = "application/x-ndjson";

/// Validate a store-relative path: no absolute paths, no `..`, conservative charset.
pub fn valid_rel_path(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.ends_with('/')
        && rel
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && rel
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'-' | b'_' | b'.'))
}

#[derive(Deserialize, Default)]
struct StorageErr {
    #[serde(default, rename = "statusCode")]
    status_code: Option<serde_json::Value>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Map a Storage API failure. Storage often answers HTTP 400 with the real status in the
/// body's `statusCode` (e.g. `"403"` for an RLS violation, `"404"` for a missing object).
pub fn classify_storage(status: u16, bytes: &[u8]) -> CloudError {
    let e: StorageErr = serde_json::from_slice(bytes).unwrap_or_default();
    let code = match &e.status_code {
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(status),
        Some(serde_json::Value::Number(n)) => n.as_u64().map(|n| n as u16).unwrap_or(status),
        _ => status,
    };
    let msg: String = e
        .message
        .or(e.error)
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    match code {
        500..=599 => CloudError::Server(code),
        401 => CloudError::Unauthorized,
        403 => CloudError::Forbidden,
        404 if msg.to_ascii_lowercase().contains("bucket") => CloudError::SchemaMissing(msg),
        404 => CloudError::NotFound,
        409 => CloudError::Conflict(msg),
        413 => CloudError::InvalidValue("object too large".into()),
        _ => CloudError::Storage(code, msg),
    }
}

/// A row of `public.store_events` (metadata of one journal publication).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreEvent {
    pub profile_id: Uuid,
    pub device_id: Uuid,
    pub path: String,
    pub seq_from: i64,
    pub seq_to: i64,
    pub hlc: String,
    #[serde(default, skip_serializing)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageEntry {
    /// Store-relative path.
    pub path: String,
    pub is_folder: bool,
}

enum Body {
    None,
    Json(serde_json::Value),
    Raw(Vec<u8>),
}

impl<T: RestTransport> CloudClient<T> {
    fn object_name(&self, session: &Session, rel: &str) -> Result<String, CloudError> {
        if !valid_rel_path(rel) || Uuid::parse_str(&session.user_id).is_err() {
            return Err(CloudError::InvalidValue("invalid store path".into()));
        }
        Ok(format!("{}/{rel}", session.user_id))
    }

    async fn storage(
        &self,
        session: &Session,
        method: &str,
        path: &str,
        extra: &[(&str, &str)],
        body: Body,
    ) -> Result<Vec<u8>, CloudError> {
        let owned = auth_headers(&self.cfg, session);
        let mut headers: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        headers.extend_from_slice(extra);
        let url = format!("{}/storage/v1{path}", self.cfg.url.trim_end_matches('/'));
        let res = match body {
            Body::None => self.transport.request(method, &url, &headers, None).await,
            Body::Json(v) => {
                self.transport
                    .request(method, &url, &headers, Some(&v))
                    .await
            }
            Body::Raw(b) => self.transport.request_raw(method, &url, &headers, b).await,
        };
        let (status, bytes) = res.map_err(|e| CloudError::Network(e.to_string()))?;
        if (200..=299).contains(&status) {
            Ok(bytes)
        } else {
            Err(classify_storage(status, &bytes))
        }
    }

    /// Upload (create or replace) one store object.
    pub async fn put_object(
        &self,
        session: &Session,
        rel: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<(), CloudError> {
        let name = self.object_name(session, rel)?;
        self.storage(
            session,
            "POST",
            &format!("/object/{STORE_BUCKET}/{name}"),
            &[("x-upsert", "true"), ("Content-Type", content_type)],
            Body::Raw(bytes),
        )
        .await
        .map(|_| ())
    }

    /// Download one store object; `None` if it does not exist (or is not this account's).
    pub async fn get_object(
        &self,
        session: &Session,
        rel: &str,
    ) -> Result<Option<Vec<u8>>, CloudError> {
        let name = self.object_name(session, rel)?;
        match self
            .storage(
                session,
                "GET",
                &format!("/object/authenticated/{STORE_BUCKET}/{name}"),
                &[],
                Body::None,
            )
            .await
        {
            Ok(b) => Ok(Some(b)),
            Err(CloudError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Download by FULL object name (any account prefix). Exists so validation can prove
    /// that another account's object is unreachable: RLS answers "not found".
    pub async fn get_object_by_full_name(
        &self,
        session: &Session,
        full_name: &str,
    ) -> Result<Option<Vec<u8>>, CloudError> {
        let ok = full_name
            .split_once('/')
            .is_some_and(|(a, rel)| Uuid::parse_str(a).is_ok() && valid_rel_path(rel));
        if !ok {
            return Err(CloudError::InvalidValue("invalid object name".into()));
        }
        match self
            .storage(
                session,
                "GET",
                &format!("/object/authenticated/{STORE_BUCKET}/{full_name}"),
                &[],
                Body::None,
            )
            .await
        {
            Ok(b) => Ok(Some(b)),
            Err(CloudError::NotFound) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Upload by FULL object name (any account prefix). Exists so validation can prove that
    /// writing under another account's prefix is refused by RLS.
    pub async fn put_object_by_full_name(
        &self,
        session: &Session,
        full_name: &str,
        bytes: Vec<u8>,
    ) -> Result<(), CloudError> {
        let ok = full_name
            .split_once('/')
            .is_some_and(|(a, rel)| Uuid::parse_str(a).is_ok() && valid_rel_path(rel));
        if !ok {
            return Err(CloudError::InvalidValue("invalid object name".into()));
        }
        self.storage(
            session,
            "POST",
            &format!("/object/{STORE_BUCKET}/{full_name}"),
            &[("x-upsert", "true"), ("Content-Type", JOURNAL_CONTENT_TYPE)],
            Body::Raw(bytes),
        )
        .await
        .map(|_| ())
    }

    /// Names listed under a FULL prefix (any account). Validation only: RLS returns nothing
    /// for another account's prefix.
    pub async fn list_full_prefix(
        &self,
        session: &Session,
        full_prefix: &str,
    ) -> Result<usize, CloudError> {
        let bytes = self
            .storage(
                session,
                "POST",
                &format!("/object/list/{STORE_BUCKET}"),
                &[],
                Body::Json(
                    serde_json::json!({ "prefix": full_prefix, "limit": 1000, "offset": 0 }),
                ),
            )
            .await?;
        serde_json::from_slice::<Vec<serde_json::Value>>(&bytes)
            .map(|v| v.len())
            .map_err(|e| CloudError::InvalidResponse(format!("storage list: {e}")))
    }

    /// One level of a store "folder".
    pub async fn list_dir(
        &self,
        session: &Session,
        rel_dir: &str,
    ) -> Result<Vec<StorageEntry>, CloudError> {
        let name = self.object_name(session, rel_dir)?;
        #[derive(Deserialize)]
        struct Item {
            name: String,
            #[serde(default)]
            id: Option<serde_json::Value>,
        }
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let bytes = self
                .storage(
                    session,
                    "POST",
                    &format!("/object/list/{STORE_BUCKET}"),
                    &[],
                    Body::Json(serde_json::json!({
                        "prefix": name, "limit": 1000, "offset": offset,
                        "sortBy": { "column": "name", "order": "asc" }
                    })),
                )
                .await?;
            let items: Vec<Item> = serde_json::from_slice(&bytes)
                .map_err(|e| CloudError::InvalidResponse(format!("storage list: {e}")))?;
            let n = items.len();
            for it in items {
                if it.name.is_empty() || it.name.starts_with('.') {
                    continue; // placeholder objects
                }
                out.push(StorageEntry {
                    path: format!("{rel_dir}/{}", it.name),
                    is_folder: it.id.as_ref().map_or(true, |v| v.is_null()),
                });
            }
            if n < 1000 {
                break;
            }
            offset += n;
        }
        Ok(out)
    }

    /// Every object under a store folder, recursively (store-relative paths).
    pub async fn list_tree(
        &self,
        session: &Session,
        rel_dir: &str,
    ) -> Result<Vec<String>, CloudError> {
        let mut out = Vec::new();
        let mut dirs = vec![rel_dir.to_string()];
        while let Some(d) = dirs.pop() {
            for e in self.list_dir(session, &d).await? {
                if e.is_folder {
                    dirs.push(e.path);
                } else {
                    out.push(e.path);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Delete store objects (store-relative paths), in batches.
    pub async fn delete_objects(
        &self,
        session: &Session,
        rels: &[String],
    ) -> Result<usize, CloudError> {
        let mut deleted = 0;
        for batch in rels.chunks(100) {
            let names: Vec<String> = batch
                .iter()
                .map(|r| self.object_name(session, r))
                .collect::<Result<_, _>>()?;
            let bytes = self
                .storage(
                    session,
                    "DELETE",
                    &format!("/object/{STORE_BUCKET}"),
                    &[],
                    Body::Json(serde_json::json!({ "prefixes": names })),
                )
                .await?;
            deleted += serde_json::from_slice::<Vec<serde_json::Value>>(&bytes)
                .map(|v| v.len())
                .unwrap_or(0);
        }
        Ok(deleted)
    }

    /// Device ids that have journal objects for a profile (first level of `journal/`).
    pub async fn journal_devices(
        &self,
        session: &Session,
        profile_id: Uuid,
    ) -> Result<Vec<String>, CloudError> {
        Ok(self
            .list_dir(session, &format!("profiles/{profile_id}/journal"))
            .await?
            .into_iter()
            .filter(|e| e.is_folder)
            .filter_map(|e| e.path.rsplit('/').next().map(str::to_string))
            .collect())
    }

    /// Remove every store object of a profile (before deleting the profile row).
    pub async fn delete_profile_objects(
        &self,
        session: &Session,
        profile_id: Uuid,
    ) -> Result<usize, CloudError> {
        let all = self
            .list_tree(session, &format!("profiles/{profile_id}"))
            .await?;
        self.delete_objects(session, &all).await
    }

    /// Record a journal publication. Idempotent: the row is unique per
    /// `(profile_id, device_id, seq_to)` and duplicates are ignored.
    pub async fn record_store_event(
        &self,
        session: &Session,
        ev: &StoreEvent,
    ) -> Result<(), CloudError> {
        let body = serde_json::to_value(ev).map_err(|e| CloudError::InvalidValue(e.to_string()))?;
        self.rest_call(
            session,
            "POST",
            "/store_events?on_conflict=profile_id,device_id,seq_to",
            &[
                ("Prefer", "resolution=ignore-duplicates,return=minimal"),
                ("Content-Type", "application/json"),
            ],
            Some(&body),
        )
        .await
        .map(|_| ())
    }

    /// Delete one publication row of this account (returns whether a row was deleted).
    /// Used by validation to simulate a missing event; RLS limits it to own rows.
    pub async fn delete_store_event(
        &self,
        session: &Session,
        profile_id: Uuid,
        device_id: Uuid,
        seq_to: i64,
    ) -> Result<bool, CloudError> {
        let bytes = self
            .rest_call(
                session,
                "DELETE",
                &format!(
                    "/store_events?profile_id=eq.{profile_id}&device_id=eq.{device_id}&seq_to=eq.{seq_to}&select=id"
                ),
                &[("Prefer", "return=representation")],
                None,
            )
            .await?;
        Ok(serde_json::from_slice::<Vec<serde_json::Value>>(&bytes)
            .map(|v| !v.is_empty())
            .unwrap_or(false))
    }

    pub async fn list_store_events(
        &self,
        session: &Session,
        profile_id: Uuid,
    ) -> Result<Vec<StoreEvent>, CloudError> {
        let bytes = self
            .rest_call(
                session,
                "GET",
                &format!(
                    "/store_events?profile_id=eq.{profile_id}&select=profile_id,device_id,path,seq_from,seq_to,hlc,created_at&order=seq_to.asc"
                ),
                &[],
                None,
            )
            .await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| CloudError::InvalidResponse(format!("store_events: {e}")))
    }

    async fn rest_call(
        &self,
        session: &Session,
        method: &str,
        path: &str,
        extra: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> Result<Vec<u8>, CloudError> {
        self.call(session, method, path, extra, body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::RestFuture;
    use std::sync::Mutex;
    use synchrome_auth::Secret;
    use synchrome_config::SupabaseConfig;

    type Seen = (String, String, Vec<(String, String)>, Vec<u8>);

    struct Mock {
        replies: Mutex<Vec<(u16, Vec<u8>)>>,
        seen: Mutex<Vec<Seen>>,
    }
    impl Mock {
        fn new(r: Vec<(u16, &str)>) -> Self {
            Self {
                replies: Mutex::new(
                    r.into_iter()
                        .map(|(s, b)| (s, b.as_bytes().to_vec()))
                        .collect(),
                ),
                seen: Mutex::new(vec![]),
            }
        }
        fn push(&self, m: &str, u: &str, h: &[(&str, &str)], b: Vec<u8>) -> RestFuture<'_> {
            self.seen.lock().unwrap().push((
                m.into(),
                u.into(),
                h.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                b,
            ));
            let r = self.replies.lock().unwrap().remove(0);
            Box::pin(async move { Ok(r) })
        }
    }
    impl RestTransport for Mock {
        fn request<'a>(
            &'a self,
            m: &'a str,
            u: &'a str,
            h: &'a [(&'a str, &'a str)],
            b: Option<&'a serde_json::Value>,
        ) -> RestFuture<'a> {
            self.push(
                m,
                u,
                h,
                b.map(|v| v.to_string().into_bytes()).unwrap_or_default(),
            )
        }
        fn request_raw<'a>(
            &'a self,
            m: &'a str,
            u: &'a str,
            h: &'a [(&'a str, &'a str)],
            b: Vec<u8>,
        ) -> RestFuture<'a> {
            self.push(m, u, h, b)
        }
    }

    const ACCT: &str = "36542536-7edf-4fbb-81c0-4426683f62a0";

    fn session() -> Session {
        Session {
            user_id: ACCT.into(),
            email: Some("a@example.invalid".into()),
            access_token: Secret::new("jwt-a"),
            refresh_token: Secret::new("r"),
            expires_at: i64::MAX,
            token_type: "bearer".into(),
        }
    }
    fn client(m: Mock) -> CloudClient<Mock> {
        CloudClient::with_transport(
            SupabaseConfig {
                url: "https://sb.example".into(),
                anon_key: "anon".into(),
            },
            m,
        )
    }

    #[tokio::test]
    async fn upload_is_upsert_under_the_own_account_prefix() {
        let c = client(Mock::new(vec![(200, r#"{"Key":"x"}"#)]));
        c.put_object(
            &session(),
            "profiles/p/journal/d/2026/10/00000001.jsonl",
            b"{}\n".to_vec(),
            JOURNAL_CONTENT_TYPE,
        )
        .await
        .unwrap();
        let seen = c.transport.seen.lock().unwrap();
        let (m, u, h, b) = &seen[0];
        assert_eq!(m, "POST");
        assert_eq!(
            u,
            &format!("https://sb.example/storage/v1/object/stores/{ACCT}/profiles/p/journal/d/2026/10/00000001.jsonl")
        );
        assert!(h.contains(&("x-upsert".into(), "true".into())));
        assert!(h.contains(&("Authorization".into(), "Bearer jwt-a".into())));
        assert_eq!(b, b"{}\n");
    }

    #[tokio::test]
    async fn paths_are_validated_before_any_request() {
        let c = client(Mock::new(vec![]));
        for bad in ["../x", "/abs", "a//b", "a/../b", "a b", ""] {
            assert!(matches!(
                c.put_object(&session(), bad, vec![], JOURNAL_CONTENT_TYPE)
                    .await,
                Err(CloudError::InvalidValue(_))
            ));
        }
        assert!(c.transport.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn storage_errors_are_classified_from_the_body_status() {
        let rls = br#"{"statusCode":"403","error":"Unauthorized","message":"new row violates row-level security policy"}"#;
        assert_eq!(classify_storage(400, rls), CloudError::Forbidden);
        let nf = br#"{"statusCode":"404","error":"not_found","message":"Object not found"}"#;
        assert_eq!(classify_storage(400, nf), CloudError::NotFound);
        let nb = br#"{"statusCode":"404","error":"Bucket not found","message":"Bucket not found"}"#;
        assert!(matches!(
            classify_storage(400, nb),
            CloudError::SchemaMissing(_)
        ));
        assert_eq!(
            classify_storage(502, b"bad gateway"),
            CloudError::Server(502)
        );
        assert_eq!(classify_storage(401, b"{}"), CloudError::Unauthorized);
    }

    #[tokio::test]
    async fn list_and_journal_devices_and_missing_object() {
        let c = client(Mock::new(vec![
            (
                200,
                r#"[{"name":"dev-1","id":null},{"name":"dev-2","id":null},{"name":".emptyFolderPlaceholder","id":"x"}]"#,
            ),
            (400, r#"{"statusCode":"404","message":"Object not found"}"#),
        ]));
        let devs = c.journal_devices(&session(), Uuid::nil()).await.unwrap();
        assert_eq!(devs, vec!["dev-1", "dev-2"]);
        assert_eq!(
            c.get_object(&session(), "profiles/x/y.jsonl")
                .await
                .unwrap(),
            None
        );
        let seen = c.transport.seen.lock().unwrap();
        assert!(String::from_utf8_lossy(&seen[0].3).contains(&format!(
            "\"prefix\":\"{ACCT}/profiles/{}/journal\"",
            Uuid::nil()
        )));
        assert!(seen[1].1.contains("/object/authenticated/stores/"));
    }

    #[tokio::test]
    async fn store_event_insert_ignores_duplicates() {
        let c = client(Mock::new(vec![(201, "")]));
        let ev = StoreEvent {
            profile_id: Uuid::nil(),
            device_id: Uuid::nil(),
            path: "profiles/p/journal/d/2026/10/00000001.jsonl".into(),
            seq_from: 1,
            seq_to: 6,
            hlc: "1790000000000-0000-d".into(),
            created_at: None,
        };
        c.record_store_event(&session(), &ev).await.unwrap();
        let seen = c.transport.seen.lock().unwrap();
        assert!(seen[0]
            .1
            .ends_with("/rest/v1/store_events?on_conflict=profile_id,device_id,seq_to"));
        assert!(seen[0].2.contains(&(
            "Prefer".into(),
            "resolution=ignore-duplicates,return=minimal".into()
        )));
        let body = String::from_utf8_lossy(&seen[0].3);
        assert!(!body.contains("account_id") && !body.contains("created_at"));
    }
}
