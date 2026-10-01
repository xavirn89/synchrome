# Synchrome — Original Project Brief

> Verbatim brief provided at project start (2026-10-01). Kept for reference.
> The technical specification derived from it lives in the other files under `docs/`.
> Planning-phase instructions added by the author are at the end of this file.

---

## Project Overview

Synchrome is a synchronization system for Chromium-based browser profiles.

Its goal is to allow a user to keep multiple Chromium profiles synchronized across multiple computers from one or more shared storage backends.

Synchrome should not depend on Google Chrome Sync.

It should work with Chromium-based browsers such as:

- Helium
- Chromium
- Brave
- Chrome
- Edge
- Other Chromium-compatible browsers where technically possible

The system should support multiple profiles, multiple devices and multiple independent synchronization stores.

## Main Goal

A user should be able to have:

- several Chromium profiles
- several computers
- one or more shared stores

and keep the equivalent profiles synchronized automatically.

```text
MacBook
├── Personal
├── Work
└── Development

Desktop
├── Personal
├── Work
└── Development

Laptop
├── Personal
├── Work
└── Development
```

Synchrome should make:

```text
Personal ↔ Personal ↔ Personal
Work ↔ Work ↔ Work
Development ↔ Development ↔ Development
```

Each Synchrome profile belongs to a synchronization store and has its own internal identifier.

## Core Concept

Synchrome separates three concepts.

**Device** — a physical computer running Synchrome. Example: `device_id: macbook-xavi`

**Profile** — a Chromium browser profile. Example: `profile_id: work`, `profile_name: Work`

**Store** — the shared synchronization backend. Examples: GitHub repository, Google Drive, local folder, network folder, WebDAV, S3-compatible storage, Synchrome server.

A store can contain multiple profiles:

```text
synchrome-store/
├── profiles/
│   ├── personal/
│   ├── work/
│   └── development/
└── metadata/
```

A user may also configure multiple stores:

```text
Personal Store
└── personal

Company Store
├── work
├── support
└── development
```

## Data to Synchronize

### Bookmarks
Complete bookmark structure: folders, bookmarks, order, names, URLs.

### Extensions
Installed extensions: extension ID, name if available, installation source if available, enabled/disabled state where technically possible. On a new device, attempt to install missing extensions automatically where browser APIs permit it; otherwise show the missing extensions and provide an easy installation workflow.

### Extension Settings
Synchronize extension settings when technically possible. Relevant profile locations may include `Local Extension Settings`, `Sync Extension Settings`, `Extension State`, `IndexedDB`, `Local Storage`. Must be handled carefully because extension storage formats vary. Architecture should allow per-extension synchronization adapters in the future.

### History
URL, title, visit timestamp, visit count, typed count. Should support deduplication. Probably optional because it may grow significantly.

### Preferences
Selected Chromium preferences: browser settings, startup behavior, appearance, download preferences, search preferences, language settings, profile preferences. Must not be copied blindly; some values are device-specific. Maintain: syncable preferences, device-specific preferences, ignored preferences.

### Not synced by default
Passwords, authentication cookies, active sessions, browser cache, temporary files, GPU cache, crash data, machine-specific paths, OS-specific configuration, browser lock files. Potentially dangerous or sensitive data requires explicit support.

## Architecture

```text
Synchrome
├── Core
├── Browser Extension
├── Local Agent
├── Storage Adapters
└── Optional UI
```

**Core** handles profile metadata, data normalization, synchronization logic, conflict resolution, change detection, serialization, encryption, store abstraction. Must not depend on any specific storage provider; uses a common interface:

```ts
interface StorageProvider {
  read(path: string): Promise<Uint8Array | null>
  write(path: string, data: Uint8Array): Promise<void>
  delete(path: string): Promise<void>
  list(path: string): Promise<string[]>
  exists(path: string): Promise<boolean>
}
```

**Storage adapters** (modular): Local Folder (dev, testing, NAS, Syncthing, Dropbox, iCloud, network storage), GitHub (batched commits: local changes → debounce → batch → sync → single commit), Google Drive, WebDAV (Nextcloud, ownCloud, NAS), S3 (AWS S3, Cloudflare R2, Backblaze B2, MinIO).

**Browser extension** provides setup, profile configuration, sync status, manual sync, sync history, conflicts, storage configuration, data category configuration. Uses Chromium APIs (`chrome.bookmarks`, `chrome.history`, `chrome.storage`, `chrome.management`).

**Local agent** — some Chromium data cannot safely or reliably be accessed from an extension. The agent accesses the actual profile directory: Preferences, extension files, extension local storage, SQLite databases, filesystem-level sync, profile discovery, backup, restore. Communication via Native Messaging.

```text
Chromium
   ├── Synchrome Extension
   │     Native Messaging
   ▼
Synchrome Local Agent → Synchrome Core → Storage Provider
```

## Profile Discovery
Detect installed Chromium browsers and profiles where possible; let the user map a local browser profile to a Synchrome profile.

## Profile Identity
Profiles have a stable UUID independent of local folder names. The same Synchrome profile can map to `Helium/Profile 2` on one machine and `Brave/Profile 1` on another.

## Device Identity
Each installation has its own device UUID and name, so Synchrome knows which device generated a change.

