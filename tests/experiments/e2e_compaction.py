"""Two/three-device E2E of journal compaction + Realtime wake-up — M1 slice 13.

Devices A and B (USER_A), later C (bootstrap from the current snapshot + tail), D (full-replay
timing, before GC) and E (current snapshot corrupted in transit → previous + tail); USER_B only
for isolation. Disposable Helium profiles only. Real history in the four collections:
bookmarks and extensions through the extension's worker, extension settings through the dummy's
own page, browser preferences through chrome://settings. Compaction / GC are run on demand with
`--dev-compact <mapping> force` (thresholds and the client-side 6 h rule bypassed in this debug
build; every database rule applies) and `--dev-gc`; the 10 min journal-GC grace is the real
server one (the harness waits). Realtime is measured on B's real app process (embedded
frontend) through its counters file.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_compaction.py <creds-env-file>
"""
import json, os, shutil, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402
import e2e_conflicts as cf  # noqa: E402
import e2e_extensions as xe  # noqa: E402
import e2e_settings as es  # noqa: E402
import e2e_prefs as ep  # noqa: E402

check = h.check
FIX = [xe.DUMMY_DIR, xe.DUMMY2_DIR]
M = {"compaction_ms": [], "realtime_ms": [], "polling_ms": [], "bootstrap_snapshot_ms": None, "bootstrap_full_ms": None,
     "bootstrap_import_ms": None, "usage_before": None, "usage_after": None, "gc": None}


# ------------------------------------------------------------------ helpers
def bm_round(d, r, n=10):
    js = f"""const bar = (await chrome.bookmarks.getTree())[0].children.find(c => c.folderType === 'bookmarks-bar' || c.id === '1');
      const f = await chrome.bookmarks.create({{parentId: bar.id, title: 'Round {r} {d.name}'}});
      for (let i = 0; i < {n}; i++) await chrome.bookmarks.create({{parentId: f.id, title: 'R{r} {d.name} item ' + i, url: 'https://example.com/r{r}/{d.name}/' + i}});
      const kids = await chrome.bookmarks.getChildren(f.id);
      await chrome.bookmarks.update(kids[0].id, {{title: 'R{r} {d.name} renamed'}});
      await chrome.bookmarks.remove(kids[1].id)"""
    ra.edit_and_publish(d, js)


def closed(d, offline=False):
    xe.stop(d)
    d.dev("--dev-settings-cycle", d.mid, offline=offline)
    d.dev("--dev-prefs-cycle", d.mid, offline=offline)
    xe.start(d, offline=offline, fixtures=FIX)


def file_round(d, r):
    """extension settings (dummy's own storage) + preferences (settings page), then a closed cycle"""
    es.put(d, "local", {"round": r, "theme": f"t{r}"})
    ep.user_sets(d, {"intl.selected_languages": ["es,en", "fr,en", "de,en", "it,en"][r % 4], "download.prompt_for_download": r % 2 == 0})
    closed(d)


def converge(*devs):
    for d in devs:
        xe.pull(d)  # bookmarks + extensions applied through the extension
        closed(d)   # settings + preferences applied with the browser closed
    for d in devs:
        d.dev("--dev-sync", d.mid)
        d.dev("--dev-report-cursors", d.mid)


def digest(d):
    return d.dev("--dev-shared-digest", d.mid).get("digest") or {}


def status(d):
    return d.dev("--dev-compaction-status", d.mid)


def usage(a, pid):
    return a.dev("--dev-cloud-usage", pid)


def ranges(a, pid):
    return sorted(tuple(x) for x in (a.dev("--dev-cloud-tree", pid).get("store_event_ranges") or []))


def cursor(d, pid, dev):
    return d.dev("--dev-remote-cursor", pid, dev).get("applied_through") or 0


def rt(d):
    p = os.path.join(d.data, "realtime-status.json")
    try:
        return json.load(open(p))
    except (OSError, ValueError):
        return {}


