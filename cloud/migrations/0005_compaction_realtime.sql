-- ============================================================================
-- Synchrome — 0005: journal compaction metadata + guarded journal GC + Realtime wake-up
-- ----------------------------------------------------------------------------
-- M1 slice 13 (docs/03 §3.7). Changes relative to 0004:
--
--   public.sync_cursors       NEW. One row per (profile, reader device): applied cursors,
--                             own published seq, snapshot generation in use, the snapshot
--                             protocol the device supports (`snapshot_protocol`, 0 = none),
--                             `updated_at` (last report, SERVER time) and `retired_at` (SERVER
--                             time, set only through an explicit user action in the app).
--
--   public.profile_snapshots  NEW. Snapshot generations (building -> ready -> superseded); the
--                             unique (profile_id, generation) is the compaction lock. On
--                             building -> ready the SERVER validates the watermarks: integers,
--                             monotonic w.r.t. the previous ready generation, each one on a
--                             store_events boundary with a contiguous chain of events since the
--                             previous watermark, and already applied by every active reader
--                             that supports snapshots. Current + previous ready generations are
--                             always kept.
--
--   journal GC                The generic DELETE on public.store_events from 0004 is REPLACED:
--                             a row may be deleted only if synchrome_journal_gc_allowed() says
--                             so (own account; seq_to <= the PREVIOUS ready generation's
--                             watermark for that source device; 10 min after the current
--                             generation's server ready_at; no blocking device). Clients use
--                             the RPC synchrome_gc_journal_events(); a raw DELETE can only ever
--                             remove the same eligible rows. FK cascades (profile / device
--                             deletion) are unaffected (referential actions bypass RLS).
--
--   GC blockers               Every device mapped to the profile (profile_mappings) must have a
--                             sync_cursors row with snapshot_protocol >= 1, or be explicitly
--                             retired; otherwise journal GC is refused (clients that cannot read
--                             snapshots never lose history they still need).
--
--   storage.objects           INSERT / UPDATE also accept snapshot objects of the caller's own
--                             unexpired `building` claim. A RESTRICTIVE DELETE policy forbids
--                             deleting objects of a `ready` generation and journal chunks that
--                             journal GC does not allow (unless their profile no longer exists).
--
--   Realtime                  public.store_events joins the `supabase_realtime` publication
--                             (wake-up hint only), idempotently.
--
-- Constants (docs/03 §3.7): claim TTL 30 min; journal GC 10 min after ready_at; generation
-- stability 24 h; one claim per 6 h per profile; active reader = report within 30 days;
-- required snapshot protocol = 1. All time comparisons use the database clock.
--
-- Owner-scoped (account_id = auth.uid(), RLS enabled + forced), composite FKs, column grants,
-- SECURITY INVOKER functions only, nothing for anon, no service role. Nothing here deletes data.
--
-- Requires 0002 + 0003 + 0004. Idempotent (safe to run again), explicit, reversible
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
  if to_regclass('public.profile_mappings') is null
     or not exists (select 1 from pg_constraint where conname = 'profiles_account_id_id_key')
     or not exists (select 1 from pg_constraint where conname = 'devices_account_id_device_id_key') then
    raise exception 'objects from 0003 missing. Aborting 0005.';
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
  snapshot_protocol    integer     not null default 0,  -- 0 = cannot read snapshots; 1 = synchrome-snapshot/1
  updated_at           timestamptz not null default now(),   -- last report, server time (trigger)
  retired_at           timestamptz,                         -- explicit user action, server time (trigger)

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
    check (snapshot_generation is null or snapshot_generation >= 1),
  constraint sync_cursors_protocol_chk
    check (snapshot_protocol between 0 and 1000)
);

comment on table public.sync_cursors is
  'Synchrome: per (profile, reader device) applied cursors, snapshot protocol support, last report (server time) and explicit retirement; input of compaction (docs/03 §3.7).';
comment on column public.sync_cursors.retired_at is
  'Set only by an explicit user action in the app ("remove this device"); stamped with server time; never set by age.';
