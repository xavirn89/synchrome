# 05 — Architecture

Answers brief question 20 in detail and fixes the components, runtime model, protocols, platform integration and security posture for the first complete version.

Guiding decisions (2026-10-01, superseding earlier drafts):

1. **App first.** The user installs one desktop application. It owns the account, the sync engine, the store adapters, browser/profile discovery, profile creation and import, backups, and all file-level work. There is nothing else to install by hand.
2. **Cloud account by default.** Users sign in to a Synchrome account hosted on a private Supabase instance; their synced data lives there unless they choose an alternative store (own GitHub repository, own Drive, local/network folder). No tokens to paste for the default path.
3. **Invisible extension.** The app places the Synchrome browser extension into every synced profile through the browser's external-extension mechanism. The extension is a thin live adapter (bookmarks, history, extension state events, instant applies); the app never depends on it being present — without it, changes apply when the browser is closed and reopened.
4. **Passwords are never synced.** The app can install the user's password-manager extension like any other extension, and never reads `Login Data` or `Cookies`.

## 1. Components

```text
┌─────────────────────────────── Synchrome desktop app (one install per computer) ───────────────────────────────┐
│  UI (window + tray)                                                                                             │
│   ├─ sign in / sign up, account & store settings                                                               │
│   ├─ browsers → local profiles ↔ Synchrome profiles (import existing, create new, map, unmap)                   │
│   ├─ per-profile scope toggles, status, pending-restart badge, conflicts, backups/restore, missing extensions   │
│  Background service ("synchromed", starts at login, keeps running when the window is closed)                   │
│   ├─ core:      materialised state, HLC, merge, conflicts, compaction, bootstrap pairing      (packages/core)   │
│   ├─ storage:   SupabaseStore (default), GitHub, LocalFolder, [Drive later], EncryptedStore    (packages/storage)│
│   ├─ crypto:    Argon2id, HKDF, AES-GCM (optional end-to-end mode)                              (packages/crypto) │
│   ├─ browser:   discovery, liveness, profile creation, file sensors/actuators, SafeWriter      (packages/browser)│
│   ├─ extension manager: writes External Extensions descriptors / registry keys for Synchrome + synced extensions │
│   ├─ scheduler: debounce, realtime subscription or polling, pending-apply queue (executed when profiles close)   │
│   └─ native-messaging host (`--host` mode of the same binary) ⇄ extension instances                             │
└───────────────────────────────┬───────────────────────────────────────────────┬────────────────────────────────┘
                                │ native messaging (per synced profile)         │ HTTPS (Supabase REST/Storage/Realtime, GitHub…)
┌───────────────────────────────▼──────────────────────┐          ┌─────────────▼──────────────────────────────┐
│ Chromium-based browser profile                       │          │ Store                                       │
│  └─ Synchrome extension (thin, auto-installed)       │          │  default: Synchrome Cloud (private Supabase) │
│      bookmarks / history / management events,        │          │  optional: GitHub repo, Drive, local folder  │
│      live applies, tiny status popup                 │          └─────────────────────────────────────────────┘
└──────────────────────────────────────────────────────┘
```

### 1.1 Desktop app (apps/desktop)

- **Runtime: Tauri 2 with a Rust backend** (decided 2026-10-01). The UI is web tech (TypeScript, Svelte) inside Tauri's webview; everything else — sync engine, store adapters, Supabase client, browser/profile discovery, file sensors and actuators, SafeWriter, pending-apply queue, native-messaging host — is a Rust workspace shared by the app and the background service. Rationale: the extension is thin so there is little TypeScript to share; the app's job (SQLite via `rusqlite`, LevelDB via `rusty-leveldb` or our own codec, `flock`/Win32 liveness checks, OS keychain via `keyring`, long-running tray service) suits Rust; a single static binary of ~15 MB doubles as app, service and host with no runtime dependency. Cost accepted: slower feature development than TypeScript, Supabase consumed through REST + WebSocket (`reqwest`, `tokio-tungstenite`, PostgREST conventions) rather than an official SDK.
- One binary, three roles: `synchrome` (window + tray), `synchrome service` (background service, launched at login, the single writer of all local state), `synchrome host` (native-messaging shim spawned by browsers; forwards frames to the service over a user-private socket).
- Local state in `<appData>/Synchrome/`: session refresh token in the OS keychain (never on disk), `state.sqlite` (materialised state per profile, own op log + `seq`, cursors, pending-apply queue, file snapshot hashes), `backups/`, `logs/`.
- Packaging: Tauri bundler → signed + notarized `.dmg`/`.pkg` (macOS), signed `.msi`/NSIS + winget (Windows), `.deb`/`.AppImage` (Linux); Tauri updater plugin against our signed release feed.
- Schemas are defined once in Rust (`serde` + `schemars`) and generated to TypeScript (JSON Schema → `zod`/types) for the UI and the extension, so the two languages cannot drift.

