//! Safe reads and edits of a profile's `Preferences` file (`docs/03` §3.6).
//!
//! Rules:
//! - only while the browser is closed (no live `SingletonLock`): Chromium keeps prefs in
//!   memory and rewrites the whole file on change and on exit;
//! - an advisory `flock` on `Preferences` is held for the whole read-modify-swap (Chromium
//!   does not use one; it serialises Synchrome processes and cooperating tools) — held by
//!   someone else → [`PrefsError::Locked`];
//! - only paths accepted by the caller's `allowed` predicate are edited, **in place in the
//!   original bytes** (span splice): every other byte of the file is unchanged;
//! - backup outside the profile → staging file next to it (fsync) → verify (parses, each
//!   edit reads back, the document minus the edited paths equals the original minus them) →
//!   atomic rename → re-read; a failure before the rename changes nothing, a failure after
//!   it restores the backup;
//! - `Secure Preferences`, `Local State` and every other file are never written.

use crate::access::is_browser_running;
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const PREFERENCES: &str = "Preferences";
/// Nesting deeper than this is refused (Chromium's own limit is 200).
const MAX_DEPTH: usize = 200;

#[derive(Debug, Error)]
pub enum PrefsError {
    #[error("the browser is running for this user-data directory; close it first")]
    BrowserRunning,
    #[error("Preferences is locked by another process; retry")]
    Locked,
    #[error("Preferences not found for this profile")]
    NotFound,
    #[error("Preferences is not valid JSON: {0}")]
    InvalidJson(String),
    #[error("refusing to write a path outside the allowlist")]
    Denied,
    #[error("unexpected Preferences shape at an edited path")]
    Shape,
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("verification after write failed: {0}")]
    Verify(String),
    #[error("the written file did not read back; the previous file was restored")]
    RolledBack,
}

