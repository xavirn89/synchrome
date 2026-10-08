//! Extensions inventory (`docs/03` §3.4): what `chrome.management` reports, classified and
//! reduced to what may be synchronised. Pure; no I/O.
//!
//! Identity is the Chromium extension id (32 letters `a`–`p`). Shared, synchronised state is
//! `installed` (a tombstone = uninstalled) and `enabled`, plus informational `name`,
//! `version` and the install *source class* — never the update URL, paths, permissions or
//! any extension data.

use serde::{Deserialize, Serialize};

pub const COLLECTION: &str = "extensions";
pub const MAX_NAME_CHARS: usize = 256;
pub const MAX_VERSION_CHARS: usize = 64;

/// One `chrome.management.ExtensionInfo`, reduced to the fields the extension sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub enabled: bool,
    /// `extension`, `theme`, `hosted_app`, `packaged_app`, `legacy_packaged_app`, `login_screen_extension`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `admin`, `development`, `normal`, `sideload`, `other`.
    pub install_type: String,
    pub may_disable: bool,
    /// Only used locally to classify the source; never stored or published.
    #[serde(default)]
    pub update_url: Option<String>,
    /// `unknown` or `permissions_increase` when disabled.
    #[serde(default)]
    pub disabled_reason: Option<String>,
}

/// An incremental `chrome.management` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtEvent {
    Installed { info: ExtInfo },
    Uninstalled { id: String },
    Enabled { info: ExtInfo },
    Disabled { info: ExtInfo },
}

impl ExtEvent {
    pub fn id(&self) -> &str {
        match self {
            ExtEvent::Installed { info }
            | ExtEvent::Enabled { info }
            | ExtEvent::Disabled { info } => &info.id,
            ExtEvent::Uninstalled { id } => id,
        }
    }
}

/// How an observed extension is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// Inventory and enabled state are synchronised.
    Synced,
    /// Installed by policy (`installType: admin`): shown locally as protected, never
    /// published, never touched.
    Protected,
    /// Not an extension Synchrome handles (apps, the Synchrome extension itself, a malformed
    /// id): ignored completely.
    Ignored,
}

/// Where an extension can be installed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Webstore,
    EdgeStore,
    Development,
    Sideload,
    Other,
    Unknown,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Webstore => "webstore",
            Source::EdgeStore => "edge_store",
            Source::Development => "development",
            Source::Sideload => "sideload",
            Source::Other => "other",
            Source::Unknown => "unknown",
        }
    }
    pub fn parse(s: &str) -> Source {
        match s {
            "webstore" => Source::Webstore,
            "edge_store" => Source::EdgeStore,
            "development" => Source::Development,
            "sideload" => Source::Sideload,
            "other" => Source::Other,
            _ => Source::Unknown,
        }
    }
}

/// Whether a missing extension can be installed on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// An official store page can be opened (the user installs there).
    Available,
    /// Not installable from a store (unpacked, sideloaded, self-hosted, private).
    Unavailable,
    /// From a store this browser cannot install from.
    Incompatible,
    Unknown,
}

/// Chromium extension ids are 32 characters in `a`..=`p`.
pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|c| (b'a'..=b'p').contains(&c))
}

pub fn classify(info: &ExtInfo, synchrome_id: &str) -> Class {
    if !valid_id(&info.id) || info.id == synchrome_id {
        return Class::Ignored;
    }
    if info.kind != "extension" {
        return Class::Ignored; // apps and themes are out of scope (docs/03 §3.4)
    }
    if info.install_type == "admin" {
        return Class::Protected;
    }
    Class::Synced
}

pub fn source_of(info: &ExtInfo) -> Source {
    match info.install_type.as_str() {
        "development" => Source::Development,
        "sideload" => Source::Sideload,
        "other" => Source::Other,
        "normal" => {
            let host = info
                .update_url
                .as_deref()
                .and_then(|u| u.split("://").nth(1))
                .and_then(|r| r.split('/').next())
                .unwrap_or("")
                .to_ascii_lowercase();
            if host == "clients2.google.com" || host.ends_with("chromewebstore.google.com") {
                Source::Webstore
            } else if host.ends_with("edge.microsoft.com") {
                Source::EdgeStore
            } else if host.is_empty() {
                Source::Unknown
            } else {
                Source::Other
            }
        }
        _ => Source::Unknown,
    }
}