### 1.2 Browser extension (apps/extension) — thin, managed by the app

- MV3, published on the Chrome Web Store (and Edge Add-ons if needed); fixed id via manifest `key`.
- Permissions: `bookmarks`, `history`, `management`, `storage`, `nativeMessaging`, `alarms`; optional `privacy`, `fontSettings`, `contentSettings`.
- Role: emit normalised observations (bookmark events + periodic tree hash, visits, extension state) to the app over the native port; execute live applies the app sends (bookmark CRUD, `setEnabled`, API-settable prefs); show a minimal popup (synced / pending / open the app). **No sync logic, no credentials, no store I/O.**
- Installed by the app through `External Extensions/<id>.json` (macOS, Chromium-branded Linux) or `HKCU\…\Extensions\<id>` (Windows). The browser downloads it from the Web Store at next start and shows a one-time "enable" bubble on Windows/macOS. Advanced option: `ExtensionSettings` policy with `force_installed` for a fully silent install, at the cost of the "managed by your organization" badge.
- If the extension is absent or disabled in a profile, the app still syncs that profile in file mode: capture from `Bookmarks`/`Preferences`/LevelDB (live-safe reads) and `History` (authoritative at close), apply everything at close.

### 1.3 Core (crates/core) — runs in the background service

Pure Rust, no I/O: entity/field model, HLC, journal encoding, merge rules, bookmark tree invariants, conflict detection, compaction, bootstrap pairing (import of existing profiles), diff/plan generation for both live applies (via extension) and file applies (via adapters). Testable with a simulator (N virtual devices, randomised schedules, property-based convergence assertions with `proptest`).

### 1.4 Browser adapters (crates/browser)

- **Discovery**: known-browser table (bundle/exe names, user-data-dir per OS, native-messaging manifest location, external-extensions location, policy domain, branding → tracked-pref seed availability). Enumerates profiles via `Local State → profile.info_cache`, including display name, avatar and last-active flags.
- **Profile creation**: `browser --profile-directory="Profile N" --no-first-run about:blank`, wait for the directory and `Local State` entry, quit, then set `profile.info_cache["Profile N"].name` while the browser is closed (experiment E12).
- **Liveness**: `SingletonLock` + pid (POSIX), `Chrome_MessageWindow` + `lockfile` (Windows); `waitForClose(userDataDir)`.
- **File sensors/actuators**: `PreferencesFile` (read/diff/write, MAC forging where supported — `hmac`/`sha2` crates), `LevelDbStore` (`rusty-leveldb` or own codec; regenerate on write), `HistoryDb` (`rusqlite` bundled SQLite, read copies, write when closed, schema gate), `BookmarksFile` (read for guids and for file-mode capture; write in file mode), `ExternalExtensions`, `NativeMessagingManifests`.
- **SafeWriter**: backup → temp → verify → atomic replace → post-verify → rollback.

### 1.5 Storage (crates/storage), crypto (crates/crypto)

Per `04-storage-backends.md`: `StorageAdapter` trait; `SupabaseStore` default (PostgREST + Storage REST + Realtime WebSocket); `GitHub`, `LocalFolder`; `EncryptedStore` wrapper for the optional end-to-end mode (`argon2`, `hkdf`, `aes-gcm` crates from RustCrypto).

