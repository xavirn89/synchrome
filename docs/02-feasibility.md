# 02 — Feasibility: What Is Realistically Syncable, and How

This document turns the findings in `01-chromium-internals.md` into decisions. It answers brief questions 1, 3–11 and 20: for each data category, whether Synchrome can sync it, through which access path, under which conditions, and what the closest safe alternative is when full fidelity is impossible.

## 1. Access paths available to Synchrome

| Path | Can read | Can write | Constraints |
|---|---|---|---|
| **A. WebExtension APIs** (service worker in the browser) | bookmarks, history, extension list/state, a few prefs via `chrome.privacy`/`fontSettings`/`contentSettings` | bookmarks (full), history (lossy `addUrl`), enable/disable extensions (user gesture), the same few prefs | live, event-driven; no filesystem; MV3 worker is killed after 30 s idle unless a native port is open |
| **B. Agent, profile open** | `Preferences`, `Secure Preferences`, `Bookmarks`, `Local State` (atomic JSON files); LevelDB stores via own parser (best-effort); SQLite only via file copy (best-effort) | **nothing** – every write is either locked out or overwritten | needs liveness detection; on macOS needs TCC access for Chrome/Brave/Edge |
| **C. Agent, profile closed** | everything | `Preferences` (untracked freely; tracked with MAC forging), `Bookmarks`, `History` SQLite, LevelDB extension stores, `External Extensions/` descriptors | must be certain the user-data-dir is not in use; always back up first |

Rule of thumb that drives the whole design: **read live, write live when an API exists, otherwise queue the write and apply it the next time the profile is closed.**

## 2. Category-by-category verdict

### 2.1 Bookmarks — ✅ fully syncable, live

- Read/write/events through `chrome.bookmarks` (path A). No file access needed.
- Identity: Synchrome assigns its own UUID per node and persists a `localId ↔ uuid` map per device. Bootstrap pairing of pre-existing nodes uses `(root folderType, path of titles, title, url, dateAdded)`. The agent can additionally read `guid`s from the `Bookmarks` file to re-anchor the map if Chromium ever reassigns ids (recovery).
- Loss: `dateAdded`/`dateLastUsed` cannot be set via API; recreated nodes get "now". Synchrome keeps the original `createdAt` in its own data model and can optionally write it into the `Bookmarks` file while closed ("fidelity mode", not in v1).
- Skip `managed` nodes; handle account-bookmark duplicates by `folderType` + `syncing` (v1: sync only the non-`syncing` tree and warn if an account tree exists).

### 2.2 Installed extensions — ✅ list and enabled-state syncable; installation is assisted, not silent everywhere

- Read via `chrome.management.getAll()` (path A). Sync only `installType == "normal"` (Web Store) items; dev/unpacked/sideload are device-local by nature.
- Enabled/disabled: `management.setEnabled` from a UI click in the Synchrome popup (gesture requirement) — the popup shows "N extension changes pending — Apply". Attempt silently from the worker first; fall back to the gesture path on error.
- Uninstall propagation: Chromium always shows a confirmation dialog for cross-extension uninstall → surfaced as a pending action the user confirms in the popup. Never silently uninstall.
- **Installation on a new device**, in order of preference, chosen per browser/platform by the agent:
  1. *External extension descriptor* (path C, but the directory is per user-data-dir and may be written while running; Chromium reads it at startup): macOS and Windows for Chrome/Chromium/Brave/Edge, Linux for Chromium-branded builds. User sees one "enable" bubble per extension. Blocklist-on-uninstall is acceptable (it mirrors user intent).
  2. *Guided install*: Synchrome lists missing extensions with one-click links to the store page. This is the only route that works everywhere, including Helium (whose proxying of the update URL makes descriptor/policy installs unreliable) and Edge Add-ons-only items.
  3. *Policy `ExtensionSettings` (normal_installed)*: deferred; works from HKCU on Windows, needs root on Linux, probably ignored at user level on macOS, and brands the browser "managed".
- Identity of an extension across browsers: the Web Store id. Edge Add-ons ids differ; v1 treats an Edge-store id as a separate extension.

### 2.3 Extension local settings — ✅ syncable with constraints (agent, closed-profile writes)

