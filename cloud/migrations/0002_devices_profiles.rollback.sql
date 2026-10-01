-- ============================================================================
-- Synchrome — ROLLBACK for 0002_devices_profiles.sql
-- ----------------------------------------------------------------------------
-- Drops the production devices/profiles tables and the shared updated_at trigger function.
-- DESTRUCTIVE: all rows in public.devices and public.profiles are lost. Take a backup first.
-- Idempotent. Does not touch e9_* experimental objects.
-- ============================================================================

begin;

drop table if exists public.devices;   -- policies, triggers and indexes drop with the table
drop table if exists public.profiles;

drop function if exists public.synchrome_set_updated_at();

commit;

-- End of 0002_devices_profiles.rollback.sql