comment on column public.sync_cursors.snapshot_protocol is
  'Highest snapshot protocol the device can bootstrap from (0 = none). Journal GC requires >= 1 for every mapped, non-retired device.';

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
     or new.snapshot_generation is distinct from old.snapshot_generation
     or new.snapshot_protocol is distinct from old.snapshot_protocol then
    new.updated_at := now();
  else
    new.updated_at := old.updated_at;          -- retiring / un-retiring is not activity
  end if;
  if new.retired_at is not null and (tg_op = 'INSERT' or old.retired_at is null) then
    new.retired_at := now();                   -- whatever the client sent
  elsif tg_op = 'UPDATE' and new.retired_at is not null then
    new.retired_at := old.retired_at;          -- an existing retirement keeps its time
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
grant insert (profile_id, device_id, applied, published_through, snapshot_generation, snapshot_protocol, retired_at)
  on public.sync_cursors to authenticated;
grant update (applied, published_through, snapshot_generation, snapshot_protocol, retired_at)
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
  claimed_at       timestamptz not null default now(),         -- server time (trigger)
  expires_at       timestamptz not null default now() + interval '30 minutes',
  ready_at         timestamptz,                                -- server time (trigger)

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
  'Synchrome: compacted snapshot generations of a profile (building -> ready -> superseded); unique generation = compaction lock; current + previous ready generations always kept (docs/03 §3.7).';

create index if not exists profile_snapshots_ready_idx
  on public.profile_snapshots (account_id, profile_id, generation desc) where state = 'ready';