/// Store page for a missing extension, for the browser of this device.
pub fn install_url(id: &str, source: Source, browser: &str) -> (Availability, Option<String>) {
    if !valid_id(id) {
        return (Availability::Unknown, None);
    }
    match source {
        Source::Webstore => (
            Availability::Available,
            Some(format!("https://chromewebstore.google.com/detail/{id}")),
        ),
        Source::EdgeStore if browser == "edge" => (
            Availability::Available,
            Some(format!(
                "https://microsoftedge.microsoft.com/addons/detail/{id}"
            )),
        ),
        Source::EdgeStore => (Availability::Incompatible, None),
        Source::Development | Source::Sideload | Source::Other => (Availability::Unavailable, None),
        Source::Unknown => (Availability::Unknown, None),
    }
}

pub fn clean_name(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect()
}

pub fn clean_version(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .take(MAX_VERSION_CHARS)
        .collect()
}

/// Dotted numeric version order (`1.10` > `1.9`).
pub fn version_newer(a: &str, b: &str) -> bool {
    let p = |s: &str| -> Vec<u64> { s.split('.').map(|x| x.parse().unwrap_or(0)).collect() };
    p(a) > p(b)
}

/// The user disabled it (as opposed to Chromium disabling it after a permissions increase,
/// which is a local condition needing the user, not a state to publish).
pub fn user_disabled(info: &ExtInfo) -> bool {
    !info.enabled && info.disabled_reason.as_deref() != Some("permissions_increase")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: &str, it: &str, url: Option<&str>) -> ExtInfo {
        ExtInfo {
            id: id.into(),
            name: "X".into(),
            version: "1.0".into(),
            enabled: true,
            kind: "extension".into(),
            install_type: it.into(),
            may_disable: true,
            update_url: url.map(Into::into),
            disabled_reason: None,
        }
    }

    const ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    #[test]
    fn classification_excludes_self_apps_and_policy() {
        let me = "ceflmoojogokmklgcmcihdgakbefgiee";
        assert_eq!(classify(&info(ID, "normal", None), me), Class::Synced);
        assert_eq!(classify(&info(me, "development", None), me), Class::Ignored);
        assert_eq!(classify(&info(ID, "admin", None), me), Class::Protected);
        let mut app = info(ID, "normal", None);
        app.kind = "hosted_app".into();
        assert_eq!(classify(&app, me), Class::Ignored);
        assert_eq!(
            classify(&info("not-an-id", "normal", None), me),
            Class::Ignored
        );
    }

    #[test]
    fn sources_and_install_urls() {
        let ws = info(
            ID,
            "normal",
            Some("https://clients2.google.com/service/update2/crx"),
        );
        assert_eq!(source_of(&ws), Source::Webstore);
        assert_eq!(
            install_url(ID, Source::Webstore, "helium").0,
            Availability::Available
        );
        let edge = info(
            ID,
            "normal",
            Some("https://edge.microsoft.com/extensionwebstorebase/v1/crx"),
        );
        assert_eq!(source_of(&edge), Source::EdgeStore);
        assert_eq!(
            install_url(ID, Source::EdgeStore, "chrome").0,
            Availability::Incompatible
        );
        assert_eq!(
            install_url(ID, Source::EdgeStore, "edge").0,
            Availability::Available
        );
        assert_eq!(
            source_of(&info(ID, "development", None)),
            Source::Development
        );
        assert_eq!(
            install_url(ID, Source::Development, "helium"),
            (Availability::Unavailable, None)
        );
        assert_eq!(
            source_of(&info(ID, "normal", Some("https://updates.corp.example/x"))),
            Source::Other
        );
    }

    #[test]
    fn versions_and_disable_reasons() {
        assert!(version_newer("1.10.0", "1.9.9"));
        assert!(!version_newer("1.0", "1.0"));
        let mut i = info(ID, "normal", None);
        i.enabled = false;
        assert!(user_disabled(&i));
        i.disabled_reason = Some("permissions_increase".into());
        assert!(!user_disabled(&i));
        assert_eq!(clean_version("1.2.3-beta\u{0}"), "1.2.3");
    }
}
