-- ============================================================================
-- Synchrome — rollback of 0005_compaction_realtime.sql
-- ----------------------------------------------------------------------------
-- Restores exactly what 0004 defined: the generic owner DELETE policy on public.store_events,
-- the storage.objects INSERT / UPDATE policies, no restrictive DELETE policy; removes
-- store_events from the Realtime publication only if it is an explicit member; drops the 0005
-- tables and functions. Idempotent (safe to run again).
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

-- 5. Realtime: drop only an explicit membership (never from a FOR ALL TABLES publication)
do $$
begin
  if exists (select 1
               from pg_publication p
               join pg_publication_rel r on r.prpubid = p.oid
              where p.pubname = 'supabase_realtime'
                and not p.puballtables
                and r.prrelid = 'public.store_events'::regclass) then
    execute 'alter publication supabase_realtime drop table public.store_events';
  else
    raise notice 'store_events is not an explicit member of supabase_realtime; nothing to drop.';
  end if;
end $$;

-- 4. Storage policies back to 0004
drop policy if exists synchrome_stores_delete_guard on storage.objects;
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

-- 3. store_events: back to 0004's owner DELETE policy
drop policy if exists store_events_delete_gc on public.store_events;
drop policy if exists store_events_delete on public.store_events;
create policy store_events_delete on public.store_events
  for delete to authenticated using (account_id = auth.uid());

drop function if exists public.synchrome_object_delete_ok(text);
drop function if exists public.synchrome_snapshot_object_ok(text);
drop function if exists public.synchrome_snapshot_object_protected(text);
drop function if exists public.synchrome_gc_journal_events(uuid, uuid, bigint);
drop function if exists public.synchrome_journal_gc_allowed(uuid, uuid, bigint);
drop function if exists public.synchrome_compaction_status(uuid);
drop function if exists public.synchrome_gc_blockers(uuid);

-- 2. profile_snapshots (only `building` claims can exist at this point)
drop table if exists public.profile_snapshots;   -- trigger, policies, index go with it
drop function if exists public.synchrome_profile_snapshots_guard();

-- 1. sync_cursors
drop table if exists public.sync_cursors;
drop function if exists public.synchrome_sync_cursors_touch();

commit;

-- End of 0005_compaction_realtime.rollback.sql
