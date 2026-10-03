# Synchrome cloud migrations

SQL migrations for the Synchrome Supabase instance. **Experimental** files are prefixed
with the experiment id (`e9_`) and are fully reversible. Claude Code authors these;
they are executed on the server by Codex against the Synchrome PostgreSQL; no database
password or connection string is kept in this repo or in the local environment.

## 0001 — E9 optimistic-concurrency experiment

- Migration: [`0001_e9_occ_experiment.sql`](0001_e9_occ_experiment.sql)
- Rollback:  [`0001_e9_occ_experiment.rollback.sql`](0001_e9_occ_experiment.rollback.sql)

### What it creates / modifies

| Object | Kind | Notes |
|---|---|---|
| `public.e9_objects` | table | CAS guard: `(account_id, store_id, path)` PK, `version bigint`, `hlc`, `device_id`, timestamps. RLS enabled + forced. |
| `e9_objects_select/insert/update/delete` | RLS policies on `public.e9_objects` | each restricts rows to `account_id = auth.uid()` |
| grants | privileges | `usage` on schema `public` to `anon, authenticated`; CRUD on `e9_objects` to `authenticated` |
| `public.e9_cas_put(uuid, text, bigint, text, uuid)` | function (SECURITY INVOKER) | atomic compare-and-swap upsert; returns `(ok, new_version, current_version)`; `execute` granted to `authenticated` |
| `storage.buckets` row `e9-experiments` | data (insert, on conflict do nothing) | private bucket |
| `e9_storage_select/insert/update/delete` | RLS policies on `storage.objects` | restrict to `bucket_id = 'e9-experiments'` AND first path segment `= auth.uid()::text` |

It does **not** touch any production table, and it adds nothing outside the `e9_*`
names, the `e9-experiments` bucket, and standard `anon/authenticated` grants. A safety
`DO` block aborts if `auth.uid()` is absent (i.e. not a Supabase database).

### How Codex should apply it (on the Synchrome server)

Run the migration, then confirm the objects exist. Using the local superuser socket or
the Supabase Postgres container — no password needs to leave the server:

```bash
# from the Synchrome server, against the Synchrome database (adjust db name if needed):
psql "$SYNCHROME_DATABASE_URL" -v ON_ERROR_STOP=1 -f 0001_e9_occ_experiment.sql
# or, if using the Supabase CLI on the server:
# supabase db execute --file 0001_e9_occ_experiment.sql
```

If applied inside the `supabase-db` container:

```bash
docker exec -i supabase-db psql -U postgres -d postgres -v ON_ERROR_STOP=1 \
  < 0001_e9_occ_experiment.sql
```

Quick verification query (optional):

```sql
select to_regclass('public.e9_objects') is not null                       as table_ok,
       to_regprocedure('public.e9_cas_put(uuid,text,bigint,text,uuid)') is not null as fn_ok,
       exists (select 1 from storage.buckets where id = 'e9-experiments')  as bucket_ok,
       (select count(*) from pg_policies
         where tablename = 'objects' and policyname like 'e9_storage_%')   as storage_policies;
```

To undo everything:

```bash
psql "$SYNCHROME_DATABASE_URL" -v ON_ERROR_STOP=1 -f 0001_e9_occ_experiment.rollback.sql
```

### Validation Claude Code runs afterwards (closes E9)

`tests/experiments/e9_occ.py` (URL + anon key + two real test users created via the
service role; no DB access) asserts:

1. authenticated user A **creates** a CAS row via `rpc/e9_cas_put(store, path, expected=0)` → `ok=true, new_version=1`
2. user A **uploads** an object to Storage under their own prefix `<uid>/…` with the **user JWT** → succeeds (was blocked before the policy)
3. user B **cannot read** A's object (403) and **cannot see** A's CAS row (RLS → `rpc` update returns conflict/not-found)
4. **CAS race**: two `e9_cas_put(..., expected=1)` calls → exactly one `ok=true (version 2)`, the other `ok=false` with `current_version=2`
5. correct retry: user A calls `e9_cas_put(..., expected=2)` → `ok=true, new_version=3`
6. user A can delete only their own objects/rows; teardown removes test users and objects

## 0002 — devices & profiles (first production tables)

- Migration: [`0002_devices_profiles.sql`](0002_devices_profiles.sql)
- Rollback:  [`0002_devices_profiles.rollback.sql`](0002_devices_profiles.rollback.sql) — **destructive** (drops both tables)

