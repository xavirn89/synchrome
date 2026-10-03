-- ============================================================================
-- Synchrome — functional verification of 0005 (run AFTER applying 0005)
-- ----------------------------------------------------------------------------
-- Runs as the `authenticated` role with synthetic JWT claims for two random accounts, entirely
-- inside ONE transaction that ends in ROLLBACK: it writes nothing permanent and needs no real
-- user, profile or device. Every check raises 'FAIL: …' on an unexpected result; reaching the
-- final NOTICE 'ALL 0005 CHECKS PASSED' means every check passed.
--
--   docker exec -i supabase-db psql -U postgres -d postgres -v ON_ERROR_STOP=1 \
--     < cloud/migrations/0005_compaction_realtime.verify.sql
--
-- The only privileged steps (as the connecting superuser, inside the same rolled-back
-- transaction) back-date `ready_at` to simulate the 10 min / 24 h server-time windows.
-- ============================================================================

begin;

select gen_random_uuid() as a, gen_random_uuid() as b, gen_random_uuid() as p,
       gen_random_uuid() as d1, gen_random_uuid() as d2 \gset v_
select set_config('verify.a', :'v_a', true), set_config('verify.b', :'v_b', true),
       set_config('verify.p', :'v_p', true), set_config('verify.d1', :'v_d1', true),
       set_config('verify.d2', :'v_d2', true);

-- ---------------------------------------------------------------- as account A
select set_config('request.jwt.claims', json_build_object('sub', :'v_a', 'role', 'authenticated')::text, true),
       set_config('request.jwt.claim.sub', :'v_a', true);
set local role authenticated;

insert into public.profiles (id, name) values (:'v_p', 'verify-0005');
insert into public.devices (device_id, name, platform, app_version)
values (:'v_d1', 'verify d1', 'macos', '0.0.0'), (:'v_d2', 'verify d2', 'macos', '0.0.0');
-- d1 published seq 1..2, 3..5, 6..8
insert into public.store_events (profile_id, device_id, path, seq_from, seq_to, hlc)
select :'v_p'::uuid, :'v_d1'::uuid,
       format('profiles/%s/journal/%s/2026/10/%s.jsonl', :'v_p', :'v_d1', lpad(f::text, 8, '0')),
       f, t, format('1790000000000-0000-%s', :'v_d1')
  from (values (1, 2), (3, 5), (6, 8)) v(f, t);

do $$
declare
  p uuid := current_setting('verify.p')::uuid;
  d1 text := current_setting('verify.d1');
  n bigint;
  ok boolean;
  st record;
