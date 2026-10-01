//! Profile CRUD and local ↔ cloud mapping operations (user JWT + RLS only).

use crate::client::{now_rfc3339, CloudClient, CloudError, RestTransport};
use crate::mapping::{Mapping, MappingToken};
use crate::models::ProfileSummary;
use serde::Deserialize;
use synchrome_auth::Session;
use synchrome_types::BrowserKind;
use uuid::Uuid;

const PROFILE_COLUMNS: &str = "id,name,created_at,updated_at";
const MAPPING_COLUMNS: &str = "id,profile_id,device_id,browser_kind,browser_profile_dir,\
mapping_token,token_state,token_confirmed_at,created_at,updated_at";

const JSON_REPR: [(&str, &str); 2] = [
    ("Prefer", "return=representation"),
    ("Content-Type", "application/json"),
];

/// Percent-encode a value for a PostgREST filter (`col=eq.<value>`). Keeps RFC 3986
/// unreserved characters, encodes everything else (spaces, `,`, `.`-free safety, UTF-8).
pub fn encode_filter_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn validate_profile_name(name: &str) -> Result<String, CloudError> {
    let n = name.trim();
    let len = n.chars().count();
    if len == 0 || len > 200 {
        return Err(CloudError::InvalidValue(
            "profile name must be 1–200 characters".into(),
        ));
    }
    Ok(n.to_string())
}

/// Mirrors `profile_mappings_dir_chk`: a bare directory name, never a path.
fn validate_profile_dir(dir: &str) -> Result<(), CloudError> {
    let len = dir.chars().count();
    if len == 0 || len > 255 || dir.contains('/') || dir.contains('\\') || dir == "." || dir == ".."
    {
        return Err(CloudError::InvalidValue(format!(
            "invalid browser profile directory name: {dir:?}"
        )));
    }
    Ok(())
}

fn rows<D: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<Vec<D>, CloudError> {
    serde_json::from_slice(bytes).map_err(|e| CloudError::InvalidResponse(format!("rows: {e}")))
}

/// For PATCH/DELETE: RLS turns "someone else's row" into "0 rows" — report it as NotFound.
fn first_or_not_found<D: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<D, CloudError> {
    rows::<D>(bytes)?
        .into_iter()
        .next()
        .ok_or(CloudError::NotFound)
}

impl<T: RestTransport> CloudClient<T> {
    // ---------------- profiles ----------------

    /// `POST /profiles` — the server stamps `account_id = auth.uid()`.
    pub async fn create_profile(
        &self,
        session: &Session,
        name: &str,
    ) -> Result<ProfileSummary, CloudError> {
        let name = validate_profile_name(name)?;
        let body = serde_json::json!({ "name": name });
        let bytes = self
            .call(
                session,
                "POST",
                &format!("/profiles?select={PROFILE_COLUMNS}"),
                &JSON_REPR,
                Some(&body),
            )
            .await?;
        Self::one(&bytes)
    }

    /// `PATCH /profiles?id=eq.<id>`. `NotFound` if the profile is not in this account.
    pub async fn rename_profile(
        &self,
        session: &Session,
        profile_id: Uuid,
        name: &str,
    ) -> Result<ProfileSummary, CloudError> {
        let name = validate_profile_name(name)?;
        let body = serde_json::json!({ "name": name });
        let bytes = self
            .call(
                session,
                "PATCH",
                &format!("/profiles?id=eq.{profile_id}&select={PROFILE_COLUMNS}"),
                &JSON_REPR,
                Some(&body),
            )
            .await?;
        first_or_not_found(&bytes)
    }

