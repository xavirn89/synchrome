# Synchrome

**Sync your Chromium profiles across devices.** One profile, every device — without Google Sync.

Synchrome is a desktop application that keeps your Chromium browser profiles synchronised across your computers. **Version 1.0 (macOS)** syncs bookmarks, installed extensions and their enabled state, extension settings and a set of browser preferences for Helium and Google Chrome (validated); browsing history, tracked preferences, other browsers and other platforms are on the roadmap. Passwords are never synced; bring your own password manager.

## Status

**Synchrome 1.0 (macOS)** — the first complete version: Helium and Google Chrome on macOS, with
bookmarks, installed extensions (+ enabled state), extension settings (`storage.local` /
`storage.sync`) and the allow-listed browser preferences kept in sync across devices through
Synchrome Cloud (private Supabase) — journals, snapshots + compaction, Realtime wake-up. The
rest of the original brief (history, tracked preferences, IndexedDB / Local Storage, Windows,
Linux, GitHub / folder stores, E2E encryption) is **post-v1**: see `docs/06` §"SYNCHROME V1 —
DONE" for the exact scope, exclusions and known limitations, and `docs/07` for the release
checklist.

## How it works for the user (1.0)

1. Install Synchrome from the DMG into Applications and sign in (account on the project's private Supabase).
2. The app lists your browsers and their profiles. Link a profile to a Synchrome profile (create one, or pick one you already sync from another computer).
3. Install the connector for that browser and load the companion extension once; from then on bookmarks and extension state sync live, and extension settings and preferences sync whenever the browser is closed.

Data lives in Synchrome Cloud (the project's private Supabase). Other stores (GitHub, local/network folder) are designed but not part of 1.0.

## Specification

| Document | Content |
|---|---|
| [00 — Original brief](docs/00-original-brief.md) | the project brief and the planning-phase requirements, verbatim |
| [01 — Chromium internals](docs/01-chromium-internals.md) | where each kind of profile data lives, how Chromium locks and writes it, what we verified by experiment, browser and OS differences |
| [02 — Feasibility](docs/02-feasibility.md) | what is realistically syncable, through which access path, and the closest safe alternative where fidelity is limited |
| [03 — Data model and sync](docs/03-data-model-and-sync.md) | identities, clocks, entities, store layout, the sync cycle, merge rules, conflicts, deletions, backups, encryption, preference classification |
| [04 — Account, stores and backends](docs/04-storage-backends.md) | account model, Synchrome Cloud on Supabase, optional GitHub/folder/Drive stores, adapter interface |
| [05 — Architecture](docs/05-architecture.md) | desktop app + background service + invisible extension, onboarding flows, protocols, platform integration, security, repository layout |
| [06 — v1 plan](docs/06-v1-plan.md) | **§0 SYNCHROME V1 — DONE** (what 1.0 ships, exclusions, requirements, limitations, post-v1), the original v1 definition, experiments, milestones, risks |
| [07 — Release checklist](docs/07-release-checklist.md) | macOS release gates (code, build, signing, notarization, install, permissions, browsers, server) |

## Key conclusions of the feasibility study (short version)

- **App first.** One installer. The app owns the account, the sync engine, browser and profile discovery, profile creation, imports, backups and all file-level work. It installs its own thin extension into synced profiles; the extension holds no logic and the app works without it (changes then apply at browser close).
- **Live vs. closed.** Bookmarks and extension enable/disable apply instantly via APIs. Preferences, extension settings and history are written only while the profile is closed, from a pending queue, always after a backup — because Chromium overwrites or locks those files while running.
- **Verified constraints.** Chromium enforces preference MACs on Windows and macOS in every build tested (Helium, Chrome, Brave); externally inserted history rows and bookmark nodes survive; macOS protects Chrome/Brave/Edge profile folders (Full Disk Access needed; Helium exempt).
- **Account and stores.** Synchrome Cloud (private Supabase: Auth, Postgres, Storage, Realtime push) by default; GitHub repository and local folder as optional stores; end-to-end encryption optional (default on for third-party stores).
- **Never synced:** passwords, cookies, sessions.

## Install and run (macOS)

1. Open the DMG and drag **Synchrome** to **Applications**, replacing an earlier copy if
   present. Open it from Applications: browser connector installation requires that stable location.
2. Sign in or create a Synchrome Cloud account. Confirm your email if account creation asks you to.
3. For **Google Chrome**, grant Synchrome **Full Disk Access** under *System Settings →
   Privacy & Security → Full Disk Access*, then reopen Synchrome or refresh the workspace.
   Helium does not need this permission. Chrome and Helium are the validated v1 browsers.
4. Choose a browser during onboarding, or open **Profiles → New profile** and use a browser
   profile as the initial source. *Start fresh* creates an unconnected Synchrome profile;
   *Connect browser* links a browser to an existing one. Each Synchrome profile supports one
   browser connection per Mac. Close the browser when the setup flow asks you to.
5. In **Finish setup**, choose **Install connector**, then restart the browser. Open
   `chrome://extensions`, enable *Developer mode*, choose *Load unpacked*, and select the folder
   revealed by **Show companion folder** (`Synchrome.app/Contents/Resources/extension`).
   Open the companion popup and choose **Check connection** in Synchrome. Wait for the first
   sync to complete; a stored connection alone does not mean browser data has been imported.
6. Bookmarks and extension state can then sync live. Extension settings and supported browser
   preferences are optional: enable them for the connection when wanted. They are applied only
   while the browser is closed. Passwords, cookies and sessions are never synchronized.

**Notarization:** the 1.0 DMG is signed with Developer ID but not notarized yet (pending notary
credentials). On another Mac, macOS blocks the first launch: open *System Settings → Privacy &
Security* and choose *Open Anyway* once (or wait for the notarized build).

**Updates are manual in 1.0**: download the new version and replace the app in Applications
(your account, links and data stay). Requirements: macOS 12+, a Synchrome Cloud account; the
app ships only the public cloud URL and anon key (RLS protects the data), never an admin secret.

Release build: `scripts/release-macos.sh` (embedded frontend, Developer ID signature, `.app` +
`.dmg`, bundle audit; notarizes when notary credentials are in the environment). Use
`scripts/release-macos.sh --local-only` to sign and package without uploading to Apple.

Current local delivery: **1.0.2**, with Lucide internal icons and restrained Morphicons
transitions. See [icon update and verification](docs/ui/ICONS-UPDATE.md) and
[identity update and verification](docs/ui/BRAND-UPDATE.md). After replacing the app, reload
already-loaded unpacked companions to refresh their artwork and popup.

## Development

The app is a Tauri 2 desktop application with a Rust backend and a Svelte frontend; the
companion extension is `apps/extension` (MV3, fixed id). Build and test:

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test
cd apps/desktop && pnpm install && pnpm build
cargo build -p synchrome-desktop --features tauri/custom-protocol   # app with the embedded frontend
```

Debug builds also carry a development CLI (`synchrome-desktop --dev-…`), a UI driver and test
hooks (all `cfg(debug_assertions)`, absent from release builds) used by the end-to-end harnesses
in `tests/experiments/` (disposable browser profiles only; credentials in a local env file that
is never committed).

Workspace layout:

```text
crates/
  synchrome-types     shared domain types (BrowserKind, AccessState, DiscoveredBrowser/Profile, user_agent)
  synchrome-browser   browser & profile discovery, macOS TCC access detection, Preferences / LevelDB writers, native host manifest
  synchrome-config    client configuration (public values; baked in at release build time)
  synchrome-auth      Supabase Auth (Keychain session)
  synchrome-cloud     Synchrome Cloud client (profiles, mappings, devices, Storage, store_events, snapshots, cursors, Realtime frames)
  synchrome-device    device identity + app data dir
  synchrome-sync      sync engine: HLC, ops, per-collection merge rules, snapshots, wake-up coalescing
  synchrome-state     local state (SQLite): capture, publish, pull, conflicts, compaction, bootstrap
  synchrome-bridge    extension <-> app native-messaging protocol + local token registry
apps/desktop/         Tauri 2 app (Rust commands, background sync loop, Realtime supervisor) + Svelte UI
apps/extension/       companion extension (bookmarks + extension events, applies remote changes)
cloud/migrations/     Supabase SQL (0001–0005, applied on the server; never run by the client)
tests/experiments/    Milestone 0 experiments and the end-to-end harnesses
docs/                 specification (00–06) and the release checklist (07)
```