fn io(path: &Path, source: std::io::Error) -> PrefsError {
    PrefsError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// `<udd>/<profile>/Preferences`
pub fn preferences_path(user_data_dir: &Path, profile_dir: &str) -> PathBuf {
    user_data_dir.join(profile_dir).join(PREFERENCES)
}

/// Contents of `<udd>/Last Version` (e.g. `154.0.8037.92`), if present.
pub fn browser_last_version(user_data_dir: &Path) -> Option<String> {
    std::fs::read_to_string(user_data_dir.join("Last Version"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn check_profile_dir(profile_dir: &str) -> Result<(), PrefsError> {
    if profile_dir.is_empty()
        || profile_dir.len() > 255
        || profile_dir.contains('/')
        || profile_dir.contains('\\')
        || profile_dir == "."
        || profile_dir == ".."
    {
        return Err(PrefsError::Invalid(format!("profile dir {profile_dir:?}")));
    }
    Ok(())
}

/// An exclusive advisory lock on the file, released on drop.
struct FileLock(std::fs::File);

impl FileLock {
    fn try_take(path: &Path) -> Result<FileLock, PrefsError> {
        use std::os::unix::io::AsRawFd;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    PrefsError::NotFound
                } else {
                    io(path, e)
                }
            })?;
        // SAFETY: plain flock on a descriptor we own
        let got = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if !got {
            return Err(PrefsError::Locked);
        }
        Ok(FileLock(f))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // SAFETY: unlocking our own descriptor
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn parse_checked(bytes: &[u8]) -> Result<Value, PrefsError> {
    let mut i = 0;
    let root = node(bytes, &mut i, 0).map_err(PrefsError::InvalidJson)?;
    skip_ws(bytes, &mut i);
    if i != bytes.len() {
        return Err(PrefsError::InvalidJson("trailing data".into()));
    }
    if !matches!(root, Node::Obj { .. }) {
        return Err(PrefsError::InvalidJson("top level is not an object".into()));
    }
    serde_json::from_slice(bytes).map_err(|e| PrefsError::InvalidJson(e.to_string()))
}

/// Parse `Preferences` of a closed profile (browser running or file locked → refused).
pub fn read_preferences(user_data_dir: &Path, profile_dir: &str) -> Result<Value, PrefsError> {
    check_profile_dir(profile_dir)?;
    if is_browser_running(user_data_dir) {
        return Err(PrefsError::BrowserRunning);
    }
    let path = preferences_path(user_data_dir, profile_dir);
    let _lock = FileLock::try_take(&path)?;
    let bytes = std::fs::read(&path).map_err(|e| io(&path, e))?;
    parse_checked(&bytes)
}

/// One edit: set `path` (dot-separated) to `value`, or remove it (`None`).
#[derive(Debug, Clone, PartialEq)]
pub struct PrefEdit {
    pub path: String,
    pub value: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefsReport {
    pub path: PathBuf,
    pub backup_path: Option<PathBuf>,
    /// Edits that changed the file (setting an equal value / removing an absent key is a no-op).
    pub changed: usize,
    pub bytes_before: usize,
    pub bytes_after: usize,
}

/// Apply `edits` to `Preferences` of a closed profile, safely (see the module docs). Every
/// edit path must satisfy `allowed`; nothing is written otherwise.
pub fn apply_preferences(
    user_data_dir: &Path,
    profile_dir: &str,
    edits: &[PrefEdit],
    allowed: &dyn Fn(&str) -> bool,
    backup_root: &Path,
) -> Result<PrefsReport, PrefsError> {
    apply_inner(
        user_data_dir,
        profile_dir,
        edits,
        allowed,
        backup_root,
        &|_| {},
    )
}

fn apply_inner(
    user_data_dir: &Path,
    profile_dir: &str,
    edits: &[PrefEdit],
    allowed: &dyn Fn(&str) -> bool,
    backup_root: &Path,
    after_swap: &dyn Fn(&Path),
) -> Result<PrefsReport, PrefsError> {
    check_profile_dir(profile_dir)?;
    for e in edits {
        if e.path.is_empty() || e.path.split('.').any(str::is_empty) {
            return Err(PrefsError::Invalid("empty path segment".into()));
        }
        if !allowed(&e.path) {
            return Err(PrefsError::Denied);
        }
    }
    if is_browser_running(user_data_dir) {
        return Err(PrefsError::BrowserRunning);
    }
    let path = preferences_path(user_data_dir, profile_dir);
    let _lock = FileLock::try_take(&path)?;
    let before = std::fs::read(&path).map_err(|e| io(&path, e))?;
    let old_doc = parse_checked(&before)?;
    let mut bytes = before.clone();
    let mut changed = 0;
    for e in edits {
        let segs: Vec<&str> = e.path.split('.').collect();
        let next = match &e.value {
            Some(v) => {
                if pointer(&parse_checked(&bytes)?, &segs) == Some(v) {
                    None // already there
                } else {
                    Some(splice_set(
                        &bytes,
                        &segs,
                        &serde_json::to_string(v).unwrap_or_default(),
                    )?)
                }
            }
            None => splice_remove(&bytes, &segs)?,
        };
        if let Some(n) = next {
            bytes = n;
            changed += 1;
        }
    }
    if changed == 0 {
        return Ok(PrefsReport {
            path,
            backup_path: None,
            changed: 0,
            bytes_before: before.len(),
            bytes_after: before.len(),
        });
    }
    // verify before anything is written
    let new_doc = parse_checked(&bytes)?;
    for e in edits {
        let segs: Vec<&str> = e.path.split('.').collect();
        if pointer(&new_doc, &segs) != e.value.as_ref() {
            return Err(PrefsError::Verify(format!(
                "an edit did not read back ({} segments)",
                segs.len()
            )));
        }
    }
    let (mut a, mut b) = (old_doc, new_doc);
    for e in edits {
        let segs: Vec<&str> = e.path.split('.').collect();
        strip(&mut a, &segs);
        strip(&mut b, &segs);
    }
    if a != b {
        return Err(PrefsError::Verify("an unrelated preference changed".into()));
    }
    // backup outside the profile
    std::fs::create_dir_all(backup_root).map_err(|e| io(backup_root, e))?;
    let suffix = unique_suffix();
    let backup = backup_root.join(format!(
        "{suffix}-{}-Preferences.json",
        profile_dir.replace(' ', "_")
    ));
    write_synced(&backup, &before)?;
    // staging next to the file, then atomic rename
    let staging = path.with_file_name(format!("{PREFERENCES}.synchrome-staging-{suffix}"));
    write_synced(&staging, &bytes)?;
    if let Ok(m) = std::fs::metadata(&path) {
        let _ = std::fs::set_permissions(&staging, m.permissions());
    }
    if is_browser_running(user_data_dir) {
        let _ = std::fs::remove_file(&staging);
        return Err(PrefsError::BrowserRunning);
    }
    if let Err(e) = std::fs::rename(&staging, &path) {
        let _ = std::fs::remove_file(&staging);
        return Err(io(&path, e));
    }
    after_swap(&path);
    let back = std::fs::read(&path).unwrap_or_default();
    if back != bytes {
        // rollback: the previous bytes, again through a staging file + rename
        let undo = path.with_file_name(format!("{PREFERENCES}.synchrome-undo-{suffix}"));
        write_synced(&undo, &before)?;
        std::fs::rename(&undo, &path).map_err(|e| io(&path, e))?;
        return Err(PrefsError::RolledBack);
    }
    Ok(PrefsReport {
        path,
        backup_path: Some(backup),
        changed,
        bytes_before: before.len(),
        bytes_after: bytes.len(),
    })
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), PrefsError> {
    let mut f = std::fs::File::create(path).map_err(|e| io(path, e))?;
    f.write_all(bytes).map_err(|e| io(path, e))?;
    f.sync_all().map_err(|e| io(path, e))?;
    Ok(())
}

fn unique_suffix() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

fn pointer<'a>(doc: &'a Value, segs: &[&str]) -> Option<&'a Value> {
    segs.iter().try_fold(doc, |d, k| d.as_object()?.get(*k))
}

/// Remove `segs` and every parent object it leaves empty (applied to both sides of the
/// verification, so a parent created or emptied by an edit compares equal).
fn strip(doc: &mut Value, segs: &[&str]) {
    let Some((last, parents)) = segs.split_last() else {
        return;
    };
    if let Some(Value::Object(m)) = parents
        .iter()
        .try_fold(&mut *doc, |d, k| d.as_object_mut()?.get_mut(*k))
    {
        m.remove(*last);
    }
    for depth in (1..segs.len()).rev() {
        let prefix = &segs[..depth];
        let empty = matches!(pointer(doc, prefix), Some(Value::Object(m)) if m.is_empty());
        if !empty {
            break;
        }
        let (k, pp) = prefix.split_last().unwrap();
        if let Some(Value::Object(m)) = pp
            .iter()
            .try_fold(&mut *doc, |d, k| d.as_object_mut()?.get_mut(*k))
        {
            m.remove(*k);
        }
    }
}

// ------------------------------------------------------------------ span parser

struct Member {
    key: String,
    /// Offset of the key's opening quote.
    start: usize,
    /// End offset (exclusive) of the member's value.
    end: usize,
    value: Node,
}

enum Node {
    Obj {
        start: usize,
        end: usize,
        members: Vec<Member>,
    },
    Leaf {
        start: usize,
        end: usize,
    },
}

impl Node {
    fn span(&self) -> (usize, usize) {
        match self {
            Node::Obj { start, end, .. } | Node::Leaf { start, end } => (*start, *end),
        }
    }
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

fn string_span(b: &[u8], i: &mut usize) -> Result<(usize, usize), String> {
    let start = *i;
    if b.get(*i) != Some(&b'"') {
        return Err("expected string".into());
    }
    *i += 1;
    while *i < b.len() {
        match b[*i] {
            b'"' => {
                *i += 1;
                return Ok((start, *i));
            }
            b'\\' => *i += 2,
            c if c < 0x20 => return Err("control character in string".into()),
            _ => *i += 1,
        }
    }
    Err("unterminated string".into())
}

fn node(b: &[u8], i: &mut usize, depth: usize) -> Result<Node, String> {
    if depth > MAX_DEPTH {
        return Err("nesting too deep".into());
    }
    skip_ws(b, i);
    let start = *i;
    match b.get(*i) {
        Some(b'{') => {
            *i += 1;
            let mut members = Vec::new();
            let mut seen = HashSet::new();
            skip_ws(b, i);
            if b.get(*i) == Some(&b'}') {
                *i += 1;
                return Ok(Node::Obj {
                    start,
                    end: *i,
                    members,
                });
            }
            loop {
                skip_ws(b, i);
                let (ks, ke) = string_span(b, i)?;
                let key: String = serde_json::from_slice(&b[ks..ke]).map_err(|e| e.to_string())?;
                if !seen.insert(key.clone()) {
                    return Err("duplicate key".into());
                }
                skip_ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return Err("expected ':'".into());
                }
                *i += 1;
                let value = node(b, i, depth + 1)?;
                let (_, end) = value.span();
                members.push(Member {
                    key,
                    start: ks,
                    end,
                    value,
                });
                skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b'}') => {
                        *i += 1;
                        return Ok(Node::Obj {
                            start,
                            end: *i,
                            members,
                        });
                    }
                    _ => return Err("expected ',' or '}'".into()),
                }
            }
        }
        Some(b'[') => {
            *i += 1;
            skip_ws(b, i);
            if b.get(*i) == Some(&b']') {
                *i += 1;
                return Ok(Node::Leaf { start, end: *i });
            }
            loop {
                node(b, i, depth + 1)?;
                skip_ws(b, i);
                match b.get(*i) {
                    Some(b',') => *i += 1,
                    Some(b']') => {
                        *i += 1;
                        return Ok(Node::Leaf { start, end: *i });
                    }
                    _ => return Err("expected ',' or ']'".into()),
                }
            }
        }
        Some(b'"') => {
            let (s, e) = string_span(b, i)?;
            Ok(Node::Leaf { start: s, end: e })
        }
        Some(c) if *c == b'-' || c.is_ascii_digit() => {
            while *i < b.len() && matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                *i += 1;
            }
            Ok(Node::Leaf { start, end: *i })
        }
        Some(_) => {
            for lit in [&b"true"[..], b"false", b"null"] {
                if b[*i..].starts_with(lit) {
                    *i += lit.len();
                    return Ok(Node::Leaf { start, end: *i });
                }
            }
            Err("unexpected token".into())
        }
        None => Err("unexpected end".into()),
    }
}

