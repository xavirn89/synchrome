//! Safe writes into an extension's `chrome.storage.local` LevelDB store
//! (`<user-data-dir>/<profile>/Local Extension Settings/<extension-id>/`).
//!
//! Rules (Milestone 0, E4/E8 + this slice's compatibility check):
//! - only while the browser is closed (no live `SingletonLock`) and the store's `LOCK` is free;
//! - never open the live store directory: changes are made on a staging copy, verified, then
//!   swapped in with two renames (the window between them is tiny and the browser is closed);
//! - every existing key is preserved; only the requested key is set;
//! - the previous store is backed up outside the browser profile first;
//! - `Sync Extension Settings` and `IndexedDB` are never touched.
//!
//! Values are stored the way Chromium's value store does: key = plain UTF-8, value = JSON text.

use crate::access::is_browser_running;
use rusty_leveldb::{LdbIterator, Options, DB};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const LOCAL_EXTENSION_SETTINGS: &str = "Local Extension Settings";
pub const SYNC_EXTENSION_SETTINGS: &str = "Sync Extension Settings";

/// Which `chrome.storage` area (both are LevelDB stores of the same format; `session` is
/// memory-only and out of scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Area {
    Local,
    Sync,
}

impl Area {
    pub fn dir(self) -> &'static str {
        match self {
            Area::Local => LOCAL_EXTENSION_SETTINGS,
            Area::Sync => SYNC_EXTENSION_SETTINGS,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Area::Local => "local",
            Area::Sync => "sync",
        }
    }
    pub fn parse(s: &str) -> Option<Area> {
        match s {
            "local" => Some(Area::Local),
            "sync" => Some(Area::Sync),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ExtStorageError {
    #[error("the browser is running for this user-data directory; close it first")]
    BrowserRunning,
    #[error("the extension store is locked (LevelDB LOCK held); close the browser and retry")]
    Locked,
    #[error("invalid argument: {0}")]
    Invalid(String),
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("leveldb error: {0}")]
    LevelDb(String),
    #[error("verification after write failed: {0}")]
    Verify(String),
}

/// What a successful write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReport {
    pub store_path: PathBuf,
    /// Backup of the previous store, if one existed.
    pub backup_path: Option<PathBuf>,
    /// True when no store existed before (fresh store created).
    pub created: bool,
    /// Number of pre-existing keys carried over (excluding the written key).
    pub preserved_keys: usize,
}

fn io(path: &Path, source: std::io::Error) -> ExtStorageError {
    ExtStorageError::Io {
        path: path.display().to_string(),
        source,
    }
}
fn ldb(e: impl std::fmt::Display) -> ExtStorageError {
    ExtStorageError::LevelDb(e.to_string())
}

/// `<udd>/<profile>/Local Extension Settings/<ext_id>`
pub fn extension_store_path(user_data_dir: &Path, profile_dir: &str, ext_id: &str) -> PathBuf {
    area_store_path(user_data_dir, profile_dir, ext_id, Area::Local)
}

/// `<udd>/<profile>/{Local,Sync} Extension Settings/<ext_id>`
pub fn area_store_path(
    user_data_dir: &Path,
    profile_dir: &str,
    ext_id: &str,
    area: Area,
) -> PathBuf {
    user_data_dir
        .join(profile_dir)
        .join(area.dir())
        .join(ext_id)
}

fn validate(profile_dir: &str, ext_id: &str, key: &str) -> Result<(), ExtStorageError> {
    let bad_dir = profile_dir.is_empty()
        || profile_dir.len() > 255
        || profile_dir.contains('/')
        || profile_dir.contains('\\')
        || profile_dir == "."
        || profile_dir == "..";
    if bad_dir {
        return Err(ExtStorageError::Invalid(format!(
            "profile dir {profile_dir:?}"
        )));
    }
    if ext_id.len() != 32 || !ext_id.bytes().all(|b| (b'a'..=b'p').contains(&b)) {
        return Err(ExtStorageError::Invalid("extension id".into()));
    }
    if key.is_empty() {
        return Err(ExtStorageError::Invalid("empty key".into()));
    }
    Ok(())
}

