-- ============================================================================
-- Synchrome — 0005: journal compaction metadata + Realtime wake-up
-- ----------------------------------------------------------------------------
-- M1 slice 13 (docs/03 §3.7). Adds, without changing anything 0004 created:
--
--   public.sync_cursors       one row per (profile, reader device): what the device has
--                             applied of every other device's journal (`applied`), its own
--                             contiguous published seq, the snapshot generation it is based on,
--                             server-side `updated_at` (activity) and `retired_at`.
--                             Compaction folds only history every ACTIVE reader has applied.
--
--   public.profile_snapshots  one row per snapshot generation of a profile: claimed as
--                             `building` (unique (profile_id, generation) = the compaction
--                             lock), switched to `ready` with the manifest path + SHA-256,
--                             watermarks and counts after the objects were verified, later
--                             `superseded`. Readers only ever use `ready` rows.
--
--   storage.objects policies  INSERT / UPDATE additionally accept snapshot objects
--                             <uid>/profiles/<profile>/snapshots/<generation:10>/manifest.json
--                             <uid>/profiles/<profile>/snapshots/<generation:10>/<collection>-<nnnn>.jsonl
--                             but only while the caller's row for that generation is `building`
--                             and unexpired: a ready snapshot can never be overwritten.
--
--   Realtime                  public.store_events is added to the `supabase_realtime`
--                             publication so the app can receive INSERTs as a wake-up hint.
--                             Realtime applies the table's RLS with the subscriber's JWT
--                             (store_events_select: account_id = auth.uid()).
--
-- Design decisions (see cloud/migrations/README.md §0005):
--   * Owner-scoped like everything else: account_id = auth.uid(), RLS enabled + forced,
--     composite FKs (account_id, profile_id) / (account_id, device_id) because FK checks
--     bypass RLS. Column-level grants; the server owns timestamps and state transitions
--     (triggers), the client cannot back-date activity or resurrect a superseded snapshot.
--   * profile_snapshots.created_by is informational (no FK): deleting a device must never
--     cascade away the snapshot that holds its folded history.
--   * Deletion of folded journals uses the DELETE policies 0004 already grants on
--     store_events and storage.objects (own account only). Nothing here deletes data.
--   * The service role is never used by the app; nothing here grants anything to anon.
--
-- Requires 0002 + 0003 + 0004. Idempotent, explicit, reversible
-- (0005_compaction_realtime.rollback.sql), Supabase-guarded. NOT YET APPLIED.
-- ============================================================================

begin;

-- 0. Safety guards ------------------------------------------------------------
do $$
begin
  if to_regprocedure('auth.uid()') is null then
    raise exception 'auth.uid() not found: not a Supabase database. Aborting 0005.';
  end if;
  if to_regclass('storage.objects') is null or to_regprocedure('storage.foldername(text)') is null then
    raise exception 'Supabase Storage schema missing. Aborting 0005.';
  end if;
  if to_regclass('public.store_events') is null
     or to_regprocedure('public.synchrome_journal_object_ok(text)') is null then
    raise exception 'objects from 0004 missing: apply 0004 first. Aborting 0005.';
  end if;
  if not exists (select 1 from pg_constraint where conname = 'profiles_account_id_id_key')
     or not exists (select 1 from pg_constraint where conname = 'devices_account_id_device_id_key') then
    raise exception 'composite keys from 0003 missing. Aborting 0005.';
  end if;
end $$;

-- 1. sync_cursors ---------------------------------------------------------------
create table if not exists public.sync_cursors (
  account_id           uuid        not null default auth.uid(),
  profile_id           uuid        not null,
  device_id            uuid        not null,        -- the reader
  applied              jsonb       not null default '{}'::jsonb,  -- {"<source device>": applied_through}
  published_through    bigint      not null default 0,
  snapshot_generation  bigint,
  updated_at           timestamptz not null default now(),   -- server time (trigger)
  retired_at           timestamptz,

  primary key (profile_id, device_id),
  constraint sync_cursors_profile_fk
    foreign key (account_id, profile_id) references public.profiles (account_id, id) on delete cascade,
  constraint sync_cursors_device_fk
    foreign key (account_id, device_id) references public.devices (account_id, device_id) on delete cascade,
  constraint sync_cursors_applied_chk
    check (jsonb_typeof(applied) = 'object' and pg_column_size(applied) <= 65536),
  constraint sync_cursors_published_chk
    check (published_through >= 0),
  constraint sync_cursors_generation_chk
    check (snapshot_generation is null or snapshot_generation >= 1)
);

comment on table public.sync_cursors is
  'Synchrome: per (profile, reader device) applied cursors, activity and retirement; input of the compaction watermark (docs/03 §3.7).';

-- activity is server time; a change that only retires/unretires a device does not count as activity
create or replace function public.synchrome_sync_cursors_touch()
returns trigger
language plpgsql
security invoker
set search_path = ''
as $$
begin
  if tg_op = 'INSERT'
     or new.applied is distinct from old.applied
     or new.published_through is distinct from old.published_through
     or new.snapshot_generation is distinct from old.snapshot_generation then
    new.updated_at := now();
  else
    new.updated_at := old.updated_at;
  end if;
  return new;
