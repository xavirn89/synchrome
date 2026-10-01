//! Handshake resolution: `Hello { mapping_token }` -> [`HandshakeReply`].
//!
//! Order of checks (each returns early):
//! 1. token absent -> `TokenMissing`; malformed -> `TokenUnknown` (no network)
//! 2. no session -> `NotSignedIn`
//! 3. find the token among the account's mappings (RLS-scoped list, constant-time compare;
//!    the token never goes into a URL); cloud errors -> `CloudUnavailable` / `NotSignedIn`
//! 4. not found: use the local registry to tell `TokenOtherAccount` / `CloudProfileDeleted` /
//!    `MappingDeleted` / `TokenUnknown`
//! 5. found but for another device -> `WrongDevice` (no confirmation)
//! 6. found for this device -> confirm in the cloud (only now that the extension proved it read
//!    the right token), mark the local record confirmed, reply `Connected`.
//!
//! The reply never contains the token.

use crate::protocol::{HandshakeReply, HandshakeStatus, WireToken};
use crate::registry::{ct_eq, TokenRegistry};
use std::future::Future;
use synchrome_cloud::{CloudError, Mapping, TokenState};
use uuid::Uuid;

/// Cloud operations the handshake needs. Implemented over `CloudClient` in production and
/// mocked in tests.
pub trait CloudLookup: Send + Sync {
    fn list_mappings(&self) -> impl Future<Output = Result<Vec<Mapping>, CloudError>> + Send;
    fn profile_name(
        &self,
        profile_id: Uuid,
    ) -> impl Future<Output = Result<Option<String>, CloudError>> + Send;
    fn confirm(&self, mapping_id: Uuid)
        -> impl Future<Output = Result<Mapping, CloudError>> + Send;
}

/// Who is asking, from the app's point of view.
#[derive(Debug, Clone)]
pub struct HandshakeContext {
    /// `Some(user_id)` when the app has a valid session.
    pub account_id: Option<String>,
    /// This installation's device id.
    pub this_device: Uuid,
}

fn cloud_err(e: CloudError) -> HandshakeReply {
    match e {
        CloudError::Unauthorized => HandshakeReply::status(HandshakeStatus::NotSignedIn)
            .with_detail("session rejected by Synchrome Cloud"),
        other => {
            HandshakeReply::status(HandshakeStatus::CloudUnavailable).with_detail(other.to_string())
        }
    }
}

