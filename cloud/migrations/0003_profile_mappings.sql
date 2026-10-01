-- ============================================================================
-- Synchrome — 0003: profile_mappings (local browser profile <-> cloud profile, per device)
-- ----------------------------------------------------------------------------
-- A mapping binds ONE local Chromium profile (device + browser + profile directory) to ONE
-- Synchrome cloud profile of the same account, and carries the random `mapping_token` the app
-- will later seed into the Synchrome extension's `Local Extension Settings/<ext-id>` store
-- (finding E8) so the extension can identify which mapping it belongs to.
--
-- Design decisions (see cloud/migrations/README.md §0003):
--   * Ownership: account_id = auth.uid(), RLS enabled AND forced.
--   * Cross-account safety: foreign keys are COMPOSITE (account_id, profile_id) and
--     (account_id, device_id). Postgres FK checks bypass RLS, so a single-column FK would let
--     a user reference another account's profile/device by UUID; the composite FK makes that
--     impossible because account_id is pinned to auth.uid().
--   * Deleting a cloud profile that is still mapped is REJECTED (ON DELETE RESTRICT):
--     the user must unmap first, so no device silently loses its link.
--   * Deleting a device row CASCADES to its mappings (mappings are device-local state).
--   * Uniqueness:
--       - a local profile (device, browser, profile dir) is mapped at most once;
--       - a cloud profile MAY be mapped to several local profiles, on the same device
--         (e.g. Helium/Profile 1 and Chrome/Profile 3) and on other devices;
--       - mapping_token is globally unique (64 lowercase hex chars, 256 bits).
--   * Known debt (not changed here): public.devices.device_id is a global PK, so a second
--     account on the same installation collides; see cloud/migrations/README.md §0003.
--   * Column-level grants: clients may insert only the identifying columns and update only
--     the token state; they can never set account_id or rewrite a mapping's identity/token.
--
-- Requires 0002 (public.devices, public.profiles, public.synchrome_set_updated_at).
-- Idempotent, explicit, reversible (0003_profile_mappings.rollback.sql), Supabase-guarded.
-- ============================================================================

begin;

-- 0. Safety guards ------------------------------------------------------------
do $$
begin
  if to_regprocedure('auth.uid()') is null then
    raise exception 'auth.uid() not found: not a Supabase database. Aborting 0003.';
  end if;
  if to_regclass('public.devices') is null or to_regclass('public.profiles') is null then
    raise exception 'public.devices / public.profiles missing: apply 0002 first. Aborting 0003.';
  end if;
end $$;

-- 1. Composite-unique targets on the parent tables (needed by the composite FKs) -------
do $$
begin
  if not exists (select 1 from pg_constraint where conname = 'profiles_account_id_id_key') then
    alter table public.profiles add constraint profiles_account_id_id_key unique (account_id, id);
  end if;
  if not exists (select 1 from pg_constraint where conname = 'devices_account_id_device_id_key') then
    alter table public.devices add constraint devices_account_id_device_id_key unique (account_id, device_id);
  end if;
end $$;

-- 2. profile_mappings ---------------------------------------------------------
create table if not exists public.profile_mappings (
  id                  uuid        primary key default gen_random_uuid(),
  account_id          uuid        not null default auth.uid(),
  profile_id          uuid        not null,
  device_id           uuid        not null,
  browser_kind        text        not null,
  browser_profile_dir text        not null,
  mapping_token       text        not null,
  token_state         text        not null default 'pending_local_write',
  token_confirmed_at  timestamptz,
  created_at          timestamptz not null default now(),
  updated_at          timestamptz not null default now(),

  constraint profile_mappings_profile_fk
    foreign key (account_id, profile_id)
    references public.profiles (account_id, id)
    on delete restrict,
  constraint profile_mappings_device_fk
    foreign key (account_id, device_id)
    references public.devices (account_id, device_id)
    on delete cascade,

  constraint profile_mappings_local_unique
    unique (device_id, browser_kind, browser_profile_dir),
  constraint profile_mappings_token_unique
    unique (mapping_token),

  constraint profile_mappings_browser_kind_chk
    check (browser_kind in ('helium', 'chrome', 'brave', 'edge', 'chromium')),
  constraint profile_mappings_dir_chk
    check (char_length(browser_profile_dir) between 1 and 255
           and browser_profile_dir !~ '[/\\]'
           and browser_profile_dir not in ('.', '..')),
  constraint profile_mappings_token_chk
    check (mapping_token ~ '^[0-9a-f]{64}$'),
  constraint profile_mappings_token_state_chk
    check (token_state in ('pending_local_write', 'confirmed')),
  constraint profile_mappings_confirmed_at_chk
    check ((token_state = 'confirmed') = (token_confirmed_at is not null))
);

comment on table public.profile_mappings is
  'Synchrome: binds a local browser profile (device, browser, dir) to a cloud profile. Owner-scoped via RLS.';
comment on column public.profile_mappings.mapping_token is
  'Random 256-bit hex token seeded into the extension storage (E8). Sensitive identifier, not an account credential.';

create index if not exists profile_mappings_account_id_idx      on public.profile_mappings (account_id);
create index if not exists profile_mappings_account_profile_idx on public.profile_mappings (account_id, profile_id);
create index if not exists profile_mappings_account_device_idx  on public.profile_mappings (account_id, device_id);

drop trigger if exists profile_mappings_set_updated_at on public.profile_mappings;
create trigger profile_mappings_set_updated_at
  before update on public.profile_mappings
  for each row execute function public.synchrome_set_updated_at();

-- 3. RLS ----------------------------------------------------------------------
alter table public.profile_mappings enable row level security;
alter table public.profile_mappings force row level security;

drop policy if exists profile_mappings_select on public.profile_mappings;
create policy profile_mappings_select on public.profile_mappings
  for select to authenticated using (account_id = auth.uid());

drop policy if exists profile_mappings_insert on public.profile_mappings;
create policy profile_mappings_insert on public.profile_mappings
  for insert to authenticated with check (account_id = auth.uid());

drop policy if exists profile_mappings_update on public.profile_mappings;
create policy profile_mappings_update on public.profile_mappings
  for update to authenticated
  using (account_id = auth.uid()) with check (account_id = auth.uid());

drop policy if exists profile_mappings_delete on public.profile_mappings;
create policy profile_mappings_delete on public.profile_mappings
  for delete to authenticated using (account_id = auth.uid());

-- 4. Minimal privileges (column-level for insert/update) ----------------------
revoke all on public.profile_mappings from anon, authenticated;
grant select, delete on public.profile_mappings to authenticated;
grant insert (profile_id, device_id, browser_kind, browser_profile_dir, mapping_token)
  on public.profile_mappings to authenticated;
grant update (token_state, token_confirmed_at)
  on public.profile_mappings to authenticated;
-- anon gets nothing.

-- 5. Hardening for the 0002 tables -----------------------------------------------
-- Defense in depth, independent of instance defaults: some Supabase setups grant ALL on new
-- public tables to `anon` via default privileges. On this instance anon already gets 401 on
-- /devices (verified before 0003), and RLS policies are TO authenticated anyway; revoking
-- explicitly keeps the posture correct regardless. `authenticated` keeps the CRUD it needs;
-- service_role is untouched. Idempotent.
revoke all on public.devices  from anon;
revoke all on public.profiles from anon;

commit;

-- End of 0003_profile_mappings.sql