begin
  -- generation must be latest + 1
  begin
    insert into public.profile_snapshots (profile_id, generation, created_by) values (p, 2, d1::uuid);
    raise exception 'FAIL: generation skip accepted';
  exception when check_violation then null; end;
  insert into public.profile_snapshots (profile_id, generation, created_by) values (p, 1, d1::uuid);
  if (select state from public.profile_snapshots where profile_id = p and generation = 1) <> 'building' then
    raise exception 'FAIL: claim not building';
  end if;
  begin
    insert into public.profile_snapshots (profile_id, generation, created_by) values (p, 1, d1::uuid);
    raise exception 'FAIL: generation reuse accepted';
  exception when unique_violation or check_violation then null; end;
  -- snapshot object names: only for the building claim
  if not public.synchrome_snapshot_object_ok(format('%s/profiles/%s/snapshots/0000000001/manifest.json', current_setting('verify.a'), p)) then
    raise exception 'FAIL: building claim object refused';
  end if;
  if public.synchrome_snapshot_object_ok(format('%s/profiles/%s/snapshots/0000000002/manifest.json', current_setting('verify.a'), p)) then
    raise exception 'FAIL: object of an unclaimed generation accepted';
  end if;
  -- watermark not on a publication boundary / beyond the published chain: refused
  begin
    update public.profile_snapshots
       set state = 'ready', manifest_path = format('profiles/%s/snapshots/0000000001/manifest.json', p),
           manifest_sha256 = repeat('a', 64), watermarks = jsonb_build_object(d1, 4)
     where profile_id = p and generation = 1;
    raise exception 'FAIL: watermark inside a chunk accepted';
  exception when check_violation then null; end;
  begin
    update public.profile_snapshots
       set state = 'ready', manifest_path = format('profiles/%s/snapshots/0000000001/manifest.json', p),
           manifest_sha256 = repeat('a', 64), watermarks = jsonb_build_object(d1, 9)
     where profile_id = p and generation = 1;
    raise exception 'FAIL: watermark beyond the published chain accepted';
  exception when check_violation then null; end;
  -- an active snapshot-capable reader that has applied only 2 holds the watermark at 2
  insert into public.sync_cursors (profile_id, device_id, applied, snapshot_protocol)
  values (p, current_setting('verify.d2')::uuid, jsonb_build_object(d1, 2), 1);
  begin
    update public.profile_snapshots
       set state = 'ready', manifest_path = format('profiles/%s/snapshots/0000000001/manifest.json', p),
           manifest_sha256 = repeat('a', 64), watermarks = jsonb_build_object(d1, 5)
     where profile_id = p and generation = 1;
    raise exception 'FAIL: watermark above an active reader''s applied cursor accepted';
  exception when check_violation then null; end;
  update public.profile_snapshots
     set state = 'ready', manifest_path = format('profiles/%s/snapshots/0000000001/manifest.json', p),
         manifest_sha256 = repeat('a', 64), watermarks = jsonb_build_object(d1, 2)
   where profile_id = p and generation = 1;
  if (select ready_at from public.profile_snapshots where profile_id = p and generation = 1) is null then
    raise exception 'FAIL: ready_at not stamped';
  end if;
  -- a ready snapshot: no overwrite, no edit, no delete, objects protected
  if public.synchrome_snapshot_object_ok(format('%s/profiles/%s/snapshots/0000000001/x-0001.jsonl', current_setting('verify.a'), p)) then
    raise exception 'FAIL: write into a ready generation accepted';
  end if;
  if not public.synchrome_snapshot_object_protected(format('%s/profiles/%s/snapshots/0000000001/manifest.json', current_setting('verify.a'), p))
     or public.synchrome_object_delete_ok(format('%s/profiles/%s/snapshots/0000000001/manifest.json', current_setting('verify.a'), p)) then
    raise exception 'FAIL: ready snapshot object not protected';
  end if;
  begin
    update public.profile_snapshots set counts = '{"x": 1}' where profile_id = p and generation = 1;
    raise exception 'FAIL: ready snapshot edited';
  exception when check_violation then null; end;
  delete from public.profile_snapshots where profile_id = p and generation = 1;
  if not exists (select 1 from public.profile_snapshots where profile_id = p and generation = 1) then
    raise exception 'FAIL: ready snapshot deleted';
  end if;
  -- with a single ready generation nothing is GC-eligible
  select * into st from public.synchrome_compaction_status(p);
  if st.journal_gc_ok or st.journal_gc_watermarks is not null then
    raise exception 'FAIL: GC eligible with one generation';
  end if;
  -- generation 2: reader now applied 8; watermark 8 (monotonic, contiguous 3..5, 6..8)
  update public.sync_cursors set applied = jsonb_build_object(d1, 8)
   where profile_id = p and device_id = current_setting('verify.d2')::uuid;
  insert into public.profile_snapshots (profile_id, generation, created_by) values (p, 2, d1::uuid);
  begin
    update public.profile_snapshots
       set state = 'ready', manifest_path = format('profiles/%s/snapshots/0000000002/manifest.json', p),
           manifest_sha256 = repeat('b', 64), watermarks = '{}'::jsonb
     where profile_id = p and generation = 2;
    raise exception 'FAIL: a device dropped from the watermarks accepted';
  exception when check_violation then null; end;
  update public.profile_snapshots
     set state = 'ready', manifest_path = format('profiles/%s/snapshots/0000000002/manifest.json', p),
         manifest_sha256 = repeat('b', 64), watermarks = jsonb_build_object(d1, 8)
   where profile_id = p and generation = 2;
  -- before the 10 min server grace: nothing deletable, RPC refuses
  delete from public.store_events where profile_id = p;
  get diagnostics n = row_count;
  if n <> 0 then raise exception 'FAIL: % store_events deleted before the grace period', n; end if;
  begin
    perform public.synchrome_gc_journal_events(p, d1::uuid, 2);
    raise exception 'FAIL: RPC GC before the grace period';
  exception when insufficient_privilege then null; end;
  -- generation 1 cannot be superseded (it is the previous / fallback)
  begin
    update public.profile_snapshots set state = 'superseded' where profile_id = p and generation = 1;
    raise exception 'FAIL: previous generation superseded';
  exception when check_violation then null; end;
end $$;

-- ---------------------------------------------------------------- server-time windows (privileged, rolled back)
reset role;
alter table public.profile_snapshots disable trigger profile_snapshots_guard;
update public.profile_snapshots set ready_at = ready_at - interval '1 hour' where profile_id = :'v_p';
alter table public.profile_snapshots enable trigger profile_snapshots_guard;
set local role authenticated;

-- a device mapped to the profile without snapshot support blocks GC
insert into public.profile_mappings (profile_id, device_id, browser_kind, browser_profile_dir, mapping_token)
values (:'v_p', :'v_d1', 'helium', 'Default', md5(random()::text) || md5(random()::text));

