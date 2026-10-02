//! Browser preferences (`docs/03` §3.6): the v1 allowlist of Chromium `Preferences` paths,
//! their value schemas, browser compatibility and version gate, the explicit denylist, and
//! the per-browser representation of each logical preference. Pure; no I/O.
//!
//! Entity id = the preference path (e.g. `bookmark_bar.show_on_all_tabs`). Everything not in
//! [`ALLOWLIST`] is never read into the dataset and never written.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const COLLECTION: &str = "browser_preferences";

/// Newest browser major whose semantics were validated for the allowlist (E13). Values only
/// cross majors when both are at most this one; equal majors always pass.
pub const VALIDATED_THROUGH: u32 = 154;

/// Browser kinds the `chromium_generic` class covers (validated by E13).
pub const GENERIC_KINDS: &[&str] = &["helium", "chrome"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compat {
    /// Helium and Google Chrome.
    ChromiumGeneric,
    HeliumOnly,
    ChromeOnly,
    Unsupported,
}

impl Compat {
    pub fn allows(self, kind: &str) -> bool {
        match self {
            Compat::ChromiumGeneric => GENERIC_KINDS.contains(&kind),
            Compat::HeliumOnly => kind == "helium",
            Compat::ChromeOnly => kind == "chrome",
            Compat::Unsupported => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schema {
    Bool,
    /// Comma-separated BCP-47 language tags, 1..=32 of them.
    LangList,
}

/// One allowlisted preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spec {
    pub path: &'static str,
    pub schema: Schema,
    pub compat: Compat,
    /// Lowest browser major validated for this path.
    pub since_major: u32,
}

/// v1 allowlist (E13, Helium 0.18.2.1 / Chromium 154 and Google Chrome 154).
pub const ALLOWLIST: &[Spec] = &[
    Spec {
        path: "bookmark_bar.show_on_all_tabs",
        schema: Schema::Bool,
        compat: Compat::ChromiumGeneric,
        since_major: 154,
    },
    Spec {
        path: "browser.enable_spellchecking",
        schema: Schema::Bool,
        compat: Compat::ChromiumGeneric,
        since_major: 154,
    },
    Spec {
        path: "download.prompt_for_download",
        schema: Schema::Bool,
        compat: Compat::ChromiumGeneric,
        since_major: 154,
    },
    Spec {
        path: "intl.selected_languages",
        schema: Schema::LangList,
        compat: Compat::ChromiumGeneric,
        since_major: 154,
    },
];

/// Helium stores the bookmark bar visibility also here; when present it wins (E13b).
pub const HELIUM_BOOKMARK_BAR_STATE: &str = "bookmark_bar.visibility_state";

/// Blocks never touched, whatever the allowlist says (defence in depth: the writer refuses
/// any path under one of these prefixes, and any non-allowlisted path).
pub const DENYLIST: &[&str] = &[
    "account_info",
    "account_values",
    "google",
    "signin",
    "sync",
    "profile",
    "credentials_enable_service",
    "credentials_enable_autosignin",
    "password_manager",
    "autofill",
    "payments",
    "media",
    "media_device_id_salt",
    "extensions",
    "custom_handlers",
    "protocol_handler",
    "content_settings",
    "enterprise_signin",
    "policy",
    "protection",
    "session",
    "default_search_provider",
    "default_search_provider_data",
    "pinned_tabs",
    "homepage",
    "homepage_is_newtabpage",
    "browser.show_home_button",
    "browser.enabled_labs_experiments",
    "variations",
    "download.default_directory",
    "savefile",
    "selectfile",
];

pub fn spec(path: &str) -> Option<&'static Spec> {
    ALLOWLIST.iter().find(|s| s.path == path)
}

/// `path` equals a denylisted block or lies inside one (dot boundaries).
pub fn is_denied(path: &str) -> bool {
    DENYLIST
        .iter()
        .any(|d| path == *d || path.starts_with(&format!("{d}.")))
        || path.split('.').any(|seg| {
            let s = seg.to_ascii_lowercase();
            s.contains("device_id") || s.contains("managed")
        })
}

/// Raw paths Synchrome may ever write (allowlist + the Helium companion), none denylisted.
pub fn writable(path: &str) -> bool {
    (spec(path).is_some() || path == HELIUM_BOOKMARK_BAR_STATE) && !is_denied(path)
}

fn lang_tag(t: &str) -> bool {
    let mut parts = t.split('-');
    let first = parts.next().unwrap_or_default();
    (2..=3).contains(&first.len())
        && first.chars().all(|c| c.is_ascii_alphabetic())
        && parts.all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// The value matches the path's schema.
pub fn valid(spec: &Spec, v: &Value) -> bool {
    match spec.schema {
        Schema::Bool => v.is_boolean(),
        Schema::LangList => v.as_str().is_some_and(|s| {
            let tags: Vec<&str> = s.split(',').collect();
            (1..=32).contains(&tags.len()) && tags.iter().all(|t| lang_tag(t))
        }),
    }
}

/// Canonical JSON text of a value (sorted object keys).
pub fn canonical(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

/// The browser major of a `Last Version` string (`154.0.8037.92` → 154).
pub fn major_of(last_version: &str) -> Option<u32> {
    last_version.trim().split('.').next()?.parse().ok()
}

/// Can this path be read/written on this browser at all?
pub fn supported(spec: &Spec, kind: &str, major: Option<u32>) -> bool {
    spec.compat.allows(kind) && major.is_some_and(|m| m >= spec.since_major)
}

/// May a value written on `writer` major be applied on `target` major?
pub fn same_semantics(writer: Option<u32>, target: Option<u32>) -> bool {
    match (writer, target) {
        (Some(w), Some(t)) => w == t || (w <= VALIDATED_THROUGH && t <= VALIDATED_THROUGH),
        _ => false,
    }
}

fn get<'a>(doc: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(doc, |d, k| d.as_object()?.get(k))
}

/// Why an allowlisted path is not in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    /// Not supported on this browser kind / major.
    Unsupported,
    /// Present but its value does not match the schema.
    Invalid,
}

/// Logical values of the allowlist in a parsed `Preferences` document, as this browser
/// stores them. Absent paths (browser default) are not in the map.
pub fn extract(
    doc: &Value,
    kind: &str,
    major: Option<u32>,
) -> (BTreeMap<&'static str, Value>, BTreeMap<&'static str, Skip>) {
    let mut out = BTreeMap::new();
    let mut skip = BTreeMap::new();
    for s in ALLOWLIST {
        if !supported(s, kind, major) {
            skip.insert(s.path, Skip::Unsupported);
            continue;
        }
        let v = if s.path == "bookmark_bar.show_on_all_tabs" && kind == "helium" {
            match get(doc, HELIUM_BOOKMARK_BAR_STATE) {
                Some(st) if st.is_number() => Some(Value::Bool(st.as_i64() == Some(0))),
                _ => get(doc, s.path).cloned(),
            }
        } else {
            get(doc, s.path).cloned()
        };
        match v {
            None => {}
            Some(v) if valid(s, &v) => {
                out.insert(s.path, v);
            }
            Some(_) => {
                skip.insert(s.path, Skip::Invalid);
            }
        }
    }
    (out, skip)
}

/// One raw edit of the `Preferences` file: set a value, or remove the key (`None`).
#[derive(Debug, Clone, PartialEq)]
pub struct RawEdit {
    pub path: String,
    pub value: Option<Value>,
}

/// The raw edits that store logical `value` (or the browser default, `None`) of `path` on
/// this browser kind.
pub fn edits_for(kind: &str, path: &str, value: Option<&Value>) -> Vec<RawEdit> {
    let mut v = vec![RawEdit {
        path: path.to_string(),
        value: value.cloned(),
    }];
    if path == "bookmark_bar.show_on_all_tabs" && kind == "helium" {
        // the shape Helium's own UI writes: the boolean, without the state that overrides it
        v.push(RawEdit {
            path: HELIUM_BOOKMARK_BAR_STATE.into(),
            value: None,
        });
    }
    v
}

/// Per-profile choice (default Off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Off,
    /// The whole allowlist.
    Recommended,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Recommended => "recommended",
        }
    }
    pub fn parse(s: &str) -> Mode {
        if s == "recommended" {
            Mode::Recommended
        } else {
            Mode::Off
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn allowlist_is_small_valid_and_never_denylisted() {
        assert!(ALLOWLIST.len() <= 5);
        for s in ALLOWLIST {
            assert!(!is_denied(s.path), "{}", s.path);
            assert!(writable(s.path));
        }
        for p in [
            "homepage",
            "browser.show_home_button",
            "sync.requested",
            "profile.content_settings.exceptions.cookies",
            "extensions.settings.x",
            "media.device_id_salt",
            "download.default_directory",
            "intl.accept_languages",
            "spellcheck.dictionaries",
            "translate.enabled",
            "foo.managed_bar",
        ] {
            assert!(!writable(p), "{p}");
        }
        assert!(writable(HELIUM_BOOKMARK_BAR_STATE));
    }

    #[test]
    fn schemas() {
        let b = spec("download.prompt_for_download").unwrap();
        assert!(valid(b, &json!(true)) && !valid(b, &json!("true")) && !valid(b, &json!(1)));
        let l = spec("intl.selected_languages").unwrap();
        for ok in ["es,en", "en-US,en", "pt-BR", "zh-Hant-TW,fil"] {
            assert!(valid(l, &json!(ok)), "{ok}");
        }
        for bad in [
            json!(""),
            json!("es,,en"),
            json!("e"),
            json!("es;en"),
            json!(["es"]),
            json!("/Users/x"),
        ] {
            assert!(!valid(l, &bad), "{bad}");
        }
        let many = vec!["en"; 33].join(",");
        assert!(!valid(l, &json!(many)));
    }

    #[test]
    fn compatibility_and_versions() {
        let s = spec("download.prompt_for_download").unwrap();
        assert!(supported(s, "helium", Some(154)) && supported(s, "chrome", Some(160)));
        assert!(
            !supported(s, "brave", Some(154))
                && !supported(s, "helium", Some(153))
                && !supported(s, "helium", None)
        );
        assert!(same_semantics(Some(154), Some(154)) && same_semantics(Some(160), Some(160)));
        assert!(
            !same_semantics(Some(155), Some(154))
                && !same_semantics(Some(154), Some(155))
                && !same_semantics(None, Some(154))
        );
        assert_eq!(major_of("154.0.8037.92\n"), Some(154));
        assert_eq!(major_of("x"), None);
        assert!(Compat::HeliumOnly.allows("helium") && !Compat::HeliumOnly.allows("chrome"));
        assert!(Compat::ChromeOnly.allows("chrome") && !Compat::Unsupported.allows("helium"));
    }

    #[test]
    fn extract_reads_only_the_allowlist_and_the_helium_representation() {
        let doc = json!({
            "bookmark_bar": {"show_on_all_tabs": false, "visibility_state": 0},
            "download": {"prompt_for_download": true, "default_directory": "/Users/x/Downloads"},
            "intl": {"selected_languages": "es,en", "accept_languages": "es,en"},
            "browser": {"enable_spellchecking": "yes", "show_home_button": true},
            "homepage": "https://example.com"
        });
        let (h, hs) = extract(&doc, "helium", Some(154));
        assert_eq!(h.get("bookmark_bar.show_on_all_tabs"), Some(&json!(true)));
        assert_eq!(h.get("download.prompt_for_download"), Some(&json!(true)));
        assert_eq!(h.get("intl.selected_languages"), Some(&json!("es,en")));
        assert_eq!(h.len(), 3);
        assert_eq!(hs.get("browser.enable_spellchecking"), Some(&Skip::Invalid));
        let (c, _) = extract(&doc, "chrome", Some(154));
        assert_eq!(c.get("bookmark_bar.show_on_all_tabs"), Some(&json!(false)));
        let (b, bs) = extract(&doc, "brave", Some(154));
        assert!(b.is_empty() && bs.values().all(|s| *s == Skip::Unsupported));
        let none = extract(&json!({}), "helium", Some(154)).0;
        assert!(none.is_empty());
    }

    #[test]
    fn helium_writes_the_shape_of_its_own_ui() {
        let e = edits_for(
            "helium",
            "bookmark_bar.show_on_all_tabs",
            Some(&json!(false)),
        );
        assert_eq!(e.len(), 2);
        assert_eq!(
            e[1],
            RawEdit {
                path: HELIUM_BOOKMARK_BAR_STATE.into(),
                value: None
            }
        );
        assert_eq!(
            edits_for(
                "chrome",
                "bookmark_bar.show_on_all_tabs",
                Some(&json!(true))
            )
            .len(),
            1
        );
        assert_eq!(
            edits_for("helium", "download.prompt_for_download", None)[0].value,
            None
        );
    }
}