    /// `DELETE /profiles?id=eq.<id>`. The server REJECTS deleting a profile that is still
    /// mapped (FK `ON DELETE RESTRICT`), surfaced as [`CloudError::ProfileInUse`].
    pub async fn delete_profile(
        &self,
        session: &Session,
        profile_id: Uuid,
    ) -> Result<(), CloudError> {
        let res = self
            .call(
                session,
                "DELETE",
                &format!("/profiles?id=eq.{profile_id}&select=id"),
                &JSON_REPR,
                None,
            )
            .await;
        match res {
            Ok(bytes) => first_or_not_found::<serde_json::Value>(&bytes).map(|_| ()),
            Err(CloudError::UnknownReference(_)) => Err(CloudError::ProfileInUse),
            Err(e) => Err(e),
        }
    }

    // ---------------- mappings ----------------

    /// Create a mapping for a local profile on `device_id`, generating a fresh 256-bit
    /// `mapping_token`. The token is persisted in the cloud with state `pending_local_write`;
    /// writing it into the extension's LevelDB store is a later, separate step.
    pub async fn map_local_profile(
        &self,
        session: &Session,
        profile_id: Uuid,
        device_id: Uuid,
        browser_kind: BrowserKind,
        browser_profile_dir: &str,
    ) -> Result<Mapping, CloudError> {
        validate_profile_dir(browser_profile_dir)?;
        let token = MappingToken::generate()
            .map_err(|e| CloudError::InvalidResponse(format!("CSPRNG unavailable: {e}")))?;
        let body = serde_json::json!({
            "profile_id": profile_id,
            "device_id": device_id,
            "browser_kind": browser_kind,
            "browser_profile_dir": browser_profile_dir,
            "mapping_token": token.expose(),
        });
        let bytes = self
            .call(
                session,
                "POST",
                &format!("/profile_mappings?select={MAPPING_COLUMNS}"),
                &JSON_REPR,
                Some(&body),
            )
            .await?;
        Self::one(&bytes)
    }

    /// `DELETE /profile_mappings?id=eq.<id>`.
    pub async fn unmap_local_profile(
        &self,
        session: &Session,
        mapping_id: Uuid,
    ) -> Result<(), CloudError> {
        let bytes = self
            .call(
                session,
                "DELETE",
                &format!("/profile_mappings?id=eq.{mapping_id}&select=id"),
                &JSON_REPR,
                None,
            )
            .await?;
        first_or_not_found::<serde_json::Value>(&bytes).map(|_| ())
    }

    /// Every mapping of the account (all devices), oldest first.
    pub async fn list_mappings(&self, session: &Session) -> Result<Vec<Mapping>, CloudError> {
        let bytes = self
            .call(
                session,
                "GET",
                &format!("/profile_mappings?select={MAPPING_COLUMNS}&order=created_at.asc"),
                &[],
                None,
            )
            .await?;
        rows(&bytes)
    }

    /// The mapping of one local profile (device + browser + dir), if any.
    pub async fn mapping_for_local_profile(
        &self,
        session: &Session,
        device_id: Uuid,
        browser_kind: BrowserKind,
        browser_profile_dir: &str,
    ) -> Result<Option<Mapping>, CloudError> {
        validate_profile_dir(browser_profile_dir)?;
        let path = format!(
            "/profile_mappings?select={MAPPING_COLUMNS}&device_id=eq.{device_id}\
             &browser_kind=eq.{}&browser_profile_dir=eq.{}",
            browser_kind.id(),
            encode_filter_value(browser_profile_dir)
        );
        let bytes = self.call(session, "GET", &path, &[], None).await?;
        Ok(rows::<Mapping>(&bytes)?.into_iter().next())
    }

    /// All mappings of one cloud profile (one per device at most).
    pub async fn mapping_for_cloud_profile(
        &self,
        session: &Session,
        profile_id: Uuid,
    ) -> Result<Vec<Mapping>, CloudError> {
        let bytes = self
            .call(
                session,
                "GET",
                &format!(
                    "/profile_mappings?select={MAPPING_COLUMNS}&profile_id=eq.{profile_id}&order=created_at.asc"
                ),
                &[],
                None,
            )
            .await?;
        rows(&bytes)
    }

