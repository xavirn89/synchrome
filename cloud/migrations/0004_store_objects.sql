-- ============================================================================
-- Synchrome — 0004: store objects (Storage bucket `stores`) + public.store_events
-- ----------------------------------------------------------------------------
-- First production data path (M1 slice 7, bookmarks upload-only). Per docs/03 §4 and
-- docs/04 §3, synced data lives as OBJECTS in Supabase Storage, Postgres keeps metadata:
--
--   bucket `stores` (private), object names:
--     <accountId>/profiles/<profileId>/journal/<deviceId>/<yyyy>/<mm>/<seq:08>.jsonl
--   (the Synchrome Cloud store root is the account prefix; docs/04 §3)
--
--   public.store_events: one row per journal publication (profile, device, path, seq range,
--   HLC). Unique per (profile_id, device_id, seq_to) so a retried publication is a no-op.
--   It is the hook Realtime will use later (not added to the publication here).
--
-- Design decisions (see cloud/migrations/README.md §0004):
--   * Storage RLS (storage.objects): an authenticated user may READ / DELETE only objects
--     whose first path segment is their own auth.uid(); may INSERT / UPDATE (upsert) only
--     well-formed JOURNAL names whose profile and device rows belong to them. Nothing else
--     can be written yet (snapshots, profile.json, cursors come with later migrations).
--   * The name check is a SECURITY INVOKER function, so its lookups into public.profiles /
--     public.devices are themselves subject to RLS: another account's ids are invisible.
--   * store_events: account_id = auth.uid() (RLS enabled + forced), composite FKs
--     (account_id, profile_id) / (account_id, device_id) like 0003 because FK checks bypass
--     RLS; ON DELETE CASCADE for both (events are derived metadata). Column-level insert
--     grant; no UPDATE at all (append-only).
--   * Deleting a profile row does NOT delete its Storage objects: the app deletes the
--     profile's objects first (Storage API), then the row. Documented in README §0004.
--   * The service role is never used by the app; nothing here grants anything to anon.
--
-- Requires 0002 + 0003 (public.devices, public.profiles and their composite-unique keys).
-- Idempotent, explicit, reversible (0004_store_objects.rollback.sql), Supabase-guarded.
-- ============================================================================

begin;

-- 0. Safety guards ------------------------------------------------------------
do $$
begin
  if to_regprocedure('auth.uid()') is null then
    raise exception 'auth.uid() not found: not a Supabase database. Aborting 0004.';
  end if;
  if to_regclass('storage.objects') is null or to_regclass('storage.buckets') is null then
    raise exception 'storage schema missing: Supabase Storage not installed. Aborting 0004.';
  end if;
  if to_regprocedure('storage.foldername(text)') is null then
    raise exception 'storage.foldername(text) missing: unexpected Storage version. Aborting 0004.';
  end if;
  if not exists (select 1 from pg_constraint where conname = 'profiles_account_id_id_key')
     or not exists (select 1 from pg_constraint where conname = 'devices_account_id_device_id_key') then
    raise exception 'composite keys from 0003 missing: apply 0003 first. Aborting 0004.';
  end if;
end $$;

-- 1. Private bucket -----------------------------------------------------------
-- 4 MiB per object (journal chunks rotate at 256 KiB; headroom for future snapshots of
-- large bookmark trees). JSON / NDJSON only.
insert into storage.buckets (id, name, public, file_size_limit, allowed_mime_types)
values ('stores', 'stores', false, 4194304, array['application/x-ndjson', 'application/json'])
on conflict (id) do update
  set public             = false,
      file_size_limit    = excluded.file_size_limit,
      allowed_mime_types = excluded.allowed_mime_types;