fn root(b: &[u8]) -> Result<Node, PrefsError> {
    let mut i = 0;
    node(b, &mut i, 0).map_err(PrefsError::InvalidJson)
}

fn splice(b: &[u8], from: usize, to: usize, with: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len() + with.len());
    out.extend_from_slice(&b[..from]);
    out.extend_from_slice(with.as_bytes());
    out.extend_from_slice(&b[to..]);
    out
}

fn member_text(segs: &[&str], value: &str) -> String {
    let (first, rest) = segs.split_first().expect("non-empty path");
    let k = serde_json::to_string(first).unwrap_or_default();
    if rest.is_empty() {
        format!("{k}:{value}")
    } else {
        format!("{k}:{{{}}}", member_text(rest, value))
    }
}

/// Set `segs` to the JSON text `value`, touching only the bytes of that member (or inserting
/// it, in key order like Chromium writes, creating missing parent objects).
fn splice_set(b: &[u8], segs: &[&str], value: &str) -> Result<Vec<u8>, PrefsError> {
    let mut cur = root(b)?;
    for (n, seg) in segs.iter().enumerate() {
        let Node::Obj {
            start,
            end,
            members,
        } = cur
        else {
            return Err(PrefsError::Shape);
        };
        match members.iter().position(|m| m.key == *seg) {
            Some(ix) => {
                let m = members.into_iter().nth(ix).expect("member");
                if n + 1 == segs.len() {
                    let (vs, ve) = m.value.span();
                    return Ok(splice(b, vs, ve, value));
                }
                cur = m.value;
            }
            None => {
                let text = member_text(&segs[n..], value);
                return Ok(match members.iter().find(|m| m.key.as_str() > *seg) {
                    Some(after) => splice(b, after.start, after.start, &format!("{text},")),
                    None => match members.last() {
                        Some(last) => splice(b, last.end, last.end, &format!(",{text}")),
                        None => {
                            let _ = end;
                            splice(b, start + 1, start + 1, &text)
                        }
                    },
                });
            }
        }
    }
    Err(PrefsError::Shape)
}