### What it creates / modifies

| Object | Kind | Notes |
|---|---|---|
| `public.synchrome_set_updated_at()` | trigger function | sets `updated_at = now()` on update |
| `public.devices` | table | PK `device_id uuid` (client-generated), `account_id uuid default auth.uid()`, `name`, `platform`, `os_version`, `app_version`, `created_at`, `updated_at`, `last_seen_at`; length checks; index on `account_id`; RLS **enabled + forced** |
| `devices_select/insert/update/delete` | RLS policies | `account_id = auth.uid()` for role `authenticated` |
| `public.profiles` | table | PK `id uuid default gen_random_uuid()`, `account_id uuid default auth.uid()`, `name`, timestamps; `unique (account_id, name)`; index; RLS **enabled + forced** |
| `profiles_select/insert/update/delete` | RLS policies | `account_id = auth.uid()` for role `authenticated` |
| grants | privileges | `usage` on `public` to `anon, authenticated`; CRUD on both tables to `authenticated`; **nothing** to `anon` |

Does **not** touch `e9_*` objects. Aborts if `auth.uid()` is absent (non-Supabase DB). Idempotent (`if not exists`, `create or replace`, `drop … if exists` before `create`).

### How Codex should apply it (on the Synchrome server)

```bash
# backup first (destructive rollback exists)
psql "$SYNCHROME_DATABASE_URL" -v ON_ERROR_STOP=1 -f 0002_devices_profiles.sql
# or inside the container:
# docker exec -i supabase-db psql -U postgres -d postgres -v ON_ERROR_STOP=1 < 0002_devices_profiles.sql
```

Verification query:

```sql
select to_regclass('public.devices')  is not null as devices_ok,
       to_regclass('public.profiles') is not null as profiles_ok,
       (select count(*) from pg_policies where tablename in ('devices','profiles')) as policies, -- expect 8
       (select relforcerowsecurity from pg_class where relname = 'devices')  as devices_forced,
       (select relforcerowsecurity from pg_class where relname = 'profiles') as profiles_forced;
```

PostgREST picks up new tables automatically (schema cache reload on DDL via `pg_notify`); if `/rest/v1/devices` still 404s, `NOTIFY pgrst, 'reload schema';`.

### Validation Claude Code runs afterwards (URL + anon + the existing test user; no DB access)

1. `register_device` upserts this installation → one row with `account_id = auth.uid()`, `is_current = true`
2. a second `register_device` (same `device_id`) **updates** the row (merge-duplicates), no duplicate
3. `touch_device` advances `last_seen_at`
4. `list_devices` returns the account's devices with exactly one `is_current`
5. `list_profiles` returns `[]` for the fresh test account (normal empty state)
6. RLS: a request with another user's JWT sees none of these rows
7. `DeviceSummary`/`ProfileSummary` payloads carry no `account_id`, no tokens

## 0003 — profile_mappings (local browser profile ↔ cloud profile, per device)

- Migration: [`0003_profile_mappings.sql`](0003_profile_mappings.sql) — requires 0002
- Rollback:  [`0003_profile_mappings.rollback.sql`](0003_profile_mappings.rollback.sql) — **destructive** (drops all mappings and tokens)

### What it creates / modifies