-- 2. Journal object-name check (SECURITY INVOKER: lookups obey the caller's RLS) -------
create or replace function public.synchrome_journal_object_ok(object_name text)
returns boolean
language sql
stable
security invoker
set search_path = ''
as $$
  select object_name ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/profiles/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/journal/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/[0-9]{4}/[0-9]{2}/[0-9]{8,12}\.jsonl$'
     and split_part(object_name, '/', 1) = (select auth.uid())::text
     and exists (select 1 from public.profiles p
                  where p.account_id = (select auth.uid())
                    and p.id::text = split_part(object_name, '/', 3))
     and exists (select 1 from public.devices d
                  where d.account_id = (select auth.uid())
                    and d.device_id::text = split_part(object_name, '/', 5));
$$;

comment on function public.synchrome_journal_object_ok(text) is
  'Synchrome: true if a Storage object name is a journal chunk of the caller''s own profile and device (used by storage.objects policies).';

revoke all on function public.synchrome_journal_object_ok(text) from public, anon;
grant execute on function public.synchrome_journal_object_ok(text) to authenticated;

-- 3. storage.objects policies for bucket `stores` -------------------------------
drop policy if exists synchrome_stores_select on storage.objects;
create policy synchrome_stores_select on storage.objects
  for select to authenticated
  using (bucket_id = 'stores'
         and (storage.foldername(name))[1] = (select auth.uid())::text);

drop policy if exists synchrome_stores_insert on storage.objects;
create policy synchrome_stores_insert on storage.objects
  for insert to authenticated
  with check (bucket_id = 'stores'
              and public.synchrome_journal_object_ok(name));

drop policy if exists synchrome_stores_update on storage.objects;
create policy synchrome_stores_update on storage.objects
  for update to authenticated
  using (bucket_id = 'stores'
         and (storage.foldername(name))[1] = (select auth.uid())::text)
  with check (bucket_id = 'stores'
              and public.synchrome_journal_object_ok(name));

drop policy if exists synchrome_stores_delete on storage.objects;
create policy synchrome_stores_delete on storage.objects
  for delete to authenticated
  using (bucket_id = 'stores'
         and (storage.foldername(name))[1] = (select auth.uid())::text);

-- 4. store_events -------------------------------------------------------------
create table if not exists public.store_events (
  id          bigint      generated always as identity primary key,
  account_id  uuid        not null default auth.uid(),
  profile_id  uuid        not null,
  device_id   uuid        not null,
  path        text        not null,
  seq_from    bigint      not null,
  seq_to      bigint      not null,
  hlc         text        not null,
  created_at  timestamptz not null default now(),

  constraint store_events_profile_fk
    foreign key (account_id, profile_id)
    references public.profiles (account_id, id)
    on delete cascade,
  constraint store_events_device_fk
    foreign key (account_id, device_id)
    references public.devices (account_id, device_id)
    on delete cascade,

  constraint store_events_publication_unique
    unique (profile_id, device_id, seq_to),

  constraint store_events_seq_chk
    check (seq_from >= 1 and seq_to >= seq_from),
  constraint store_events_path_chk
    check (char_length(path) <= 255
           and path ~ '^profiles/[0-9a-f-]{36}/journal/[0-9a-f-]{36}/[0-9]{4}/[0-9]{2}/[0-9]{8,12}\.jsonl$'
           and split_part(path, '/', 2) = profile_id::text
           and split_part(path, '/', 4) = device_id::text),
  constraint store_events_hlc_chk
    check (hlc ~ '^[0-9]{13}-[0-9]{4}-[0-9a-f-]{36}$')
);

comment on table public.store_events is
  'Synchrome: append-only log of journal publications (which object, which seq range, HLC). Owner-scoped via RLS.';

create index if not exists store_events_account_profile_idx
  on public.store_events (account_id, profile_id, seq_to);

alter table public.store_events enable row level security;
alter table public.store_events force row level security;

drop policy if exists store_events_select on public.store_events;
create policy store_events_select on public.store_events
  for select to authenticated using (account_id = auth.uid());

drop policy if exists store_events_insert on public.store_events;
create policy store_events_insert on public.store_events
  for insert to authenticated with check (account_id = auth.uid());

drop policy if exists store_events_delete on public.store_events;
create policy store_events_delete on public.store_events
  for delete to authenticated using (account_id = auth.uid());

revoke all on public.store_events from anon, authenticated;
grant select, delete on public.store_events to authenticated;
grant insert (profile_id, device_id, path, seq_from, seq_to, hlc)
  on public.store_events to authenticated;
-- no UPDATE: append-only. anon gets nothing.

commit;

-- End of 0004_store_objects.sql