end;
$$;

drop trigger if exists sync_cursors_touch on public.sync_cursors;
create trigger sync_cursors_touch
  before insert or update on public.sync_cursors
  for each row execute function public.synchrome_sync_cursors_touch();

alter table public.sync_cursors enable row level security;
alter table public.sync_cursors force row level security;

drop policy if exists sync_cursors_select on public.sync_cursors;
create policy sync_cursors_select on public.sync_cursors
  for select to authenticated using (account_id = (select auth.uid()));
drop policy if exists sync_cursors_insert on public.sync_cursors;
create policy sync_cursors_insert on public.sync_cursors
  for insert to authenticated with check (account_id = (select auth.uid()));
drop policy if exists sync_cursors_update on public.sync_cursors;
create policy sync_cursors_update on public.sync_cursors
  for update to authenticated
  using (account_id = (select auth.uid())) with check (account_id = (select auth.uid()));
drop policy if exists sync_cursors_delete on public.sync_cursors;
create policy sync_cursors_delete on public.sync_cursors
  for delete to authenticated using (account_id = (select auth.uid()));

revoke all on public.sync_cursors from anon, authenticated;
grant select, delete on public.sync_cursors to authenticated;
grant insert (profile_id, device_id, applied, published_through, snapshot_generation, retired_at)
  on public.sync_cursors to authenticated;
grant update (applied, published_through, snapshot_generation, retired_at)
  on public.sync_cursors to authenticated;

-- 2. profile_snapshots ------------------------------------------------------------
create table if not exists public.profile_snapshots (
  id               bigint      generated always as identity primary key,
  account_id       uuid        not null default auth.uid(),
  profile_id       uuid        not null,
  generation       bigint      not null,
  state            text        not null default 'building',
  created_by       uuid        not null,               -- device (informational, no FK)
  manifest_path    text,
  manifest_sha256  text,
  watermarks       jsonb       not null default '{}'::jsonb,   -- {"<device>": seq}
  counts           jsonb       not null default '{}'::jsonb,   -- {"<collection>": {"entities": n, "tombstones": n}}
  bytes            bigint      not null default 0,
  claimed_at       timestamptz not null default now(),
  expires_at       timestamptz not null default now() + interval '30 minutes',
  ready_at         timestamptz,

  constraint profile_snapshots_generation_unique unique (profile_id, generation),
  constraint profile_snapshots_profile_fk
    foreign key (account_id, profile_id) references public.profiles (account_id, id) on delete cascade,
  constraint profile_snapshots_state_chk
    check (state in ('building', 'ready', 'superseded')),
  constraint profile_snapshots_generation_chk
    check (generation >= 1 and generation <= 9999999999),
  constraint profile_snapshots_ready_chk
    check (state = 'building'
           or (manifest_path is not null and manifest_sha256 is not null and ready_at is not null)),
  constraint profile_snapshots_manifest_path_chk
    check (manifest_path is null
           or (manifest_path ~ '^profiles/[0-9a-f-]{36}/snapshots/[0-9]{10}/manifest\.json$'
               and split_part(manifest_path, '/', 2) = profile_id::text
               and split_part(manifest_path, '/', 4) = lpad(generation::text, 10, '0'))),
  constraint profile_snapshots_sha_chk
    check (manifest_sha256 is null or manifest_sha256 ~ '^[0-9a-f]{64}$'),
  constraint profile_snapshots_json_chk
    check (jsonb_typeof(watermarks) = 'object' and jsonb_typeof(counts) = 'object'
           and pg_column_size(watermarks) <= 65536 and pg_column_size(counts) <= 16384),
  constraint profile_snapshots_bytes_chk
    check (bytes >= 0)
);

comment on table public.profile_snapshots is
  'Synchrome: compacted snapshot generations of a profile (building -> ready -> superseded); the unique generation is the compaction lock (docs/03 §3.7).';

create index if not exists profile_snapshots_ready_idx
  on public.profile_snapshots (account_id, profile_id, generation desc) where state = 'ready';

-- the server owns timestamps and the state machine
create or replace function public.synchrome_profile_snapshots_guard()
returns trigger
language plpgsql
security invoker
set search_path = ''
as $$
declare
  latest bigint;