- Scope: `Local Extension Settings/<id>` (`storage.local`) and `Sync Extension Settings/<id>` (`storage.sync`) for Web Store extensions. Out of scope: `Extension State`, `Local Storage`, `IndexedDB`, `Session Storage` of extension origins (Chromium-internal or binary formats; v1 excludes them explicitly, architecture leaves room for adapters).
- Read: agent parses LevelDB directly (own TypeScript codec: `.log` + `.ldb` tables, Snappy, CRC). Live reads are best-effort snapshots; a confirmed snapshot is taken at profile close.
- Write: only when closed. The agent regenerates the store directory as a fresh LevelDB (one `.log` WriteBatch, `MANIFEST`, `CURRENT`) and swaps it in atomically after backing up the old one. No library linking against Chromium's LevelDB is needed.
- Merge granularity: per key, three-way against the last synced snapshot, last-writer-wins on true conflicts, every conflict logged. Values are opaque JSON.
- Safety valves: per-extension toggle (default on for Web Store extensions), per-key exclusion patterns (seeded with common machine-specific names such as `clientId`, `installId`, `*token*`, `*cache*`), per-extension size cap (default 5 MB; larger stores are reported, not synced), and the user can mark an extension "device-local".
- Honest limitation: an extension can store device-bound or encrypted data under innocuous keys. Synchrome cannot know; it can only make exclusion easy, back up before writing, and keep the previous store for rollback.

### 2.4 History — ✅ syncable (read live, write closed) with a defined fidelity boundary

- Read via `chrome.history` (path A): lossless visits with timestamps; `onVisited` for incremental capture; `onVisitRemoved` for deletions.
- Write via agent into `History` SQLite while closed (path C): insert `urls`/`visits`/`visit_source = EXTENSION`, maintain `urls.visit_count/typed_count/last_visit_time`. Segments (`segments`, `segment_usage`) are **not** maintained in v1 → synced visits don't influence the New Tab "most visited" tiles; omnibox ranking (which uses `urls` counters) is preserved.
- Only visits younger than Chromium's 90-day expiry are applied into a profile; the Synchrome store may retain longer (configurable, default 365 days) for search/export.
- Deletions: user deletions propagate as tombstones (URL-level or time-range). Expiry-driven removals must not propagate; heuristic: an `onVisitRemoved` for visits older than 89 days is treated as expiry [U — verify whether the API fires for expiry at all].
- Explicitly rejected: `history.addUrl` as the default write path (loses time/title). It remains an opt-in "approximate import" for setups without the agent.
- Edge case: typed/transition qualifiers are preserved as-is; `from_visit` chains are not reconstructed cross-device (set to 0).

### 2.5 Chromium preferences — ✅ syncable for an allow-listed set; tracked prefs conditional

- Read: agent parses `Preferences` (live-safe) and diffs against the last synced snapshot.
- Write: when closed, temp + rename. Untracked allow-listed prefs: unconditional.
- Tracked prefs (`homepage*`, `session.restore_on_startup`, `session.startup_urls`, `browser.show_home_button`, default search provider, `pinned_tabs`): require a valid MAC and `super_mac`.
  - Linux: no enforcement → write freely.
  - Windows/macOS, non-branded builds (Helium, Chromium, Brave, Edge presumed): seed is empty, device id is derivable (`IOPlatformUUID` / Windows machine id or empty with `--disable-machine-id`) → forge MAC, drop `*_encrypted_hash` for touched paths so the HMAC-fallback healing path re-stamps them. **Experiment required before relying on it.**
  - Google Chrome on Windows/macOS: seed must be extracted from the installed `resources.pak`. Planned as a follow-up; in v1 these five settings are shown as "apply manually" with a deep link to `chrome://settings` for Chrome.
- Classification lives in the schemas package as three lists: `syncable` (allow), `device` (never), `tracked` (allow with MAC). Anything not listed is ignored by default and can be opted in by key.
- Extension-API-settable prefs (`chrome.privacy.*`, `fontSettings`) are *also* applied live by the extension when present in the allow-list, so those take effect without a restart.

### 2.6 Data excluded by design