-- the server owns timestamps, the state machine and the validity of watermarks
create or replace function public.synchrome_profile_snapshots_guard()
returns trigger
language plpgsql
security invoker
set search_path = ''
as $$
declare
  latest      bigint;
  newer       bigint;
  current_at  timestamptz;
  prev_wm     jsonb;
  rec         record;
  w           bigint;
  lo          bigint;
  at_w        integer;
  breaks      integer;
  straddle    integer;
  min_applied bigint;
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

  if old.state = 'building' and new.state = 'building' then
    new.ready_at := null;                     -- builder progress (counts/bytes)
    return new;
  end if;

  if old.state = 'building' and new.state = 'ready' then
    if old.expires_at <= now() then
      raise exception 'snapshot claim expired' using errcode = '23514';
    end if;
    select s.watermarks into prev_wm
      from public.profile_snapshots s
     where s.profile_id = old.profile_id and s.state = 'ready' and s.generation < old.generation
     order by s.generation desc limit 1;
    prev_wm := coalesce(prev_wm, '{}'::jsonb);
    -- shape: {"<uuid>": non-negative integer}
    for rec in select key, value from jsonb_each(new.watermarks) loop
      if rec.key !~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
         or jsonb_typeof(rec.value) <> 'number' or rec.value::text !~ '^[0-9]{1,15}$' then
        raise exception 'invalid watermark entry' using errcode = '23514';
      end if;
    end loop;
    -- monotonic: no device of the previous generation dropped or lowered
    for rec in select key, value from jsonb_each(prev_wm) loop
      if not (new.watermarks ? rec.key)
         or (new.watermarks ->> rec.key)::bigint < (rec.value::text)::bigint then
        raise exception 'watermark of % lower than in the previous generation', rec.key using errcode = '23514';
      end if;
    end loop;
    for rec in select key, value from jsonb_each(new.watermarks) loop
      w := (rec.value::text)::bigint;
      lo := coalesce((prev_wm ->> rec.key)::bigint, 0);
      if w > lo then
        -- a contiguous chain of publications (lo, w], ending exactly at w, none crossing lo or w
        select count(*) filter (where e.seq_to = w) into at_w
          from public.store_events e
         where e.profile_id = old.profile_id and e.device_id::text = rec.key
           and e.seq_from > lo and e.seq_to <= w;
        select count(*) into breaks
          from (select e.seq_from, lag(e.seq_to) over (order by e.seq_to) as prev_to
                  from public.store_events e
                 where e.profile_id = old.profile_id and e.device_id::text = rec.key
                   and e.seq_from > lo and e.seq_to <= w) t
         where t.seq_from <> coalesce(t.prev_to, lo) + 1;
        select count(*) into straddle
          from public.store_events e
         where e.profile_id = old.profile_id and e.device_id::text = rec.key
           and ((e.seq_from <= lo and e.seq_to > lo) or (e.seq_from <= w and e.seq_to > w));
        if at_w <> 1 or breaks <> 0 or straddle <> 0 then
          raise exception 'watermark % of % is not the end of a contiguous chain of publications', w, rec.key
            using errcode = '23514';
        end if;
        -- already applied by every active reader that supports snapshots
        select min(coalesce((c.applied ->> rec.key)::bigint, 0)) into min_applied
          from public.sync_cursors c
         where c.profile_id = old.profile_id and c.device_id::text <> rec.key
           and c.retired_at is null and c.snapshot_protocol >= 1
           and c.updated_at >= now() - interval '30 days';
        if min_applied is not null and w > min_applied then
          raise exception 'watermark % of % not yet applied by every active reader', w, rec.key
            using errcode = '23514';
        end if;
      end if;
    end loop;
    new.ready_at := now();
    return new;
  end if;

  if old.state = 'ready' and new.state = 'superseded' then
    if new.manifest_path is distinct from old.manifest_path
       or new.manifest_sha256 is distinct from old.manifest_sha256
       or new.watermarks is distinct from old.watermarks
       or new.counts is distinct from old.counts or new.bytes <> old.bytes then
      raise exception 'a ready snapshot is immutable' using errcode = '23514';
    end if;
    -- keep current + previous: two newer ready generations, the newest ready for >= 24 h
    select count(*) into newer
      from public.profile_snapshots s
     where s.profile_id = old.profile_id and s.state = 'ready' and s.generation > old.generation;
    select s.ready_at into current_at
      from public.profile_snapshots s
     where s.profile_id = old.profile_id and s.state = 'ready'
     order by s.generation desc limit 1;
    if newer < 2 or current_at is null or current_at > now() - interval '24 hours' then
      raise exception 'generation % is still current/previous or the current one is not stable yet', old.generation
        using errcode = '23514';
    end if;
    new.ready_at := old.ready_at;
    return new;
  end if;

  raise exception 'invalid snapshot transition % -> %', old.state, new.state using errcode = '23514';
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
-- rows go away only when superseded, as an expired claim, or with the profile (FK cascade)
drop policy if exists profile_snapshots_delete on public.profile_snapshots;
create policy profile_snapshots_delete on public.profile_snapshots
  for delete to authenticated
  using (account_id = (select auth.uid())
         and (state = 'superseded' or (state = 'building' and expires_at <= now())));

revoke all on public.profile_snapshots from anon, authenticated;
grant select, delete on public.profile_snapshots to authenticated;
grant insert (profile_id, generation, created_by) on public.profile_snapshots to authenticated;
grant update (state, manifest_path, manifest_sha256, watermarks, counts, bytes)
  on public.profile_snapshots to authenticated;

-- 3. Decisions with the database clock + GC blockers ---------------------------------
-- devices mapped to the profile that cannot read snapshots and are not explicitly retired
create or replace function public.synchrome_gc_blockers(p_profile uuid)
returns integer
language sql
stable
security invoker
set search_path = ''
as $$
  select count(distinct m.device_id)::integer
    from public.profile_mappings m
   where m.profile_id = p_profile
     and not exists (select 1 from public.sync_cursors c
                      where c.profile_id = p_profile and c.device_id = m.device_id
                        and (c.retired_at is not null or c.snapshot_protocol >= 1));
$$;