begin
  if tg_op = 'INSERT' then
    select coalesce(max(s.generation), 0) into latest
      from public.profile_snapshots s
     where s.profile_id = new.profile_id;
    if new.generation <> latest + 1 then
      raise exception 'snapshot generation must be % (got %)', latest + 1, new.generation
        using errcode = '23514';
    end if;
    new.state := 'building';
    new.manifest_path := null;
    new.manifest_sha256 := null;
    new.ready_at := null;
    new.claimed_at := now();
    new.expires_at := now() + interval '30 minutes';
    return new;
  end if;
  -- UPDATE
  if new.profile_id <> old.profile_id or new.generation <> old.generation
     or new.account_id <> old.account_id or new.created_by <> old.created_by
     or new.claimed_at <> old.claimed_at or new.expires_at <> old.expires_at then
    raise exception 'immutable snapshot columns' using errcode = '23514';
  end if;
  if old.state = 'building' and new.state = 'ready' then
    if old.expires_at < now() then
      raise exception 'snapshot claim expired' using errcode = '23514';
    end if;
    new.ready_at := now();
  elsif old.state = 'ready' and new.state = 'superseded' then
    if new.manifest_path is distinct from old.manifest_path
       or new.manifest_sha256 is distinct from old.manifest_sha256
       or new.watermarks is distinct from old.watermarks
       or new.counts is distinct from old.counts or new.bytes <> old.bytes then
      raise exception 'a ready snapshot is immutable' using errcode = '23514';
    end if;
    new.ready_at := old.ready_at;
  elsif old.state = 'building' and new.state = 'building' then
    null;  -- progress updates of the builder (counts/bytes) are allowed
  else
    raise exception 'invalid snapshot transition % -> %', old.state, new.state using errcode = '23514';
  end if;
  return new;
end;
$$;

drop trigger if exists profile_snapshots_guard on public.profile_snapshots;
create trigger profile_snapshots_guard
  before insert or update on public.profile_snapshots
  for each row execute function public.synchrome_profile_snapshots_guard();

alter table public.profile_snapshots enable row level security;
alter table public.profile_snapshots force row level security;

drop policy if exists profile_snapshots_select on public.profile_snapshots;
create policy profile_snapshots_select on public.profile_snapshots
  for select to authenticated using (account_id = (select auth.uid()));
drop policy if exists profile_snapshots_insert on public.profile_snapshots;
create policy profile_snapshots_insert on public.profile_snapshots
  for insert to authenticated with check (account_id = (select auth.uid()));
drop policy if exists profile_snapshots_update on public.profile_snapshots;
create policy profile_snapshots_update on public.profile_snapshots
  for update to authenticated
  using (account_id = (select auth.uid())) with check (account_id = (select auth.uid()));
drop policy if exists profile_snapshots_delete on public.profile_snapshots;
create policy profile_snapshots_delete on public.profile_snapshots
  for delete to authenticated using (account_id = (select auth.uid()));

revoke all on public.profile_snapshots from anon, authenticated;
grant select, delete on public.profile_snapshots to authenticated;
grant insert (profile_id, generation, created_by) on public.profile_snapshots to authenticated;
grant update (state, manifest_path, manifest_sha256, watermarks, counts, bytes)
  on public.profile_snapshots to authenticated;

-- 3. Storage: snapshot objects (only for the caller's own unexpired `building` claim) ----
create or replace function public.synchrome_snapshot_object_ok(object_name text)
returns boolean
language sql
stable
security invoker
set search_path = ''
as $$
  select object_name ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/profiles/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/snapshots/[0-9]{10}/(manifest\.json|[a-z][a-z_]{0,39}-[0-9]{4}\.jsonl)$'
     and split_part(object_name, '/', 1) = (select auth.uid())::text
     and exists (select 1 from public.profile_snapshots s
                  where s.account_id = (select auth.uid())
                    and s.profile_id::text = split_part(object_name, '/', 3)
                    and s.generation = split_part(object_name, '/', 5)::bigint
                    and s.state = 'building'
                    and s.expires_at > now());
$$;

comment on function public.synchrome_snapshot_object_ok(text) is
  'Synchrome: true if a Storage object name is a part/manifest of a snapshot generation the caller has claimed (building, unexpired).';

revoke all on function public.synchrome_snapshot_object_ok(text) from public, anon;
grant execute on function public.synchrome_snapshot_object_ok(text) to authenticated;

drop policy if exists synchrome_stores_insert on storage.objects;
create policy synchrome_stores_insert on storage.objects
  for insert to authenticated
  with check (bucket_id = 'stores'
              and (public.synchrome_journal_object_ok(name)
                   or public.synchrome_snapshot_object_ok(name)));

drop policy if exists synchrome_stores_update on storage.objects;
create policy synchrome_stores_update on storage.objects
  for update to authenticated
  using (bucket_id = 'stores'
         and (storage.foldername(name))[1] = (select auth.uid())::text)
  with check (bucket_id = 'stores'
              and (public.synchrome_journal_object_ok(name)
                   or public.synchrome_snapshot_object_ok(name)));
-- select / delete policies from 0004 are unchanged (own prefix only)

-- 4. Realtime: store_events INSERTs as wake-up hints ------------------------------
do $$
begin
  if exists (select 1 from pg_publication where pubname = 'supabase_realtime') then
    if not exists (select 1 from pg_publication_tables
                    where pubname = 'supabase_realtime'
                      and schemaname = 'public' and tablename = 'store_events') then
      execute 'alter publication supabase_realtime add table public.store_events';
    end if;
  else
    raise notice 'publication supabase_realtime not found: Realtime wake-up unavailable, polling keeps working.';
  end if;
end $$;

commit;

-- End of 0005_compaction_realtime.sql