/// Remove the member at `segs` (and its separating comma); `None` when it is absent.
fn splice_remove(b: &[u8], segs: &[&str]) -> Result<Option<Vec<u8>>, PrefsError> {
    let mut cur = root(b)?;
    for (n, seg) in segs.iter().enumerate() {
        let Node::Obj { members, .. } = cur else {
            return Ok(None); // a parent is not an object: nothing of ours there
        };
        let Some(ix) = members.iter().position(|m| m.key == *seg) else {
            return Ok(None);
        };
        if n + 1 == segs.len() {
            let (from, to) = if ix + 1 < members.len() {
                (members[ix].start, members[ix + 1].start)
            } else if ix > 0 {
                (members[ix - 1].end, members[ix].end)
            } else {
                (members[ix].start, members[ix].end)
            };
            return Ok(Some(splice(b, from, to, "")));
        }
        cur = members.into_iter().nth(ix).expect("member").value;
    }
    Ok(None)
}

/// The original bytes with the members at `paths` removed (and parents they leave empty),
/// everything else verbatim: what must stay byte-identical across a Synchrome write.
pub fn strip_members(bytes: &[u8], paths: &[&str]) -> Result<Vec<u8>, PrefsError> {
    let mut b = bytes.to_vec();
    for p in paths {
        let segs: Vec<&str> = p.split('.').collect();
        if let Some(n) = splice_remove(&b, &segs)? {
            b = n;
        }
        for depth in (1..segs.len()).rev() {
            if !empty_object_at(&b, &segs[..depth])? {
                break;
            }
            if let Some(n) = splice_remove(&b, &segs[..depth])? {
                b = n;
            }
        }
    }
    Ok(b)
}