pub async fn resolve<C: CloudLookup>(
    token: Option<&WireToken>,
    ctx: &HandshakeContext,
    cloud: Option<&C>,
    registry: &TokenRegistry,
) -> HandshakeReply {
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return HandshakeReply::status(HandshakeStatus::TokenMissing);
    };
    if !token.is_well_formed() {
        return HandshakeReply::status(HandshakeStatus::TokenUnknown)
            .with_detail("malformed token");
    }
    let (Some(account), Some(cloud)) = (ctx.account_id.as_deref(), cloud) else {
        return HandshakeReply::status(HandshakeStatus::NotSignedIn);
    };

    let mappings = match cloud.list_mappings().await {
        Ok(m) => m,
        Err(e) => return cloud_err(e),
    };
    let found = mappings
        .into_iter()
        .find(|m| ct_eq(m.mapping_token.expose(), token.expose()));

    let Some(m) = found else {
        return match registry.find_by_token(token.expose()) {
            Some(rec) if rec.account_id != account => {
                HandshakeReply::status(HandshakeStatus::TokenOtherAccount)
            }
            Some(rec) => match cloud.profile_name(rec.profile_id).await {
                Ok(None) => HandshakeReply::status(HandshakeStatus::CloudProfileDeleted),
                Ok(Some(_)) => HandshakeReply::status(HandshakeStatus::MappingDeleted),
                Err(e) => cloud_err(e),
            },
            None => HandshakeReply::status(HandshakeStatus::TokenUnknown),
        };
    };

    if m.device_id != ctx.this_device {
        let mut r = HandshakeReply::status(HandshakeStatus::WrongDevice);
        r.device_id = Some(m.device_id);
        return r;
    }

    let confirmed = if m.token_state == TokenState::Confirmed {
        m
    } else {
        match cloud.confirm(m.id).await {
            Ok(c) => c,
            Err(e) => return cloud_err(e),
        }
    };
    let _ = registry.mark_confirmed(confirmed.id); // best effort; cloud is the source of truth
    let profile_name = cloud
        .profile_name(confirmed.profile_id)
        .await
        .ok()
        .flatten();

    HandshakeReply {
        status: HandshakeStatus::Connected,
        ui_state: HandshakeStatus::Connected.ui_state(),
        mapped: true,
        profile_id: Some(confirmed.profile_id),
        profile_name,
        device_id: Some(confirmed.device_id),
        token_state: Some(match confirmed.token_state {
            TokenState::Confirmed => "confirmed",
            TokenState::PendingLocalWrite => "pending_local_write",
        }),
        detail: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{token_sha256, LocalTokenRecord, LocalTokenState};
    use std::sync::Mutex;
    use synchrome_types::BrowserKind;

    const ME: &str = "11111111-1111-4111-8111-111111111111";
    const OTHER_DEV: &str = "22222222-2222-4222-8222-222222222222";
    const PID: &str = "33333333-3333-4333-8333-333333333333";
    const MID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    fn uid(s: &str) -> Uuid {
        Uuid::parse_str(s).unwrap()
    }
    fn mapping(token: &str, device: &str, state: &str) -> Mapping {
        serde_json::from_value(serde_json::json!({
            "id": MID, "profile_id": PID, "device_id": device, "browser_kind": "helium",
            "browser_profile_dir": "Default", "mapping_token": token, "token_state": state,
            "token_confirmed_at": if state == "confirmed" { serde_json::json!("t") } else { serde_json::Value::Null }
        }))
        .unwrap()
    }

    #[derive(Default)]
    struct Mock {
        mappings: Vec<Mapping>,
        profile_exists: bool,
        list_err: Option<CloudError>,
        confirms: Mutex<u32>,
    }
    impl CloudLookup for Mock {
        async fn list_mappings(&self) -> Result<Vec<Mapping>, CloudError> {
            match &self.list_err {
                Some(e) => Err(e.clone()),
                None => Ok(self.mappings.clone()),
            }
        }
        async fn profile_name(&self, _p: Uuid) -> Result<Option<String>, CloudError> {
            Ok(self.profile_exists.then(|| "Personal".to_string()))
        }
        async fn confirm(&self, _m: Uuid) -> Result<Mapping, CloudError> {
            *self.confirms.lock().unwrap() += 1;
            let mut m = self.mappings[0].clone();
            m.token_state = TokenState::Confirmed;
            m.token_confirmed_at = Some("t".into());
            Ok(m)
        }
    }

    fn ctx(signed_in: bool) -> HandshakeContext {
        HandshakeContext {
            account_id: signed_in.then(|| "acct-a".to_string()),
            this_device: uid(ME),
        }
    }
    fn reg() -> (tempfile::TempDir, TokenRegistry) {
        let d = tempfile::tempdir().unwrap();
        let r = TokenRegistry::in_dir(d.path());
        (d, r)
    }
    fn record(token: &str, account: &str) -> LocalTokenRecord {
        LocalTokenRecord {
            token_sha256: token_sha256(token),
            account_id: account.into(),
            mapping_id: uid(MID),
            profile_id: uid(PID),
            browser_kind: BrowserKind::Helium,
            user_data_dir: "/u".into(),
            profile_dir: "Default".into(),
            state: LocalTokenState::Written,
            updated_at: 0,
            last_error: None,
        }
    }
    fn tok() -> String {
        "5e".repeat(32)
    }

    #[tokio::test]
    async fn correct_handshake_confirms_once_and_never_returns_the_token() {
        let (_d, registry) = reg();
        registry.upsert(record(&tok(), "acct-a")).unwrap();
        let cloud = Mock {
            mappings: vec![mapping(&tok(), ME, "pending_local_write")],
            profile_exists: true,
            ..Default::default()
        };
        let r = resolve(
            Some(&WireToken::new(tok())),
            &ctx(true),
            Some(&cloud),
            &registry,
        )
        .await;
        assert_eq!(r.status, HandshakeStatus::Connected);
        assert!(r.mapped);
        assert_eq!(r.token_state, Some("confirmed"));
        assert_eq!(r.profile_name.as_deref(), Some("Personal"));
        assert_eq!(*cloud.confirms.lock().unwrap(), 1);
        assert_eq!(
            registry.find_by_mapping(uid(MID)).unwrap().state,
            LocalTokenState::Confirmed
        );
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains(&tok()));
    }

    #[tokio::test]
    async fn already_confirmed_is_not_reconfirmed() {
        let (_d, registry) = reg();
        let cloud = Mock {
            mappings: vec![mapping(&tok(), ME, "confirmed")],
            profile_exists: true,
            ..Default::default()
        };
        let r = resolve(
            Some(&WireToken::new(tok())),
            &ctx(true),
            Some(&cloud),
            &registry,
        )
        .await;
        assert_eq!(r.status, HandshakeStatus::Connected);
        assert_eq!(*cloud.confirms.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn missing_and_malformed_tokens_never_hit_the_network() {
        let (_d, registry) = reg();
        let cloud = Mock {
            list_err: Some(CloudError::Server(500)),
            ..Default::default()
        };
        assert_eq!(
            resolve(None, &ctx(true), Some(&cloud), &registry)
                .await
                .status,
            HandshakeStatus::TokenMissing
        );
        assert_eq!(
            resolve(
                Some(&WireToken::new("")),
                &ctx(true),
                Some(&cloud),
                &registry
            )
            .await
            .status,
            HandshakeStatus::TokenMissing
        );
        assert_eq!(
            resolve(
                Some(&WireToken::new("XYZ")),
                &ctx(true),
                Some(&cloud),
                &registry
            )
            .await
            .status,
            HandshakeStatus::TokenUnknown
        );
    }

    #[tokio::test]
    async fn not_signed_in() {
        let (_d, registry) = reg();
        let r = resolve::<Mock>(Some(&WireToken::new(tok())), &ctx(false), None, &registry).await;
        assert_eq!(r.status, HandshakeStatus::NotSignedIn);
        assert_eq!(r.ui_state, "app_unavailable");
    }

    #[tokio::test]
    async fn unknown_token_wrong_token_and_other_account() {
        let (_d, registry) = reg();
        // account has a mapping, but with a different token -> unknown
        let cloud = Mock {
            mappings: vec![mapping(&"77".repeat(32), ME, "pending_local_write")],
            profile_exists: true,
            ..Default::default()
        };
        assert_eq!(
            resolve(
                Some(&WireToken::new(tok())),
                &ctx(true),
                Some(&cloud),
                &registry
            )
            .await
            .status,
            HandshakeStatus::TokenUnknown
        );
        // token was written by this device while signed in to another account
        registry.upsert(record(&tok(), "acct-b")).unwrap();
        let r = resolve(
            Some(&WireToken::new(tok())),
            &ctx(true),
            Some(&cloud),
            &registry,
        )
        .await;
        assert_eq!(r.status, HandshakeStatus::TokenOtherAccount);
        assert_eq!(r.ui_state, "token_mismatch");
        assert_eq!(*cloud.confirms.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn mapping_deleted_between_write_and_handshake() {
        let (_d, registry) = reg();
        registry.upsert(record(&tok(), "acct-a")).unwrap();
        let profile_still_there = Mock {
            mappings: vec![],
            profile_exists: true,
            ..Default::default()
        };
        assert_eq!(
            resolve(
                Some(&WireToken::new(tok())),
                &ctx(true),
                Some(&profile_still_there),
                &registry
            )
            .await
            .status,
            HandshakeStatus::MappingDeleted
        );
        let profile_gone = Mock {
            mappings: vec![],
            profile_exists: false,
            ..Default::default()
        };
        assert_eq!(
            resolve(
                Some(&WireToken::new(tok())),
                &ctx(true),
                Some(&profile_gone),
                &registry
            )
            .await
            .status,
            HandshakeStatus::CloudProfileDeleted
        );
    }

    #[tokio::test]
    async fn token_of_another_device_is_not_confirmed() {
        let (_d, registry) = reg();
        let cloud = Mock {
            mappings: vec![mapping(&tok(), OTHER_DEV, "pending_local_write")],
            profile_exists: true,
            ..Default::default()
        };
        let r = resolve(
            Some(&WireToken::new(tok())),
            &ctx(true),
            Some(&cloud),
            &registry,
        )
        .await;
        assert_eq!(r.status, HandshakeStatus::WrongDevice);
        assert_eq!(*cloud.confirms.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn cloud_errors_map_to_states() {
        let (_d, registry) = reg();
        let down = Mock {
            list_err: Some(CloudError::Network("dns".into())),
            ..Default::default()
        };
        assert_eq!(
            resolve(
                Some(&WireToken::new(tok())),
                &ctx(true),
                Some(&down),
                &registry
            )
            .await
            .status,
            HandshakeStatus::CloudUnavailable
        );
        let expired = Mock {
            list_err: Some(CloudError::Unauthorized),
            ..Default::default()
        };
        assert_eq!(
            resolve(
                Some(&WireToken::new(tok())),
                &ctx(true),
                Some(&expired),
                &registry
            )
            .await
            .status,
            HandshakeStatus::NotSignedIn
        );
    }
}