## Synchronization Strategy
Bidirectional. No machine is the master.

**Change tracking** — every synchronized entity carries metadata (`updatedAt`, `updatedBy`); hashes (SHA-256) to detect changes.

**Conflict resolution** — configurable per data type: bookmarks merge; history merge automatically; preferences latest update wins; extension settings manual conflict if incompatible.

**Deletions** — explicit, via tombstones (`deleted`, `deletedAt`, `deletedBy`), cleaned after a configurable retention period.

**Sync frequency** — event-driven when possible (change → mark dirty → debounce → sync). Suggested defaults: debounce 30 s, remote polling 5 min, configurable. Manual sync always available.

## Encryption
Assume the backend is untrusted. Client-side encryption for sensitive data (serialize → compress → encrypt → upload). Key never stored remotely. Candidates: AES-256-GCM, Argon2id. Design early even if not in the first MVP.

## Backups
Before destructive local changes, create backups under `.synchrome/backups/<timestamp>/`; user can restore.

## Store Structure

```text
synchrome/
├── store.json
├── profiles/<profile-uuid>/{profile.json, bookmarks.json, extensions.json, preferences.json, history/, extension-settings/}
├── devices/<device-uuid>.json
└── metadata/{schema.json, migrations/}
```

## Schema Versioning
Every store defines `{"synchrome": 1, "schemaVersion": 1}`; future versions support migrations.

## Multiple Stores
An installation supports multiple stores; profiles independently choose their store.

## Original MVP ladder (superseded by the planning-phase requirements below)
MVP 1: extension, Helium, multiple profiles, one GitHub repo, bookmarks, manual+auto bidirectional sync, basic conflicts.
MVP 2: installed extensions, missing-extension detection, multiple stores, local FS provider, Google Drive, better conflicts, backups.
MVP 3: local agent, profile discovery, Preferences, Local Extension Settings, FS-level backup/restore.
MVP 4: history, encryption, WebDAV, S3, advanced conflicts, selective sync, profile cloning.

## Desired User Experience
First device: Install → create/connect store → select Chromium profile → create Synchrome profile → choose what to sync → sync.
Other device: Install → connect existing store → choose existing Synchrome profile → map to local profile → sync.

## Design Principle
Not tightly coupled to Helium, Chrome, GitHub or Google Drive: `Browser Adapter → Synchrome Core → Storage Adapter`.

## Suggested Repository Structure

```text
synchrome/
├── apps/{extension, agent}
├── packages/{core, storage, browser, schemas, crypto}
├── docs/
├── tests/
├── package.json, README.md, LICENSE
```

Stack candidates: TypeScript, Node.js, WebExtension APIs, Native Messaging, SQLite where needed, Zod, Vitest.

## Development Principles
1. Data safety 2. Predictable synchronization 3. Recoverability 4. Modularity 5. Privacy 6. Cross-browser compatibility 7. Cross-platform compatibility.
Never perform destructive synchronization without detecting the previous state, keeping a backup, and being able to recover.

## Name
Synchrome. Tagline: "Sync your Chromium profiles across devices." / "One profile. Every device."

---

## Planning-phase requirements (added 2026-10-01)

Before implementing anything, produce a technical architecture and feasibility study. **Do not start coding yet.**

**The first usable version must already fulfil the complete core goal**: synchronize at least bookmarks, installed extensions, browser history, Chromium preferences and local extension settings across multiple computers, with multiple Chromium profiles, multiple profiles per store, bidirectional sync, profile mapping between computers, conflict detection and resolution, safe deletion propagation, backups and recovery, and a storage abstraction allowing new backends later. Only one storage backend is required in v1.

Investigate and define:

1. What Chromium data actually needs to be synchronized
2. Where Chromium stores each type of data
3. Which data can be accessed through WebExtension APIs
4. Which data requires direct filesystem access
5. Which data requires a native/local companion application
6. Which files or databases cannot safely be copied while Chromium is running
7. Which data is device-specific and should never be synchronized directly
8. How extension settings are stored and how portable they are between machines
9. How history should be merged without corrupting Chromium databases
10. How preferences should be filtered and normalized
11. How extension installation should work on a new machine
12. How profile identity should work independently of Chromium's local profile folder names
13. How synchronization conflicts should be represented and resolved
14. How deletions should propagate
15. How backups and rollback should work
16. Whether encryption should be mandatory or optional
17. What storage backend makes the most sense for the first implementation
18. What limitations exist on macOS, Windows and Linux
19. What differences exist between Helium, Chrome, Brave, Edge and Chromium
20. Whether we need both a browser extension and a native local agent

Process:

```text
Research Chromium profile internals
        ↓
Identify what is realistically syncable
        ↓
Define the data model
        ↓
Define the synchronization strategy
        ↓
Define conflict and recovery behaviour
        ↓
Choose the first storage backend
        ↓
Design the architecture
        ↓
Define the first complete version
        ↓
Only then start implementation
```

Challenge assumptions. If directly copying a Chromium file is unsafe, propose a safer mechanism. If an extension API cannot provide enough information, determine whether a native agent is required. If a type of data cannot realistically be synchronized reliably, explain exactly why and propose the closest safe alternative. The end result is a concrete technical specification that can be implemented step by step.
