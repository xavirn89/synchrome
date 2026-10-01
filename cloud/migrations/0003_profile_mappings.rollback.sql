-- ============================================================================
-- Synchrome — ROLLBACK for 0003_profile_mappings.sql
-- ----------------------------------------------------------------------------
-- DESTRUCTIVE: drops public.profile_mappings (all local<->cloud links and mapping tokens are
-- lost) and the composite-unique constraints 0003 added to public.profiles / public.devices.
-- Leaves 0002 objects (devices, profiles, trigger function) intact. Idempotent.
-- ============================================================================

begin;

drop table if exists public.profile_mappings;   -- policies, trigger, indexes, FKs go with it

alter table if exists public.profiles drop constraint if exists profiles_account_id_id_key;
alter table if exists public.devices  drop constraint if exists devices_account_id_device_id_key;

commit;

-- End of 0003_profile_mappings.rollback.sql