do $$
declare
  p uuid := current_setting('verify.p')::uuid;
  d1 uuid := current_setting('verify.d1')::uuid;
  n bigint;
  st record;
begin
  select * into st from public.synchrome_compaction_status(p);
  if st.gc_blockers <> 1 or st.journal_gc_ok then
    raise exception 'FAIL: mapped device without snapshot support did not block GC (%)', st.gc_blockers;
  end if;
  delete from public.store_events where profile_id = p;
  get diagnostics n = row_count;
  if n <> 0 then raise exception 'FAIL: % rows deleted while blocked', n; end if;
  -- the device reports snapshot support: unblocked; GC only up to W(G1) = 2, one generation behind
  insert into public.sync_cursors (profile_id, device_id, snapshot_protocol) values (p, d1, 1);
  select * into st from public.synchrome_compaction_status(p);
  if st.gc_blockers <> 0 or not st.journal_gc_ok or (st.journal_gc_watermarks ->> d1::text)::bigint <> 2 then
    raise exception 'FAIL: GC status after unblock: %', row_to_json(st);
  end if;
  begin
    perform public.synchrome_gc_journal_events(p, d1, 5);
    raise exception 'FAIL: GC above W(previous) accepted';
  exception when insufficient_privilege then null; end;
  if public.synchrome_object_delete_ok(format('%s/profiles/%s/journal/%s/2026/10/00000003.jsonl', current_setting('verify.a'), p, d1)) then
    raise exception 'FAIL: journal chunk above W(previous) deletable';
  end if;
  if not public.synchrome_object_delete_ok(format('%s/profiles/%s/journal/%s/2026/10/00000001.jsonl', current_setting('verify.a'), p, d1)) then
    raise exception 'FAIL: eligible journal chunk not deletable';
  end if;
  n := public.synchrome_gc_journal_events(p, d1, 2);
  if n <> 1 then raise exception 'FAIL: RPC deleted % rows (expected 1)', n; end if;
  -- a raw DELETE can still only remove eligible rows (none left)
  delete from public.store_events where profile_id = p;
  get diagnostics n = row_count;
  if n <> 0 then raise exception 'FAIL: raw DELETE removed % non-eligible rows', n; end if;
  if (select count(*) from public.store_events where profile_id = p) <> 2 then
    raise exception 'FAIL: retained tail changed';
  end if;
  -- retirement: server time, not activity
  update public.sync_cursors set retired_at = '2000-01-01' where profile_id = p and device_id = d1;
  if (select retired_at < now() - interval '1 minute' from public.sync_cursors where profile_id = p and device_id = d1) then
    raise exception 'FAIL: client-supplied retired_at kept';
  end if;
end $$;

-- ---------------------------------------------------------------- as account B (isolation)
reset role;
select set_config('request.jwt.claims', json_build_object('sub', :'v_b', 'role', 'authenticated')::text, true),
       set_config('request.jwt.claim.sub', :'v_b', true);
set local role authenticated;

do $$
declare
  p uuid := current_setting('verify.p')::uuid;
  d1 uuid := current_setting('verify.d1')::uuid;
  n bigint;
  st record;
begin
  if (select count(*) from public.profile_snapshots) <> 0 or (select count(*) from public.sync_cursors) <> 0 then
    raise exception 'FAIL: B sees A''s snapshots/cursors';
  end if;
  select * into st from public.synchrome_compaction_status(p);
  if st.current_generation is not null or st.journal_gc_ok then
    raise exception 'FAIL: B sees A''s compaction status';
  end if;
  delete from public.store_events where profile_id = p;
  get diagnostics n = row_count;
  if n <> 0 then raise exception 'FAIL: B deleted A''s store_events'; end if;
  begin
    perform public.synchrome_gc_journal_events(p, d1, 1);
    raise exception 'FAIL: B ran A''s GC';
  exception when insufficient_privilege then null; end;
  begin
    insert into public.profile_snapshots (profile_id, generation, created_by) values (p, 3, d1);
    raise exception 'FAIL: B claimed a snapshot of A''s profile';
  exception when foreign_key_violation or insufficient_privilege or check_violation then null; end;
  if public.synchrome_snapshot_object_ok(format('%s/profiles/%s/snapshots/0000000003/manifest.json', current_setting('verify.a'), p)) then
    raise exception 'FAIL: B may write snapshot objects under A''s prefix';
  end if;
end $$;

-- ---------------------------------------------------------------- publication
reset role;
do $$
begin
  if exists (select 1 from pg_publication where pubname = 'supabase_realtime')
     and not exists (select 1 from pg_publication_tables
                      where pubname = 'supabase_realtime' and schemaname = 'public' and tablename = 'store_events') then
    raise exception 'FAIL: store_events not in supabase_realtime';
  end if;
  raise notice 'ALL 0005 CHECKS PASSED';
end $$;

rollback;