/// Is the LevelDB `LOCK` file of `store` currently held by another process?
/// (Chromium holds it with `flock` while the store is open — observed in E4.)
pub fn is_store_locked(store: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let lock = store.join("LOCK");
        let Ok(f) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock)
        else {
            return false; // no LOCK file -> nobody holds it
        };
        let fd = f.as_raw_fd();
        // SAFETY: fd is a valid open descriptor owned by `f` for the duration of the calls.
        let got = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if got {
            unsafe { libc::flock(fd, libc::LOCK_UN) };
        }
        !got
    }
    #[cfg(not(unix))]
    {
        let _ = store;
        false
    }
}

fn copy_dir(src: &Path, dst: &Path) -> Result<(), ExtStorageError> {
    std::fs::create_dir_all(dst).map_err(|e| io(dst, e))?;
    for entry in std::fs::read_dir(src).map_err(|e| io(src, e))? {
        let entry = entry.map_err(|e| io(src, e))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ft = entry.file_type().map_err(|e| io(&from, e))?;
        if ft.is_dir() {
            copy_dir(&from, &to)?;
        } else if ft.is_file() {
            std::fs::copy(&from, &to).map_err(|e| io(&from, e))?;
        } // symlinks are skipped on purpose: a LevelDB store has none
    }
    Ok(())
}

fn read_all(dir: &Path) -> Result<BTreeMap<String, String>, ExtStorageError> {
    let opt = Options {
        create_if_missing: false,
        ..Options::default()
    };
    let mut db = DB::open(dir, opt).map_err(ldb)?;
    let mut it = db.new_iter().map_err(ldb)?;
    let mut out = BTreeMap::new();
    while let Some((k, v)) = it.next() {
        out.insert(
            String::from_utf8_lossy(&k).into_owned(),
            String::from_utf8_lossy(&v).into_owned(),
        );
    }
    Ok(out)
}

/// Read every key of an extension store **from a temporary copy** (the live store is never
/// opened, so reading cannot change it). Returns an empty map when no store exists.
pub fn read_extension_settings(
    user_data_dir: &Path,
    profile_dir: &str,
    ext_id: &str,
) -> Result<BTreeMap<String, String>, ExtStorageError> {
    validate(profile_dir, ext_id, "x")?;
    read_store_copy(&extension_store_path(user_data_dir, profile_dir, ext_id))
}

