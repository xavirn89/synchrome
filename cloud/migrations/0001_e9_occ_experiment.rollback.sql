-- ============================================================================
-- Synchrome — E9 experiment ROLLBACK for 0001_e9_occ_experiment.sql
-- ----------------------------------------------------------------------------
-- Removes every object the E9 migration created. Idempotent and safe to re-run.
-- By default it also deletes the experimental bucket and its objects; to keep
-- the bucket, comment out section 4.
-- Run only against the Synchrome Supabase database.
-- ============================================================================

begin;

-- 1. Storage policies ---------------------------------------------------------
drop policy if exists e9_storage_select on storage.objects;
drop policy if exists e9_storage_insert on storage.objects;
drop policy if exists e9_storage_update on storage.objects;
drop policy if exists e9_storage_delete on storage.objects;

-- 2. OCC function -------------------------------------------------------------
drop function if exists public.e9_cas_put(uuid, text, bigint, text, uuid);

-- 3. OCC table and its policies (policies drop with the table) ----------------
drop table if exists public.e9_objects;

-- 4. Experimental bucket and its objects (comment out to keep them) -----------
delete from storage.objects where bucket_id = 'e9-experiments';
delete from storage.buckets where id = 'e9-experiments';

commit;

-- End of 0001_e9_occ_experiment.rollback.sql