def bar_titles(d):
    return d.hl.js("""(async () => { const bar = (await chrome.bookmarks.getTree())[0].children.find(c => c.folderType === 'bookmarks-bar' || c.id === '1');
        const walk = (n) => [n.title + '|' + (n.url || ''), ...(n.children || []).flatMap(walk)];
        return (bar.children || []).flatMap(walk).sort(); })()""")


def joiner(name, creds, pid, env=None):
    """A new device of USER_A mapped to the profile; its Helium starts (the host's first pull
    bootstraps). Returns (device, seconds until its shared state equals the reference)."""
    d = ra.Device(name)
    d.env.update(env or {})
    d.dev("--dev-sign-in", creds, "A")
    d.device_id = d.dev("--dev-register-device").get("device_id")
    d.mid = d.dev("--dev-map-existing", pid, "helium", "Default").get("mapping_id")
    d.dev("--dev-write-token", d.mid, d.udd)
    d.dev("--dev-install-host", d.udd)
    return d


def wait_digest(d, ref_dev, timeout=240):
    """Seconds until d's shared state equals ref_dev's (re-synced while waiting: a joining
    device may publish its own inventory), or None."""
    t = time.time()
    n = 0
    while time.time() - t < timeout:
        try:
            if n % 5 == 0:
                ref_dev.dev("--dev-sync", ref_dev.mid)
            n += 1
            if digest(d) == digest(ref_dev):
                return time.time() - t
        except Exception:  # noqa: BLE001
            pass
        time.sleep(1)
    return None


def publish_change(a, r):
    """A small published change on A (a preference, through a closed cycle); returns when the
    publication is done."""
    ep.user_sets(a, {"intl.selected_languages": ["pt,en", "nl,en", "sv,en", "da,en", "fi,en", "pl,en"][r % 6]})
    xe.stop(a)
    a.dev("--dev-prefs-cycle", a.mid)
    t0 = time.time()
    xe.start(a, fixtures=FIX)
    return t0


def wait_cursor(b, pid, dev, before, timeout=90):
    t = time.time()
    while time.time() - t < timeout:
        if cursor(b, pid, dev) > before:
            return time.time()
        time.sleep(0.25)
    return None


def ui_app(d, env):
    old = {k: os.environ.get(k) for k in env}
    os.environ.update(env)
    try:
        ui = cf.UI(d)
    finally:
        for k, v in old.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
    return ui


