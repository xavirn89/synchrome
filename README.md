# Synchrome

**Sync your Chromium profiles across devices.** One profile, every device — without Google Sync.

Synchrome is a desktop application that keeps bookmarks, installed extensions, extension settings, browsing history and Chromium preferences of your browser profiles synchronised across your computers, for Chromium-based browsers (Helium, Chromium, Chrome, Brave, Edge). Passwords are never synced; bring your own password manager.

## Status

**Planning phase — no code yet.** The technical architecture and feasibility study is complete and lives in `docs/`. Implementation follows the milestones in `docs/06-v1-plan.md`.

## How it works for the user

1. Install Synchrome. Sign in (account on the project's private Supabase).
2. The app lists your installed browsers and their profiles. Tick the ones to sync, or create new profiles. On a second computer, import your account's profiles into any browser.
3. Done. The app runs in the background, drops its thin extension into the synced profiles for live sync, and applies file-level changes whenever a browser is closed.

Data lives in Synchrome Cloud by default; users can instead keep a profile in their own GitHub repository or a local/network folder.

## Specification

| Document | Content |
|---|---|
| [00 — Original brief](docs/00-original-brief.md) | the project brief and the planning-phase requirements, verbatim |
| [01 — Chromium internals](docs/01-chromium-internals.md) | where each kind of profile data lives, how Chromium locks and writes it, what we verified by experiment, browser and OS differences |
| [02 — Feasibility](docs/02-feasibility.md) | what is realistically syncable, through which access path, and the closest safe alternative where fidelity is limited |
| [03 — Data model and sync](docs/03-data-model-and-sync.md) | identities, clocks, entities, store layout, the sync cycle, merge rules, conflicts, deletions, backups, encryption, preference classification |
| [04 — Account, stores and backends](docs/04-storage-backends.md) | account model, Synchrome Cloud on Supabase, optional GitHub/folder/Drive stores, adapter interface |
| [05 — Architecture](docs/05-architecture.md) | desktop app + background service + invisible extension, onboarding flows, protocols, platform integration, security, repository layout |
| [06 — v1 plan](docs/06-v1-plan.md) | definition of the first complete version, experiments to run first, milestones, risks, index of answers to the brief's questions |

## Key conclusions (short version)

- **App first.** One installer. The app owns the account, the sync engine, browser and profile discovery, profile creation, imports, backups and all file-level work. It installs its own thin extension into synced profiles; the extension holds no logic and the app works without it (changes then apply at browser close).
- **Live vs. closed.** Bookmarks and extension enable/disable apply instantly via APIs. Preferences, extension settings and history are written only while the profile is closed, from a pending queue, always after a backup — because Chromium overwrites or locks those files while running.
- **Verified constraints.** Chromium enforces preference MACs on Windows and macOS in every build tested (Helium, Chrome, Brave); externally inserted history rows and bookmark nodes survive; macOS protects Chrome/Brave/Edge profile folders (Full Disk Access needed; Helium exempt).
- **Account and stores.** Synchrome Cloud (private Supabase: Auth, Postgres, Storage, Realtime push) by default; GitHub repository and local folder as optional stores; end-to-end encryption optional (default on for third-party stores).
- **Never synced:** passwords, cookies, sessions.

## Implementation (Milestone 1)

The app is a Tauri 2 desktop application with a Rust backend and a Svelte frontend; the
Chromium extension is a companion for live events (not in this slice). Build:

```bash
# Rust core
cargo build && cargo test
# Frontend + app (needs the Tauri CLI: pnpm add -D @tauri-apps/cli already in apps/desktop)
cd apps/desktop && pnpm install && pnpm tauri dev
```

Workspace layout:

```text
crates/
  synchrome-types     shared domain types (BrowserKind, AccessState, DiscoveredBrowser/Profile, user_agent)
  synchrome-browser   read-only browser & profile discovery + macOS TCC access detection
  synchrome-storage   StorageAdapter trait (CAS via opaque version token) — interfaces only
  synchrome-supabase  Supabase Cloud adapter skeleton (default store) — no network yet
  synchrome-sync      sync engine vocabulary (HLC, Scope) — interfaces only
  synchrome-bridge    extension <-> app native-messaging frames — interfaces only
apps/desktop/
  src-tauri/          Tauri 2 Rust app (commands: list_browsers, app_version)
  src/                Svelte UI (browser/profile list with access badges)
cloud/migrations/     Supabase SQL (E9 OCC experiment, applied on the server)
tests/experiments/    Milestone 0 validation scripts
```

The first vertical slice detects installed Chromium browsers and their profiles read-only and
shows each one's access state (`accessible`, `requires_full_disk_access`, `browser_running`,
`unknown`). No profile is modified, no permission is requested, no secret ships in the client.
