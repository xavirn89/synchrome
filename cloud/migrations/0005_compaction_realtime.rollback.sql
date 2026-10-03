-- ============================================================================
-- Synchrome — rollback of 0005_compaction_realtime.sql
-- ----------------------------------------------------------------------------
-- Restores exactly what 0004 defined for storage.objects INSERT / UPDATE, removes
-- store_events from the Realtime publication, and drops the 0005 tables and functions.
--
-- Snapshot OBJECTS in Storage are not deleted here (SQL must not touch storage internals).
-- Before rolling back, make sure no profile depends on a snapshot: if journals were already
-- compacted (folded + deleted), dropping profile_snapshots removes the only pointer to the
-- folded history. The README §0005 describes the check; run this only on an instance where
-- no compaction ran, or after the objects were re-expanded.
-- ============================================================================

begin;

do $$
begin
  if to_regclass('public.profile_snapshots') is not null
     and exists (select 1 from public.profile_snapshots where state = 'ready') then
    raise exception 'ready snapshots exist: rolling back would orphan compacted history. Aborting.';
  end if;
end $$;

-- 4. Realtime
do $$
begin
  if exists (select 1 from pg_publication_tables
              where pubname = 'supabase_realtime'
                and schemaname = 'public' and tablename = 'store_events') then
    execute 'alter publication supabase_realtime drop table public.store_events';
  end if;
end $$;

-- 3. Storage policies back to 0004
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

drop function if exists public.synchrome_snapshot_object_ok(text);

-- 2. profile_snapshots
drop table if exists public.profile_snapshots;   -- trigger, policies, index go with it
drop function if exists public.synchrome_profile_snapshots_guard();

-- 1. sync_cursors
drop table if exists public.sync_cursors;
drop function if exists public.synchrome_sync_cursors_touch();

commit;

-- End of 0005_compaction_realtime.rollback.sql
