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