create or replace function public.synchrome_compaction_status(p_profile uuid)
returns table (
  server_now              timestamptz,
  current_generation      bigint,
  current_ready_at        timestamptz,
  previous_generation     bigint,
  journal_gc_watermarks   jsonb,       -- W(previous): GC runs one generation behind; null = none
  gc_blockers             integer,     -- mapped devices without snapshot support (and not retired)
  journal_gc_ok           boolean,     -- previous exists, now() >= ready_at(current) + 10 min, no blockers
  generation_cleanup_ok   boolean,     -- current ready for >= 24 h
  last_claimed_at         timestamptz,
  claim_allowed           boolean      -- no live claim and >= 6 h since the last claim
)
language sql
stable
security invoker
set search_path = ''
as $$
  with ready as (
    select s.generation, s.ready_at, s.watermarks,
           row_number() over (order by s.generation desc) as rk
      from public.profile_snapshots s
     where s.profile_id = p_profile and s.state = 'ready'
  ), cur as (select * from ready where rk = 1),
     prev as (select * from ready where rk = 2),
     claims as (
    select max(s.claimed_at) as last_claimed,
           bool_or(s.state = 'building' and s.expires_at > now()) as live_claim
      from public.profile_snapshots s
     where s.profile_id = p_profile
  ), blk as (select public.synchrome_gc_blockers(p_profile) as n)
  select now(),
         (select generation from cur),
         (select ready_at from cur),
         (select generation from prev),
         (select watermarks from prev),
         (select n from blk),
         exists (select 1 from prev)
           and coalesce((select now() >= ready_at + interval '10 minutes' from cur), false)
           and (select n from blk) = 0,
         coalesce((select now() >= ready_at + interval '24 hours' from cur), false),
         (select last_claimed from claims),
         not coalesce((select live_claim from claims), false)
           and coalesce((select now() >= last_claimed + interval '6 hours' from claims), true);
$$;

-- the single eligibility rule of journal GC (used by the RLS policy, the RPC and Storage)
create or replace function public.synchrome_journal_gc_allowed(p_profile uuid, p_device uuid, p_seq_to bigint)
returns boolean
language sql
stable
security invoker
set search_path = ''
as $$
  select coalesce((select s.journal_gc_ok
                          and (s.journal_gc_watermarks ->> p_device::text)::bigint >= p_seq_to
                     from public.synchrome_compaction_status(p_profile) s), false);
$$;

-- the RPC clients use: one source device, up to an explicit seq, all-or-nothing validation
create or replace function public.synchrome_gc_journal_events(p_profile uuid, p_device uuid, p_through bigint)
returns bigint
language plpgsql
volatile
security invoker
set search_path = ''
as $$
declare
  n bigint;
begin
  if p_through is null or p_through < 1
     or not public.synchrome_journal_gc_allowed(p_profile, p_device, p_through) then
    raise exception 'journal GC not allowed for this profile/device/seq now' using errcode = '42501';
  end if;
  with d as (
    delete from public.store_events e
     where e.profile_id = p_profile and e.device_id = p_device and e.seq_to <= p_through
     returning 1)
  select count(*) into n from d;
  return n;
end;
$$;

comment on function public.synchrome_compaction_status(uuid) is
  'Synchrome: compaction decisions with the database clock (journal GC after ready_at + 10 min, one generation behind, no blocking device; generation cleanup after 24 h; one claim per 6 h).';
comment on function public.synchrome_journal_gc_allowed(uuid, uuid, bigint) is
  'Synchrome: the only rule under which a store_events row / journal chunk may be deleted while its profile exists.';
comment on function public.synchrome_gc_journal_events(uuid, uuid, bigint) is
  'Synchrome: delete the store_events of one source device up to an eligible seq (SECURITY INVOKER; RLS re-checks every row).';

-- store_events: replace 0004's generic DELETE by the GC rule
drop policy if exists store_events_delete on public.store_events;
drop policy if exists store_events_delete_gc on public.store_events;
create policy store_events_delete_gc on public.store_events
  for delete to authenticated
  using (account_id = (select auth.uid())
         and public.synchrome_journal_gc_allowed(profile_id, device_id, seq_to));
-- 0004's grants stay (select, delete; insert on 6 columns; no update)

-- 4. Storage: snapshot objects + guarded deletes -----------------------------------
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