Passwords, cookies, sessions/tabs, caches, `Local State` (beyond reading the profile list), `Extension State`, extension code, `Secure Preferences` as a file, device-specific prefs, Brave wallet/rewards material. Reasons and file list in `01-chromium-internals.md` §7. These require explicit, separately designed support if ever added (passwords in particular would need their own end-to-end encrypted vault model, which Chromium's OSCrypt format cannot provide across machines).

## 2.7 Scope — everything on by default, passwords never

The desktop app syncs every category above by default. Each is an **individually switchable scope** per profile (bookmarks, extensions, extension settings, history, preferences) for users who want less. There is no "requires the agent" distinction any more: the app *is* the agent. What the app will never sync, regardless of settings: passwords, cookies, sessions (§2.6). Users who rely on a password manager get it installed as an ordinary synced extension; the app can additionally sync the preference that turns off the browser's own password saving.

Two delivery modes exist per profile, chosen automatically:

| Mode | When | Capture | Apply |
|---|---|---|---|
| **Live** | the Synchrome extension is present and enabled in the profile (the app installs it) | API events in real time | bookmarks and extension enable/disable instantly; prefs / extension settings / history on next browser close |
| **File** | extension absent or disabled | live-safe file reads (`Bookmarks`, `Preferences`, LevelDB best-effort) every 15 min, authoritative at close | everything on next browser close |

Both modes share the same store format; devices in different modes converge.

## 3. Do we need both an extension and a native agent? — Yes, packaged as one app plus an invisible extension.

| Requirement | Extension only | Agent only | Both |
|---|---|---|---|
| Bookmarks live sync with events | ✅ | ❌ (closed-profile file edits only) | ✅ |
| History capture with timestamps | ✅ read | ✅ when closed | ✅ |
| History write with timestamps | ❌ | ✅ | ✅ |
| Preferences (beyond ~15 %) | ❌ | ✅ | ✅ |
| Extension settings | ❌ (no cross-extension access) | ✅ | ✅ |
| Install extensions | ❌ | partial (descriptors) | ✅ + guided UI |
| Profile discovery, mapping | ❌ | ✅ | ✅ |
| Backups / restore of profile files | ❌ | ✅ | ✅ |
| Background sync while browser closed | ❌ | ✅ | ✅ |
| UI inside the browser, status, conflicts | ✅ | ❌ | ✅ |
| Works on macOS for Chrome/Brave/Edge without Full Disk Access | n/a | ❌ | ✅ via native-host inheritance [U] or FDA |

The first complete version therefore ships **both**: the user installs the **desktop app** (account, sync engine, store adapters, discovery, profile creation/import, file-level sensors and actuators, backups) and the app itself places the **thin extension** into each synced profile via the browser's external-extension mechanism for live capture and instant applies. The extension holds no logic beyond translating API events and executing applies. The sync algorithm exists in exactly one place: the app. Details in `05-architecture.md`.

## 4. "Which files can be copied while Chromium is running?" — summary

| Operation | Safe? |
|---|---|
| Read `Preferences`, `Bookmarks`, `Local State`, `Secure Preferences` | ✅ atomic rename writes; read is consistent |
| Copy `History`/`Favicons`/`Web Data` | ⚠️ usually fine, torn copy possible; verify with `integrity_check`; Synchrome does not need it (API read) |
| Read LevelDB stores with own parser | ⚠️ `.ldb` files are immutable; current `.log` may be torn → stop at first bad CRC; treat as best-effort |
| Open LevelDB with a LevelDB library | ❌ `LOCK` held |
| Open SQLite normally | ❌ exclusive lock |
| Write any of the above | ❌ overwritten or corrupted; wait for close |
| Write `External Extensions/<id>.json`, `NativeMessagingHosts/*.json` | ✅ read at startup / on demand |

## 5. Platform caveats that shape v1

- **macOS TCC**: Chrome/Brave/Edge profile dirs are unreadable without Full Disk Access (observed). v1 asks for FDA during agent setup when such a browser is selected, and investigates native-host inheritance as a way to avoid it. Helium currently needs nothing.
- **Windows**: file replacement must retry on sharing violations even after the browser "closed" (handles linger ~seconds); the native host must be an `.exe`.
- **Linux**: no pref enforcement (simplest platform for prefs); Flatpak/Snap browsers unsupported in v1.
- **Liveness is per user-data-dir**: with two profiles of the same browser open, neither can receive closed-profile writes. Synchrome shows "pending until browser restarts" and offers "restart browser now" (agent quits the browser gracefully, applies, relaunches) as an explicit user action.

## 6. Decisions carried into the specification

1. App first: the desktop app is the brain and the only thing the user installs; it places a thin extension into synced profiles for live sync (§2.7, §3).
2. Live writes for bookmarks and extension enable/disable; closed-profile writes for preferences, extension settings and history, via a persistent pending-apply queue executed by the app, with backups.
3. History fidelity boundary: counters yes, segments no, 90-day application window.
4. Extension installation: descriptor where supported, guided everywhere, policies deferred.
5. Preferences by allow-list; tracked prefs via MAC forging on non-branded builds and Linux, manual on branded Chrome until the seed path is validated.
6. Extension settings as opaque per-key JSON with exclusions, size caps, per-extension opt-out.
7. Excluded data stays excluded; no "copy the profile folder" mode.