## 2. Onboarding flows (what the user actually does)

**First computer**

```text
Install Synchrome → open → Sign up / Sign in (email + password or magic link; OAuth later)
→ "We found: Helium (Profile 2 'Xavi', Profile 4 'Trabajo', Profile 5 'Dev'), Chrome (Default), Brave (Default, Profile 1)"
→ pick a browser → tick the profiles to sync, optionally rename them for Synchrome (default: the browser's name)
   [+ New profile] creates one in that browser
→ scope: everything on by default (bookmarks, extensions, extension settings, history, preferences); passwords: never
→ Sync → initial import runs; the app drops the extension into those profiles; tray icon turns green
```

**Another computer (account already has profiles)**

```text
Install → Sign in → "Your account has: Personal, Trabajo, Dev"
→ "Import to this computer": pick a browser (e.g. Brave) → for each account profile choose
   "map to existing local profile" or "create new profile here" (default) → Sync
→ repeat for another browser if desired (Trabajo → Helium here, Brave there — any mix)
```

**Any time later**: Settings → Browsers → toggle which local profiles map to which account profiles; a Synchrome profile is one identity across every computer ("unique and horizontal"). Unmapping never deletes local data; deleting a Synchrome profile from the account asks for confirmation and leaves local browsers untouched.

## 3. Runtime behaviour

- **Login start**: `synchromed` starts, refreshes the session from the keychain, subscribes to store change notifications (Supabase Realtime) or starts polling (other stores), checks the pending-apply queue for profiles that are closed.
- **Browser open, extension present**: live loop — events → ops → debounce (60 s) → push; incoming ops → live applies immediately; file-level changes accumulate in the pending plan; tray/popup show "N changes apply when you close the browser".
- **Browser open, extension absent**: file-mode capture every 15 min from live-safe files; same pending plan for applies.
- **Browser closes**: liveness clears → grace period → authoritative snapshots → execute plan with backups → verify → push resulting ops.
- **Browser closed, remote changes arrive**: the service is running, so it merges immediately and applies to the closed profile right away (no lag — an advantage of app-first over extension-first).
- **"Restart to apply"**: explicit button; the app quits the browser gracefully, applies, relaunches the same profile.
- **Offline**: ops accumulate locally; retries with backoff.
- **Multiple stores**: a profile belongs to exactly one store; the service runs one loop per store with its own credentials.

## 4. Native messaging protocol (extension ⇄ host ⇄ service)

JSON frames with `type`, `id`, `profileRef`; host→extension payloads chunked at ~900 KB.

| Direction | Message | Purpose |
|---|---|---|
| ext → app | `hello { extVersion, browser, mappingToken? }` | identify the profile (§4.1) |
| ext → app | `observe { col, entities[] }`, `event { col, kind, payload }` | full scans + incremental events |
| app → ext | `apply { ops[] }` → `applied { results[] }` | live applies, returns new local ids |
| app → ext | `status { state, lastSync, pendingRestart, conflicts }` | popup |
| ext → app | `command { syncNow, openApp }` | user actions |
| both | `ping/pong` | keep-alive, skew |

### 4.1 Which profile is this extension instance?

The app knows the profile because **it installed the extension there**: when it writes the descriptor it also writes a per-profile `mappingToken` into that profile's `Local Extension Settings/<synchrome-ext-id>` store before the browser starts (the store is created by the app as a fresh LevelDB; the extension reads it via `chrome.storage.local` on first run and echoes it in `hello`). For profiles where the extension was installed by other means, fall back to the user confirming in the app. Experiment E8.

## 5. Platform integration

