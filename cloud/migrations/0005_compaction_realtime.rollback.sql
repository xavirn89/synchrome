-- ============================================================================
-- Synchrome — rollback of 0005_compaction_realtime.sql
-- ----------------------------------------------------------------------------
-- Restores exactly what 0004 defined for storage.objects INSERT / UPDATE, removes the
-- RESTRICTIVE delete policy, removes store_events from the Realtime publication, and drops the
-- 0005 tables and functions.
--
-- Refuses (and changes nothing) while any snapshot generation is `ready` or `superseded`:
-- once a compaction ran, folded journals may already be deleted and profile_snapshots is the
-- only pointer to that history. Snapshot OBJECTS in Storage are never deleted by SQL (delete
-- them with the Storage API; see README §0005).
-- ============================================================================

begin;

do $$
begin
  if to_regclass('public.profile_snapshots') is not null
     and exists (select 1 from public.profile_snapshots where state in ('ready', 'superseded')) then
    raise exception 'snapshot generations exist (ready/superseded): rolling back would orphan compacted history. Aborting; nothing changed.';
  end if;
end $$;

-- 5. Realtime
do $$
begin
  if exists (select 1 from pg_publication_tables
              where pubname = 'supabase_realtime'
                and schemaname = 'public' and tablename = 'store_events') then
    execute 'alter publication supabase_realtime drop table public.store_events';
  end if;
end $$;

-- 4. Storage policies back to 0004
drop policy if exists synchrome_stores_delete_keep_ready on storage.objects;

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
drop function if exists public.synchrome_snapshot_object_protected(text);

-- 3. status function
drop function if exists public.synchrome_compaction_status(uuid);

-- 2. profile_snapshots (only `building` claims can exist at this point)
drop table if exists public.profile_snapshots;   -- trigger, policies, index go with it
drop function if exists public.synchrome_profile_snapshots_guard();

-- 1. sync_cursors
drop table if exists public.sync_cursors;
drop function if exists public.synchrome_sync_cursors_touch();

commit;

-- End of 0005_compaction_realtime.rollback.sql
