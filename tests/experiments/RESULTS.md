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
