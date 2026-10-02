# Experiment results

Machine: macOS 27.0.1 (26A434), Apple Silicon. Helium 0.18.2.1 / Chromium 154.0.8037.92, Google Chrome 154.0.8037.92, Brave. Date: 2026-10-01. All runs used throwaway `--user-data-dir` profiles; no real profile was touched.

## E12 — Profile creation and rename — PASS (Helium, Chrome)

Launching `<browser> --user-data-dir=<tmp> --profile-directory="Profile 7"` creates the profile directory and a `Local State → profile.info_cache["Profile 7"]` entry (default name "Your Helium" / "Your Chrome", `is_using_default_name: true`). Editing that entry's `name` and `is_using_default_name` while the browser is closed sticks across relaunch (`name = "Synchrome Test"`). A second launch with `--profile-directory="Profile 8"` adds a second profile. `Preferences` is created.

Conclusion: the app can create and name profiles from outside with the browser closed. No API needed.

## E3 — External-extension descriptor install — PARTIAL, with a design consequence

Descriptor `<user-data-dir>/External Extensions/<id>.json = {"external_update_url":"https://clients2.google.com/service/update2/crx"}`, extension id `cjpalhdlnbpafiamejdnhcphjbkeiagm` (uBlock Origin).

- **Google Chrome**: did **not** read the per-user `<user-data-dir>/External Extensions/` dir — no update request for our id was issued. Chrome still installed its *own* default external extensions (Google Docs Offline `ghbmn…`, Wallet `nmmhk…`) from its **fixed** location. Confirms the research [U]: Google-branded Chrome does not wire the per-user external dir under a custom user-data-dir; it uses `/Library/Application Support/Google/Chrome/External Extensions/` (and the app bundle), which is global to the browser install and TCC-protected on macOS.
- **Helium** (Chromium-branded): **did** read the per-user dir — verbose log shows the external update request `clients2.google.com/service/update2/crx?...id=cjpalhdl…installedby=external` (note `os=win&arch=x64`, Helium's platform spoof). The CRX did not finish installing in the short headless run (Helium routes Web Store traffic through its own services; raw `clients2.google.com` download behaviour under Helium still to confirm with a real window).

Design consequences (recorded in the spec):

1. **Descriptors are per-browser-install, not per-profile.** The descriptor directory is tied to the user-data-dir (Chromium-branded) or a fixed path (Chrome/Brave/Edge) and applies to **every profile** of that browser. "Install the Synchrome extension only in synced profiles" cannot be done with descriptors alone; the extension installs into all profiles and then only activates sync for mapped ones (or the app enables/disables per profile).
2. For **Google Chrome / Brave / Edge** the app must write the descriptor to the browser's **canonical external-extensions directory** (fixed path, shared across profiles), which on macOS needs Full Disk Access.
3. For **Helium / Chromium** the per-user path works but the actual download depends on the build's update endpoint (Helium proxy). The store-link fallback remains necessary.

Remaining E3 work: real-window install to completion on each browser; whether Helium installs via its proxy; the exact user-facing "enable" bubble; Windows HKCU path; `force_installed` policy as the silent alternative.

## Pending (require other environments or decisions)

- **E1** macOS TCC inheritance by a native host — needs a signed/launched browser host harness.
- **E2** tracked-pref MAC regeneration — deferred pending approach agreed with the author.
- **E9** Supabase store (ETag/CAS, RLS, Realtime) — needs the private Supabase credentials or a local stack.
- **E4–E8, E10–E11, E13** — as listed in `docs/06-v1-plan.md` §2.

## E9 — Supabase instance as a Synchrome store — PASS (with concrete CAS finding)

Target: `https://synchrome-supabase.garduc.com` (self-hosted Supabase behind Cloudflare Tunnel + Kong). Scripts: `e9_http.py` (13/13), `e9_realtime.py` (3/3). All resources named `e9-*` and torn down.

Verified:
- **Cloudflare error 1010**: any request with a default library User-Agent (`Python-urllib/*`, and by extension `reqwest/*`) is blocked with HTTP 403 `error code: 1010`. Sending **any custom `User-Agent`** fixes it. The Synchrome client and all tooling MUST set an explicit User-Agent.
- **Auth**: admin user creation (`/auth/v1/admin/users`, service role) and password login (`/auth/v1/token`) work; JWT issued.
- **PostgREST**: reachable and authorized with anon key + user JWT.
- **Storage CRUD**: create bucket, upload, download (byte-exact), list by prefix, update, delete — all work with the service role.
- **Object metadata**: the `/storage/v1/object/info/<bucket>/<key>` endpoint returns `etag` (MD5 hex, quoted) and a per-write `version` UUID and `last_modified`. ETag is **not** present in GET/upload response headers, only via `info`.
- **Realtime**: two WebSocket clients connect, join a topic, and a Broadcast from one reaches the other in ~**110 ms**. Push works → use it instead of polling for Synchrome Cloud.

Key findings that shape the design:
1. **`storage.objects` has RLS enabled with no policies** → an authenticated user cannot write (`new row violates row-level security policy`); only the service role can. Production needs **prefix-scoped RLS policies** on `storage.objects` so a user may write only under `"<their accountId>/%"`. The service role must never ship in the client; user writes go with the user JWT under those policies (or, if we prefer, through a PostgREST RPC / edge function).
2. **The standard Storage REST endpoint ignores `If-Match` / `If-None-Match`** (returns 200, overwrites) → **no native CAS**. Two options exist: (a) the **S3-compatible endpoint** (`/storage/v1/s3` is present, not 404) which supports conditional writes but needs separate S3 access keys; (b) **emulate CAS in Postgres**: a metadata row per mutable shared object with a monotonic `version`, updated via PostgREST `PATCH ...?version=eq.<n>` (atomic; affects 0 rows if stale). **Decision: Postgres-version-row CAS.** Per-device journals are owner-written and need no CAS; only `store.json`, `profiles/*/profile.json` and snapshots do, and those get a `synchrome.objects` row `(path, version, hlc, device)` guarding each write. The object bytes live in Storage; the guard row makes the write a compare-and-swap.
3. The `version` UUID from `info` changes on every write and can serve as the opaque `StorageAdapter` version token for change detection (cheap `list`/`info` diffing), independent of the CAS guard.

Remaining E9 work (needs DDL / Postgres or a migration path): create the production RLS policies and the `synchrome.objects` CAS table and test user-JWT writes + live OCC end to end. Blocked only on a way to run SQL migrations (Supabase CLI `db push` with the DB URL, or psql). Flag to the author.

### E9 OCC — migration validated end to end — PASS (11/11)

After Codex applied `cloud/migrations/0001_e9_occ_experiment.sql` on the Synchrome server, `e9_occ.py` ran against the real instance (URL + anon + two service-role-created test users; no DB access). Store `bac9f2e2…`.

| # | Check | Result |
|---|---|---|
| 1 | User A creates OCC row, `expected_version=0` | PASS — `ok, new_version=1, current_version=0` |
| 2 | User A writes to Storage with its **user JWT** (not service role) under its own prefix | PASS — 200 |
| 3 | User B cannot read A's object | PASS — 400 (RLS) |
| 3b | User B cannot write under A's prefix | PASS — 400 (RLS) |
| 3c | User B's CAS row isolated from A | PASS — B's own row created at v1 independently |
| 4–5 | Two writers both `expected=1` → exactly one wins | PASS — winner `new_version=2`, loser `ok=false` |
| 6 | Loser gets conflict + current version | PASS — `current_version=2` |
| 7 | Retry with `expected=2` | PASS — `ok, new_version=3` |
| 8 | Stale `expected=1` rejected | PASS — `ok=false, current_version=3` |
| 9 | RLS on both `public.e9_objects` and `storage.objects` | PASS (rows 3, 3b, 3c) |
| 10 | Cleanup removes test users + objects | PASS — 0 test users remain, bucket empty |

CAS race versions observed: create → **1**; two concurrent writers from v1 → winner **2** (loser sees current **2**); retry from v2 → **3**; stale write from v1 → rejected, current **3**. Monotonic, atomic, no lost update.

Conclusion: the Postgres version-row CAS (`e9_cas_put`) plus prefix-scoped RLS on `storage.objects` fully provide the compare-and-swap and per-account isolation that Supabase Storage does not offer natively. **E9 is closed.** The `e9-experiments` bucket and `e9_*` objects remain in place as the validated reference for the production schema; remove with `0001_e9_occ_experiment.rollback.sql` when no longer needed.

## E4 — Extension storage in LevelDB — PASS (Helium full round-trip; Chrome format-identical)

Probe extension (`tests/experiments/e4-ext/`) writes known values to each storage area; `e4_leveldb.py` reads them with a pure-Python LevelDB reader, then copies the stores into a second disposable profile (both browsers closed) and verifies the write-once probe adopts and reads the restored values. No real profile touched.

**Browser support for the test harness**: Helium (ungoogled) honours `--load-extension` → full round-trip. **Google Chrome stable 154 silently ignores `--load-extension`** (CLI extension loading was removed around Chrome 137), so the probe cannot be side-loaded there; the on-disk format is identical (same Chromium 154 base, confirmed against the real Chrome profile), and Synchrome installs its extension via the Web Store, not `--load-extension`, so this does not affect the product.

**What lives where, and portability** (none of these areas is OSCrypt-encrypted; values are raw):

| Concept | On-disk location | Format | Portable? |
|---|---|---|---|
| **Extension installed** (registration, enabled, grants) | `Secure Preferences → extensions.settings.<id>` (tracked) + `Extensions/<id>/<version>/` payload | JSON + files | as "(re)install + enabled flag": yes. The payload is re-downloadable; its `path`/`version` are device-bound → never copy the payload. |
| **Extension settings** (`chrome.storage.sync`) | `Sync Extension Settings/<id>/` | LevelDB, keys plain UTF-8, values JSON | **yes** — stored locally even without Google sync; verified copy+restore |
| **Extension local data** (`chrome.storage.local`) | `Local Extension Settings/<id>/` | LevelDB, keys plain UTF-8, values JSON | **yes** — verified copy+restore, read back by the extension |
| **`chrome.storage.session`** | in-memory only | — | not on disk; nothing to sync |
| **IndexedDB** (extension origin) | `IndexedDB/chrome-extension_<id>_0.indexeddb.leveldb` | LevelDB, binary keys, `idb_cmp1` comparator, V8-serialized values | dir-level copy works between same Chromium version (verified), but not parseable with the settings codec and may hold origin/device-bound data → **out of scope for v1**, handle later per-extension |
| **Local Storage** (`window.localStorage`) | `Local Storage/leveldb` (one store **shared by all origins**) | LevelDB, binary type-prefixed keys | not per-extension isolable at file level → **out of scope for v1** |

**Round-trip result** (Helium): probe id `ibknknhdfhdegdaefmboiaphhilheiop`, stable across profiles (same load path = same id; in production the Web Store id is fixed). src stamp `E4-…444`; a fresh second profile wrote its own stamp `…426`; after copying `Local Extension Settings` + `Sync Extension Settings` + `IndexedDB` into it (closed) and relaunching, the write-once probe kept the restored data and read back `…444` for both local and sync. PASS.

**Operational rules confirmed** (consistent with `01` §5): LevelDB stores can be read with our own codec (keys plain, values JSON) and copied/restored wholesale **only while the browser is closed** (the live browser holds a `flock` on `LOCK` for opened stores). Restore = replace the whole `<id>/` directory after backup; never edit individual `.log`/`.ldb` files. A fresh store is a single `.log` WriteBatch + `MANIFEST` + `CURRENT`.

## E5 — Chromium history — PASS (Helium and Chrome)

`e5_history.py` on disposable profiles. No real profile touched.

**Format (both browsers, identical):**
- SQLite ("SQLite format 3" magic). `meta.version = 70`.
- Journal mode **delete** (rollback journal, `History-journal`); **no WAL** by default (`-wal` absent, `-journal` present). WAL exists only behind a disabled feature flag.
- Relevant tables: `urls`, `visits`, `visit_source`, `segments`, `segment_usage`, `keyword_search_terms`, `downloads`, `meta` (full schema in `01` §4).

**Closed-profile read / write:**
- Reading the closed DB: trivial (plain SQLite).
- Inserting a visit into the closed profile (`urls` + `visits` + `visit_source=2`, counters maintained) → accepted, `integrity_check = ok`, **survives relaunch** with the row intact on both browsers.

**Lock / copy while running (nuance):**
- On these idle headless profiles, a read-only open **succeeded** and a copy taken while running had `integrity_check = ok`.
- But on the real, actively-used Helium profile (session 1) the same read failed with **"database is locked"**. So the exclusive lock is situational: held during active use, sometimes not on an idle instance. **Rule stands: treat live reads as best-effort (may be locked); never write while running.**

**Cross-profile import:**
- Copied `History` (+ `-journal`) into a second disposable profile while both browsers were closed (after backing up the destination `History`). After relaunch, the imported row was present and `integrity_check = ok`. **PASS on Helium and Chrome.** Wholesale replace is valid for importing into a new/empty profile; incremental sync into a populated profile must insert rows individually (id remapping, counters, segments) rather than overwrite — as designed in `03` §3/§6.

**Risks documented:**
- **Torn copy**: copying `History` while the browser is actively writing can capture an inconsistent snapshot (committed pages may still be in the rollback journal). Mitigation: copy only when closed, or use the SQLite backup API / `VACUUM INTO` for a live snapshot, and always run `integrity_check`.
- **Locks**: `locking_mode=EXCLUSIVE` during active use blocks external opens; writes are impossible/overwritten while running.
- **WAL**: not used by default; if ever enabled, a copy must include `-wal`/`-shm` (or checkpoint first) or lose recent data. The import copies journal siblings.
- **Schema gate**: operate only within a supported `meta.version` range (70 today); refuse unknown versions.
- **90-day expiry** (session-1 research): visits older than ~90 days are purged by Chromium, so injecting older rows is pointless.
- **AUTOINCREMENT id collisions**: wholesale replace is safe for a fresh profile; merging into an existing DB requires allocating new ids and fixing `visits.url`/`from_visit` references.

## E1 — macOS TCC / native-host permissions — PARTIAL (negative case proven; positive case deferred)

Minimal Rust harness `tests/experiments/e1-harness/` (std only, non-destructive: reads/stats paths, write-tests only disposable dirs; never writes a real profile or the real External Extensions dir). Run two ways with **no Full Disk Access granted**: (1) the binary from the shell, (2) an ad-hoc-signed `.app` (bundle id `com.synchrome.e1probe`) launched via LaunchServices (`open`) — the TCC identity a Tauri app has.

**Both execution modes gave identical access** (TCC decides by *who* is responsible, not by binary vs app):

| Operation | Helium | Chrome | Brave | Edge | Permission needed |
|---|---|---|---|---|---|
| Read `Local State` | READ ok | EPERM | EPERM | EPERM | Chrome/Brave/Edge: **Full Disk Access** |
| List profile dir | ok | EPERM | EPERM | EPERM | same |
| Read `Preferences` | READ ok | EPERM | — | — | same |
| Read `History` | READ ok | EPERM | EPERM | — | same |
| List `Local Extension Settings` | ok | EPERM | — | — | same |
| List `NativeMessagingHosts` | ok | **ok** | (n/t) | (n/t) | **none** — subpath not TCC-protected |
| Stat `External Extensions` (canonical) | n/a | STAT ok (exists) | — | — | none (metadata only) |
| List `External Extensions` (canonical) | n/a | **EPERM** | — | — | **Full Disk Access** |
| Write to a disposable profile dir | WRITE ok | — | — | — | none |

Canary: `~/Library/Application Support/com.apple.TCC/TCC.db` not accessible (confirms no FDA).

**Findings**
1. **Helium needs no special permission.** Its data dir (`~/Library/Application Support/net.imput.helium`) is not a TCC-guarded location; full read/write of `Local State`, `Preferences`, `History` and LevelDB stores works from both a plain binary and an unsigned `.app`.
2. **Chrome, Brave and Edge profile data require Full Disk Access.** Every profile-data read is EPERM without it. This is the single macOS gate for those browsers.
3. **`NativeMessagingHosts` is NOT under TCC protection** — it is readable inside Chrome's dir even while the rest is EPERM. So installing Synchrome's native-messaging host manifest into Chrome/Brave/Edge does **not** need FDA. (Write not exercised to avoid altering the real install; the directory is outside the TCC boundary, so a write is expected to succeed — to be confirmed non-destructively with a temp file on request.)
4. **The canonical `External Extensions` dir IS under TCC protection** (stat succeeds — the path exists — but listing is EPERM). Installing an extension into Chrome/Brave/Edge via their canonical `External Extensions` descriptor therefore **requires FDA on macOS**. This is the E1 × E3 intersection: on Helium the per-user `External Extensions` dir lives under its own (unprotected) Application Support and needs nothing; on Chrome/Brave/Edge it needs FDA.
5. **Binary vs .app:** identical capability. The difference is attribution: a shell binary runs under the responsible parent app's TCC identity (grant FDA to Terminal → applies to all it spawns); an `.app` launched by LaunchServices is its own responsible process and the grant attaches to its code-signing identity.

**TCC grant target and durability (mechanism; live positive test deferred — see below)**
- FDA for a notarized Tauri app is granted to the app's **code-signing designated requirement** (Team ID + bundle id), so it **survives app updates** signed by the same Developer ID. Users add the app once in System Settings → Privacy & Security → Full Disk Access.
- An **ad-hoc / unsigned** build is keyed by cdhash + path, so the grant **breaks on every rebuild** (new cdhash) — a dev-time nuisance only; production (signed + notarized) is stable.
- FDA cannot be requested programmatically; macOS shows no prompt for it. The app must **detect** the EPERM and **guide** the user to System Settings (Tauri can open `x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles`). "Files and Folders" per-folder prompts do not cover `~/Library/Application Support/<browser>` — only FDA does.
- Separately, **Automation / Apple Events** is a distinct TCC bucket: Synchrome's "restart browser to apply" (quit + relaunch Chrome) sends Apple Events and triggers a one-time Automation prompt per target app. Not required for file sync, only for the restart convenience.

**Degraded mode if the user declines FDA (macOS):** Helium fully works. Chrome/Brave/Edge: bookmarks, history capture, extension list/state and live applies still work **through the extension** (the browser reads its own files); what breaks is everything that needs the app to touch those profiles' files directly — full Preferences, extension settings, history import with dates, and `External Extensions` auto-install. Those become "needs Full Disk Access" in the UI.

### DEFERRED (to run before closing development): live positive FDA test
Per the author's decision, the positive path will be verified at the end of development with a **manual FDA grant**: add the signed `Synchrome.app` (or the test `E1Probe.app`) in System Settings → Privacy & Security → Full Disk Access, then re-run the harness to confirm (a) Chrome/Brave/Edge profile reads succeed, (b) `External Extensions` becomes writable, and (c) the grant survives a notarized update (same identity) vs. breaks on an ad-hoc rebuild. Tracked here and in `docs/06` as the remaining E1 item.

## E6 — history.onVisitRemoved on expiry — PASS (hypothesis disproven; important design consequence)

**Hypothesis:** the 90-day expiry deletes rows silently, so `onVisitRemoved` would NOT fire for it.
**Result: FALSE — the event DOES fire for age-expired visits.** Disposable profiles.

Procedure: insert 2 recent visits + 3 visits dated ~120 days ago into the closed `History`, relaunch with the probe extension (Helium) listening to `history.onVisitRemoved`, wait 40 s, read the probe's results from `storage.local` off disk; Chrome validated DB-level only (no `--load-extension`).

Observed (Helium):
- `onVisitRemoved` fired reporting exactly the three ~120-day-old URLs (`old0/old1/old2`), **not** the recent ones. Same insertion method for both groups → the trigger is **age/expiry**, not injected-row cleanup.
- Event shape: `{ allHistory: false, urls: [...] }` — **identical in shape to a user deletion**; nothing distinguishes expiry from an intentional delete in the payload.
- DB caveat: the `urls` rows were still present at SIGTERM (the expirer's on-disk delete is asynchronous and did not complete in the 40 s window), so expiry is a **background process, not an instant startup purge**. Chrome: same, old rows not yet purged in-window.

**Portable / bound:**
- History rows themselves are portable (see E5); this experiment is about *events*, which are per-run and local — not portable.
- Expiry threshold (90 days) is a Chromium build constant (bound to **browser/version**), not per-profile.

**Design implications for Synchrome (firm):**
1. **Do not propagate `onVisitRemoved` blindly as a user deletion** — it also fires for expiry, which would wrongly delete those URLs on every other device. Confirms the `03` §8 / `02` §2.4 heuristic empirically.
2. Treat a removal as a propagatable user deletion only when it is **not age-driven**: ignore removals whose visits are older than ~89 days, and/or reconcile against the synced store (only delete remotely if the URL was recently present and the removal is recent).
3. Because expiry is asynchronous and version-constant, Synchrome should not sync visits older than the expiry window at all (they will vanish anyway), which also sidesteps most spurious `onVisitRemoved` events.

**Verdict: PASS** (question conclusively answered; the answer changes the deletion design, already reflected in the data model).

## E7 — management.setEnabled from the service worker without a gesture — PARTIAL

**Hypothesis:** SW-initiated `setEnabled` on another extension needs a user gesture / shows a confirmation, so Synchrome must route it through a popup click.
**Result on Helium (disposable profile): it worked unattended — both directions.**

Procedure: load the probe + a dummy extension (both enabled) via `--load-extension`; the probe's service worker calls `chrome.management.setEnabled(dummyId, false)` then `(…, true)` with no user gesture; results read off-disk from `storage.local`. Chrome 154 could not run this (ignores `--load-extension`).

Observed (Helium): dummy `aplnkbnomlpnifbafnbgcdfpoelnndlk`, `mayDisable: true`. Disable → no error, `enabled` became `false`. Re-enable → no error, `enabled` became `true`. No dialog (headless) and no gesture.

**Caveats (why PARTIAL):**
- The dummy is a **development** extension (`installType: "development"`); the gesture/confirmation requirement documented by Chrome may apply differently to **Web Store ("normal")** extensions — the production case — which we could not load via CLI on Chrome 154.
- Ungoogled/Helium may relax behaviour vs branded Chrome; `setEnabled` UI differences across builds are untested.
- `mayDisable:false` (policy/managed extensions) would reject disable regardless.

**Portable / bound:** enabled/disabled state is a per-profile pref (`extensions.settings.<id>` in Secure Preferences, tracked) — portable as a boolean (see E4). The *ability* to toggle it via API is bound to **browser/build** and the target's `installType`/`mayDisable`.

**Design implication (firm, robust to the uncertainty):** Synchrome should **attempt `setEnabled` from the background first** (it may succeed with no UI, as here) and **fall back to a popup "Apply" click** (user gesture) when it errors. Never assume it is silent on every build; never try to toggle `mayDisable:false` extensions. This is exactly the attempt-then-gesture path already in `02` §2.2 / `05` §1.2.

**Verdict: PARTIAL** — mechanism confirmed working unattended on Helium for a dev extension; the Web-Store/branded-Chrome case remains to confirm (will be covered once the real Web Store extension exists). Design is safe either way.

## E8 — profile identification by pre-seeding mappingToken — PASS

**Hypothesis:** the app can write a `mappingToken` into `Local Extension Settings/<ext-id>/` before the browser launches, and the extension reads it via `chrome.storage.local`, learning which Synchrome profile it maps to.
**Result: confirmed on Helium.**

Procedure (disposable profile): deterministic ext id `bahaodcchlkcffhkjkemenhdomfnjekm` derived from the manifest `key` (RSA pubkey → SHA256 → a–p mapping); created `Default/Local Extension Settings/<id>/` and seeded `mappingToken = {token, profileId, deviceId}` with **classic-level** (an external LevelDB writer standing in for the Rust app); our own reader confirmed the seed; launched the probe (same id); the probe's SW called `chrome.storage.local.get('mappingToken')` and recorded the value. Readback **equalled the seeded object exactly**. PASS.

**What this proves:**
1. An external process can create a LevelDB store that Chromium's `chrome.storage.local` reads — not just copy an existing one (E4) but author a fresh one. (Validated with classic-level, whose leveldb is format-compatible with Chromium's; production uses the Rust codec in `crates/browser`.)
2. A deterministic extension id (manifest `key`) lets the app know the store path in advance — so it can seed per profile before launch.
3. The seed→read handshake gives the app a reliable, per-profile identification channel without the extension needing filesystem access.

**Portable / bound:**
- `mappingToken` is a **local handshake**, intentionally not synced: bound to **profile** (store is per-profile) + **extension id** (installation-level, fixed by the Web Store key) + **browser** (LevelDB format). Not cross-device.
- The deterministic id is bound to the **installation** (manifest key), identical across all installs of the Synchrome extension.

**Corruption / timing risks:** the seed must be written **while the profile is closed** (LevelDB `LOCK` is held when the browser has the store open); writing to a live profile is ignored/overwritten. So the handshake is: app seeds token while the profile is closed → next launch, the extension reads it. If the profile is already open, fall back to the user confirming the mapping in the app (as designed in `05` §3.1).

**Design implication:** `05` §3.1's profile-identification handshake is viable as specified. The app seeds a per-profile `mappingToken` into the Synchrome extension's `storage.local` before the browser opens that profile; the extension echoes it in `hello`; the daemon matches it to a mapping. User confirmation remains the fallback for already-open profiles or externally-installed extensions.

**Verdict: PASS** (Helium; browser-independent mechanism, Chrome CLI load unavailable but the format/semantics are identical).

> Note: `e8_seed.js` references `classic-level` installed under `/tmp/e8seed/` for the experiment; it is not a repo dependency. Production writes the store with the Rust LevelDB codec.

## E13 — Browser preferences allowlist (M1 slice 12, 2026-10-02)

Scripts: `e13_preferences.py`, `e13b_prefs_detail.py`, `e13d_writer_on_helium.py`. Disposable `--user-data-dir` profiles, `--headless=new`, Helium 0.18.2.1 (Chromium 154.0.8037.92) and Google Chrome 154.0.8037.93; values set through `chrome.settingsPrivate` on `chrome://settings` (the settings page's own API) and/or edited in `Preferences` with the browser closed, then read back after a relaunch.

| path | type | stored | external edit kept | verdict |
|---|---|---|---|---|
| `browser.show_home_button`, `homepage`, `homepage_is_newtabpage` | bool / url / bool | `Secure Preferences` + MAC (both) | **no** (reset) | excluded: tracked prefs |
| `download.prompt_for_download` | bool, default false (both) | `Preferences` | yes | **allowlist** |
| `browser.enable_spellchecking` | bool, default true (both) | `Preferences` | yes | **allowlist** |
| `intl.selected_languages` | string, default from the OS | `Preferences` | yes; `accept_languages` recomputed from it | **allowlist** |
| `intl.accept_languages` | string | `Preferences` | **no** (recomputed from `selected_languages`) | excluded: derived |
| `spellcheck.dictionaries` | list | `Preferences` | **no** (recomputed at startup) | excluded |
| `spellcheck.use_spelling_service` | bool | `Preferences` | yes | excluded: sends text to Google |
| `translate.enabled` | bool, default **false in Helium, true in Chrome** | `Preferences` | yes | excluded: diverging defaults, Google service |
| `bookmark_bar.show_on_all_tabs` | bool, default false (both) | `Preferences` | Chrome yes; **Helium: overridden by `bookmark_bar.visibility_state`** | **allowlist** with the Helium representation |
| `download.default_directory` | string (local path) | — | — | excluded: local path |

**Helium's bookmark bar.** Helium 154 also keeps `bookmark_bar.visibility_state` (0 = shown on all tabs, 1 and 2 = not); when present it wins: an external `show_on_all_tabs: false` next to `visibility_state: 0` was reverted to true, removing `visibility_state` makes `show_on_all_tabs` authoritative, and Helium's own UI writes "hide" as `{show_on_all_tabs: false}` without `visibility_state` and "show" as `{show_on_all_tabs: true, visibility_state: 0}`. Chrome ignores `visibility_state`.

**Other facts.** `<user-data-dir>/Last Version` holds the full browser version in both (`154.0.8037.92`). `Preferences` is one line, top-level keys sorted, `<` not escaped; a relaunch without changes rewrites it (`NewTabPage`, `in_product_help`, `profile`, `sessions`, …), so byte preservation is only meaningful for Synchrome's own write, not across a browser run. No `prefs.preference_reset_time` marker after any external edit of an untracked path.

**E13d — the product writer on a real Helium.** `--dev-write-prefs` (the slice's `apply_preferences`) wrote the four allowlisted values with Helium closed: the rest of the document (everything but the allowlist) digest-identical, `Secure Preferences` untouched, a backup outside the profile; after relaunch Helium showed all four (and recomputed `accept_languages`); writing `false` for the bookmark bar after Helium had re-added `visibility_state: 0` was effective; removing `download.prompt_for_download` gave the default. **Verdict: PASS.**

