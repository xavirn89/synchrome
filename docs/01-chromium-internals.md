# 01 — Chromium Profile Internals (Research Findings)

Status: research complete for the planning phase. Date: 2026-10-01.

This document answers brief questions 1–8, 18 and 19: what data exists, where it lives, how Chromium accesses it, what can be touched from outside and when, and how the target browsers differ.

Evidence labels used throughout:

- **[E]** — verified by an experiment run on this machine (macOS 27.0.1, Helium 0.18.2.1 / Chromium 154.0.8037.92, Google Chrome, Brave). Experiment scripts are described in §9.
- **[S]** — verified against Chromium / vendor source code or official documentation (source noted).
- **[U]** — plausible but unverified; listed again in `06-v1-plan.md` as an experiment to run before relying on it.

---

## 1. Anatomy of a Chromium user data directory

```text
<User Data>/                        one per browser installation (per OS user)
├── Local State                     JSON: profile list, browser-wide state
├── SingletonLock -> host-pid       POSIX liveness lock (symlink)   [E]
├── SingletonSocket, SingletonCookie
├── NativeMessagingHosts/           user-level native messaging manifests
├── External Extensions/            (optional) external-install descriptors
├── Default/                        first profile (Helium on this machine has no "Default"; profiles start at "Profile 2") [E]
├── Profile 1/, Profile 2/, …       further profiles
└── System Profile/, Guest Profile/
```

Inside a profile directory the files relevant to Synchrome are:

| Data | File(s) | Format | Written by browser | Readable while running | Writable while running |
|---|---|---|---|---|---|
| Bookmarks | `Bookmarks`, `Bookmarks.bak`, `AccountBookmarks` (signed-in account tree, M134+) | JSON | atomic temp+rename, 2.5 s debounce [S] | yes (atomic file) | **no** – overwritten within 2.5 s, read only at startup [S] |
| Preferences | `Preferences` | JSON | atomic temp+rename, 10 s debounce [S] | yes | **no** – same reason [S][E] |
| Tracked prefs MACs | `Secure Preferences` | JSON (`protection.macs`, `super_mac`, `*_encrypted_hash`) | same | yes | **no**; and tampering is detected [E] |
| History | `History`, `History-journal` | SQLite, schema v70, rollback journal (TRUNCATE) [E][S] | continuously | **no** – `locking_mode=EXCLUSIVE`, "database is locked" [E] | no |
| Favicons | `Favicons` (v9) | SQLite, exclusive | continuously | no [E] | no |
| Extension list + per-extension install state | `Secure Preferences` → `extensions.settings.<id>` (tracked) | JSON | debounced | yes | no |
| `chrome.storage.local` of extension X | `Local Extension Settings/<id>/` | LevelDB | on write | partially (see §5) | **no** – `LOCK` held via flock once the store is opened [E] |
| `chrome.storage.sync` of extension X | `Sync Extension Settings/<id>/` | LevelDB | on write | partially | no |
| Extension internal state | `Extension State/`, `Extension Rules/`, `Extension Scripts/` | LevelDB | on write | partially | no |
| Extension code | `Extensions/<id>/<version>/` | files | on install/update | yes | pointless (re-downloadable) |
| Web storage of extension origins | `Local Storage/leveldb` (shared, binary keys), `IndexedDB/chrome-extension_<id>_0.indexeddb.leveldb` | LevelDB, custom comparators | on write | hard | no |
| Cookies, Login Data, autofill | `Cookies`, `Login Data`, `Web Data` | SQLite, OSCrypt-encrypted columns | continuously | no | no (and non-portable) |

Timestamps: every Chromium time value on disk is **microseconds since 1601-01-01 UTC** (the "WebKit epoch"), stored as integers in SQLite and as decimal strings in JSON (e.g. `"date_added": "13420140031319402"`) [E]. The WebExtension APIs expose milliseconds since the Unix epoch instead. Conversion: `unix_ms = webkit_us / 1000 − 11644473600000`.

---

## 2. Bookmarks

