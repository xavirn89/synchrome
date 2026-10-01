//! Safe, token-free models returned to the UI. Mirror the `0002` schema.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One Synchrome installation registered on the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSummary {
    pub device_id: Uuid,
    pub name: String,
    pub platform: String,
    #[serde(default)]
    pub os_version: Option<String>,
    pub app_version: String,
    /// RFC 3339 timestamps as returned by PostgREST.
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub last_seen_at: Option<String>,
    /// Set client-side: is this the installation running right now?
    #[serde(default)]
    pub is_current: bool,
}

/// One Synchrome profile ("Personal", "Trabajo", …) on the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_have_no_token_or_account_fields() {
        let d = DeviceSummary {
            device_id: Uuid::nil(),
            name: "m".into(),
            platform: "macos".into(),
            os_version: None,
            app_version: "0.1.0".into(),
            created_at: None,
            last_seen_at: None,
            is_current: true,
        };
        let v = serde_json::to_value(&d).unwrap();
        for forbidden in ["access_token", "refresh_token", "account_id", "apikey"] {
            assert!(v.get(forbidden).is_none(), "{forbidden} leaked");
        }
        let p = ProfileSummary {
            id: Uuid::nil(),
            name: "Personal".into(),
            created_at: None,
            updated_at: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("account_id").is_none());
    }
}
