# 06 — The First Complete Version: Scope, Experiments, Milestones, Risks

## 1. Definition of v1 ("Synchrome 1.0")

v1 is complete when a user installs **one desktop application**, signs in to a Synchrome account, picks which local browser profiles to sync (or creates new ones), and from then on **bookmarks, installed extensions, extension settings, browsing history and Chromium preferences** of those profiles stay synchronised across all their computers — on **macOS, Windows and Linux**, for **Helium, Chromium, Google Chrome, Brave and Microsoft Edge** (Flatpak/Snap browsers excluded) — with the behaviour below. Passwords, cookies and sessions are never synced.

| Capability | v1 behaviour |
|---|---|
| Installation | one signed installer per OS; the app installs its background service, native-messaging hooks and the browser extension into synced profiles by itself |
| Account | sign up / sign in (email + password or magic link) against the project's private Supabase; session in the OS keychain |
| Stores | Synchrome Cloud (default) + optional GitHub repository + optional local/network folder; profile belongs to one store; move by export/import |
| Browser & profile discovery | installed browsers and their profiles listed with names; import existing profiles; create new profiles in a chosen browser |
| Profile mapping | a Synchrome profile is one identity across computers; per computer and per browser the user maps or creates local profiles; change any time; unmapping never deletes |
| Bidirectional sync, no master | per-device journals, HLC, per-field LWW, snapshots (`03`) |
| Bookmarks | live when the extension is active, at browser close otherwise; full tree incl. order; `dateAdded` not preserved on recreation |
| Extensions | list + enabled state; missing ones auto-installed via external descriptor where the browser supports it (one "enable" bubble on Win/Mac), store link otherwise; uninstall requires confirmation; password managers are just extensions |
| Extension settings | `storage.local` + `storage.sync` of Web Store extensions; per-key merge; exclusions; 5 MB cap; per-extension opt-out; applied at close |
| History | lossless capture (extension or file); written with real timestamps into `History` at close; 90-day apply window; URL/range deletions; unified history view in the app |
| Preferences | allow-listed untracked prefs everywhere; tracked prefs on Linux and non-branded builds via MAC forging (if E2 passes); "apply manually" for Google Chrome on Win/Mac |
| Scopes | all on by default; per-profile toggles for each category |
| Delivery modes | live (extension present) / file (extension absent); automatic |
| Conflicts | deterministic convergence + conflict records + override in the app; per-collection strategies |
| Deletions | tombstones, retention, bootstrap confirmation, local trash export |
| Backups & recovery | file-level backups before every closed write, bookmark exports, store snapshots, restore from the app |
| Encryption | optional E2E; off by default for Synchrome Cloud, on by default for GitHub |
| Sync cadence | debounce 60 s; push via Realtime on Synchrome Cloud, 5 min poll elsewhere; manual "Sync now"; configurable |
| Not in v1 | passwords, cookies, sessions, `Local Storage`/IndexedDB of extensions, Drive/WebDAV/S3 stores, Flatpak/Snap, policy-based silent installs as default (available as advanced option), bookmark `dateAdded` fidelity, account-bookmark (signed-in) trees, Edge Collections, sharing profiles between accounts |

## 2. Experiments (Milestone 0)

Scripted against headless throwaway profiles (`tests/experiments/`). Pass/fail criteria explicit so a failure changes the plan instead of surprising us.