fn empty_object_at(b: &[u8], segs: &[&str]) -> Result<bool, PrefsError> {
    let mut cur = root(b)?;
    for seg in segs {
        let Node::Obj { members, .. } = cur else {
            return Ok(false);
        };
        let Some(m) = members.into_iter().find(|m| m.key == *seg) else {
            return Ok(false);
        };
        cur = m.value;
    }
    Ok(matches!(cur, Node::Obj { ref members, .. } if members.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ORIG: &str = r#"{"a":{"x":1,"y":"<tag>"},"bookmark_bar":{"show_on_all_tabs":false,"visibility_state":0},"download":{"default_directory":"/Users/x/Downloads","prompt_for_download":false},"intl":{"selected_languages":"en-US,en"},"zeta":[1,2.50,{"k":null}]}"#;
    const ALLOW: &[&str] = &[
        "bookmark_bar.show_on_all_tabs",
        "bookmark_bar.visibility_state",
        "download.prompt_for_download",
        "intl.selected_languages",
        "browser.enable_spellchecking",
    ];

    fn allowed(p: &str) -> bool {
        ALLOW.contains(&p)
    }

    fn profile(content: &str) -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("Default")).unwrap();
        std::fs::write(d.path().join("Default").join(PREFERENCES), content).unwrap();
        let backups = d.path().join("backups");
        (d, backups)
    }

    fn edit(p: &str, v: Option<Value>) -> PrefEdit {
        PrefEdit {
            path: p.into(),
            value: v,
        }
    }

    #[test]
    fn valid_file_reads_and_invalid_json_is_refused() {
        let (d, _) = profile(ORIG);
        let doc = read_preferences(d.path(), "Default").unwrap();
        assert_eq!(doc["intl"]["selected_languages"], json!("en-US,en"));
        for bad in [
            "{",
            "{\"a\":1,}",
            "[1]",
            "{\"a\":1}x",
            "{\"a\":1,\"a\":2}",
            "",
        ] {
            std::fs::write(d.path().join("Default").join(PREFERENCES), bad).unwrap();
            assert!(
                matches!(
                    read_preferences(d.path(), "Default"),
                    Err(PrefsError::InvalidJson(_))
                ),
                "{bad}"
            );
        }
        std::fs::remove_file(d.path().join("Default").join(PREFERENCES)).unwrap();
        assert!(matches!(
            read_preferences(d.path(), "Default"),
            Err(PrefsError::NotFound)
        ));
        assert!(matches!(
            read_preferences(d.path(), "../x"),
            Err(PrefsError::Invalid(_))
        ));
    }

    #[test]
    fn edits_touch_only_their_bytes_and_preserve_everything_else() {
        let (d, backups) = profile(ORIG);
        let edits = [
            edit("bookmark_bar.show_on_all_tabs", Some(json!(true))),
            edit("bookmark_bar.visibility_state", None),
            edit("download.prompt_for_download", Some(json!(true))),
            edit("intl.selected_languages", Some(json!("es,en"))),
            edit("browser.enable_spellchecking", Some(json!(false))),
        ];
        let r = apply_preferences(d.path(), "Default", &edits, &allowed, &backups).unwrap();
        assert_eq!(r.changed, 5);
        let after = std::fs::read_to_string(&r.path).unwrap();
        let expected = r#"{"a":{"x":1,"y":"<tag>"},"bookmark_bar":{"show_on_all_tabs":true},"browser":{"enable_spellchecking":false},"download":{"default_directory":"/Users/x/Downloads","prompt_for_download":true},"intl":{"selected_languages":"es,en"},"zeta":[1,2.50,{"k":null}]}"#;
        assert_eq!(
            after, expected,
            "only the edited members changed, byte for byte"
        );
        // backup holds the original bytes, outside the profile
        let b = r.backup_path.unwrap();
        assert!(b.starts_with(&backups));
        assert_eq!(std::fs::read_to_string(b).unwrap(), ORIG);
        // no staging leftovers
        let left: Vec<_> = std::fs::read_dir(d.path().join("Default"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![std::ffi::OsString::from(PREFERENCES)]);
        // same edits again: no-op, no new backup
        let r2 = apply_preferences(d.path(), "Default", &edits, &allowed, &backups).unwrap();
        assert_eq!((r2.changed, r2.backup_path), (0, None));
    }

    #[test]
    fn removing_and_inserting_keeps_valid_json() {
        for (orig, path, val, want) in [
            (r#"{"a":{"b":1}}"#, "a.b", None, r#"{"a":{}}"#),
            (r#"{"a":{"b":1,"c":2}}"#, "a.b", None, r#"{"a":{"c":2}}"#),
            (r#"{"a":{"b":1,"c":2}}"#, "a.c", None, r#"{"a":{"b":1}}"#),
            (
                r#"{ "a" : { "b" : 1 , "c" : 2 } }"#,
                "a.b",
                None,
                r#"{ "a" : { "c" : 2 } }"#,
            ),
            (r#"{}"#, "a.b", Some(json!(true)), r#"{"a":{"b":true}}"#),
            (r#"{"a":{}}"#, "a.b", Some(json!("x")), r#"{"a":{"b":"x"}}"#),
            (
                r#"{"b":1}"#,
                "a.c",
                Some(json!(1)),
                r#"{"a":{"c":1},"b":1}"#,
            ),
            (
                r#"{"a":1}"#,
                "b.c",
                Some(json!(1)),
                r#"{"a":1,"b":{"c":1}}"#,
            ),
        ] {
            let segs: Vec<&str> = path.split('.').collect();
            let out = match val {
                Some(v) => splice_set(orig.as_bytes(), &segs, &v.to_string()).unwrap(),
                None => splice_remove(orig.as_bytes(), &segs).unwrap().unwrap(),
            };
            assert_eq!(std::str::from_utf8(&out).unwrap(), want, "{orig} {path}");
            serde_json::from_slice::<Value>(&out).unwrap();
        }
        assert!(splice_remove(br#"{"a":1}"#, &["b", "c"]).unwrap().is_none());
        assert!(matches!(
            splice_set(br#"{"a":1}"#, &["a", "b"], "1"),
            Err(PrefsError::Shape)
        ));
    }

    #[test]
    fn browser_open_lock_denied_paths_and_shape_change_nothing() {
        let (d, backups) = profile(ORIG);
        let p = d.path().join("Default").join(PREFERENCES);
        // a path outside the allowlist
        let e = apply_preferences(
            d.path(),
            "Default",
            &[edit("homepage", Some(json!("x")))],
            &allowed,
            &backups,
        );
        assert!(matches!(e, Err(PrefsError::Denied)));
        // lock held by another holder
        {
            use std::os::unix::io::AsRawFd;
            let f = std::fs::File::open(&p).unwrap();
            assert_eq!(
                unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                0
            );
            let e = apply_preferences(
                d.path(),
                "Default",
                &[edit("intl.selected_languages", Some(json!("es")))],
                &allowed,
                &backups,
            );
            assert!(matches!(e, Err(PrefsError::Locked)));
            assert!(matches!(
                read_preferences(d.path(), "Default"),
                Err(PrefsError::Locked)
            ));
        }
        // browser running (live SingletonLock of this host + this pid)
        let host = crate::access::local_hostname();
        std::os::unix::fs::symlink(
            format!("{host}-{}", std::process::id()),
            d.path().join("SingletonLock"),
        )
        .unwrap();
        let e = apply_preferences(
            d.path(),
            "Default",
            &[edit("intl.selected_languages", Some(json!("es")))],
            &allowed,
            &backups,
        );
        assert!(matches!(e, Err(PrefsError::BrowserRunning)));
        assert!(matches!(
            read_preferences(d.path(), "Default"),
            Err(PrefsError::BrowserRunning)
        ));
        std::fs::remove_file(d.path().join("SingletonLock")).unwrap();
        // an edited path whose parent is not an object
        std::fs::write(&p, r#"{"intl":"oops"}"#).unwrap();
        let e = apply_preferences(
            d.path(),
            "Default",
            &[edit("intl.selected_languages", Some(json!("es")))],
            &allowed,
            &backups,
        );
        assert!(matches!(e, Err(PrefsError::Shape)));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), r#"{"intl":"oops"}"#);
        assert!(!backups.exists() || std::fs::read_dir(&backups).unwrap().count() == 0);
    }

    #[test]
    fn a_file_that_does_not_read_back_after_the_swap_is_rolled_back() {
        let (d, backups) = profile(ORIG);
        let e = apply_inner(
            d.path(),
            "Default",
            &[edit("download.prompt_for_download", Some(json!(true)))],
            &allowed,
            &backups,
            &|p| std::fs::write(p, b"{corrupted").unwrap(),
        );
        assert!(matches!(e, Err(PrefsError::RolledBack)));
        assert_eq!(
            std::fs::read_to_string(d.path().join("Default").join(PREFERENCES)).unwrap(),
            ORIG
        );
        let left: Vec<_> = std::fs::read_dir(d.path().join("Default"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 1);
    }

    #[test]
    fn strip_members_keeps_everything_else_verbatim() {
        let b = br#"{"a":{"x":1},"bookmark_bar":{"show_on_all_tabs":true,"visibility_state":0},"z":{"n":1.50}}"#;
        let out = strip_members(
            b,
            &[
                "bookmark_bar.show_on_all_tabs",
                "bookmark_bar.visibility_state",
                "intl.selected_languages",
            ],
        )
        .unwrap();
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            r#"{"a":{"x":1},"z":{"n":1.50}}"#
        );
    }

    #[test]
    fn last_version_file() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(browser_last_version(d.path()), None);
        std::fs::write(d.path().join("Last Version"), "154.0.8037.92").unwrap();
        assert_eq!(
            browser_last_version(d.path()).as_deref(),
            Some("154.0.8037.92")
        );
    }
}
