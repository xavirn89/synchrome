-- ============================================================================
-- Synchrome — ROLLBACK for 0004_store_objects.sql
-- ----------------------------------------------------------------------------
-- DESTRUCTIVE for metadata: drops public.store_events and the `stores` bucket policies and
-- name-check function. The bucket itself is removed only if it is EMPTY: empty it first
-- through the Storage API (or the dashboard) so the underlying files are deleted too —
-- deleting rows of storage.objects directly would orphan the stored files. If the bucket
-- still has objects this script keeps it and raises a NOTICE. Idempotent.
-- Leaves 0002/0003 objects intact.
-- ============================================================================

begin;

drop policy if exists synchrome_stores_select on storage.objects;
drop policy if exists synchrome_stores_insert on storage.objects;
drop policy if exists synchrome_stores_update on storage.objects;
drop policy if exists synchrome_stores_delete on storage.objects;

drop function if exists public.synchrome_journal_object_ok(text);

drop table if exists public.store_events;   -- policies, indexes, FKs go with it

do $$
begin
  if exists (select 1 from storage.objects where bucket_id = 'stores') then
    raise notice 'bucket "stores" is not empty: kept. Empty it via the Storage API, then re-run.';
  else
    delete from storage.buckets where id = 'stores';
  end if;
end $$;

commit;

-- End of 0004_store_objects.rollback.sql