| Concern | macOS | Windows | Linux |
|---|---|---|---|
| Background service at login | `~/Library/LaunchAgents/net.synchrome.service.plist` | `HKCU\…\CurrentVersion\Run` | `~/.config/systemd/user/synchromed.service` + autostart |
| Native host manifest | per browser dir (Helium and Edge also read Chrome's) | `HKCU\SOFTWARE\<Vendor>\<Product>\NativeMessagingHosts\…`; host `.exe` | `~/.config/<browser>/NativeMessagingHosts/…` |
| Extension auto-install | per-user `External Extensions/<id>.json` | `HKCU\SOFTWARE\<Vendor>\<Product>\Extensions\<id>` | Chromium-branded builds only [U]; otherwise the app opens the store page |
| Profile data access | **Full Disk Access** for Chrome/Brave/Edge (TCC, observed); Helium exempt. Onboarding step with deep link; E1 explores inheritance via the native host | retry on sharing violations | — |
| Liveness | `SingletonLock` | `Chrome_MessageWindow` + `lockfile` | `SingletonLock` |
| Distribution | notarized `.dmg`/`.pkg`, Homebrew cask | signed installer, winget | `.deb`, `.AppImage`; Flatpak/Snap browsers unsupported |

## 6. Security model

- **Account**: Supabase Auth; refresh token in the OS keychain (Keychain / Credential Manager / libsecret). Row-Level Security isolates each user's data; the service role key never ships in the app.
- **Data at rest on the server**: TLS in transit, Postgres/Storage encryption at rest, RLS. **End-to-end encryption is optional** ("Advanced → Encrypt my data with a passphrase"): Argon2id-derived key, AES-256-GCM per object, key only on the user's devices. Default off for the cloud store because it forces a passphrase and makes password resets destructive; default on for third-party stores (GitHub/Drive) where the provider is not us. Documented trade-off.
- **Local**: all profile writes go through SafeWriter with backups; writes restricted to known profile paths; no symlink traversal; the native host only accepts the allow-listed extension id; IPC socket is user-private.
- **Never**: passwords, cookies, sessions, OSCrypt-encrypted columns. The app can set `credentials_enable_service=false` (optional, synced pref) so the browser stops offering to save passwords when the user relies on a password manager.
- Logs redact URLs and titles.

## 7. Repository layout and stack

```text
synchrome/
├── apps/
│   ├── desktop/          Tauri 2 app: src-tauri (Rust: window, tray, commands, service + host entry points), src (Svelte UI)
│   └── extension/        MV3 thin extension (TypeScript)
├── crates/               Rust workspace
│   ├── core/             sync engine + simulator
│   ├── schemas/          serde types, schemars → JSON Schema, pref lists, browser table, IPC + API types
│   ├── browser/          discovery, liveness, profile creation, file adapters, SafeWriter
│   ├── storage/          StorageAdapter trait, SupabaseStore, GitHub, LocalFolder, EncryptedStore
│   ├── crypto/           Argon2id, HKDF, AES-GCM, key check
│   └── service/          background service: scheduler, pending-apply queue, IPC server, native host
├── packages/
│   └── schemas-ts/       generated TypeScript types + zod from crates/schemas (UI + extension)
├── cloud/                Supabase schema (SQL migrations), RLS policies, edge functions (if any)
├── docs/
├── tests/{fixtures,experiments,e2e}
├── Cargo.toml            workspace
└── package.json          pnpm workspaces (UI, extension, generated schemas)
```

Stack: Rust stable (tokio, serde, rusqlite, rusty-leveldb, reqwest, tokio-tungstenite, keyring, RustCrypto, proptest); Tauri 2 (plugins: updater, autostart, single-instance, shell); UI Svelte + TypeScript via Vite; extension TypeScript + Vite; Playwright (`--load-extension`) for extension e2e; Supabase CLI for local dev (Postgres + Storage + Realtime in Docker); CI matrix macOS/Windows/Linux.

## 8. Open architectural questions (tracked in 06)

1. ~~Electron vs Tauri~~ — decided: Tauri 2 + Rust backend.
2. macOS TCC inheritance via the native host (E1).
3. Tracked-pref MAC forging (E2).
4. External-extension descriptor behaviour per browser, especially Helium (E3).
5. Profile creation via `--profile-directory` + rename (E12).
6. End-to-end encryption default for the cloud store (off, per §6) — revisit after user research.