create or replace function public.synchrome_snapshot_object_protected(object_name text)
returns boolean
language sql
stable
security invoker
set search_path = ''
as $$
  select object_name ~ '^[0-9a-f-]{36}/profiles/[0-9a-f-]{36}/snapshots/[0-9]{10}/'
     and exists (select 1 from public.profile_snapshots s
                  where s.profile_id::text = split_part(object_name, '/', 3)
                    and s.generation = split_part(object_name, '/', 5)::bigint
                    and s.state = 'ready');
$$;

-- may this object be deleted? snapshot of a ready generation: never; journal chunk: only under
-- the journal GC rule (file name = first seq of the chunk; watermarks are chunk boundaries), or
-- when its profile no longer exists (profile cleanup); anything else: the 0004 owner rule.
create or replace function public.synchrome_object_delete_ok(object_name text)
returns boolean
language sql
stable
security invoker
set search_path = ''
as $$
  select case
    when object_name ~ '^[0-9a-f-]{36}/profiles/[0-9a-f-]{36}/snapshots/' then
      not public.synchrome_snapshot_object_protected(object_name)
    when object_name ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/profiles/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/journal/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/[0-9]{4}/[0-9]{2}/[0-9]{8,12}\.jsonl$' then
      not exists (select 1 from public.profiles p where p.id::text = split_part(object_name, '/', 3))
      or public.synchrome_journal_gc_allowed(
           split_part(object_name, '/', 3)::uuid,
           split_part(object_name, '/', 5)::uuid,
           split_part(split_part(object_name, '/', 8), '.', 1)::bigint)
    else true
  end;
$$;

revoke all on function public.synchrome_gc_blockers(uuid) from public, anon;
revoke all on function public.synchrome_compaction_status(uuid) from public, anon;
revoke all on function public.synchrome_journal_gc_allowed(uuid, uuid, bigint) from public, anon;
revoke all on function public.synchrome_gc_journal_events(uuid, uuid, bigint) from public, anon;
revoke all on function public.synchrome_snapshot_object_ok(text) from public, anon;
revoke all on function public.synchrome_snapshot_object_protected(text) from public, anon;
revoke all on function public.synchrome_object_delete_ok(text) from public, anon;
grant execute on function public.synchrome_gc_blockers(uuid) to authenticated;
grant execute on function public.synchrome_compaction_status(uuid) to authenticated;
grant execute on function public.synchrome_journal_gc_allowed(uuid, uuid, bigint) to authenticated;
grant execute on function public.synchrome_gc_journal_events(uuid, uuid, bigint) to authenticated;
grant execute on function public.synchrome_snapshot_object_ok(text) to authenticated;
grant execute on function public.synchrome_snapshot_object_protected(text) to authenticated;
grant execute on function public.synchrome_object_delete_ok(text) to authenticated;

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

-- AND-ed with 0004's synchrome_stores_delete (own prefix)
drop policy if exists synchrome_stores_delete_keep_ready on storage.objects;   -- earlier draft name
drop policy if exists synchrome_stores_delete_guard on storage.objects;
create policy synchrome_stores_delete_guard on storage.objects
  as restrictive
  for delete to authenticated
  using (bucket_id <> 'stores' or public.synchrome_object_delete_ok(name));

-- 5. Realtime: store_events INSERTs as wake-up hints (idempotent) -----------------
do $$
declare
  pub record;
begin
  select p.oid, p.puballtables into pub from pg_publication p where p.pubname = 'supabase_realtime';
  if not found then
    raise notice 'publication supabase_realtime not found: Realtime wake-up unavailable, polling keeps working.';
  elsif pub.puballtables then
    raise notice 'supabase_realtime is FOR ALL TABLES: store_events is already included; nothing to add.';
  elsif exists (select 1 from pg_publication_tables t
                 where t.pubname = 'supabase_realtime'
                   and t.schemaname = 'public' and t.tablename = 'store_events') then
    raise notice 'store_events already in supabase_realtime; nothing to add.';
  else
    execute 'alter publication supabase_realtime add table public.store_events';
  end if;
end $$;

commit;

-- End of 0005_compaction_realtime.sql