| Object | Kind | Notes |
|---|---|---|
| `profiles_account_id_id_key` | unique `(account_id, id)` on `public.profiles` | target of the composite FK |
| `devices_account_id_device_id_key` | unique `(account_id, device_id)` on `public.devices` | target of the composite FK |
| `public.profile_mappings` | table | `id`, `account_id default auth.uid()`, `profile_id`, `device_id`, `browser_kind`, `browser_profile_dir`, `mapping_token`, `token_state`, `token_confirmed_at`, `created_at`, `updated_at`; RLS **enabled + forced** |
| `profile_mappings_profile_fk` | FK `(account_id, profile_id) → profiles(account_id, id)` **ON DELETE RESTRICT** | a mapped cloud profile cannot be deleted; unmap first |
| `profile_mappings_device_fk` | FK `(account_id, device_id) → devices(account_id, device_id)` **ON DELETE CASCADE** | removing a device removes its mappings |
| `profile_mappings_local_unique` | unique `(device_id, browser_kind, browser_profile_dir)` | one mapping per local profile |
| `profile_mappings_token_unique` | unique `(mapping_token)` | |
| checks | `browser_kind ∈ {helium,chrome,brave,edge,chromium}`; dir is a bare name (1–255 chars, no `/` `\`, not `.`/`..`); token `^[0-9a-f]{64}$`; `token_state ∈ {pending_local_write,confirmed}`; `confirmed ⇔ token_confirmed_at not null` | |
| 3 indexes, `updated_at` trigger | | reuses `synchrome_set_updated_at()` from 0002 |
| `profile_mappings_select/insert/update/delete` | RLS policies | `account_id = auth.uid()`, role `authenticated` |
| grants | column-level | `revoke all` from anon+authenticated, then: `select, delete` (table); `insert (profile_id, device_id, browser_kind, browser_profile_dir, mapping_token)`; `update (token_state, token_confirmed_at)` → clients can never set `account_id` or rewrite a mapping's identity/token |
| hardening | `revoke all on devices, profiles from anon` | defense in depth (anon already 401 today) |

**One cloud profile, many local profiles:** there is deliberately **no** `unique (profile_id, device_id)`. The same cloud profile may be mapped to several local profiles on the same device (e.g. "Personal" → Helium/Profile 1 and Chrome/Profile 3) and from any number of devices. Only a concrete local profile is limited to a single mapping.

**Known technical debt — `devices.device_id` is a global primary key.** It is generated per installation, so if a second account signs in on the same installation its device upsert collides with the first account's row and is rejected (RLS → 403), and that account cannot register the device or create mappings. This must be fixed before supporting multiple accounts per installation, most likely by migrating `devices` to a composite PK `(account_id, device_id)` (the composite FKs added here already reference `(account_id, device_id)`, so they keep working). Not changed in 0003.

**Why composite FKs:** Postgres FK checks bypass RLS. With a plain `profile_id → profiles(id)` FK, user B could insert a mapping that references user A's profile by UUID. Pinning `account_id` into the FK (and `account_id = auth.uid()` via RLS `with check`) makes cross-account references impossible.

### How Codex should apply it (on the Synchrome server)

```bash
# backup first (rollback is destructive)
psql "$SYNCHROME_DATABASE_URL" -v ON_ERROR_STOP=1 -f 0003_profile_mappings.sql
# or: docker exec -i supabase-db psql -U postgres -d postgres -v ON_ERROR_STOP=1 < 0003_profile_mappings.sql
```

Verification query:

```sql
select to_regclass('public.profile_mappings') is not null                                as table_ok,
       (select relforcerowsecurity from pg_class where relname = 'profile_mappings')     as rls_forced,
       (select count(*) from pg_policies where tablename = 'profile_mappings')           as policies,      -- expect 4
       (select count(*) from pg_constraint where conrelid = 'public.profile_mappings'::regclass
          and contype = 'f')                                                             as fks,           -- expect 2
       (select confdeltype from pg_constraint where conname = 'profile_mappings_profile_fk') as profile_del, -- expect 'r'
       (select confdeltype from pg_constraint where conname = 'profile_mappings_device_fk')  as device_del,  -- expect 'c'
       exists (select 1 from pg_constraint where conname = 'profiles_account_id_id_key')     as profiles_uk,
       exists (select 1 from pg_constraint where conname = 'devices_account_id_device_id_key') as devices_uk;
-- column privileges for authenticated (expect insert on 5 cols, update on 2):
select privilege_type, column_name from information_schema.column_privileges
 where table_name = 'profile_mappings' and grantee = 'authenticated' order by 1, 2;
```

If `/rest/v1/profile_mappings` still 404s afterwards: `NOTIFY pgrst, 'reload schema';`.

## 0004 — store objects (Storage bucket `stores`) + `store_events`

- Migration: [`0004_store_objects.sql`](0004_store_objects.sql)
- Rollback:  [`0004_store_objects.rollback.sql`](0004_store_objects.rollback.sql) — **destructive** for `store_events`; keeps a non-empty bucket (see below)

First production data path (M1 slice 7, bookmarks upload-only). Synced data lives as
objects in Storage, Postgres keeps metadata (`docs/03` §4, `docs/04` §3). Object names:

```text
stores/<accountId>/profiles/<profileId>/journal/<deviceId>/<yyyy>/<mm>/<seq:08>.jsonl
```

### What it creates / modifies

| Object | Kind | Notes |
|---|---|---|
| `storage.buckets` row `stores` | data (upsert) | **private**, `file_size_limit` 4 MiB, `allowed_mime_types` `application/x-ndjson`, `application/json` |
| `public.synchrome_journal_object_ok(text)` | function, `SECURITY INVOKER`, `search_path = ''` | true iff the name is a well-formed journal chunk whose 1st segment is `auth.uid()` and whose profile / device rows are the caller's (lookups obey RLS) · `execute` to `authenticated` only |
| `synchrome_stores_select` / `_delete` | RLS on `storage.objects` | `bucket_id = 'stores'` and first folder = `auth.uid()` |
| `synchrome_stores_insert` / `_update` | RLS on `storage.objects` | `bucket_id = 'stores'` and `synchrome_journal_object_ok(name)` (update also requires the own prefix in `using`) — upsert needs select+insert+update |
| `public.store_events` | table | `id` identity, `account_id default auth.uid()`, `profile_id`, `device_id`, `path`, `seq_from`, `seq_to`, `hlc`, `created_at`; RLS **enabled + forced** |
| `store_events_profile_fk` / `_device_fk` | composite FKs `(account_id, …)` | to `profiles (account_id, id)` / `devices (account_id, device_id)`, both **ON DELETE CASCADE** (derived metadata) |
| `store_events_publication_unique` | unique `(profile_id, device_id, seq_to)` | a retried publication (`resolution=ignore-duplicates`) is a no-op |
| checks | `seq_from ≥ 1`, `seq_to ≥ seq_from`, `path` = journal shape whose profile/device segments equal the row's ids, `hlc` = `^\d{13}-\d{4}-<uuid>$` | |
| `store_events_select/insert/delete` | RLS policies | `account_id = auth.uid()`; **no update** (append-only) |
| grants | column-level | `revoke all` from anon+authenticated; `select, delete`; `insert (profile_id, device_id, path, seq_from, seq_to, hlc)` |

Not touched: `e9_*`, 0002/0003 tables (only read by the name check), Realtime publication
(added when the download slice subscribes). Nothing is granted to `anon`; the app never
uses the service role.

**Deleting a profile**: the `profiles` row's FKs do not reach Storage, so the app deletes
the profile's objects through the Storage API first, then the row (`store_events` cascade).

### How Codex should apply it (on the Synchrome server)

```bash
psql "$SYNCHROME_DATABASE_URL" -v ON_ERROR_STOP=1 -f 0004_store_objects.sql
# or: docker exec -i supabase-db psql -U postgres -d postgres -v ON_ERROR_STOP=1 < 0004_store_objects.sql
```

Verification query:

```sql
select exists (select 1 from storage.buckets where id = 'stores' and not public)            as bucket_private,
       (select file_size_limit from storage.buckets where id = 'stores')                     as size_limit,      -- 4194304
       (select allowed_mime_types from storage.buckets where id = 'stores')                  as mime_types,
       (select count(*) from pg_policies where schemaname = 'storage' and tablename = 'objects'
          and policyname like 'synchrome_stores_%')                                         as storage_policies, -- expect 4
       to_regprocedure('public.synchrome_journal_object_ok(text)') is not null               as name_fn,
       (select prosecdef from pg_proc where proname = 'synchrome_journal_object_ok')         as security_definer,-- expect false
       to_regclass('public.store_events') is not null                                         as events_ok,
       (select relforcerowsecurity from pg_class where relname = 'store_events')            as events_forced,
       (select count(*) from pg_policies where tablename = 'store_events')                  as events_policies,  -- expect 3
       (select count(*) from pg_constraint where conrelid = 'public.store_events'::regclass
          and contype = 'f')                                                                 as events_fks;       -- expect 2
select privilege_type, column_name from information_schema.column_privileges
 where table_name = 'store_events' and grantee = 'authenticated' order by 1, 2;   -- insert on 6 cols, select/delete
```

If `/rest/v1/store_events` 404s afterwards: `NOTIFY pgrst, 'reload schema';`.

**Rollback**: empty the bucket through the Storage API first (deleting `storage.objects`
rows directly orphans the stored files), then run the rollback; with objects present it
keeps the bucket and raises a NOTICE. On Storage versions that forbid direct `DELETE` on
`storage.buckets`, remove the empty bucket with the Storage API (`DELETE /storage/v1/bucket/stores`)
from the server instead.

### Validation Claude Code runs afterwards (URL + anon + two test users; no DB access)

`tests/experiments/e2e_bookmarks.py <creds>` with a disposable Helium profile and the real
extension: initial snapshot of a known tree reaches the cloud with correct structure and
order; repeated snapshot adds nothing; create / rename / move / delete each publish only their
delta; Helium restart continues with 0 new ops; cloud unreachable → changes queued (Offline)
and published after recovery with contiguous `seq`; user B cannot list, read or write A's
objects (GET → not found, PUT into A's profile → 403 by `synchrome_journal_object_ok`) nor
see or insert A's `store_events`; cleanup purges the objects and rows (accounts back to 0).

## 0005 — compaction metadata, guarded journal GC, Realtime on `store_events`

- Migration: [`0005_compaction_realtime.sql`](0005_compaction_realtime.sql)
- Rollback:  [`0005_compaction_realtime.rollback.sql`](0005_compaction_realtime.rollback.sql)
- Functional verification (rolled back, leaves nothing): [`0005_compaction_realtime.verify.sql`](0005_compaction_realtime.verify.sql)
- Spec: `docs/03` §3.7 (M1 slice 13). **Status: reviewed (2026-10-03, two rounds), NOT applied.**

### What it creates / modifies

| Object | Kind | Notes |
|---|---|---|
| `public.sync_cursors` | table (new) | PK `(profile_id, device_id)`; `applied jsonb {source device: applied_through}`, `published_through`, `snapshot_generation`, `snapshot_protocol int` (0 = cannot read snapshots, 1 = `synchrome-snapshot/1`), `updated_at` (last report, server time), `retired_at` (explicit user action only, server time). Composite FKs to `profiles` / `devices` (cascade). RLS enabled + forced, 4 owner policies. Column grants only. |
| `synchrome_sync_cursors_touch()` | trigger fn | `updated_at := now()` only when cursors/protocol change (retiring is not activity); a new `retired_at` becomes `now()`, an existing one keeps its time, `null` un-retires. Nothing retires by age. |
| `public.profile_snapshots` | table (new) | unique `(profile_id, generation)` = compaction lock; `building → ready → superseded`; manifest path/hash checks; server times `claimed_at`, `expires_at` (+30 min), `ready_at`. FK to `profiles` (cascade); `created_by` without FK on purpose. RLS enabled + forced; DELETE only `superseded` rows or expired claims. |
| `synchrome_profile_snapshots_guard()` | trigger fn | insert: generation = latest + 1, server sets state/times. `building → ready` (before expiry) **re-validates the watermarks**: integers; monotonic vs. the previous ready generation (no device dropped/lowered); each new `W_D` is the `seq_to` of a publication, `(W_prev, W_D]` is a contiguous chain of `store_events` with none crossing either end; `W_D ≤` applied cursor of every active (≤ 30 d), non-retired, `snapshot_protocol ≥ 1` reader. `ready → superseded` only with two newer ready generations and the newest ready ≥ 24 h; ready contents frozen. |
| `synchrome_gc_blockers(uuid)` | fn | devices mapped to the profile (`profile_mappings`) without a `sync_cursors` row that has `snapshot_protocol ≥ 1` or `retired_at`. |
| `synchrome_compaction_status(uuid)` | fn | database-clock decisions: current/previous generation, `journal_gc_watermarks` = W(previous) (GC one generation behind), `gc_blockers`, `journal_gc_ok` (previous exists ∧ `now() ≥ ready_at(current) + 10 min` ∧ no blockers), `generation_cleanup_ok` (current ready ≥ 24 h), `claim_allowed` (no live claim ∧ ≥ 6 h since the last claim). |
| `synchrome_journal_gc_allowed(uuid, uuid, bigint)` | fn | **the** journal GC rule: `journal_gc_ok ∧ seq ≤ W(previous)[device]`. |
| `synchrome_gc_journal_events(uuid, uuid, bigint)` | RPC (VOLATILE) | deletes one source device's `store_events` with `seq_to ≤ through`; raises `42501` unless `through` is eligible; RLS re-checks each row. |
| `store_events_delete` → `store_events_delete_gc` | RLS policy (**replaced**) | 0004 let any client of the account delete any row; now only rows `synchrome_journal_gc_allowed(profile_id, device_id, seq_to)` allows. FK cascades (profile/device deletion) unaffected. |
| `synchrome_snapshot_object_ok(text)` | fn | snapshot names only for the caller's own unexpired `building` claim. |
| `synchrome_snapshot_object_protected(text)` | fn | object of a `ready` generation. |
| `synchrome_object_delete_ok(text)` | fn | snapshot object: not protected; journal chunk: profile gone, or `synchrome_journal_gc_allowed(profile, device, <first seq from the file name>)`; anything else: true. |
| `synchrome_stores_insert` / `synchrome_stores_update` | storage policies (replaced) | `journal_ok(name) OR snapshot_ok(name)`. |
| `synchrome_stores_delete_guard` | storage policy (new, **RESTRICTIVE**, delete) | `bucket_id <> 'stores' OR synchrome_object_delete_ok(name)`, AND-ed with 0004's own-prefix delete policy. |
| publication `supabase_realtime` | membership | `store_events` added **only if** the publication exists, is not `FOR ALL TABLES` and does not already list it (`pg_publication` / `pg_publication_tables`); otherwise a NOTICE. Re-running is a no-op. |

All functions are `SECURITY INVOKER` with an empty `search_path`; `EXECUTE` revoked from `public`/`anon`,
granted to `authenticated`. Nothing is deleted by the migration itself.

**App-side consequences (implemented with the slice):** profile cleanup = unmap → delete journal
objects (allowed once the profile row is gone, or GC-eligible) → delete the profile row (cascades
`store_events`, `profile_snapshots`, `sync_cursors`) → delete remaining journal and snapshot objects.
The slice-8 harness command that hides a single `store_events` row (`--dev-delete-event`) no longer
works on an instance with 0005; gap tests run against the in-memory fake.

### How Codex should apply it (on the Synchrome server)

1. Prerequisites (read-only; all `true`):

```sql
select to_regclass('public.store_events') is not null                                  as has_0004,
       to_regprocedure('public.synchrome_journal_object_ok(text)') is not null           as has_journal_fn,
       to_regclass('public.profile_mappings') is not null                                 as has_0003,
       to_regclass('public.sync_cursors') is null                                          as no_cursors_yet,
       to_regclass('public.profile_snapshots') is null                                     as no_snapshots_yet;
select pubname, puballtables from pg_publication where pubname = 'supabase_realtime';  -- expect one row, puballtables = false
```

2. Apply (the file has its own `begin; … commit;`):

```bash
docker exec -i supabase-db psql -U postgres -d postgres -v ON_ERROR_STOP=1 < cloud/migrations/0005_compaction_realtime.sql
```

3. Reload PostgREST; restart Realtime only if it caches publications:

```sql
NOTIFY pgrst, 'reload schema';
```

```bash
docker restart supabase-realtime
```

4. Structural verification:

```sql
select to_regclass('public.sync_cursors') is not null                                         as cursors_ok,
       to_regclass('public.profile_snapshots') is not null                                    as snapshots_ok,
       (select relforcerowsecurity from pg_class where relname = 'sync_cursors')              as cursors_forced,      -- true
       (select relforcerowsecurity from pg_class where relname = 'profile_snapshots')         as snapshots_forced,    -- true
       (select count(*) from pg_policies where tablename = 'sync_cursors')                    as cursors_policies,    -- 4
       (select count(*) from pg_policies where tablename = 'profile_snapshots')               as snapshots_policies,  -- 4
       (select array_agg(policyname order by policyname) from pg_policies
         where tablename = 'store_events')                                                     as events_policies,     -- {store_events_delete_gc,store_events_insert,store_events_select}
       (select count(*) from pg_trigger where tgname in ('sync_cursors_touch', 'profile_snapshots_guard')) as triggers, -- 2
       (select count(*) from pg_proc where proname in ('synchrome_gc_blockers', 'synchrome_compaction_status',
          'synchrome_journal_gc_allowed', 'synchrome_gc_journal_events', 'synchrome_snapshot_object_ok',
          'synchrome_snapshot_object_protected', 'synchrome_object_delete_ok'))               as new_functions,       -- 7
       (select bool_or(prosecdef) from pg_proc where proname like 'synchrome_%')               as any_security_definer, -- false
       (select array_agg(policyname || ':' || permissive order by policyname) from pg_policies
         where schemaname = 'storage' and tablename = 'objects' and policyname like 'synchrome_stores_%') as storage_policies,
         -- {synchrome_stores_delete:PERMISSIVE, synchrome_stores_delete_guard:RESTRICTIVE, synchrome_stores_insert:PERMISSIVE,
         --  synchrome_stores_select:PERMISSIVE, synchrome_stores_update:PERMISSIVE}
       exists (select 1 from pg_publication_tables where pubname = 'supabase_realtime'
                 and schemaname = 'public' and tablename = 'store_events')                    as realtime_store_events; -- true
select table_name, privilege_type, column_name from information_schema.column_privileges
 where table_name in ('sync_cursors', 'profile_snapshots') and grantee = 'authenticated' order by 1, 2, 3;
select table_name, grantee, privilege_type from information_schema.role_table_grants
 where table_name in ('sync_cursors', 'profile_snapshots', 'store_events') and grantee in ('anon', 'authenticated') order by 1, 2, 3;
select p.proname, has_function_privilege('anon', p.oid, 'execute') as anon_exec,
       has_function_privilege('authenticated', p.oid, 'execute') as auth_exec
  from pg_proc p where p.proname like 'synchrome_%' order by 1;  -- anon_exec false everywhere
```

Expected: `sync_cursors` insert on `applied, device_id, profile_id, published_through, retired_at,
snapshot_generation, snapshot_protocol`, update on `applied, published_through, retired_at,
snapshot_generation, snapshot_protocol`; `profile_snapshots` insert on `created_by, generation,
profile_id`, update on `bytes, counts, manifest_path, manifest_sha256, state, watermarks`;
table-level `SELECT`, `DELETE` for `authenticated` on both new tables; nothing for `anon`.

5. Functional verification (as `authenticated` with synthetic claims, everything rolled back):

```bash
docker exec -i supabase-db psql -U postgres -d postgres -v ON_ERROR_STOP=1 < cloud/migrations/0005_compaction_realtime.verify.sql
```

It must end with `NOTICE:  ALL 0005 CHECKS PASSED` (any failure raises `FAIL: …`). It checks:
generation skip/reuse refused; snapshot objects only for a building claim; watermarks inside a
chunk, beyond the published chain, above an active reader's cursor, or dropping a device are
refused; `ready_at` server-stamped; a ready snapshot cannot be written, edited, deleted nor its
objects deleted; a single ready generation makes nothing GC-eligible; before the 10 min grace
neither a raw `DELETE` nor the RPC removes anything; the previous generation cannot be superseded;
a mapped device without snapshot support blocks GC; after it reports `snapshot_protocol = 1`, GC
is allowed exactly up to `W(previous)` (RPC above it refused; journal chunk objects above it not
deletable, below it deletable; a raw `DELETE` removes nothing else); a client-supplied
`retired_at` is replaced by server time; account B sees and deletes nothing of A, cannot run A's
GC, claim a snapshot of A's profile or write under A's prefix; `store_events` is in the
publication. (The only privileged step back-dates `ready_at` inside the rolled-back transaction.)

6. Re-running the migration is a no-op apart from re-creating the same functions/policies.

**Rollback** (`0005_compaction_realtime.rollback.sql`, idempotent): refuses and changes nothing
while any generation is `ready` or `superseded`. Otherwise: removes `store_events` from
`supabase_realtime` **only if it is an explicit member** (`pg_publication_rel`, never from a
`FOR ALL TABLES` publication — note: if someone had added it manually before 0005, the rollback
removes that membership too); restores 0004's `store_events_delete` and storage INSERT/UPDATE
policies; drops the restrictive delete policy, the 7 functions and both tables. Snapshot objects are
never deleted by SQL: remove `<uid>/profiles/*/snapshots/` with the Storage API.

### Validation Claude Code runs afterwards (URL + anon + two test users; no DB access)

`tests/experiments/e2e_compaction.py <creds>` (slice 13): Realtime join with the user JWT
receives A's `store_events` INSERTs and never B's; claim → upload → ready over REST/Storage;
upload outside a claim or after expiry → 403; ready objects not deletable (403); journal chunks and
events not deletable before eligibility, deletable after; GC blocked while a mapped device has not
reported snapshot support; user B cannot read, claim, write or delete A's cursors, snapshots or
objects; cleanup leaves both accounts at 0 rows / 0 objects.