/// Read every key of the LevelDB store at `store` (e.g. a backup) from a temporary copy.
/// Returns an empty map when it does not exist.
pub fn read_store_copy(store: &Path) -> Result<BTreeMap<String, String>, ExtStorageError> {
    if !store.exists() {
        return Ok(BTreeMap::new());
    }
    let tmp = std::env::temp_dir().join(format!(
        "synchrome-read-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    // the copy is removed whatever happens (a failed copy used to leave an empty directory)
    let res = copy_dir(store, &tmp).and_then(|_| read_all(&tmp));
    let _ = std::fs::remove_dir_all(&tmp);
    res
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

/// Set `key = json_value` in an extension's `chrome.storage.local` store, safely.
///
/// `backup_root` must be outside the browser profile (e.g. the Synchrome app data dir).
pub fn write_extension_setting(
    user_data_dir: &Path,
    profile_dir: &str,
    ext_id: &str,
    key: &str,
    json_value: &str,
    backup_root: &Path,
) -> Result<WriteReport, ExtStorageError> {
    validate(profile_dir, ext_id, key)?;
    serde_json::from_str::<serde_json::Value>(json_value)
        .map_err(|e| ExtStorageError::Invalid(format!("value is not JSON: {e}")))?;

    if is_browser_running(user_data_dir) {
        return Err(ExtStorageError::BrowserRunning);
    }
    let store = extension_store_path(user_data_dir, profile_dir, ext_id);
    let existed = store.exists();
    if existed && is_store_locked(&store) {
        return Err(ExtStorageError::Locked);
    }

    // 1. backup (outside the profile)
    let backup_path = if existed {
        let b = backup_root.join(format!(
            "{}-{}-{}",
            unique_suffix(),
            profile_dir.replace(' ', "_"),
            ext_id
        ));
        copy_dir(&store, &b)?;
        Some(b)
    } else {
        None
    };

    // 2. staging copy next to the store (same filesystem, so renames are atomic)
    let parent = store
        .parent()
        .ok_or_else(|| ExtStorageError::Invalid("store has no parent".into()))?
        .to_path_buf();
    std::fs::create_dir_all(&parent).map_err(|e| io(&parent, e))?;
    let suffix = unique_suffix();
    let staging = parent.join(format!("{ext_id}.synchrome-staging-{suffix}"));
    let _ = std::fs::remove_dir_all(&staging);
    if existed {
        copy_dir(&store, &staging)?;
    } else {
        std::fs::create_dir_all(&staging).map_err(|e| io(&staging, e))?;
    }

    let result = (|| {
        // 3. modify the staging copy: preserve every key, set ours
        let before = if existed {
            read_all(&staging)?
        } else {
            BTreeMap::new()
        };
        {
            let opt = Options {
                create_if_missing: true,
                ..Options::default()
            };
            let mut db = DB::open(&staging, opt).map_err(ldb)?;
            db.put(key.as_bytes(), json_value.as_bytes()).map_err(ldb)?;
            db.flush().map_err(ldb)?;
        }
        // 4. verify on a fresh open: our key is right, every previous key survived unchanged
        let after = read_all(&staging)?;
        if after.get(key).map(String::as_str) != Some(json_value) {
            return Err(ExtStorageError::Verify("written key not read back".into()));
        }
        let preserved = before.iter().filter(|(k, _)| k.as_str() != key).count();
        for (k, v) in before.iter().filter(|(k, _)| k.as_str() != key) {
            if after.get(k) != Some(v) {
                return Err(ExtStorageError::Verify(format!(
                    "existing key {k:?} changed"
                )));
            }
        }
        Ok(preserved)
    })();
    let preserved_keys = match result {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };

    // 5. swap: store -> old, staging -> store, drop old (restore on failure)
    if existed {
        let old = parent.join(format!("{ext_id}.synchrome-old-{suffix}"));
        std::fs::rename(&store, &old).map_err(|e| io(&store, e))?;
        if let Err(e) = std::fs::rename(&staging, &store) {
            let _ = std::fs::rename(&old, &store);
            let _ = std::fs::remove_dir_all(&staging);
            return Err(io(&store, e));
        }
        let _ = std::fs::remove_dir_all(&old);
    } else {
        std::fs::rename(&staging, &store).map_err(|e| io(&store, e))?;
    }

    Ok(WriteReport {
        store_path: store,
        backup_path,
        created: !existed,
        preserved_keys,
    })
}

/// Read every key of one area store of an extension (from a temporary copy; the live store is
/// never opened). Refuses while the browser runs: a copy of a store in use can miss its
/// newest records, and a missing key would look like a delete.
pub fn read_extension_area(
    user_data_dir: &Path,
    profile_dir: &str,
    ext_id: &str,
    area: Area,
) -> Result<BTreeMap<String, String>, ExtStorageError> {
    validate(profile_dir, ext_id, "x")?;
    if is_browser_running(user_data_dir) {
        return Err(ExtStorageError::BrowserRunning);
    }
    let store = area_store_path(user_data_dir, profile_dir, ext_id, area);
    if store.exists() && is_store_locked(&store) {
        return Err(ExtStorageError::Locked);
    }
    read_store_copy(&store)
}

/// What a batch write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchReport {
    pub store_path: PathBuf,
    pub backup_path: Option<PathBuf>,
    pub written: usize,
    pub deleted: usize,
    /// Keys present before and not part of the batch (left exactly as they were).
    pub preserved_keys: usize,
}

/// Apply `puts` (key, JSON text) and `deletes` to one area store of one extension, safely:
/// browser closed, store LOCK free, backup outside the profile, staging copy, verification
/// (every written key reads back, every deleted key is gone, every other key is unchanged),
/// atomic swap; on any failure nothing changes. Only the given extension's store is touched.
pub fn apply_extension_area(
    user_data_dir: &Path,
    profile_dir: &str,
    ext_id: &str,
    area: Area,
    puts: &[(String, String)],
    deletes: &[String],
    backup_root: &Path,
) -> Result<BatchReport, ExtStorageError> {
    validate(profile_dir, ext_id, "x")?;
    for (k, v) in puts {
        if k.is_empty() {
            return Err(ExtStorageError::Invalid("empty key".into()));
        }
        serde_json::from_str::<serde_json::Value>(v)
            .map_err(|e| ExtStorageError::Invalid(format!("value of {k:?} is not JSON: {e}")))?;
    }
    if is_browser_running(user_data_dir) {
        return Err(ExtStorageError::BrowserRunning);
    }
    let store = area_store_path(user_data_dir, profile_dir, ext_id, area);
    let existed = store.exists();
    if existed && is_store_locked(&store) {
        return Err(ExtStorageError::Locked);
    }
    let backup_path = if existed {
        let b = backup_root.join(format!(
            "{}-{}-{}-{}",
            unique_suffix(),
            profile_dir.replace(' ', "_"),
            area.as_str(),
            ext_id
        ));
        copy_dir(&store, &b)?;
        Some(b)
    } else {
        None
    };
    let parent = store
        .parent()
        .ok_or_else(|| ExtStorageError::Invalid("store has no parent".into()))?
        .to_path_buf();
    std::fs::create_dir_all(&parent).map_err(|e| io(&parent, e))?;
    let suffix = unique_suffix();
    let staging = parent.join(format!("{ext_id}.synchrome-staging-{suffix}"));
    let _ = std::fs::remove_dir_all(&staging);
    if existed {
        copy_dir(&store, &staging)?;
    } else {
        std::fs::create_dir_all(&staging).map_err(|e| io(&staging, e))?;
    }
    let result = (|| {
        let before = if existed {
            read_all(&staging)?
        } else {
            BTreeMap::new()
        };
        {
            let opt = Options {
                create_if_missing: true,
                ..Options::default()
            };
            let mut db = DB::open(&staging, opt).map_err(ldb)?;
            for (k, v) in puts {
                db.put(k.as_bytes(), v.as_bytes()).map_err(ldb)?;
            }
            for k in deletes {
                db.delete(k.as_bytes()).map_err(ldb)?;
            }
            db.flush().map_err(ldb)?;
        }
        let after = read_all(&staging)?;
        for (k, v) in puts {
            if after.get(k) != Some(v) {
                return Err(ExtStorageError::Verify(format!("{k:?} not read back")));
            }
        }
        for k in deletes {
            if after.contains_key(k) {
                return Err(ExtStorageError::Verify(format!("{k:?} still present")));
            }
        }
        let touched: std::collections::BTreeSet<&str> = puts
            .iter()
            .map(|(k, _)| k.as_str())
            .chain(deletes.iter().map(String::as_str))
            .collect();
        let mut preserved = 0;
        for (k, v) in before.iter().filter(|(k, _)| !touched.contains(k.as_str())) {
            if after.get(k) != Some(v) {
                return Err(ExtStorageError::Verify(format!(
                    "unrelated key {k:?} changed"
                )));
            }
            preserved += 1;
        }
        if after.len() != preserved + puts.len() {
            return Err(ExtStorageError::Verify(
                "unexpected keys after write".into(),
            ));
        }
        Ok(preserved)
    })();
    let preserved_keys = match result {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    if existed {
        let old = parent.join(format!("{ext_id}.synchrome-old-{suffix}"));
        std::fs::rename(&store, &old).map_err(|e| io(&store, e))?;
        if let Err(e) = std::fs::rename(&staging, &store) {
            let _ = std::fs::rename(&old, &store);
            let _ = std::fs::remove_dir_all(&staging);
            return Err(io(&store, e));
        }
        let _ = std::fs::remove_dir_all(&old);
    } else {
        std::fs::rename(&staging, &store).map_err(|e| io(&store, e))?;
    }
    Ok(BatchReport {
        store_path: store,
        backup_path,
        written: puts.len(),
        deleted: deletes.len(),
        preserved_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXT: &str = "ceflmoojogokmklgcmcihdgakbefgiee";

    fn seed_store(udd: &Path, profile: &str, kv: &[(&str, &str)]) {
        let store = extension_store_path(udd, profile, EXT);
        std::fs::create_dir_all(&store).unwrap();
        let opt = Options {
            create_if_missing: true,
            ..Options::default()
        };
        let mut db = DB::open(&store, opt).unwrap();
        for (k, v) in kv {
            db.put(k.as_bytes(), v.as_bytes()).unwrap();
        }
        db.flush().unwrap();
    }

    #[test]
    fn writes_into_a_new_store() {
        let udd = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(udd.path().join("Default")).unwrap();
        let r = write_extension_setting(
            udd.path(),
            "Default",
            EXT,
            "mappingToken",
            "\"abc\"",
            backups.path(),
        )
        .unwrap();
        assert!(r.created && r.backup_path.is_none() && r.preserved_keys == 0);
        let kv = read_extension_settings(udd.path(), "Default", EXT).unwrap();
        assert_eq!(kv.get("mappingToken").map(String::as_str), Some("\"abc\""));
        // no staging/old leftovers
        let siblings: Vec<_> = std::fs::read_dir(r.store_path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(siblings, vec![EXT.to_string()]);
    }

    #[test]
    fn preserves_foreign_keys_and_backs_up() {
        let udd = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        seed_store(
            udd.path(),
            "Profile 2",
            &[("settings", "{\"a\":1}"), ("counter", "7")],
        );
        let r = write_extension_setting(
            udd.path(),
            "Profile 2",
            EXT,
            "mappingToken",
            "\"t1\"",
            backups.path(),
        )
        .unwrap();
        assert!(!r.created);
        assert_eq!(r.preserved_keys, 2);
        let kv = read_extension_settings(udd.path(), "Profile 2", EXT).unwrap();
        assert_eq!(kv.len(), 3);
        assert_eq!(kv["settings"], "{\"a\":1}");
        assert_eq!(kv["counter"], "7");
        // backup = the store as it was before (no token)
        let b = read_all(r.backup_path.as_ref().unwrap()).unwrap();
        assert_eq!(b.len(), 2);
        assert!(!b.contains_key("mappingToken"));
    }

    #[test]
    fn rewriting_replaces_only_our_key() {
        let udd = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        seed_store(udd.path(), "Default", &[("other", "true")]);
        write_extension_setting(
            udd.path(),
            "Default",
            EXT,
            "mappingToken",
            "\"t1\"",
            backups.path(),
        )
        .unwrap();
        write_extension_setting(
            udd.path(),
            "Default",
            EXT,
            "mappingToken",
            "\"t2\"",
            backups.path(),
        )
        .unwrap();
        let kv = read_extension_settings(udd.path(), "Default", EXT).unwrap();
        assert_eq!(kv["mappingToken"], "\"t2\"");
        assert_eq!(kv["other"], "true");
    }

    #[test]
    fn refuses_when_browser_running() {
        let udd = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        seed_store(udd.path(), "Default", &[("other", "1")]);
        std::os::unix::fs::symlink("host-1234", udd.path().join("SingletonLock")).unwrap();
        let e = write_extension_setting(
            udd.path(),
            "Default",
            EXT,
            "mappingToken",
            "\"t\"",
            backups.path(),
        )
        .unwrap_err();
        assert!(matches!(e, ExtStorageError::BrowserRunning));
        assert!(!read_extension_settings(udd.path(), "Default", EXT)
            .unwrap()
            .contains_key("mappingToken"));
    }

    #[test]
    fn refuses_when_store_lock_is_held() {
        use std::os::unix::io::AsRawFd;
        let udd = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        seed_store(udd.path(), "Default", &[("other", "1")]);
        let store = extension_store_path(udd.path(), "Default", EXT);
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(store.join("LOCK"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(is_store_locked(&store));
        let e = write_extension_setting(
            udd.path(),
            "Default",
            EXT,
            "mappingToken",
            "\"t\"",
            backups.path(),
        )
        .unwrap_err();
        assert!(matches!(e, ExtStorageError::Locked));
        drop(f);
        assert!(!is_store_locked(&store));
    }

    #[test]
    fn rejects_bad_input_without_touching_disk() {
        let udd = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        for (dir, ext, key, val) in [
            ("../x", EXT, "k", "1"),
            ("Default", "not-an-id", "k", "1"),
            ("Default", EXT, "", "1"),
            ("Default", EXT, "k", "not json"),
        ] {
            assert!(
                write_extension_setting(udd.path(), dir, ext, key, val, backups.path()).is_err()
            );
        }
        assert_eq!(std::fs::read_dir(udd.path()).unwrap().count(), 0);
    }

    fn seed_area(udd: &Path, profile: &str, ext: &str, area: Area, kv: &[(&str, &str)]) {
        let store = area_store_path(udd, profile, ext, area);
        std::fs::create_dir_all(&store).unwrap();
        let opt = Options {
            create_if_missing: true,
            ..Options::default()
        };
        let mut db = DB::open(&store, opt).unwrap();
        for (k, v) in kv {
            db.put(k.as_bytes(), v.as_bytes()).unwrap();
        }
        db.flush().unwrap();
    }

    const OTHER: &str = "abcdefghijklmnopabcdefghijklmnop";

    #[test]
    fn batch_writes_one_area_of_one_extension_and_preserves_everything_else() {
        let udd = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        seed_area(
            udd.path(),
            "Default",
            EXT,
            Area::Local,
            &[("keep", "1"), ("gone", "2"), ("old", "\"a\"")],
        );
        seed_area(udd.path(), "Default", EXT, Area::Sync, &[("s", "true")]);
        seed_area(udd.path(), "Default", OTHER, Area::Local, &[("x", "9")]);
        let r = apply_extension_area(
            udd.path(),
            "Default",
            EXT,
            Area::Local,
            &[
                ("old".into(), "\"b\"".into()),
                ("new".into(), "{\"n\":[1,2]}".into()),
            ],
            &["gone".into()],
            backups.path(),
        )
        .unwrap();
        assert_eq!((r.written, r.deleted, r.preserved_keys), (2, 1, 1));
        let local = read_extension_area(udd.path(), "Default", EXT, Area::Local).unwrap();
        assert_eq!(local.len(), 3);
        assert_eq!(
            (local["keep"].as_str(), local["old"].as_str()),
            ("1", "\"b\"")
        );
        assert!(!local.contains_key("gone"));
        assert_eq!(
            read_extension_area(udd.path(), "Default", EXT, Area::Sync).unwrap()["s"],
            "true"
        );
        assert_eq!(
            read_extension_area(udd.path(), "Default", OTHER, Area::Local).unwrap()["x"],
            "9"
        );
        let b = read_all(r.backup_path.as_ref().unwrap()).unwrap();
        assert_eq!(b.len(), 3, "backup = the store as it was");
    }

    #[test]
    fn batch_refuses_open_browser_or_held_lock_and_bad_values_without_changes() {
        use std::os::unix::io::AsRawFd;
        let udd = tempfile::tempdir().unwrap();
        let backups = tempfile::tempdir().unwrap();
        seed_area(udd.path(), "Default", EXT, Area::Local, &[("k", "1")]);
        let bad = apply_extension_area(
            udd.path(),
            "Default",
            EXT,
            Area::Local,
            &[("k".into(), "not json".into())],
            &[],
            backups.path(),
        );
        assert!(matches!(bad, Err(ExtStorageError::Invalid(_))));
        let store = area_store_path(udd.path(), "Default", EXT, Area::Local);
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.join("LOCK"))
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        let locked = apply_extension_area(
            udd.path(),
            "Default",
            EXT,
            Area::Local,
            &[("k".into(), "2".into())],
            &[],
            backups.path(),
        );
        assert!(matches!(locked, Err(ExtStorageError::Locked)));
        assert!(matches!(
            read_extension_area(udd.path(), "Default", EXT, Area::Local),
            Err(ExtStorageError::Locked)
        ));
        drop(f);
        std::os::unix::fs::symlink("host-1234", udd.path().join("SingletonLock")).unwrap();
        let running = apply_extension_area(
            udd.path(),
            "Default",
            EXT,
            Area::Local,
            &[("k".into(), "2".into())],
            &[],
            backups.path(),
        );
        assert!(matches!(running, Err(ExtStorageError::BrowserRunning)));
        std::fs::remove_file(udd.path().join("SingletonLock")).unwrap();
        assert_eq!(
            read_extension_area(udd.path(), "Default", EXT, Area::Local).unwrap()["k"],
            "1",
            "unchanged"
        );
        assert_eq!(
            std::fs::read_dir(backups.path()).unwrap().count(),
            0,
            "nothing even backed up"
        );
    }

    #[test]
    fn an_unreadable_store_leaves_no_temporary_copy() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let store = d.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("CURRENT"), "x").unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o000)).unwrap();
        let leftovers = || {
            std::fs::read_dir(std::env::temp_dir())
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with(&format!("synchrome-read-{}-", std::process::id()))
                })
                .count()
        };
        let before = leftovers();
        assert!(read_store_copy(&store).is_err());
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
        // other tests of this process read stores concurrently: their copies are transient,
        // a leaked one would stay
        let settled = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            leftovers() <= before
        });
        assert!(settled, "a temporary copy was left behind");
    }
}
