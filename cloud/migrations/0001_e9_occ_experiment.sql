-- ============================================================================
-- Synchrome — E9 experiment migration: optimistic concurrency control (OCC/CAS)
-- ----------------------------------------------------------------------------
-- Purpose: close experiment E9 by giving an authenticated user a way to perform
--          compare-and-swap writes on a per-object version row, plus Storage
--          policies that let a user read/write ONLY their own prefix in the
--          experimental bucket. This is EXPERIMENTAL scaffolding, not the final
--          Synchrome schema. Everything it creates is prefixed `e9_` / named for
--          the `e9-experiments` bucket and is fully removed by the matching
--          rollback file (0001_e9_occ_experiment.rollback.sql).
--
-- Properties: idempotent (safe to re-run), explicit, reversible, and guarded so
--             it only applies to a Supabase database (checks auth.uid exists).
--
-- Apply (on the Synchrome server, by Codex):  see cloud/migrations/README.md
-- Do NOT run this anywhere other than the Synchrome Supabase database.
-- ============================================================================

begin;

-- 0. Safety guard: refuse to run on a non-Supabase database --------------------
do $$
begin
  if to_regprocedure('auth.uid()') is null then
    raise exception
      'auth.uid() not found: this does not look like a Supabase database. Aborting E9 migration.';
  end if;
end $$;

-- 1. OCC table ----------------------------------------------------------------
-- One row per shared mutable object (e.g. store.json, profiles/<id>/profile.json,
-- snapshots). The object bytes live in Storage; this row is the CAS guard.
create table if not exists public.e9_objects (
  account_id uuid        not null default auth.uid(),  -- owner; equals auth.uid()
  store_id   uuid        not null,                     -- which store within the account
  path       text        not null,                     -- logical object path (no secrets)
  version    bigint      not null default 0,            -- monotonic; CAS token
  hlc        text,                                      -- last writer's hybrid logical clock
  device_id  uuid,                                      -- last writer's device
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  constraint e9_objects_pkey primary key (account_id, store_id, path),
  constraint e9_objects_path_len check (char_length(path) <= 1024)
);

comment on table  public.e9_objects is
  'E9 experiment: optimistic-concurrency guard rows for Synchrome shared objects. Safe to drop.';
comment on column public.e9_objects.version is
  'Monotonic CAS token; a write succeeds only when the caller''s expected version matches.';

-- 2. Row level security -------------------------------------------------------
alter table public.e9_objects enable row level security;
-- Make ownership tamper-proof even if a client sends a different account_id.
alter table public.e9_objects force row level security;

-- 3. RLS policies: a user may only see and change their own rows ---------------
drop policy if exists e9_objects_select on public.e9_objects;
create policy e9_objects_select on public.e9_objects
  for select to authenticated
  using (account_id = auth.uid());

drop policy if exists e9_objects_insert on public.e9_objects;
create policy e9_objects_insert on public.e9_objects
  for insert to authenticated
  with check (account_id = auth.uid());

drop policy if exists e9_objects_update on public.e9_objects;
create policy e9_objects_update on public.e9_objects
  for update to authenticated
  using (account_id = auth.uid())
  with check (account_id = auth.uid());

drop policy if exists e9_objects_delete on public.e9_objects;
create policy e9_objects_delete on public.e9_objects
  for delete to authenticated
  using (account_id = auth.uid());

-- Table privileges for the PostgREST roles (RLS still applies on top).
grant usage on schema public to anon, authenticated;
grant select, insert, update, delete on public.e9_objects to authenticated;

-- 4. Atomic OCC function ------------------------------------------------------
-- Single round-trip compare-and-swap that also creates the row on first write.
-- Returns one row: (ok, new_version, current_version).
--   ok = true  -> caller won; new_version is the stored version after the write
--   ok = false -> caller lost the race; current_version is the version now stored
--                 (null only when create was attempted with expected <> 0)
-- Semantics:
--   * create : expected_version = 0 and no row exists -> inserts version = 1
--   * update : row exists and version = expected_version -> version = expected + 1
--   * conflict: any other case -> ok = false, current_version = stored version
-- SECURITY INVOKER: runs as the caller, so RLS enforces ownership; account_id is
-- taken from auth.uid() and never from client input.
create or replace function public.e9_cas_put(
  p_store_id         uuid,
  p_path             text,
  p_expected_version bigint,
  p_hlc              text default null,
  p_device_id        uuid default null
) returns table (ok boolean, new_version bigint, current_version bigint)
language plpgsql
security invoker
set search_path = public
as $$
declare
  v_account uuid := auth.uid();
  v_current bigint;
begin
  if v_account is null then
    raise exception 'not authenticated' using errcode = '28000';
  end if;

  -- Lock the row for this (account, store, path) if it exists.
  select version into v_current
    from public.e9_objects
   where account_id = v_account and store_id = p_store_id and path = p_path
   for update;

  if not found then
    if p_expected_version <> 0 then
      -- Expected an existing row but there is none -> conflict.
      return query select false, null::bigint, null::bigint;
      return;
    end if;
    insert into public.e9_objects (account_id, store_id, path, version, hlc, device_id)
      values (v_account, p_store_id, p_path, 1, p_hlc, p_device_id);
    return query select true, 1::bigint, 0::bigint;
    return;
  end if;

  if v_current = p_expected_version then
    update public.e9_objects
       set version    = version + 1,
           hlc        = coalesce(p_hlc, hlc),
           device_id  = coalesce(p_device_id, device_id),
           updated_at = now()
     where account_id = v_account and store_id = p_store_id and path = p_path;
    return query select true, v_current + 1, v_current;
    return;
  end if;

  -- Version mismatch -> another writer won.
  return query select false, null::bigint, v_current;
end;
$$;

comment on function public.e9_cas_put(uuid, text, bigint, text, uuid) is
  'E9 experiment: atomic compare-and-swap upsert for e9_objects. Safe to drop.';

grant execute on function public.e9_cas_put(uuid, text, bigint, text, uuid) to authenticated;

-- 5. Experimental Storage bucket + prefix-scoped policies ---------------------
-- Bucket is private; objects live under "<account_id>/<store_id>/...". A user may
-- touch only objects whose first path segment equals their auth.uid().
insert into storage.buckets (id, name, public)
  values ('e9-experiments', 'e9-experiments', false)
  on conflict (id) do nothing;

drop policy if exists e9_storage_select on storage.objects;
create policy e9_storage_select on storage.objects
  for select to authenticated
  using (bucket_id = 'e9-experiments'
         and (storage.foldername(name))[1] = auth.uid()::text);

drop policy if exists e9_storage_insert on storage.objects;
create policy e9_storage_insert on storage.objects
  for insert to authenticated
  with check (bucket_id = 'e9-experiments'
              and (storage.foldername(name))[1] = auth.uid()::text);

drop policy if exists e9_storage_update on storage.objects;
create policy e9_storage_update on storage.objects
  for update to authenticated
  using (bucket_id = 'e9-experiments'
         and (storage.foldername(name))[1] = auth.uid()::text)
  with check (bucket_id = 'e9-experiments'
              and (storage.foldername(name))[1] = auth.uid()::text);

drop policy if exists e9_storage_delete on storage.objects;
create policy e9_storage_delete on storage.objects
  for delete to authenticated
  using (bucket_id = 'e9-experiments'
         and (storage.foldername(name))[1] = auth.uid()::text);

commit;

-- End of 0001_e9_occ_experiment.sql