# ------------------------------------------------------------------ main
def main(creds):
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a, b = ra.Device("A"), ra.Device("B")
    print("=== compaction + Realtime E2E ===")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())
    sa = a.dev("--dev-sign-in", creds, "A")
    check("S.1 login USER_A on A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if not sa.get("signed_in"):
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("S.2 login USER_A on B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
    a.device_id = a.dev("--dev-register-device").get("device_id")
    b.device_id = b.dev("--dev-register-device").get("device_id")
    m = a.dev("--dev-create-mapping", f"E2E COMPACT {os.urandom(3).hex()}", "helium", "Default")
    a.mid, pid = m.get("mapping_id"), m.get("profile_id")
    a.dev("--dev-write-token", a.mid, a.udd)
    a.dev("--dev-install-host", a.udd)
    xe.start(a, fixtures=FIX)
    b.mid = b.dev("--dev-map-existing", pid, "helium", "Default").get("mapping_id")
    b.dev("--dev-write-token", b.mid, b.udd)
    b.dev("--dev-install-host", b.udd)
    xe.start(b, fixtures=FIX)
    for d in (a, b):
        d.dev("--dev-settings-mode", d.mid, xe.DUMMY, "local")
        d.dev("--dev-prefs-mode", d.mid, "recommended")

    # ---------------------------------------------------------------- 1 history + convergence
    bm_round(a, 1, 12)
    xe.local_change(a, xe.DUMMY2, False)
    file_round(a, 1)
    converge(b)
    bm_round(b, 1, 6)
    converge(a, b)
    da, db = digest(a), digest(b)
    check("1 A and B converge on a real history of the four collections (shared-state digests equal)",
          da == db and all(da.get(c, [0])[0] > 0 for c in ("bookmarks", "extensions", "extension_settings", "browser_preferences")),
          json.dumps({c: v[0] for c, v in da.items()}))

    # ---------------------------------------------------------------- 3 G1
    o = a.dev("--dev-compact", a.mid, "force")
    M["compaction_ms"].append(o.get("ms"))
    g1 = (o.get("outcome") or {})
    check("3 snapshot G1 built from cloud journals, uploaded, verified and marked ready",
          g1.get("outcome") == "ready" and g1.get("generation") == 1 and g1.get("entities", 0) > 0, json.dumps(g1))

    # ---------------------------------------------------------------- B offline / behind, G2
    b_before = cursor(b, pid, a.device_id)
    bm_round(a, 2, 12)
    xe.local_change(a, xe.DUMMY2, True)
    file_round(a, 2)
    a.dev("--dev-sync", a.mid)
    a.dev("--dev-report-cursors", a.mid)
    o = a.dev("--dev-compact", a.mid, "force")
    M["compaction_ms"].append(o.get("ms"))
    g2 = (o.get("outcome") or {})
    st = status(a)
    gens = {g["generation"]: g for g in st.get("generations", [])}
    check("4 snapshot G2 ready (G1 kept as fallback)", g2.get("outcome") == "ready" and g2.get("generation") == 2
          and gens.get(1, {}).get("state") == "ready" and gens.get(2, {}).get("state") == "ready", json.dumps(g2))
    check("14 B offline (active, behind) holds the watermark: W(G2)[A] = what B applied of A, below what A published",
          gens.get(2, {}).get("watermarks", {}).get(a.device_id) == b_before and b_before > 0,
          json.dumps({"W_G2_A": gens.get(2, {}).get("watermarks", {}).get(a.device_id), "B_applied_A": b_before}))
    converge(b)
    check("14b B back online pulls the tail normally (no rebootstrap needed)",
          digest(b) == digest(a) and not (status(b).get("diagnostics") or {}).get("local", {}).get("snapshot_generation"), "")

    # full-replay bootstrap timing (before any GC): D ignores snapshots
    d = joiner("D", creds, pid, {"SYNCHROME_DEV_FULL_REPLAY": "1"})
    t0 = time.time()
    xe.start(d, fixtures=FIX)
    d.dev("--dev-sync", d.mid)
    tfull = wait_digest(d, a)
    M["bootstrap_full_ms"] = int((time.time() - t0) * 1000) if tfull is not None else None
    d.dev("--dev-report-cursors", d.mid)
    check("M full-replay bootstrap of a new device (no snapshot) converges", tfull is not None, json.dumps({"ms": M["bootstrap_full_ms"]}))
    xe.stop(d)

    # ---------------------------------------------------------------- 15 capability blocker
    fx = a.dev("--dev-fixture-old-device", pid)
    for dv in (a, b):
        dv.dev("--dev-report-cursors", dv.mid)
    st = status(a)
    diag = st.get("diagnostics") or {}
    check("15 a mapped device that cannot read snapshots (old client fixture) blocks journal GC; snapshots still allowed",
          (diag.get("cloud") or {}).get("gc_blockers") == 1 and diag.get("gc_blocked") is True, json.dumps(diag.get("cloud")))

    # ---------------------------------------------------------------- 17-20 Realtime on B's real app process (during the grace)
    env = {"SYNCHROME_DEV_NO_AUTO_COMPACTION": "1"}
    ui = ui_app(b, env)
    ok = ra.wait(lambda: rt(b).get("connected") is True, 40, 0.5)
    s0 = rt(b)
    check("17.0 B's app (embedded frontend) connects to Realtime and joins (one connection)", bool(ok) and s0.get("connects") == 1, json.dumps(s0))
    for r in range(3):
        before = cursor(b, pid, a.device_id)
        t0 = publish_change(a, r)
        t1 = wait_cursor(b, pid, a.device_id, before, 25)
        if t1:
            M["realtime_ms"].append(int((t1 - t0) * 1000))
        time.sleep(3)
    s1 = rt(b)
    check("17 a Realtime event starts B's pull long before the 30 s polling (B's cursor advanced, latency measured)",
          len(M["realtime_ms"]) == 3 and max(M["realtime_ms"]) < 15000 and s1.get("events", 0) >= 3 and s1.get("pulls", 0) >= 3,
          json.dumps({"latency_ms": M["realtime_ms"], "events": s1.get("events"), "pulls": s1.get("pulls")}))
    ui.stop()
    # duplicates
    nb = len([x for x in ranges(a, pid) if x[2].find(b.device_id) >= 0])
    ui = ui_app(b, {**env, "SYNCHROME_DEV_REALTIME_DUPLICATE": "1"})
    ra.wait(lambda: rt(b).get("connected") is True, 40, 0.5)
    before = cursor(b, pid, a.device_id)
    publish_change(a, 3)
    wait_cursor(b, pid, a.device_id, before, 25)
    time.sleep(4)
    s2 = rt(b)
    nb2 = len([x for x in ranges(a, pid) if x[2].find(b.device_id) >= 0])
    check("19 duplicated Realtime events are coalesced: 0 duplicate ops, B published nothing, states equal",
          s2.get("coalesced", 0) >= 1 and nb2 == nb, json.dumps({"coalesced": s2.get("coalesced"), "pulls": s2.get("pulls")}))
    ui.stop()
    # lost events -> polling recovers
    ui = ui_app(b, {**env, "SYNCHROME_DEV_REALTIME_DROP": "1"})
    ra.wait(lambda: rt(b).get("connected") is True, 40, 0.5)
    time.sleep(2)
    before = cursor(b, pid, a.device_id)
    t0 = publish_change(a, 4)
    t1 = wait_cursor(b, pid, a.device_id, before, 75)
    if t1:
        M["polling_ms"].append(int((t1 - t0) * 1000))
    check("18 Realtime lost (events dropped): the 30 s polling still brings the change", t1 is not None, json.dumps({"polling_ms": M["polling_ms"]}))
    ui.stop()
    # restart: reconnect + catch-up
    ui = ui_app(b, env)
    ok = ra.wait(lambda: rt(b).get("connected") is True and rt(b).get("pulls", 0) >= 1, 40, 0.5)
    check("20 app restart: Realtime reconnects and runs a catch-up sync", bool(ok), json.dumps(rt(b)))
    ui.stop()

    # ---------------------------------------------------------------- 5-7 GC one generation behind
    for dv in (a, b):
        dv.dev("--dev-sync", dv.mid)
        dv.dev("--dev-report-cursors", dv.mid)
    blocked = a.dev("--dev-gc", a.mid).get("gc") or {}
    check("15.1 GC refused while the old-client fixture is mapped (nothing deleted)", blocked.get("allowed") is False and blocked.get("events_deleted") == 0, json.dumps(blocked))
    a.dev("--dev-retire-device", pid, fx.get("device_id"))
    t = time.time()
    while time.time() - t < 15 * 60:
        c = (status(a).get("diagnostics") or {}).get("cloud") or {}
        if c.get("journal_gc_ok"):
            break
        time.sleep(20)
    st = status(a)
    gens = {g["generation"]: g for g in st.get("generations", [])}
    w1 = gens.get(1, {}).get("watermarks", {})
    M["usage_before"] = usage(a, pid)
    r_before = ranges(a, pid)
    gcr = a.dev("--dev-gc", a.mid).get("gc") or {}
    M["gc"] = gcr
    M["usage_after"] = usage(a, pid)
    r_after = ranges(a, pid)
    check("16 + 6 after the explicit retirement and the server's 10 min grace: GC allowed; deleted only journals ≤ W(G1)",
          gcr.get("allowed") is True and gcr.get("events_deleted", 0) > 0 and gcr.get("objects_deleted", 0) > 0 and gcr.get("leftover_events") == 0
          and all(to > w1.get(dev_of, 0) for (_, to, path) in r_after for dev_of in [path.split("/")[3]]),
          json.dumps({"gc": gcr, "W_G1": w1}))
    kept = [x for x in r_before if x[1] > w1.get(x[2].split("/")[3], 0)]
    check("7 journals after W(G1) intact (the fallback's tail)", all(x in r_after for x in kept) and len(r_after) == len(kept), json.dumps({"kept": len(kept), "after": len(r_after)}))
    check("7b both generations kept (current + fallback); ready snapshot objects present",
          M["usage_after"].get("snapshot_objects", 0) == M["usage_before"].get("snapshot_objects", 0) > 0, json.dumps(M["usage_after"]))

    # ---------------------------------------------------------------- 8 A/B keep syncing
    bm_round(a, 3, 3)
    converge(b)
    check("8 A/B keep synchronising after GC", digest(a) == digest(b), "")

    # ---------------------------------------------------------------- 9-11 C from zero: snapshot G2 + tail
    c = joiner("C", creds, pid)
    t0 = time.time()
    xe.start(c, fixtures=FIX)
    c.dev("--dev-sync", c.mid)
    tsnap = wait_digest(c, a)
    M["bootstrap_snapshot_ms"] = int((time.time() - t0) * 1000) if tsnap is not None else None
    ci = (status(c).get("diagnostics") or {}).get("local") or {}
    M["bootstrap_import_ms"] = ci.get("bootstrap_ms")
    xe.pull(c)
    check("10-11 device C from zero bootstraps from G2 + tail and converges exactly (digests of the four collections)",
          tsnap is not None and ci.get("snapshot_generation") == 2 and ci.get("bootstrap_fallback") is False and digest(c) == digest(a),
          json.dumps({"ms": M["bootstrap_snapshot_ms"], "import_ms": ci.get("bootstrap_ms")}))
    check("11b C's browser holds the same bookmarks bar as A", bar_titles(c) == bar_titles(a), "")
    c.dev("--dev-report-cursors", c.mid)

    # ---------------------------------------------------------------- 12-13 current corrupted -> previous + tail
    e = joiner("E", creds, pid, {"SYNCHROME_DEV_CORRUPT_SNAPSHOT_GEN": "2"})
    xe.start(e, fixtures=FIX)
    e.dev("--dev-sync", e.mid)
    te = wait_digest(e, a)
    ei = (status(e).get("diagnostics") or {}).get("local") or {}
    check("12-13 current snapshot corrupted (test env): previous G1 + retained tail used, converges",
          te is not None and ei.get("snapshot_generation") == 1 and ei.get("bootstrap_fallback") is True, json.dumps(ei))
    e.dev("--dev-report-cursors", e.mid)

    # ---------------------------------------------------------------- 20 restart (C)
    xe.restart(c, fixtures=FIX)
    c.dev("--dev-sync", c.mid)
    ci2 = (status(c).get("diagnostics") or {}).get("local") or {}
    check("20b restart of C (Helium + fresh processes): no second import, still converged",
          ci2.get("bootstrapped_at") == ci.get("bootstrapped_at") and digest(c) == digest(a), "")

    # ---------------------------------------------------------------- security (before USER_B switches the Keychain)
    for dv in (a, b, c):
        lg = dv.log()
        check(f"S device {dv.name} host log: no tokens or JWTs", h.secret_free(lg), "")
    rtxt = open(os.path.join(b.data, "realtime-status.json")).read() if os.path.exists(os.path.join(b.data, "realtime-status.json")) else ""
    check("S realtime status file: counters only (no token, no ids)", "eyJ" not in rtxt and h.EXPECTED["A"] not in rtxt and pid not in rtxt, "")

    # ---------------------------------------------------------------- metrics, 21 isolation
    for dv in (a, b, c, d, e):
        try:
            xe.stop(dv)
        except Exception:  # noqa: BLE001
            pass
    snap_path = f"profiles/{pid}/snapshots/0000000002/manifest.json"
    print("METRICS " + json.dumps({
        "objects_before": M["usage_before"].get("objects"), "objects_after": M["usage_after"].get("objects"),
        "store_events_before": M["usage_before"].get("store_events"), "store_events_after": M["usage_after"].get("store_events"),
        "journal_bytes_before": M["usage_before"].get("journal_bytes"), "journal_bytes_after": M["usage_after"].get("journal_bytes"),
        "snapshot_bytes": M["usage_after"].get("snapshot_bytes"), "compaction_ms": M["compaction_ms"],
        "bootstrap_full_replay_ms": M["bootstrap_full_ms"], "bootstrap_snapshot_ms": M["bootstrap_snapshot_ms"], "bootstrap_import_ms": M["bootstrap_import_ms"],
        "realtime_wake_ms": M["realtime_ms"], "polling_fallback_ms": M["polling_ms"],
        "realtime_events": rt(b).get("events"), "wakeups_coalesced": rt(b).get("coalesced"), "pulls_executed": rt(b).get("pulls"),
        "gc": M["gc"],
    }))
    x = ra.Device("X")
    u = x.dev("--dev-sign-in", creds, "B")
    check("21.0 login USER_B (exact id)", u.get("user_id") == h.EXPECTED["B"], f"user_id={u.get('user_id')}")
    pr = x.dev("--dev-compaction-probe-foreign", h.EXPECTED["A"], pid, a.device_id, snap_path) if u.get("signed_in") else {}
    check("21 USER_B sees no snapshots/cursors/status of A, cannot run A's GC, claim, retire, read or write A's snapshot objects",
          pr.get("snapshot_rows_visible") == 0 and pr.get("status_sees_generation") is False and pr.get("cursor_rows_visible") == 0
          and pr.get("gc_refused") and pr.get("claim_refused") and pr.get("retire_refused") and pr.get("snapshot_object_readable") is False
          and pr.get("snapshot_write_refused"), json.dumps({k: v for k, v in pr.items() if k != "as_user"}))
    a.dev("--dev-sign-in", creds, "A")

    # ---------------------------------------------------------------- 22 cleanup
    for dv in (b, c, d, e):
        dv.dev("--dev-cleanup", dv.mid)
        dv.dev("--dev-delete-this-device")
    a.dev("--dev-delete-device-id", fx.get("device_id"))
    ra_ = a.dev("--dev-cleanup", a.mid)
    gone = usage(a, pid)
    inv = a.dev("--dev-inventory")
    check("22.1 cloud: profile deleted (two phases: objects allowed → row → remaining objects), 0 objects / 0 events, no devices/mappings left",
          ra_.get("deleted_profile") and ra_.get("leftover_objects") == 0 and gone.get("objects") == 0 and (gone.get("store_events") or 0) == 0
          and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [], json.dumps({"A": ra_, "usage": gone}))
    so = a.dev("--dev-sign-out")
    check("22.2 Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, c, d, e, x):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    shutil.rmtree(os.path.join(ra.WORK, "ui-driver"), ignore_errors=True)
    if os.path.exists(os.path.join(ra.WORK, "ui-app.log")):
        os.remove(os.path.join(ra.WORK, "ui-app.log"))
    check("22.3 local: data dirs (state.sqlite v7, compaction_state, logs, backups), Helium profiles, manifests, UI files",
          not any(os.path.exists(p_) for dv in (a, b, c, d, e, x) for p_ in (dv.udd, dv.data)), json.dumps(os.listdir(ra.WORK)))


if __name__ == "__main__":
    try:
        main(sys.argv[1])
    except Exception as e:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        check("harness completed without exception", False, type(e).__name__ + ": " + str(e)[:200])
        for p_ in cf.UI_PROCS:
            if p_.poll() is None:
                p_.kill()
        ra.emergency()
    for p_ in cf.UI_PROCS:
        if p_.poll() is None:
            p_.kill()
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== compaction: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