| # | Question | Method | If it fails |
|---|---|---|---|
| E1 | **macOS TCC** — MOSTLY DONE (`RESULTS.md`): without FDA, Helium fully accessible; Chrome/Brave/Edge profile data EPERM (need FDA); `NativeMessagingHosts` accessible without FDA; canonical `External Extensions` needs FDA; binary and .app identical (grant attaches to code-signing identity, survives notarized updates). DEFERRED: live positive test with a manual FDA grant before closing development | Rust harness (e1-harness) | — |
| E2 | **Tracked-pref MAC forging** on non-branded builds (empty seed, `IOPlatformUUID`/machine id, drop `*_encrypted_hash`): does `homepage` survive relaunch in Helium, Chromium, Brave on macOS and Windows? | extend `tracked_prefs.py` | tracked prefs "apply manually" on Win/Mac |
| E3 | **External extension descriptor** installs a Web Store extension on Chrome, Chromium, Brave, Edge (macOS), Windows via HKCU, Linux Chromium-branded; what does Helium do through its proxy? What exactly does the user see? | throwaway profiles | store-link fallback on failing combinations; consider `force_installed` policy as advanced option |
| E4 | **LevelDB codec round-trip** — DONE: read/copy/restore of storage.local + storage.sync + IndexedDB verified on Helium (write-once probe reads restored values); Chrome 154 ignores --load-extension (format identical); storage.session in-memory, Local Storage shared/out-of-scope | probe extension | — |
| E5 | **History** — DONE: SQLite v70, rollback journal (no WAL); closed-profile insert accepted + survives relaunch (Helium, Chrome); cross-profile import via closed copy passes integrity; live lock is situational (locked on active real profile, open on idle headless); risks (torn copy, WAL, id collisions, 90-day expiry) documented | disposable profiles | — |
| E6 | **onVisitRemoved on expiry** — DONE (PASS): it DOES fire for ~120-day-old visits (shape identical to a user delete), so deletions must not be propagated blindly — ignore age-expired removals / reconcile against the store; expiry is async, version-constant | probe extension (Helium) | — |
| E7 | **setEnabled without gesture** — PARTIAL: on Helium the SW disabled+re-enabled a dev extension with no gesture/dialog; Web-Store ("normal") type on branded Chrome untested (CLI load disabled). Design: attempt from background, fall back to popup gesture; never toggle mayDisable:false | probe + dummy (Helium) | — |
| E8 | **Profile identification** — DONE (PASS): externally-seeded mappingToken (classic-level, det. ext id from manifest key) read by the extension via chrome.storage.local on Helium; proves an external process can author a store Chromium reads; must seed while profile closed; user-confirm fallback for open/externally-installed | probe + classic-level (Helium) | — |
| E9 | **Supabase store** — DONE (`tests/experiments/RESULTS.md`): Auth/PostgREST/Storage/Realtime all work; Cloudflare needs a custom User-Agent; `storage.objects` RLS is service-role-only until prefix policies exist; standard Storage endpoint has no `If-Match` CAS → Postgres `synchrome_objects` version-row CAS; Realtime broadcast ~110 ms. DONE: RLS + CAS migration applied on the server and validated end to end (e9_occ.py 11/11) | real instance | — |
| E10 | Windows handle release timing after browser exit | Windows VM | longer grace / retry |
| E11 | Windows liveness detection for Helium (`chrome.exe` signed by imput) and Edge | Windows VM | `lockfile` + process list |
| E12 | **Profile creation**: `--profile-directory="Profile N"` creates a usable profile; renaming via `Local State` while closed sticks; profile picker shows it | headless launch + edit | create via the browser UI with guidance |
| E13 | **GitHub adapter limits** (optional store): requests per cycle, 304s, 422 on stale ref, blob upload | scratch repo | adjust chunking/debounce |

## 2.1 Milestone 0 status — CLOSED (2026-10-01)

Sufficient to begin implementation. Done: E12, E3, E9 (full), E4, E5, E1 (negative case), E6, E7, E8. Non-blocking pending items, carried forward:

- **E1 positive (Full Disk Access)**: mandatory gate before the macOS release — verify with a manual FDA grant that Chrome/Brave/Edge become accessible and the grant survives a notarized update.
- **E2 protected preferences** (homepage/search MAC): pending an approach decision; **not blocking** the base sync (only the few tracked prefs; everything else syncs).
- **E10 / E11** (Windows handle release, liveness): pending a Windows environment.
- **E13 GitHub adapter limits**: pending; GitHub is an **optional** store, Supabase Cloud is the default.

Implementation starts at Milestone 1 (scaffold + read-only detection vertical slice).

## 3. Milestones