    /// Mark the token as read back by the extension (used by a later slice).
    pub async fn confirm_mapping_token(
        &self,
        session: &Session,
        mapping_id: Uuid,
    ) -> Result<Mapping, CloudError> {
        let body = serde_json::json!({
            "token_state": "confirmed",
            "token_confirmed_at": now_rfc3339(),
        });
        let bytes = self
            .call(
                session,
                "PATCH",
                &format!("/profile_mappings?id=eq.{mapping_id}&select={MAPPING_COLUMNS}"),
                &JSON_REPR,
                Some(&body),
            )
            .await?;
        first_or_not_found(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::RestFuture;
    use crate::mapping::TokenState;
    use std::sync::Mutex;
    use synchrome_auth::Secret;
    use synchrome_config::SupabaseConfig;

    type Seen = (String, String, Option<serde_json::Value>, String);

    /// Scripted transport. Each entry: (bearer that may read it or "*", status, body).
    /// A request carrying another user's bearer gets the RLS view: empty rows on GET/PATCH/
    /// DELETE. Records (method, url, body, bearer).
    struct Mock {
        queue: Mutex<Vec<(&'static str, u16, String)>>,
        seen: Mutex<Vec<Seen>>,
    }
    impl Mock {
        fn new(q: Vec<(&'static str, u16, String)>) -> Self {
            Self {
                queue: Mutex::new(q),
                seen: Mutex::new(vec![]),
            }
        }
        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }
    impl RestTransport for Mock {
        fn request<'a>(
            &'a self,
            method: &'a str,
            url: &'a str,
            headers: &'a [(&'a str, &'a str)],
            body: Option<&'a serde_json::Value>,
        ) -> RestFuture<'a> {
            let bearer = headers
                .iter()
                .find(|(k, _)| *k == "Authorization")
                .map(|(_, v)| v.trim_start_matches("Bearer ").to_string())
                .unwrap_or_default();
            self.seen.lock().unwrap().push((
                method.into(),
                url.into(),
                body.cloned(),
                bearer.clone(),
            ));
            let (owner, status, resp) = self.queue.lock().unwrap().remove(0);
            Box::pin(async move {
                if owner != "*" && owner != bearer {
                    return Ok((200u16, b"[]".to_vec())); // RLS: other user's rows invisible
                }
                Ok((status, resp.into_bytes()))
            })
        }
    }

    fn cfg() -> SupabaseConfig {
        SupabaseConfig {
            url: "https://cloud.test".into(),
            anon_key: "anon".into(),
        }
    }
    fn s(tok: &str) -> Session {
        Session {
            user_id: tok.into(),
            email: None,
            access_token: Secret::new(tok),
            refresh_token: Secret::new("r"),
            expires_at: i64::MAX,
            token_type: "bearer".into(),
        }
    }
    fn client(q: Vec<(&'static str, u16, String)>) -> CloudClient<Mock> {
        CloudClient::with_transport(cfg(), Mock::new(q))
    }

    const PID: &str = "33333333-3333-4333-8333-333333333333";
    const DEV1: &str = "11111111-1111-4111-8111-111111111111";
    const DEV2: &str = "22222222-2222-4222-8222-222222222222";
    const MID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn profile_row(name: &str) -> String {
        format!(r#"[{{"id":"{PID}","name":"{name}","created_at":"t","updated_at":"t"}}]"#)
    }
    fn mapping_row(device: &str, dir: &str, state: &str) -> String {
        let token = "c0ffee".repeat(10) + "c0ff";
        let confirmed = if state == "confirmed" {
            "\"t\""
        } else {
            "null"
        };
        format!(
            r#"[{{"id":"{MID}","profile_id":"{PID}","device_id":"{device}","browser_kind":"helium",
            "browser_profile_dir":"{dir}","mapping_token":"{token}","token_state":"{state}",
            "token_confirmed_at":{confirmed},"created_at":"t","updated_at":"t"}}]"#
        )
    }
    fn pg_err(code: &str) -> String {
        format!(r#"{{"code":"{code}","message":"pg {code}"}}"#)
    }
    fn uid(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }

    // ---------- profiles ----------

    #[tokio::test]
    async fn create_profile_posts_trimmed_name() {
        let c = client(vec![("A", 201, profile_row("Personal"))]);
        let p = c.create_profile(&s("A"), "  Personal ").await.unwrap();
        assert_eq!(p.name, "Personal");
        let seen = c.transport.seen();
        assert_eq!(seen[0].0, "POST");
        assert!(seen[0].1.contains("/rest/v1/profiles?select="));
        assert_eq!(seen[0].2.as_ref().unwrap()["name"], "Personal");
        assert!(
            seen[0].2.as_ref().unwrap().get("account_id").is_none(),
            "client must not send account_id"
        );
    }

    #[tokio::test]
    async fn create_profile_validation_and_duplicate() {
        let c = client(vec![("A", 409, pg_err("23505"))]);
        assert!(matches!(
            c.create_profile(&s("A"), "   ").await,
            Err(CloudError::InvalidValue(_))
        ));
        assert!(matches!(
            c.create_profile(&s("A"), &"x".repeat(201)).await,
            Err(CloudError::InvalidValue(_))
        ));
        assert!(matches!(
            c.create_profile(&s("A"), "Personal").await,
            Err(CloudError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn rename_profile_ok_and_not_found() {
        let c = client(vec![
            ("A", 200, profile_row("Work")),
            ("A", 200, "[]".into()),
        ]);
        assert_eq!(
            c.rename_profile(&s("A"), uid(PID), "Work")
                .await
                .unwrap()
                .name,
            "Work"
        );
        assert_eq!(
            c.rename_profile(&s("A"), uid(PID), "Work")
                .await
                .unwrap_err(),
            CloudError::NotFound
        );
        assert_eq!(c.transport.seen()[0].0, "PATCH");
    }

    #[tokio::test]
    async fn delete_profile_ok_in_use_and_not_found() {
        let c = client(vec![
            ("A", 200, format!(r#"[{{"id":"{PID}"}}]"#)),
            ("A", 409, pg_err("23503")),
            ("A", 200, "[]".into()),
        ]);
        c.delete_profile(&s("A"), uid(PID)).await.unwrap();
        // Cloud profile still mapped somewhere: server RESTRICT -> ProfileInUse.
        assert_eq!(
            c.delete_profile(&s("A"), uid(PID)).await.unwrap_err(),
            CloudError::ProfileInUse
        );
        assert_eq!(
            c.delete_profile(&s("A"), uid(PID)).await.unwrap_err(),
            CloudError::NotFound
        );
    }

    // ---------- mappings ----------

    #[tokio::test]
    async fn map_local_profile_sends_identity_and_fresh_token() {
        let c = client(vec![(
            "A",
            201,
            mapping_row(DEV1, "Profile 2", "pending_local_write"),
        )]);
        let m = c
            .map_local_profile(
                &s("A"),
                uid(PID),
                uid(DEV1),
                BrowserKind::Helium,
                "Profile 2",
            )
            .await
            .unwrap();
        assert_eq!(m.token_state, TokenState::PendingLocalWrite);
        assert_eq!(m.browser_kind, BrowserKind::Helium);
        let body = c.transport.seen()[0].2.clone().unwrap();
        assert_eq!(body["profile_id"], PID);
        assert_eq!(body["device_id"], DEV1);
        assert_eq!(body["browser_kind"], "helium");
        assert_eq!(body["browser_profile_dir"], "Profile 2");
        let tok = body["mapping_token"].as_str().unwrap();
        assert!(
            tok.len() == 64
                && tok
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
        for forbidden in ["account_id", "token_state", "id"] {
            assert!(
                body.get(forbidden).is_none(),
                "client must not send {forbidden}"
            );
        }
    }

    #[tokio::test]
    async fn each_mapping_gets_a_distinct_token() {
        let c = client(vec![
            (
                "A",
                201,
                mapping_row(DEV1, "Profile 2", "pending_local_write"),
            ),
            (
                "A",
                201,
                mapping_row(DEV2, "Default", "pending_local_write"),
            ),
        ]);
        c.map_local_profile(
            &s("A"),
            uid(PID),
            uid(DEV1),
            BrowserKind::Helium,
            "Profile 2",
        )
        .await
        .unwrap();
        c.map_local_profile(&s("A"), uid(PID), uid(DEV2), BrowserKind::Helium, "Default")
            .await
            .unwrap();
        let seen = c.transport.seen();
        assert_ne!(
            seen[0].2.as_ref().unwrap()["mapping_token"],
            seen[1].2.as_ref().unwrap()["mapping_token"]
        );
    }

    #[tokio::test]
    async fn same_cloud_profile_can_map_from_two_devices() {
        let c = client(vec![
            (
                "A",
                201,
                mapping_row(DEV1, "Profile 2", "pending_local_write"),
            ),
            (
                "A",
                201,
                mapping_row(DEV2, "Default", "pending_local_write"),
            ),
        ]);
        let m1 = c
            .map_local_profile(
                &s("A"),
                uid(PID),
                uid(DEV1),
                BrowserKind::Helium,
                "Profile 2",
            )
            .await
            .unwrap();
        let m2 = c
            .map_local_profile(&s("A"), uid(PID), uid(DEV2), BrowserKind::Helium, "Default")
            .await
            .unwrap();
        assert_eq!(m1.profile_id, m2.profile_id);
        assert_ne!(m1.device_id, m2.device_id);
    }

    #[tokio::test]
    async fn duplicate_local_mapping_and_token_collision_are_conflicts() {
        let c = client(vec![
            ("A", 409, pg_err("23505")),
            ("A", 409, pg_err("23505")),
        ]);
        // same (device, browser, dir) mapped twice, or same cloud profile twice on this device
        assert!(matches!(
            c.map_local_profile(
                &s("A"),
                uid(PID),
                uid(DEV1),
                BrowserKind::Helium,
                "Profile 2"
            )
            .await,
            Err(CloudError::Conflict(_))
        ));
        // (astronomically unlikely) token collision is the same unique-violation class
        assert!(matches!(
            c.map_local_profile(
                &s("A"),
                uid(PID),
                uid(DEV1),
                BrowserKind::Helium,
                "Profile 3"
            )
            .await,
            Err(CloudError::Conflict(_))
        ));
    }

    #[tokio::test]
    async fn unknown_profile_or_device_is_typed() {
        let c = client(vec![("A", 409, pg_err("23503"))]);
        assert!(matches!(
            c.map_local_profile(
                &s("A"),
                Uuid::new_v4(),
                uid(DEV1),
                BrowserKind::Chrome,
                "Default"
            )
            .await,
            Err(CloudError::UnknownReference(_))
        ));
    }

    #[tokio::test]
    async fn profile_dir_must_be_a_bare_name() {
        let c = client(vec![]);
        for bad in ["", ".", "..", "a/b", "..\\x", &"p".repeat(256)] {
            assert!(
                matches!(
                    c.map_local_profile(&s("A"), uid(PID), uid(DEV1), BrowserKind::Helium, bad)
                        .await,
                    Err(CloudError::InvalidValue(_))
                ),
                "accepted {bad:?}"
            );
        }
        assert!(
            c.transport.seen().is_empty(),
            "invalid input must not reach the network"
        );
    }

    #[tokio::test]
    async fn unmap_ok_and_not_found() {
        let c = client(vec![
            ("A", 200, format!(r#"[{{"id":"{MID}"}}]"#)),
            ("A", 200, "[]".into()),
        ]);
        c.unmap_local_profile(&s("A"), uid(MID)).await.unwrap();
        assert_eq!(
            c.unmap_local_profile(&s("A"), uid(MID)).await.unwrap_err(),
            CloudError::NotFound
        );
        assert_eq!(c.transport.seen()[0].0, "DELETE");
    }

    #[tokio::test]
    async fn lookup_by_local_profile_encodes_the_dir() {
        let c = client(vec![
            (
                "A",
                200,
                mapping_row(DEV1, "Profile 2", "pending_local_write"),
            ),
            ("A", 200, "[]".into()),
        ]);
        let m = c
            .mapping_for_local_profile(&s("A"), uid(DEV1), BrowserKind::Helium, "Profile 2")
            .await
            .unwrap();
        assert!(m.is_some());
        let url = &c.transport.seen()[0].1;
        assert!(url.contains("browser_profile_dir=eq.Profile%202"), "{url}");
        assert!(url.contains("browser_kind=eq.helium"));
        assert!(c
            .mapping_for_local_profile(&s("A"), uid(DEV1), BrowserKind::Helium, "Profile 9")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn lookup_by_cloud_profile_and_confirm() {
        let c = client(vec![
            (
                "A",
                200,
                mapping_row(DEV1, "Profile 2", "pending_local_write"),
            ),
            ("A", 200, mapping_row(DEV1, "Profile 2", "confirmed")),
        ]);
        assert_eq!(
            c.mapping_for_cloud_profile(&s("A"), uid(PID))
                .await
                .unwrap()
                .len(),
            1
        );
        let m = c.confirm_mapping_token(&s("A"), uid(MID)).await.unwrap();
        assert_eq!(m.token_state, TokenState::Confirmed);
        let body = c.transport.seen()[1].2.clone().unwrap();
        assert_eq!(body["token_state"], "confirmed");
        assert!(body["token_confirmed_at"].as_str().unwrap().ends_with('Z'));
        assert!(
            body.get("mapping_token").is_none(),
            "confirm must not touch the token"
        );
    }

    #[tokio::test]
    async fn user_b_cannot_see_or_touch_user_a_mappings() {
        // Every row in the queue belongs to A; B's requests get the RLS view.
        let c = client(vec![
            (
                "A",
                200,
                mapping_row(DEV1, "Profile 2", "pending_local_write"),
            ),
            (
                "A",
                200,
                mapping_row(DEV1, "Profile 2", "pending_local_write"),
            ),
            ("A", 200, format!(r#"[{{"id":"{MID}"}}]"#)),
            ("A", 200, mapping_row(DEV1, "Profile 2", "confirmed")),
            ("A", 200, profile_row("Personal")),
        ]);
        assert!(c.list_mappings(&s("B")).await.unwrap().is_empty());
        assert!(c
            .mapping_for_local_profile(&s("B"), uid(DEV1), BrowserKind::Helium, "Profile 2")
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            c.unmap_local_profile(&s("B"), uid(MID)).await.unwrap_err(),
            CloudError::NotFound
        );
        assert_eq!(
            c.confirm_mapping_token(&s("B"), uid(MID))
                .await
                .unwrap_err(),
            CloudError::NotFound
        );
        assert_eq!(
            c.rename_profile(&s("B"), uid(PID), "pwned")
                .await
                .unwrap_err(),
            CloudError::NotFound
        );
    }

    #[test]
    fn filter_encoding() {
        assert_eq!(encode_filter_value("Profile 2"), "Profile%202");
        assert_eq!(encode_filter_value("Default"), "Default");
        assert_eq!(encode_filter_value("a,b(c)"), "a%2Cb%28c%29");
        assert_eq!(encode_filter_value("Perfil é"), "Perfil%20%C3%A9");
    }
}