**Storage.** `Bookmarks` is `{ "version": 1, "checksum": "<md5 hex>", "roots": { "bookmark_bar", "other", "synced" }, ... }`. Each node has `id` (string, sequential int64), `guid` (RFC 4122 string, stable for the node's lifetime), `name`, `type` (`url` | `folder`), `date_added`, `date_modified` (folders), `date_last_used`, `url`, `children`, optional `meta_info` [E][S].

**Checksum is not enforced.** Chromium decodes a file with a wrong checksum, marks the model for recovery and recomputes the checksum on the next save. A file with `version != 1` is rejected [S]. **Experiment [E]:** a node appended by hand with a bogus `checksum` to a closed Helium profile was loaded and still present after relaunch.

**Live access via `chrome.bookmarks`.** Full CRUD plus events (`onCreated`, `onRemoved` with the removed subtree, `onChanged`, `onMoved`, `onChildrenReordered`, `onImportBegan/Ended`). The write-rate limits are deprecated (effectively unlimited) [S]. Limitations [S]:

- The API does **not** expose `guid`. Node identity across devices must be Synchrome's own.
- `dateAdded`, `dateLastUsed`, `dateGroupModified` are read-only; a bookmark recreated on another device gets "now".
- Root folders must be found via `folderType` (`bookmarks-bar` / `other` / `mobile` / `managed`), not by hard-coded ids `1`/`2`/`3`. With account bookmarks (M134+) a signed-in profile can have **two** bookmarks-bar folders, distinguished by `syncing`.
- `unmodifiable: "managed"` nodes (enterprise policy) must be skipped.

**Implication.** Bookmarks can be synced *live* through the API without touching files. Full-fidelity timestamps would require the agent to write the `Bookmarks` file while the browser is closed; this is an optional "fidelity mode", not a requirement.

---

## 3. Preferences and Secure Preferences

**Write/read model [S].** `JsonPrefStore` reads the file once at startup, keeps everything in memory and flushes with `ImportantFileWriter` (temp file + rename) about 10 s after a change and on shutdown. There is no file watcher. Consequently any external edit while the browser runs is silently lost; edits must happen while the profile is closed. **Experiment [E]:** untracked keys (`bookmark_bar.show_on_all_tabs`) written into `Preferences` of a closed profile were still present after relaunch in Helium, Chrome and Brave.

**Tracked preferences [S][E].** A small set of security-relevant prefs is protected by HMAC-SHA256 "MACs" stored in `Secure Preferences` under `protection.macs`, with a `super_mac` over the whole set. Observed tracked paths on this machine's Helium profile: `homepage`, `homepage_is_newtabpage`, `browser.show_home_button`, `session.restore_on_startup`, `session.startup_urls`, `default_search_provider_data.template_url_data`, `search_provider_overrides`, `pinned_tabs`, `extensions.settings.<id>` (one per extension), `extensions.ui.developer_mode`, `extensions.install.initiallist`, `media.storage_id_salt`, `prefs.preference_reset_time`, `enterprise_signin.policy_recovery_token`, plus `account_values.*` mirrors and `*_encrypted_hash` companions (newer OSCrypt-backed hash path).

MAC = `HMAC-SHA256(seed, device_id ‖ pref_path ‖ canonical_json(value))`, hex encoded [S]. `seed` comes from `IDR_PREF_HASH_SEED_BIN`, which is only compiled into Google-branded Chrome; **every other build (Chromium, Helium, Brave, Edge presumably) uses an empty seed** [S]. `device_id` is the `IOPlatformUUID` on macOS, a machine-specific id on Windows (ungoogled/Helium ship `--disable-machine-id`), and empty on Linux [S].

Enforcement is decided by `GetSettingsEnforcementGroup()`: **enforced on Windows and macOS for all builds, never on Linux** [S]. **Experiment [E]:** tampering `homepage`, `homepage_is_newtabpage`, `browser.show_home_button`, `session.restore_on_startup`, `session.startup_urls` in `Secure Preferences` of a closed profile without recomputing MACs → after relaunch all five were reset to defaults and `prefs.preference_reset_time` was stamped, identically in Helium, Google Chrome and Brave. This corrects the common assumption that ungoogled builds skip enforcement: they enforce with an empty seed.

**Implications.**

1. Untracked prefs are trivially portable JSON: filter, merge, write while closed.
2. Tracked prefs are *writable but require forging the MAC*: feasible on non-branded builds (seed empty, device id obtainable), and on Google Chrome only if the seed is extracted from its `resources.pak`. Both need the `*_encrypted_hash` companion handled (removing it should trigger the documented HMAC-fallback "healing" path [S][U]).
3. The installed-extension list (`extensions.settings.*`) is itself tracked, so "install by editing prefs" is not viable without MAC forging; §7 covers better installation routes.

**Which prefs are device-specific** (never sync): window placement (`browser.window_placement*`), download/save/select-file directories (`download.default_directory`, `savefile.default_directory`, `selectfile.last_directory`), `profile.exit_type`, `profile.name`/avatar (identity is per-device mapping), `media.*` salts, `gcm`, `invalidation`, `signin`, `sync`, `google.services`, `enterprise*`, `optimization_guide`, `segmentation_platform`, `privacy_sandbox`, metrics/timestamps (`*_last_*`, `*_time`), `extensions.last_chrome_version`, `extensions.settings` (contains absolute `path`), `hardware_acceleration_mode`, `printing`, `web_apps`, `apps`, `sessions`, `saved_tab_groups`, `countryid_at_install`, OS keychain state. The full allow/deny list is in `03-data-model-and-sync.md`.

**Which prefs are worth syncing** (observed present): `bookmark_bar.show_on_all_tabs`, `intl.selected_languages`, `spellcheck.dictionaries`, `devtools.preferences`, `default_search_provider*` (tracked), `homepage*`/`session.*` (tracked), `profile.content_settings.exceptions.*` (per-site permissions: ~100 categories observed, e.g. `cookies`, `javascript`, `notifications`, `popups`, `sound`, `geolocation`, `helium_noise`, Brave's `braveShields`), `profile.default_content_setting_values`, `autofill.*` toggles, `translate_*`, `webkit.webprefs.*` fonts/zoom, `extensions.pinned_extensions`, `browser.theme`, `toolbar`, `omnibox`, `ntp`, `custom_handlers`, `protocol_handler`, `site_search_settings`, `vertical_tabs`, `helium.*` (Helium-specific UI prefs).

Extension APIs can set only a small fraction of these (`chrome.privacy`, `chrome.fontSettings`, `chrome.contentSettings`, `chrome.proxy`, `chrome.accessibilityFeatures`) — roughly 10–15 % of the file [S]. Homepage / startup / default search have no runtime API (`chrome_settings_overrides` is static and replaces rather than syncs) [S]. **Preference sync therefore requires the native agent.**

---

## 4. History

**Schema (v70) [E][S].**

```sql
urls(id, url, title, visit_count, typed_count, last_visit_time, hidden)
visits(id, url, visit_time, from_visit, external_referrer_url, transition, segment_id,
       visit_duration, incremented_omnibox_typed_score, opener_visit,
       originator_cache_guid, originator_visit_id, originator_from_visit,
       originator_opener_visit, is_known_to_sync, consider_for_ntp_most_visited,
       visited_link_id, app_id)
visit_source(id, source)          -- only non-browsed visits have a row
-- plus segments, segment_usage, keyword_search_terms, content/context_annotations,
-- clusters*, downloads*, visited_links, history_sync_metadata, meta
```

`transition` = core type in the low byte (LINK=0, TYPED=1, AUTO_BOOKMARK=2, …, RELOAD=8) OR qualifier flags (`CHAIN_START=0x10000000`, `CHAIN_END=0x20000000`, `FROM_ADDRESS_BAR=0x02000000`, …). `visit_source.source`: SYNCED=0, BROWSED=1 (implicit), EXTENSION=2, FIREFOX_IMPORTED=3, … [S]. A real Helium visit observed: `transition=805306374` = `0x30000006` = CHAIN_START|CHAIN_END|AUTO_TOPLEVEL [E].

**Locking [E][S].** Chromium opens its SQLite databases with `locking_mode=EXCLUSIVE`; an external `sqlite3 'file:History?mode=ro'` fails with "database is locked" while the browser runs. The journal is a rollback journal (`History-journal`, TRUNCATE mode); WAL is behind a disabled feature flag. Copying the file while running produced an `integrity_check = ok` copy in our test, but a torn copy is possible mid-transaction, so live copies are a best-effort read only.

**Expiry [S].** Visits older than **90 days** are deleted continuously by the history expirer. Anything injected older than that is removed again.

**Live access via `chrome.history` [S].** Reading is lossless: `search({text:"", startTime:0, maxResults:N})` returns `url, title, lastVisitTime, visitCount, typedCount`; `getVisits({url})` returns every visit with `visitTime`, `transition`, `referringVisitId`, `isLocal`. `onVisited` fires per navigation, `onVisitRemoved` on deletions. Writing is lossy: `addUrl({url})` records a visit **now**, transition `link`, source EXTENSION, no title, no timestamp.

**External insert [E].** Inserting into `urls` + `visits` (+ `visit_source` = 2) in a closed profile, then relaunching: rows persisted, `integrity_check` ok, Chromium did not purge or rewrite them. A faithful insert must also maintain `urls.visit_count`, `urls.last_visit_time`, `urls.typed_count` and optionally `segments`/`segment_usage` (NTP "most visited"); Chromium does not repair those retroactively [S].

**Implication.** Read history live through the API (lossless); write history through the agent into SQLite while the profile is closed (full fidelity). `addUrl` is only a degraded fallback and must never run silently.

---

## 5. Extension storage (LevelDB)

**Mapping [S].** `Local Extension Settings/<id>` = `chrome.storage.local`; `Sync Extension Settings/<id>` = `chrome.storage.sync` (stored locally even without Google sync); `Managed Extension Settings/<id>` = policy; `chrome.storage.session` is in-memory only. `Extension State` holds Chromium's own bookkeeping (not an extension's data) and must not be synced.

**Encoding [E][S].** Keys are the plain UTF-8 `chrome.storage` keys; values are the JSON serialisation of the value (`true`, `5`, `"string"`, `{...}`, `[...]`). Default bytewise comparator, Snappy-compressed blocks, standard LevelDB `MANIFEST-*`, `CURRENT`, `*.ldb`, `*.log`, `LOCK`. A freshly created store consists only of `000003.log` + `MANIFEST-000001` + `CURRENT` + `LOCK` [E] — i.e. all data in the write-ahead log, no tables.

**Locking [E].** The browser holds a `flock` on `LOCK` only for extension stores that have been opened in this session (3 of 5 stores locked on a running Helium; idle extensions' stores were free). A second LevelDB *library* instance cannot open a locked store. A custom reader that parses `.ldb` tables and the `.log` directly does not need the lock; the risk is a torn tail of the current `.log`, which is self-detecting via per-record CRCs (the same thing LevelDB does on recovery).

**Portability [S][E].** Values are plain JSON, not OSCrypt-encrypted, so the data is portable. Whether the *content* is portable depends on the extension: settings, filter lists and allow-lists are; caches, device ids (`clientId` observed in one store) and session tokens are not. There is no cross-extension API to read another extension's storage [S].

**Implication.** Extension settings require the agent. Read: parse LevelDB directly (live best-effort or when closed). Write: only when closed, by replacing the directory with a freshly generated store (a single `.log` WriteBatch + `MANIFEST` + `CURRENT`), after backing up the old directory. `Local Storage` / `IndexedDB` of extension origins use binary key encodings and custom comparators and are out of scope for v1.

---

## 6. Extension list and installation state

`chrome.management.getAll()` returns `id, name, version, enabled, disabledReason, installType (admin|development|normal|sideload|other), updateUrl, homepageUrl, permissions, hostPermissions, type, mayDisable, mayEnable` [S]. `setEnabled` "in most cases must be called in the context of a user gesture and may present a native confirmation UI"; `uninstall` of another extension always shows a dialog [S]. **There is no API to install an extension** [S].

On disk, `Secure Preferences → extensions.settings.<id>` holds `path`, `manifest`, `location` (1 = internal/Web Store, 5 = component, 4 = unpacked), `from_webstore`, `first_install_time`, `last_update_time`, `disable_reasons`, `granted_permissions`, `preferences`, … [E]. It is tracked (MAC-protected) and contains absolute paths, so it is read-only input for Synchrome.

Web Store extension ids are derived from the publisher key and therefore identical on every machine; unpacked/dev extensions have path-derived ids and must be excluded from sync [S].

Installation routes without the Web Store click (details and per-browser status in `02-feasibility.md`):

1. **External extensions descriptor** — `<User Data>/External Extensions/<id>.json` containing `{"external_update_url": "https://clients2.google.com/service/update2/crx"}` (macOS and Chromium-branded Linux per-user; Windows via `HKCU\Software\<Vendor>\<Product>\Extensions\<id>` `update_url`). The user sees a one-time "enable" bubble on Windows/macOS; Linux installs silently. If the user uninstalls, the id is blocklisted from further external installs [S].
2. **Policy** `ExtensionSettings` (`installation_mode: normal_installed` = auto-install, user may disable) or `ExtensionInstallForcelist`. Readable from HKCU on Windows without admin; on macOS a user-level `defaults write` is read only at *recommended* level and probably ignored for these policies [S][U]; Linux needs root. Shows "Managed by your organization" [S].
3. **Guided install** — open `https://chromewebstore.google.com/detail/<id>` and let the user click. Works everywhere.

---

## 7. Data that must never be synchronised directly

| Data | Why |
|---|---|
| `Cookies`, `Login Data`, encrypted `Web Data` columns | OSCrypt: macOS Keychain "… Safe Storage" item, Windows DPAPI + App-Bound Encryption, Linux keyring. Ciphertext is machine/profile-bound [S]. |
| `Sessions/`, `Session Storage/`, `Current Tabs` | live session state; restoring on another device = tab hijack; written constantly |
| `Local State` (except reading the profile list) | device-wide state, machine ids, variations |
| `Extension State`, `Extension Rules`, `Extension Scripts` | Chromium internal, references local install paths/versions |
| `Extensions/` code | re-downloadable; version skew between devices |
| caches (`GPUCache`, `*ShaderCache`, `DawnCache`, `Code Cache`, `Service Worker/CacheStorage`) | ephemeral |
| `SingletonLock/Socket/Cookie`, `lockfile`, `LOCK` files | liveness primitives |
| `Secure Preferences` as a whole | MACs are bound to device id |
| device-specific prefs (§3) | paths, placement, salts |
| Brave `brave.wallet.keyrings`, `brave.rewards.*` | encrypted wallet material, account-bound |

---

## 8. Browser and platform differences

### 8.1 Data directories

| Browser | macOS (`~/Library/Application Support/…`) | Windows (`%LOCALAPPDATA%\…`) | Linux (`~/.config/…`) |
|---|---|---|---|
| Helium | `net.imput.helium` [E][S] | `imput\Helium\User Data` [S] | `net.imput.helium` (override `HELIUM_CONFIG_HOME`) [S] |
| Chromium | `Chromium` | `Chromium\User Data` | `chromium`; Flatpak `~/.var/app/org.chromium.Chromium/config/chromium`; Snap `~/snap/chromium/common/chromium` [U] |
| Google Chrome | `Google/Chrome` | `Google\Chrome\User Data` | `google-chrome` |
| Brave | `BraveSoftware/Brave-Browser` | `BraveSoftware\Brave-Browser\User Data` | `BraveSoftware/Brave-Browser`; Flatpak `~/.var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser` |
| Edge | `Microsoft Edge` (+ ` Beta/Dev/Canary`) | `Microsoft\Edge\User Data` | `microsoft-edge`; Flatpak `~/.var/app/com.microsoft.Edge/config/microsoft-edge` |

Profiles are enumerated from `Local State → profile.info_cache` (keyed by directory name; each entry has `name`, `avatar_icon`, `user_name`, `gaia_*`, `is_ephemeral`, …), `profile.last_used`, `profile.last_active_profiles`, `profile.profiles_order` [E].

### 8.2 Native messaging host manifest locations

| Browser | macOS user-level | Linux user-level | Windows registry |
|---|---|---|---|
| Helium | `net.imput.helium/NativeMessagingHosts/`, **falls back to Chrome's** user and system dirs [S][E] | `~/.config/net.imput.helium/NativeMessagingHosts/`, falls back to Chrome's [S] | `HKCU\SOFTWARE\Chromium\NativeMessagingHosts` then `…\Google\Chrome\…` (upstream Chromium-branding behaviour; Helium patch says "not supported on windows") [U] |
| Chromium | `Chromium/NativeMessagingHosts/` | `~/.config/chromium/NativeMessagingHosts/` | `HKCU\SOFTWARE\Chromium\NativeMessagingHosts` |
| Chrome | `Google/Chrome/NativeMessagingHosts/` | `~/.config/google-chrome/NativeMessagingHosts/` | `HKCU\SOFTWARE\Google\Chrome\NativeMessagingHosts` |
| Brave | `BraveSoftware/Brave-Browser/NativeMessagingHosts/` — **no Chrome fallback** (open issue since 2019) [S] | `~/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts/` | `HKCU\SOFTWARE\BraveSoftware\Brave-Browser\NativeMessagingHosts` [U] |
| Edge | `Microsoft Edge/NativeMessagingHosts/` | `~/.config/microsoft-edge/NativeMessagingHosts/` | `HKCU\SOFTWARE\Microsoft\Edge\NativeMessagingHosts`, falls back to Chromium and Chrome keys [S] |

Protocol limits: 4-byte native-endian length prefix + UTF-8 JSON; **host→extension ≤ 1 MB per message**, extension→host ≤ 64 MiB; `allowed_origins` needs the exact extension id (so the extension must pin its id via the manifest `key`) [S]. A `connectNative` port keeps the MV3 service worker alive [S]. On Windows the host must be an `.exe` (launched directly, force-killed ~2 s after disconnect) or a `.bat` wrapper [S].

### 8.3 Policy locations

| Browser | macOS plist domain | Windows | Linux |
|---|---|---|---|
| Helium | `net.imput.helium` | `HKLM/HKCU\SOFTWARE\Policies\Helium` [S] | `/etc/chromium/policies/` [S] |
| Chromium | `org.chromium.Chromium` | `…\Policies\Chromium` | `/etc/chromium/policies/` |
| Chrome | `com.google.Chrome` | `…\Policies\Google\Chrome` | `/etc/opt/chrome/policies/` |
| Brave | `com.brave.Browser` | `…\Policies\BraveSoftware\Brave` | `/etc/brave/policies/` |
| Edge | `com.microsoft.Edge` | `…\Policies\Microsoft\Edge` | `/etc/opt/edge/policies/` |

### 8.4 Helium specifics [S][E]

- Based on ungoogled-chromium with its own patch set; version 0.18.2.1 tracks Chromium 154; shipping for macOS, Windows and Linux (all "beta").
- Chrome Web Store installs and extension auto-updates work **only through Helium's proxy** (`https://services.helium.imput.net/ext/*`); the Google update URL is rewritten at runtime. If "Helium services" are disabled, extension installs fail. The binary still contains `chromewebstore.google.com` URLs and the policy names `ExtensionSettings`, `ExtensionInstallForcelist`, `HomepageLocation` [E]. Force-install policies are reported broken (helium#1737, #2259) — consistent with the URL rewriting.
- Native messaging works (Bitwarden host present in `net.imput.helium/NativeMessagingHosts/` on this machine) [E]. Helium scans Chrome's manifest directories as a fallback on macOS/Linux.
- Preference MAC enforcement is **active** (experiment §3) despite the ungoogled base.
- No `Default` profile directory here; profile directories were `Profile 2`…`Profile 6` [E]. Never assume `Default` exists.
- Helium ships its own encrypted "vault" sync stack and a disabled-by-default `syncVaultProvider` extension API (PR #2456) — a potential future integration point, not something to depend on.
- Helium-specific prefs live under `helium.*` and content-setting category `helium_noise`.

### 8.5 Brave specifics

Standard Chromium layout plus `brave.*` prefs (Shields, Rewards, Wallet) and extra content-setting categories (`braveShields`, `cosmeticFiltering*`). Brave Sync v2 is an independent engine (chain + passphrase) and does not interfere unless both run. Web Store installs work. Preference enforcement confirmed [E]. No Chrome native-messaging fallback.

### 8.6 Edge specifics

Favorites are the standard `Bookmarks` file. Both Chrome Web Store and Edge Add-ons are supported ("allow extensions from other stores" prompt). Policies and external-extension descriptors follow the Chromium model with Microsoft paths; force-install of non-Add-ons extensions is restricted on non-domain-joined machines [S]. Edge Collections are not worth syncing.

### 8.7 Platform limitations

**macOS (observed on 27.0.1) [E].** The data directories of *known* browsers — Google Chrome, Brave, Microsoft Edge, Firefox — are TCC-protected: a process without Full Disk Access gets `EPERM` on `Local State`, profile directories, `History`, `Bookmarks`, `Preferences`, even `External Extensions/`. The `NativeMessagingHosts/` subdirectory is explicitly exempt (readable). Helium's, Vivaldi's and Chromium's directories were fully readable, i.e. the protection follows a list of well-known bundle paths. Consequences: the agent needs **Full Disk Access** (one-time user grant) to manage Chrome/Brave/Edge profiles, *or* file access must be performed by a process the browser itself spawned (native messaging host), which should inherit the browser's TCC identity [U — must test]. Code signing + notarization (Apple Developer Program) is required for a distributable agent.

**Windows.** Open file handles without `FILE_SHARE_DELETE` make rename/delete fail (`ERROR_SHARING_VIOLATION`); LevelDB `LOCK` files produce "File currently in use". Liveness detection uses the `lockfile` (opened `FILE_FLAG_DELETE_ON_CLOSE`) and the hidden `Chrome_MessageWindow` whose title is the user-data-dir path [S]. Long paths need `LongPathsEnabled` + manifest. SmartScreen reputation for an unsigned/new signer is a distribution hurdle.

**Linux.** No preference MAC enforcement [S]. Native messaging hosts for Flatpak/Snap browsers require portal support that is not uniformly available → **unsupported in v1**. Policies need root. Per-user `External Extensions` is only wired for Chromium-branded builds [U].

**Liveness detection (all platforms).** POSIX: `SingletonLock` is a symlink to `<hostname>-<pid>`; the profile is in use if the hostname matches and the pid is alive (`kill(pid, 0)`) [E][S]. Windows: `FindWindow` for `Chrome_MessageWindow` titled with the user-data-dir, `lockfile` as secondary signal [S]. Note the lock covers the whole **user data dir**, not an individual profile; closing one profile window while another profile of the same browser stays open does not release it.

---

## 9. Experiments performed (reproducible)

All ran against throwaway `--user-data-dir` profiles in `--headless=new` mode, so no real profile was modified. Scripts live in the session scratchpad and will be moved into `tests/experiments/` when the repository is initialised.

1. **tracked_prefs** (Helium, Chrome, Brave): launch 15 s → quit → write tracked prefs into `Secure Preferences` without MACs and untracked prefs into `Preferences` → relaunch 15 s → quit → compare. Result: all tracked values reset, `prefs.preference_reset_time` stamped; untracked values kept. Identical on the three browsers.
2. **history_bookmarks** (Helium): one real visit → quit → insert `urls`/`visits`/`visit_source` rows, append a bookmark node to a synthesised `Bookmarks` with checksum `deadbeef` → relaunch → quit. Result: rows and node present, `integrity_check` ok, checksum untouched (no re-save because nothing changed).
3. **Lock probing** on the live Helium profile: SQLite direct open → "database is locked" for `History`, `Favicons`, `Web Data`, `Top Sites`; file copy → integrity ok; LevelDB `LOCK` flock held for 3 of 5 extension stores.
4. **LevelDB log parsing** of three extension stores: keys plain, values JSON, 197 ops in a 6 KB log.
5. **macOS TCC probing** with the harness sandbox disabled: EPERM map in §8.7.
6. **E13 browser preferences** (Helium 154, Chrome 154, slice 12): which candidate prefs are untracked, persisted in `Preferences` and keep an external edit; Helium's `bookmark_bar.visibility_state`; `intl.selected_languages` as the source of `accept_languages`; the product writer validated on a real Helium. Details and table: `tests/experiments/RESULTS.md` §E13; normative result: `docs/03` §3.6.

6. **e12_profile_creation** (Helium, Chrome): create a profile with `--profile-directory`, rename it via `Local State` while closed, relaunch → name sticks; sequential creation works. PASS.
7. **e3_external_extension** (Helium, Chrome): Google Chrome does not read `<user-data-dir>/External Extensions/` (only its fixed system path); Helium (Chromium-branded) does read it and issues the external update request. Descriptors are per-browser-install, not per-profile.

Full results in `tests/experiments/RESULTS.md`. Remaining experiments are listed in `06-v1-plan.md`.