| Milestone | Deliverable | Exit criterion |
|---|---|---|
| **M0 Experiments** | E1–E13, results appended to `01` §9 | every row has pass/fail; plan adjusted |
| **M1 Scaffold + detection slice** — IN PROGRESS | Cargo+Tauri2 workspace; crates types/browser/storage/supabase/sync/bridge; Svelte UI; read-only browser+profile discovery with TCC access state | DONE slice 1: workspace builds, Tauri launches, real detection of Helium/Chrome/Brave/Edge + per-profile access. DONE slice 2: client config (env/.env, publishable values only), common HTTP client (`Synchrome/<ver>` UA, timeout, typed errors), read-only `cloud_status` probe (Auth/PostgREST/Storage + latency), onboarding UI, admin-secret guard script; 20 unit tests, clippy clean. **Validated against the real instance (2026-10-01): configured/reachable/auth_ok/rest_ok/storage_ok all true, HTTP 200 ×3, `Synchrome/0.1.0` UA accepted by Cloudflare, latencies auth 277 ms / rest 262 ms / storage 130 ms**. DONE slice 3 (code): user auth — `synchrome-auth` crate (Session with redacted `Secret` tokens, never serialised to the webview; `CredentialStore` trait + macOS `KeychainStore` via keyring/apple-native/Security.framework, verified; GoTrue password login, explicit refresh with reuse-while-valid / clear-on-definitive-failure, logout that always clears locally); Tauri `account_status`/`sign_in`/`refresh_session`/`sign_out`; login UI; 18 auth unit tests (38 total); bundle/source/guard audits clean. **Validated against the real instance (2026-10-01) with a test user: login OK (exact user_id/email, expires_at present, no token fields in AccountStatus); Keychain item `com.synchrome.app`/`session` created; session survives process restart and is reused without network; refresh rotates both tokens (sha256 fingerprints change, expires_at advances); logout removes the Keychain item and status returns signed_in=false; logout with the remote unreachable still clears the local session; password never stored (only email inside the Keychain secret), nothing persisted on disk.** DONE slice 4 (code, migration NOT yet applied): device identity (`synchrome-device`: UUID v4 generated once, persisted in `~/Library/Application Support/Synchrome/device.json`, not hardware-derived, not a secret; name/platform/os/app version recomputed per start); production migration `cloud/migrations/0002_devices_profiles.sql` (+rollback, README) for `public.devices` / `public.profiles` with RLS enabled+forced on `account_id = auth.uid()`; `synchrome-cloud` authenticated PostgREST client (upsert device w/ merge-duplicates, touch last_seen_at, list devices/profiles; token read in one place, typed errors incl. SchemaMissing); Tauri `device_identity`/`register_device`/`list_devices`/`list_profiles`; UI Devices + Profiles with empty state. 50 unit tests. **0002 applied by Codex; validated against the real instance (2026-10-01) with two test users, normal login + anon key + own JWTs only, no service role: A registers device `f85ee85a-8546-421b-8d04-861c66588486`; re-register does not duplicate, advances last_seen_at and updates metadata in place; touch advances last_seen_at; A lists exactly one device (is_current) and [] profiles; RLS A/B isolation holds for devices (B cannot see, PATCH (0 rows), touch, upsert-hijack (403) or DELETE (0 rows) A's device; A cannot see B's) and profiles (each sees only its own; B GET/PATCH/DELETE on A's profile affect 0 rows); safe models drop the server's account_id and carry no tokens; local device_id stable across process restarts; all test rows deleted with own JWTs (accounts back to 0 devices / 0 profiles), Auth users kept.** DONE slice 5 (code, migration NOT yet applied): cloud profile CRUD + local↔cloud mapping — migration `cloud/migrations/0003_profile_mappings.sql` (+rollback, README) with composite FKs `(account_id, …)` (FK checks bypass RLS), ON DELETE RESTRICT for profiles / CASCADE for devices, uniqueness per local profile and per token (a cloud profile may map to several local profiles on the same device and on other devices), column-level grants, anon revoke; `synchrome-cloud` gains create/rename/delete profile and map/unmap/list/lookup/confirm mapping, 256-bit `MappingToken` (redacted, never serialised; UI gets a 4-char hint), `Mapping` (not Serialize) vs `MappingSummary`; typed errors Conflict / UnknownReference / ProfileInUse / InvalidValue / NotFound; Tauri commands guarded by local-profile detection, create-and-map with rollback; UI mapping per local profile + cloud profile create/rename/delete-when-safe. Token stays `pending_local_write` (no LevelDB write yet). 68 unit tests. **Waiting for Codex to apply 0003; real validation afterwards.** |
| **M1b Core engine** | `synchrome-sync` real HLC/merge + `synchrome-storage` adapters + crypto; simulator | property-based convergence tests pass; no data loss without a tombstone; cycles/orphans handled |
| **M2 Cloud** | `cloud/`: Supabase schema, RLS, Realtime events, quotas; `SupabaseStore` adapter; auth flows | two simulated devices sync through a local Supabase stack; RLS tests prove isolation |
| **M3 Browser adapters** | `crates/browser`: discovery, liveness, profile creation, Preferences (+MAC forging per E2), LevelDB, History DB, SafeWriter, backups/restore, external descriptors, native-messaging manifests | round-trip real fixtures from Helium, Chrome, Brave on macOS and Linux; Windows in CI VM |
| **M4 Desktop app** | Tauri 2 app + `crates/service`: onboarding (sign in, discover, import/create/map profiles, scopes), tray + background service, pending-apply queue, restart-to-apply, conflicts, backups UI, auto-update | end-to-end on one machine: two throwaway profiles synced via local Supabase, extension auto-installed, file applies at close |
| **M5 Extension** | thin MV3 extension: events, live applies, popup; Web Store listing | live bookmark edit on profile A appears on profile B within one cycle |
| **M6 Cross-machine validation** | two real machines (macOS + Linux), mixed live/file modes, GitHub store as alternative | converge; compaction; restore drill |
| **M7 Hardening & release** | signed/notarized installers, winget/Homebrew, docs, diagnostics (`Help → Doctor`) | one week of daily use on the author's three machines with zero data-loss incidents |

## 4. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Chromium changes a file format (History schema, LevelDB layout, pref tracking) | closed-profile writes could corrupt | version gates per adapter, refuse unknown versions, backups always, verify-after-write, fixture corpus per release |
| Tracked-pref forging stops working | homepage/startup/search not syncable on Win/Mac | E2 first; manual fallback designed; Linux unaffected |
| macOS TCC tightens further | app cannot read Chrome/Brave/Edge profiles | FDA onboarding step; E1; Helium exempt today |
| External-extension install UX differs per browser / Helium proxy | extension not present → file mode only | file mode is fully functional; store-link fallback; `force_installed` advanced option |
| Running our own cloud | availability, cost, security duty, GDPR | Supabase managed stack we already operate; quotas; optional E2E; alternative stores for users who prefer their own; export at any time |
| Extension stores contain device-bound data | an extension misbehaves after sync | exclusion patterns, opt-out, backup + rollback, size cap |
| Clock skew | wrong LWW winner | HLC, skew warnings, conflict records |
| **Tech debt:** `devices.device_id` is a global PK, so a second account on the same installation collides (its device upsert is rejected by RLS) | multi-account per installation impossible | resolve before supporting it, likely by migrating `devices` to a composite PK `(account_id, device_id)`; 0003's composite FKs already reference that pair |
| Rust backend slows feature work; immature Supabase crates | longer M2–M4 | thin REST/WebSocket client over `reqwest`; schemas generated once; UI and extension stay in TypeScript |
| Code signing / notarization | Gatekeeper, SmartScreen | Apple Developer ID; Windows signing + reputation ramp; Homebrew/winget |

## 5. Answers to the brief's 20 questions — index

| # | Question | Where answered |
|---|---|---|
| 1 | What data needs to be synchronised | `02` §2, `03` §3 |
| 2 | Where Chromium stores each type | `01` §1–§6 |
| 3 | Accessible via WebExtension APIs | `01` §2, §4, §6; `02` §1 |
| 4 | Requires direct filesystem access | `02` §1, §2 |
| 5 | Requires a native companion | `02` §3; `05` §1 |
| 6 | Cannot be safely copied while running | `01` §1, `02` §4 |
| 7 | Device-specific, never synced | `01` §3, §7; `03` §11 |
| 8 | Extension settings storage and portability | `01` §5; `02` §2.3 |
| 9 | History merge without corruption | `01` §4; `02` §2.4; `03` §3, §6 |
| 10 | Preference filtering and normalisation | `02` §2.5; `03` §11 |
| 11 | Extension installation on a new machine | `01` §6; `02` §2.2; `05` §1.2 |
| 12 | Profile identity independent of folder names | `03` §1; `05` §2 |
| 13 | Conflict representation and resolution | `03` §6–§7 |
| 14 | Deletion propagation | `03` §8 |
| 15 | Backups and rollback | `03` §9 |
| 16 | Encryption mandatory or optional | `03` §10 (optional; off for Synchrome Cloud, on for third-party stores) |
| 17 | First storage backend | `04` §3, §6 (Synchrome Cloud on Supabase; GitHub + folder optional) |
| 18 | OS limitations | `01` §8.7; `05` §5 |
| 19 | Browser differences | `01` §8.1–§8.6 |
| 20 | Extension and native agent both needed | `02` §3; `05` §1 |
