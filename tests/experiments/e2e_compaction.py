"""Real multi-device E2E of journal compaction + Realtime wake-up — M1 slice 13 (22 cases).

USER_A devices, each with its own data dir (device.json, state.sqlite, logs) and its own
disposable Helium profile:
  A, B   the two devices of the profile; A's / B's real app processes (embedded frontend,
         debug UI driver) for Realtime, restart, logout/login and the Sync health UI
  R      joins early, leaves, is retired explicitly (stand-in for a dormant device: the 30-day
         server clock cannot be faked without SQL) and returns after the GC → re-bootstrap
  D      new device forced to FULL replay (timing + final-state comparison), before the GC
  C      new device after the GC: snapshot G2 + tail
  E      new device, current snapshot corrupted in transit (debug) → previous G1 + tail
  E2     new device, BOTH generations corrupted → blocked, state untouched
  P      auxiliary profile: watermark refused by the database + two-phase profile delete
  L      isolated pre-flight logins (USER_A, USER_B); X = USER_B for the RLS probes
Compaction / GC run on demand (`--dev-compact`, `--dev-gc`; automatic compaction is off in
every process of the harness so measurements are deterministic); the 10 min journal-GC grace is
the REAL database one. Nothing printed contains passwords, JWTs, refresh/mapping tokens, titles,
URLs or setting/preference values.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_compaction.py <creds-env-file>
"""
import json, os, re, shutil, subprocess, sys, time
from datetime import datetime

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402
import e2e_conflicts as cf  # noqa: E402
import e2e_extensions as xe  # noqa: E402
import e2e_settings as es  # noqa: E402
import e2e_prefs as ep  # noqa: E402

check = h.check
FIX = [xe.DUMMY_DIR, xe.DUMMY2_DIR]
NOAUTO = {"SYNCHROME_DEV_NO_AUTO_COMPACTION": "1"}
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
RECORD_KEYS = {"id", "f", "h", "d", "dh"}
# distinctive strings of the E2E's own data that must never reach a log
SENSITIVE = ["example.com/r", "Round 1 ", " item 0", "zq-theme-", "zq-tmp", "pt,en", "ro,en", "hr,en"]
DEVS = {}
FXS = []  # fixture device ids (old clients), for the emergency cleanup
M = {"compaction_ms": {}, "snapshot_bytes": {}, "realtime_start_ms": [], "realtime_apply_ms": [], "polling_ms": [],
     "bootstrap_full_ms": None, "bootstrap_snapshot_ms": None, "bootstrap_import_ms": None, "bootstrap_previous_ms": None,
     "usage_before_compaction": {}, "usage_before_gc": {}, "usage_after_gc": {}, "gc": None, "blockers_seen": set(), "rt": {}}


# ------------------------------------------------------------------ devices + helpers
def device(name, env=None):
    d = ra.Device(name)
    d.env.update(NOAUTO)
    d.env.update(env or {})
    DEVS[name] = d
    return d


class AppUI(cf.UI):
    """The real Synchrome app (embedded frontend) on a device's data dir, own driver dir."""

    def __init__(self, dev, env=None):  # replaces cf.UI.__init__ (per-device driver dir)
        self.dir = os.path.join(ra.WORK, f"ui-driver-{dev.name}")
        shutil.rmtree(self.dir, ignore_errors=True)
        os.makedirs(os.path.join(self.dir, "in"))
        try:
            os.remove(os.path.join(dev.data, "realtime-status.json"))
        except OSError:
            pass
        self.log = open(os.path.join(ra.WORK, f"ui-app-{dev.name}.log"), "a")
        e = dict(os.environ)
        e.update(dev.env)
        e.update(env or {})
        e.update({"SYNCHROME_DATA_DIR": dev.data, "SYNCHROME_DEV_UI_DRIVER": self.dir})
        self.p = subprocess.Popen([h.APP], env=e, stdout=self.log, stderr=self.log)
        cf.UI_PROCS.append(self.p)
        self.n = 0
        self.ready = ra.wait(lambda: self.js("return document.readyState === 'complete' && !!document.querySelector('main')", 3), 60, 1)

    def health(self):
        """Text of every Sync health row after a fresh load of the cloud data."""
        self.js("setTimeout(() => location.reload(), 50); return true;", 5)
        time.sleep(2)
        ra.wait(lambda: self.js("return document.readyState === 'complete' && !!document.querySelector('main')", 3), 30, 0.5)
        if not self.until("return document.querySelectorAll('.sync-health').length > 0", 40):
            return []
        return self.js("return [...document.querySelectorAll('.sync-health')].map(e => e.innerText)") or []


def creds_of(path, who):
    v = {}
    for line in open(path, encoding="utf-8"):
        k, _, x = line.strip().removeprefix("export ").partition("=")
        v[k.strip()] = x.strip().strip('"').strip("'")
    return v.get(f"USER_{who}_EMAIL", ""), v.get(f"USER_{who}_PASSWORD", "")


def ui_sign_in(ui, creds, who):
    """The app's real login form (values from the creds file, never printed; the driver file is
    consumed and deleted by the app within 250 ms)."""
    if not ui.until("return !!document.querySelector('form.login input[type=password]')", 30):
        return False
    email, pw = creds_of(creds, who)
    r = ui.js("""const set = (el, v) => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, v);
                 el.dispatchEvent(new Event('input', {bubbles: true})); };
                 set(document.querySelector('form.login input[type=email]'), %s);
                 set(document.querySelector('form.login input[type=password]'), %s);
                 await new Promise(r => setTimeout(r, 300));
                 document.querySelector('form.login button[type=submit]').click(); return true;""" % (json.dumps(email), json.dumps(pw)))
    del email, pw
    return r is True


def ui_sign_out(ui):
    return ui.js("""const b = [...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Sign out');
                    if (!b) return false; b.click(); return true;""")


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


def file_round(d, r, extra=None):
    """extension settings (dummy's own storage) + preferences (settings page), closed cycle"""
    es.put(d, "local", {"round": r, "theme": f"zq-theme-{r}", **(extra or {})})
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


def cloud(d):
    return (status(d).get("diagnostics") or {}).get("cloud") or {}


def local(d):
    return (status(d).get("diagnostics") or {}).get("local") or {}


def gens(d):
    return {g["generation"]: g for g in status(d).get("generations", [])}


def usage(a, pid):
    return a.dev("--dev-cloud-usage", pid)


def ranges(a, pid):
    return sorted(tuple(x) for x in (a.dev("--dev-cloud-tree", pid).get("store_event_ranges") or []))


def dev_of(path):
    return path.split("/")[3]


def cursor(d, pid, dev):
    return d.dev("--dev-remote-cursor", pid, dev).get("applied_through") or 0


def rows(a, pid):
    return {r["device_id"]: r for r in (a.dev("--dev-cursor-rows", pid).get("rows") or [])}


def rt(d):
    try:
        return json.load(open(os.path.join(d.data, "realtime-status.json")))
    except (OSError, ValueError):
        return {}


def bar_titles(d):
    return d.hl.js("""(async () => { const bar = (await chrome.bookmarks.getTree())[0].children.find(c => c.folderType === 'bookmarks-bar' || c.id === '1');
        const walk = (n) => [n.title, ...(n.children || []).flatMap(walk)];
        return (bar.children || []).flatMap(walk).sort(); })()""")


def joiner(name, creds, pid, env=None):
    """A new device of USER_A mapped to the profile (no local state)."""
    d = device(name, env)
    d.dev("--dev-sign-in", creds, "A")
    d.device_id = d.dev("--dev-register-device").get("device_id")
    d.mid = d.dev("--dev-map-existing", pid, "helium", "Default").get("mapping_id")
    d.dev("--dev-write-token", d.mid, d.udd)
    d.dev("--dev-install-host", d.udd)
    return d


def wait_digest(d, ref_dev, timeout=240):
    t = time.time()
    n = 0
    while time.time() - t < timeout:
        try:
            if n % 5 == 0:
                ref_dev.dev("--dev-sync", ref_dev.mid)
                d.dev("--dev-sync", d.mid)
            n += 1
            if digest(d) == digest(ref_dev):
                return time.time() - t
        except Exception:  # noqa: BLE001
            pass
        time.sleep(1)
    return None


def publish_change(a, r):
    """A published preference change on A (settings page + closed cycle); returns the local
    time at which the publication had been recorded."""
    ep.user_sets(a, {"intl.selected_languages": ["pt,en", "nl,en", "sv,en", "da,en", "fi,en", "pl,en", "ca,en", "eu,en"][r % 8]})
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


def chain_end(rg, dev, lo=0):
    """End of dev's contiguous publication chain starting right after `lo`."""
    end = lo
    for f, t, p in sorted(x for x in rg if dev_of(x[2]) == dev and x[1] > lo):
        if f != end + 1:
            break
        end = t
    return end


def verify(a, g):
    return a.dev("--dev-snapshot-verify", a.mid, str(g))


def part_shas(cols):
    return {c: [(p["name"], p["sha256"], p["bytes"], p["entities"]) for p in i["parts"]] for c, i in cols.items()}


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def check_snapshot(label, v, g, row):
    cols = v.get("collections") or {}
    counts_ok = all((i.get("row_counts") or {}).get("entities") == i["entities"] and (i.get("row_counts") or {}).get("tombstones") == i["tombstones"]
                    for i in cols.values())
    check(f"{label} manifest: hash = row, format, generation, created_at/created_by, profile, watermarks = row, state ready",
          v.get("manifest_sha_ok") and v.get("format") == "synchrome-snapshot/1" and v.get("manifest_generation") == g
          and v.get("created_at_set") and v.get("created_by_is_device") and v.get("profile_matches") and v.get("watermarks_match_row")
          and row.get("state") == "ready",
          json.dumps({k: v.get(k) for k in ("format", "manifest_generation", "base_generation", "watermark_devices", "manifest_keys")}))
    check(f"{label} NDJSON parts: every hash/size/entity count verified, ≤ 3.5 MiB, counts/bytes = manifest = row",
          v.get("parts_ok") and v.get("total_bytes_ok") and v.get("row_bytes_ok") and counts_ok
          and v.get("max_part_bytes", 1 << 30) <= v.get("max_allowed", 0)
          and set(cols) == {"bookmarks", "extensions", "extension_settings", "browser_preferences"},
          json.dumps({c: {"entities": i["entities"], "tombstones": i["tombstones"], "bytes": i["bytes"], "parts": len(i["parts"])} for c, i in cols.items()}))
    check(f"{label} records hold only published shared state (keys ⊆ id,f,h,d,dh: no conflicts/pending/local metadata)",
          v.get("record_keys") and set(v.get("record_keys")) <= RECORD_KEYS, json.dumps(v.get("record_keys")))


# ------------------------------------------------------------------ main
def main(creds):
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    print("=== compaction + Realtime E2E ===")

    # ---------------------------------------------------------------- pre-flight logins
    lg = device("L")
    check("P.0 signed out, no Keychain session", lg.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())
    s = lg.dev("--dev-sign-in", creds, "A")
    inv = lg.dev("--dev-inventory")
    check("P.12 isolated login USER_A: exact id; cloud clean (0 profiles / 0 mappings / 0 devices)",
          s.get("user_id") == h.EXPECTED["A"] and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [],
          f"user_id={s.get('user_id')} profiles={inv.get('profiles')} mappings={inv.get('mappings')} devices={len(inv.get('devices') or [])}")
    if s.get("user_id") != h.EXPECTED["A"]:
        return
    lg.dev("--dev-sign-out")
    check("P.13 logout USER_A: Keychain empty", not h.keychain_has_session())
    s = lg.dev("--dev-sign-in", creds, "B")
    inv = lg.dev("--dev-inventory")
    check("P.14 isolated login USER_B: exact id; cloud clean",
          s.get("user_id") == h.EXPECTED["B"] and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [],
          f"user_id={s.get('user_id')} profiles={inv.get('profiles')} mappings={inv.get('mappings')} devices={len(inv.get('devices') or [])}")
    lg.dev("--dev-sign-out")
    check("P.15-16 logout USER_B: Keychain empty", not h.keychain_has_session())

    # ---------------------------------------------------------------- CASE 1 setup
    a, b = device("A"), device("B")
    sa = a.dev("--dev-sign-in", creds, "A")
    check("1.1 login USER_A on A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    sb = b.dev("--dev-sign-in", creds, "A")
    check("1.2 login USER_A on B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
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
    inv = a.dev("--dev-inventory")
    check("1.3-7 one cloud profile; confirmed mappings on A and B to the SAME profile; distinct devices",
          inv.get("profiles") == 1 and inv.get("mappings") == 2 and a.device_id and b.device_id and a.device_id != b.device_id
          and a.mid and b.mid and a.mid != b.mid and bool(pid), json.dumps({"profiles": inv.get("profiles"), "mappings": inv.get("mappings")}))

    # ---------------------------------------------------------------- CASE 2 history
    bm_round(a, 1, 12)
    xe.local_change(a, xe.DUMMY2, False)
    file_round(a, 1, {"zqtmp": "zq-tmp-1"})
    converge(b)
    bm_round(b, 1, 6)
    converge(a, b)
    rw = rows(a, pid)
    check("1.8 A and B report sync_cursors with snapshot_protocol = 1 (applied, published_through)",
          all(rw.get(x, {}).get("snapshot_protocol") == 1 and rw.get(x, {}).get("applied") for x in (a.device_id, b.device_id)),
          json.dumps({n: {k: rw.get(x, {}).get(k) for k in ("snapshot_protocol", "published_through", "retired")} for n, x in (("A", a.device_id), ("B", b.device_id))}))

    # R: joins now, leaves, explicitly retired (dormant stand-in)
    r = joiner("R", creds, pid)
    xe.start(r, fixtures=FIX)
    tr = wait_digest(r, a)
    r.dev("--dev-report-cursors", r.mid)
    r_applied_a = cursor(r, pid, a.device_id)
    xe.stop(r)
    st_now = cloud(a).get("server_now")
    r.dev("--dev-retire-device", pid, r.device_id, "2000-01-01T00:00:00Z")  # bogus client time on purpose
    rr = rows(a, pid).get(r.device_id, {})
    skew = abs(ts(rr["retired_at"]) - ts(st_now)) if rr.get("retired_at") and st_now else None
    check("14.88-90 explicit retirement: retired_at stamped with the SERVER clock (client sent 2000-01-01), never automatic",
          tr is not None and rr.get("retired") is True and skew is not None and skew < 300,
          json.dumps({"retired": rr.get("retired"), "server_skew_s": None if skew is None else round(skew, 1)}))

    # more history after R left (tombstones included), B converges
    bm_round(a, 11, 6)
    es.remove(a, "local", ["zqtmp"])
    file_round(a, 2)
    converge(b)
    da, db = digest(a), digest(b)
    check("2.9-13 A and B converge on a real history of the four collections (shared-state digests equal)",
          da == db and all(da.get(c, [0])[0] > 0 for c in ("bookmarks", "extensions", "extension_settings", "browser_preferences")),
          json.dumps({c: v[0] for c, v in da.items()}))
    M["usage_before_compaction"] = usage(a, pid)
    check("2.14-15 base digest + cloud usage before compaction recorded", bool(M["usage_before_compaction"].get("objects")),
          json.dumps(M["usage_before_compaction"]))
    rg0 = ranges(a, pid)

    # ---------------------------------------------------------------- CASE 3 G1 (threshold path)
    a.env["SYNCHROME_DEV_COMPACTION_THRESHOLDS"] = "50,5,4096"
    o = a.dev("--dev-compact", a.mid)
    a.env.pop("SYNCHROME_DEV_COMPACTION_THRESHOLDS")
    g1 = o.get("outcome") or {}
    M["compaction_ms"]["G1"] = o.get("ms")
    M["snapshot_bytes"]["G1"] = g1.get("bytes")
    check("3.16-19 G1 through the normal policy with lowered debug thresholds: built from cloud journals, verified, ready",
          g1.get("outcome") == "ready" and g1.get("generation") == 1 and g1.get("entities", 0) > 0, json.dumps(g1))
    gg = gens(a)
    v1 = verify(a, 1)
    check_snapshot("3.18 G1", v1, 1, gg.get(1, {}))
    w1 = gg.get(1, {}).get("watermarks", {})
    check("16.96 W(G1): chunk boundaries of contiguous chains, ≤ chain end; a retired device does not hold them",
          bool(w1) and all(w == 0 or any(t == w for (_, t, p) in rg0 if dev_of(p) == dv) for dv, w in w1.items())
          and all(w <= chain_end(rg0, dv) for dv, w in w1.items()) and w1.get(a.device_id, 0) > r_applied_a,
          json.dumps({"W_G1_A": w1.get(a.device_id), "chain_end_A": chain_end(rg0, a.device_id), "R_applied_A": r_applied_a}))
    g1gc = a.dev("--dev-gc", a.mid).get("gc") or {}
    check("3.20 with a single generation: no GC", g1gc.get("allowed") is False and g1gc.get("events_deleted") == 0, json.dumps(g1gc))
    rb1 = a.dev("--dev-snapshot-rebuild", a.mid, "1", "none")
    rb1b = a.dev("--dev-snapshot-rebuild", a.mid, "1", "none")
    check("17.98-100 G1 rebuilt twice from the same published cloud state: identical parts/hashes, = uploaded G1",
          part_shas(rb1.get("collections") or {}) == part_shas(rb1b.get("collections") or {}) == part_shas(v1.get("collections") or {}) != {},
          json.dumps({c: len(x) for c, x in part_shas(rb1.get("collections") or {}).items()}))

    # ---------------------------------------------------------------- CASE 12 (part 1): old client while G2 is built
    f1 = a.dev("--dev-fixture-old-device", pid)
    FXS.append(f1.get("device_id"))
    c_ = cloud(a)
    M["blockers_seen"].add(c_.get("gc_blockers"))
    check("12.79-81 a mapped old client (no sync_cursors row) is a GC blocker (count 1)", c_.get("gc_blockers") == 1, json.dumps({"gc_blockers": c_.get("gc_blockers")}))
    a.dev("--dev-fixture-cursor", pid, f1.get("device_id"), "0")
    c_ = cloud(a)
    check("12.79 … with snapshot_protocol = 0 it still blocks (count 1)", c_.get("gc_blockers") == 1, "")

    # ---------------------------------------------------------------- CASE 4 G2 (B applies part of the new history, then falls behind)
    bm_round(a, 21, 6)
    es.put(a, "local", {"zqtmp2": "zq-tmp-2"})
    closed(a)
    converge(b)
    b_before = cursor(b, pid, a.device_id)
    bm_round(a, 2, 12)
    es.remove(a, "local", ["zqtmp2"])
    xe.local_change(a, xe.DUMMY2, True)
    file_round(a, 3)
    a.dev("--dev-sync", a.mid)
    a.dev("--dev-report-cursors", a.mid)
    a.env["SYNCHROME_DEV_COMPACTION_THRESHOLDS"] = "50,5,4096"
    na = a.dev("--dev-compact", a.mid)
    a.env.pop("SYNCHROME_DEV_COMPACTION_THRESHOLDS")
    check("4 the database's 6 h rule: a second claim through the normal policy (thresholds met) is not allowed",
          (na.get("outcome") or {}).get("outcome") == "not_allowed", json.dumps(na.get("outcome")))
    o = a.dev("--dev-compact", a.mid, "force")
    g2 = o.get("outcome") or {}
    M["compaction_ms"]["G2"] = o.get("ms")
    M["snapshot_bytes"]["G2"] = g2.get("bytes")
    gg = gens(a)
    check("4.21-23 + 12.80 G2 ready (forced, debug); the old-client blocker does not stop snapshots; G1 kept",
          g2.get("outcome") == "ready" and g2.get("generation") == 2 and gg.get(1, {}).get("state") == "ready" and gg.get(2, {}).get("state") == "ready",
          json.dumps(g2))
    v2 = verify(a, 2)
    check_snapshot("4.23 G2", v2, 2, gg.get(2, {}))
    w2 = gg.get(2, {}).get("watermarks", {})
    check("16.96 an active reader behind holds the watermark: W(G1)[A] < W(G2)[A] = what B applied of A < what A published",
          w2.get(a.device_id) == b_before and w1.get(a.device_id, 0) < b_before < chain_end(ranges(a, pid), a.device_id),
          json.dumps({"W_G1_A": w1.get(a.device_id), "W_G2_A": w2.get(a.device_id), "B_applied_A": b_before}))
    rb2n = a.dev("--dev-snapshot-rebuild", a.mid, "2", "none")
    rb2p = a.dev("--dev-snapshot-rebuild", a.mid, "2", "prev")
    check("17 G2 rebuilt from journals alone and from G1 + journals: identical parts/hashes, = uploaded G2",
          part_shas(rb2n.get("collections") or {}) == part_shas(rb2p.get("collections") or {}) == part_shas(v2.get("collections") or {}) != {}
          and rb2p.get("base_generation") == 1, "")
    tomb = {c: i["tombstones"] for c, i in (v2.get("collections") or {}).items()}
    check("11.75-77 deletes are folded as tombstones and kept (bookmarks, extension settings)",
          tomb.get("bookmarks", 0) > 0 and tomb.get("extension_settings", 0) > 0, json.dumps(tomb))
    converge(b)
    check("4 B back online pulls the tail normally (no re-bootstrap needed)",
          digest(b) == digest(a) and not local(b).get("snapshot_generation"), "")

    # ---------------------------------------------------------------- CASE 9 D: full replay (before any GC)
    d = joiner("D", creds, pid, {"SYNCHROME_DEV_FULL_REPLAY": "1"})
    t0 = time.time()
    xe.start(d, fixtures=FIX)
    tfull = wait_digest(d, a)
    M["bootstrap_full_ms"] = int((time.time() - t0) * 1000) if tfull is not None else None
    d.dev("--dev-report-cursors", d.mid)
    check("9.62-63 device D forced to FULL replay converges (timed)",
          tfull is not None and not local(d).get("snapshot_generation"), json.dumps({"ms": M["bootstrap_full_ms"]}))
    xe.stop(d)

    # ---------------------------------------------------------------- CASE 12 (part 2) / 14 / 16 readers
    a.dev("--dev-fixture-cursor", pid, f1.get("device_id"), "1")
    c_ = cloud(a)
    check("12.83-84 old client upgraded to snapshot_protocol = 1: no longer a blocker (count 0)", c_.get("gc_blockers") == 0, "")
    held = a.dev("--dev-compact", a.mid, "force").get("outcome") or {}
    check("16.96 … but as an active reader that applied nothing it holds every watermark (no new generation)",
          held.get("outcome") == "not_needed" and len(gens(a)) == 2, json.dumps({"outcome": held.get("outcome")}))
    f2 = a.dev("--dev-fixture-old-device", pid)
    FXS.append(f2.get("device_id"))
    c_ = cloud(a)
    M["blockers_seen"].add(c_.get("gc_blockers"))
    check("14.88 second old-client fixture: blocker count 1", c_.get("gc_blockers") == 1, "")
    a.dev("--dev-retire-device", pid, f1.get("device_id"))

    # ---------------------------------------------------------------- CASE 5 Realtime (inside the G2 grace)
    ui = AppUI(b)
    ok = ra.wait(lambda: rt(b).get("connected") is True and rt(b).get("pulls", 0) >= 1, 40, 0.5)
    s0 = rt(b)
    check("5 B's real app (embedded frontend) connects to Realtime: one connection, catch-up pull", bool(ok) and s0.get("connects") == 1,
          json.dumps({k: s0.get(k) for k in ("connected", "connects", "pulls")}))
    hb = ui.health()
    check("22.121-124 Sync health while blocked: Realtime connected, generation 2, 2 kept, last compacted, GC paused (1 device), no ids",
          len(hb) == 1 and "Realtime connected" in hb[0] and "generation 2" in hb[0] and "2 kept" in hb[0]
          and "last compacted never" not in hb[0] and "paused: 1 device" in hb[0] and not UUID.search(hb[0]), json.dumps(hb))
    ra.wait(lambda: rt(b).get("inflight") == 0, 30, 0.5)
    for i in range(3):
        before = cursor(b, pid, a.device_id)
        p0 = rt(b).get("pulls", 0)
        t0 = publish_change(a, i)
        t1 = wait_cursor(b, pid, a.device_id, before, 25)
        s1 = rt(b)
        if t1 and s1.get("pulls", 0) > p0:
            M["realtime_start_ms"].append(int(s1.get("last_pull_started_ms", 0) - t0 * 1000))
            M["realtime_apply_ms"].append(int((t1 - t0) * 1000))
        time.sleep(3)
    s1 = rt(b)
    check("5.26-31 a Realtime event starts B's cursor-based pull long before the 30 s polling",
          len(M["realtime_apply_ms"]) == 3 and max(M["realtime_apply_ms"]) < 15000 and s1.get("events", 0) >= 3,
          json.dumps({"start_ms": M["realtime_start_ms"], "applied_ms": M["realtime_apply_ms"], "events": s1.get("events"), "pulls": s1.get("pulls")}))
    # burst: several publications in a row
    xe.stop(a)
    ra.wait(lambda: rt(b).get("inflight") == 0, 30, 0.5)
    sb0 = rt(b)
    before = cursor(b, pid, a.device_id)
    for i in range(6):
        a.dev("--dev-prefs-publish-raw", a.mid, "intl.selected_languages", json.dumps(["ro,en", "hu,en", "cs,en", "sk,en", "sl,en", "hr,en"][i]))
    end = chain_end(ranges(a, pid), a.device_id)
    ra.wait(lambda: cursor(b, pid, a.device_id) >= end and rt(b).get("inflight") == 0, 40, 0.5)
    time.sleep(3)
    sb1 = rt(b)
    ev, pu, co = (sb1.get(k, 0) - sb0.get(k, 0) for k in ("events", "pulls", "coalesced"))
    M["rt"]["burst"] = {"events": ev, "pulls": pu, "coalesced": co, "max_inflight": sb1.get("max_inflight")}
    check("5.33 6 rapid events: coalesced (fewer pulls than events), at most 1 sync in flight + 1 pending",
          ev >= 6 and 1 <= pu < ev and co >= 1 and sb1.get("max_inflight") == 1 and cursor(b, pid, a.device_id) >= end > before,
          json.dumps(M["rt"]["burst"]))
    check("5.34 late/out-of-order hints are harmless: B's cursor = A's contiguous chain end, no gap",
          cursor(b, pid, a.device_id) == chain_end(ranges(a, pid), a.device_id), "")
    xe.start(a, fixtures=FIX)
    closed(a)  # A's own Preferences file catches up
    ui.stop()
    # duplicates
    nb = len([x for x in ranges(a, pid) if dev_of(x[2]) == b.device_id])
    ui = AppUI(b, {"SYNCHROME_DEV_REALTIME_DUPLICATE": "1"})
    ra.wait(lambda: rt(b).get("connected") is True and rt(b).get("pulls", 0) >= 1 and rt(b).get("inflight") == 0, 40, 0.5)
    time.sleep(3)
    sd0 = rt(b)
    before = cursor(b, pid, a.device_id)
    publish_change(a, 3)
    wait_cursor(b, pid, a.device_id, before, 25)
    time.sleep(4)
    sd1 = rt(b)
    nb2 = len([x for x in ranges(a, pid) if dev_of(x[2]) == b.device_id])
    dup = {k: sd1.get(k, 0) - sd0.get(k, 0) for k in ("events", "pulls", "coalesced")}
    M["rt"]["duplicate"] = dup
    check("5.32 duplicated Realtime events: each duplicate coalesced (no extra pull), 0 duplicate ops, B published nothing",
          dup["events"] >= 1 and dup["coalesced"] >= dup["events"] and 1 <= dup["pulls"] <= dup["events"] and nb2 == nb, json.dumps(dup))
    ui.stop()
    # lost events -> polling
    ui = AppUI(b, {"SYNCHROME_DEV_REALTIME_DROP": "1"})
    ra.wait(lambda: rt(b).get("connected") is True, 40, 0.5)
    time.sleep(3)
    before = cursor(b, pid, a.device_id)
    t0 = publish_change(a, 4)
    t1 = wait_cursor(b, pid, a.device_id, before, 75)
    if t1:
        M["polling_ms"].append(int((t1 - t0) * 1000))
    check("5.35 Realtime events lost: the 30 s polling still brings the change", t1 is not None, json.dumps({"polling_ms": M["polling_ms"]}))
    ui.stop()
    # disconnect / reconnect
    ui = AppUI(b)
    ra.wait(lambda: rt(b).get("connected") is True, 40, 0.5)
    flag = os.path.join(b.data, "realtime-disconnect")
    open(flag, "w").close()
    off = ra.wait(lambda: rt(b).get("connected") is False, 15, 0.5)
    before = cursor(b, pid, a.device_id)
    t0 = publish_change(a, 5)
    t1 = wait_cursor(b, pid, a.device_id, before, 75)
    if t1:
        M["polling_ms"].append(int((t1 - t0) * 1000))
    sx0 = rt(b)
    check("5.36 Realtime disconnected: polling keeps syncing", bool(off) and t1 is not None and sx0.get("connected") is False,
          json.dumps({"polling_ms": M["polling_ms"][-1:]}))
    os.remove(flag)
    re_ = ra.wait(lambda: rt(b).get("connected") is True and rt(b).get("connects", 0) == sx0.get("connects", 0) + 1, 90, 0.5)
    ra.wait(lambda: rt(b).get("pulls", 0) > sx0.get("pulls", 0) and rt(b).get("inflight") == 0, 30, 0.5)
    sx1 = rt(b)
    before = cursor(b, pid, a.device_id)
    publish_change(a, 6)
    wait_cursor(b, pid, a.device_id, before, 25)
    time.sleep(3)
    sx2 = rt(b)
    check("5.37 reconnect (backoff): catch-up sync, ONE subscription (one event per publication)",
          bool(re_) and sx1.get("pulls", 0) > sx0.get("pulls", 0) and sx2.get("events", 0) - sx1.get("events", 0) == 1,
          json.dumps({"reconnects": sx1.get("reconnects"), "connects": sx1.get("connects"), "events_per_publication": sx2.get("events", 0) - sx1.get("events", 0)}))
    M["rt"]["last_process"] = {k: sx2.get(k) for k in ("events", "own_events", "ignored_events", "coalesced", "pulls", "connects", "reconnects", "max_inflight")}
    ui.stop()
    a.dev("--dev-retire-device", pid, f2.get("device_id"))
    c_ = cloud(a)
    check("14.91 explicit retirement removes the blocker (count 0)", c_.get("gc_blockers") == 0, "")

    # ---------------------------------------------------------------- CASE 6 GC one generation behind
    for dv in (a, b):
        dv.dev("--dev-sync", dv.mid)
        dv.dev("--dev-report-cursors", dv.mid)
    t = time.time()
    while time.time() - t < 15 * 60:
        if cloud(a).get("journal_gc_ok"):
            break
        time.sleep(20)
    c_ = cloud(a)
    check("6.38 after the real 10 min server grace of G2: journal_gc_ok, GC watermarks = W(G1)",
          c_.get("journal_gc_ok") is True and c_.get("journal_gc_watermarks") == w1, json.dumps({"waited_s": int(time.time() - t)}))
    M["usage_before_gc"] = usage(a, pid)
    r_before = ranges(a, pid)
    gcr = a.dev("--dev-gc", a.mid).get("gc") or {}
    M["gc"] = gcr
    M["usage_after_gc"] = usage(a, pid)
    r_after = ranges(a, pid)
    eligible = [x for x in r_before if x[1] <= w1.get(dev_of(x[2]), 0)]
    kept = [x for x in r_before if x[1] > w1.get(dev_of(x[2]), 0)]
    check("6.39-42 GC deleted exactly the publications ≤ W(G1) (objects, then the store_events RPC); everything after intact",
          gcr.get("allowed") is True and gcr.get("events_deleted") == len(eligible) > 0 and gcr.get("objects_deleted", 0) >= len(eligible)
          and gcr.get("leftover_events") == 0 and r_after == kept,
          json.dumps({"gc": gcr, "eligible": len(eligible), "kept": len(kept)}))
    check("6.40 journal objects after W(G1) intact; snapshot objects of both generations untouched",
          M["usage_after_gc"].get("journal_objects") == M["usage_before_gc"].get("journal_objects", 0) - gcr.get("objects_deleted", -1)
          and M["usage_after_gc"].get("snapshot_objects") == M["usage_before_gc"].get("snapshot_objects") > 0,
          json.dumps(M["usage_after_gc"]))
    v1b = verify(a, 1)
    rb2p2 = a.dev("--dev-snapshot-rebuild", a.mid, "2", "prev")
    check("6.43 G1 is still a valid fallback: verifies, and G1 + the retained journals rebuild G2 exactly",
          v1b.get("parts_ok") and v1b.get("manifest_sha_ok") and part_shas(rb2p2.get("collections") or {}) == part_shas(v2.get("collections") or {}) != {}, "")

    # ---------------------------------------------------------------- CASE 7 A/B keep syncing
    bm_round(a, 3, 3)
    converge(b)
    ab = digest(a) == digest(b)
    bm_round(b, 4, 3)
    converge(a)
    rg = ranges(a, pid)
    devs_ = {dev_of(p) for _, _, p in rg}
    gaps = [dv for dv in devs_ if min(f for f, _, p in rg if dev_of(p) == dv) != w1.get(dv, 0) + 1
            or chain_end(rg, dv, w1.get(dv, 0)) != max(t_ for _, t_, p in rg if dev_of(p) == dv)]
    check("7.44-48 after the GC: A → B and B → A converge; retained chains start right after W(G1), no gaps",
          ab and digest(a) == digest(b) and not gaps, json.dumps({"gaps": len(gaps)}))

    # ---------------------------------------------------------------- CASE 8 C from zero
    c = joiner("C", creds, pid)
    t0 = time.time()
    xe.start(c, fixtures=FIX)
    tsnap = wait_digest(c, a)
    M["bootstrap_snapshot_ms"] = int((time.time() - t0) * 1000) if tsnap is not None else None
    ci = local(c)
    M["bootstrap_import_ms"] = ci.get("bootstrap_ms")
    xe.pull(c)
    check("8.49-61 C (new, same user, no local state) bootstraps from G2 (manifest + parts verified) + tail, cursors ≥ W(G2), converges exactly",
          tsnap is not None and ci.get("snapshot_generation") == 2 and ci.get("bootstrap_fallback") is False and digest(c) == digest(a)
          and cursor(c, pid, a.device_id) >= w2.get(a.device_id, 0),
          json.dumps({"ms": M["bootstrap_snapshot_ms"], "import_ms": ci.get("bootstrap_ms")}))
    tc = bar_titles(c)
    check("8.60 + 11.78 C's browser holds A's bookmarks bar; deleted items are not resurrected",
          tc == bar_titles(a) and not any(x.endswith(" item 1") for x in tc), "")
    c.dev("--dev-report-cursors", c.mid)

    # ---------------------------------------------------------------- CASE 9 compare
    d.dev("--dev-sync", d.mid)
    check("9.65-66 full replay (D) and snapshot bootstrap (C) end in the identical shared state",
          digest(d) == digest(c) == digest(a), json.dumps({"full_ms": M["bootstrap_full_ms"], "snapshot_ms": M["bootstrap_snapshot_ms"]}))

    # ---------------------------------------------------------------- CASE 10 fallback previous
    e = joiner("E", creds, pid, {"SYNCHROME_DEV_CORRUPT_SNAPSHOT_GEN": "2"})
    t0 = time.time()
    xe.start(e, fixtures=FIX)
    te = wait_digest(e, a)
    M["bootstrap_previous_ms"] = int((time.time() - t0) * 1000) if te is not None else None
    ei = local(e)
    check("10.67-73 current G2 corrupted in transit (debug): integrity fails → previous G1 + retained journals → identical digest",
          te is not None and ei.get("snapshot_generation") == 1 and ei.get("bootstrap_fallback") is True and digest(e) == digest(a),
          json.dumps({k: ei.get(k) for k in ("snapshot_generation", "bootstrap_fallback", "bootstrap_ms")}))
    e.dev("--dev-report-cursors", e.mid)
    xe.stop(e)
    e2 = joiner("E2", creds, pid, {"SYNCHROME_DEV_CORRUPT_SNAPSHOT_GEN": "1,2"})
    xe.start(e2, fixtures=FIX)
    e2.dev("--dev-sync", e2.mid)
    time.sleep(3)
    e2.dev("--dev-sync", e2.mid)
    bs = e2.dev("--dev-bookmarks-status", e2.mid).get("status") or {}
    dl = bs.get("download") or {}
    check("10.74 both generations corrupted: nothing imported (no cursors, no snapshot), error reported",
          cursor(e2, pid, a.device_id) == 0 and not local(e2).get("snapshot_generation") and dl.get("state") == "error" and "snapshot integrity" in (dl.get("error") or ""),
          json.dumps({"state": dl.get("state"), "error": (dl.get("error") or "")[:60]}))
    xe.stop(e2)
    # a mapped device that never reported snapshot support (stuck) blocks journal GC: as the UI
    # suggests, the user removes it
    blk = cloud(a).get("gc_blockers")
    M["blockers_seen"].add(blk)
    e2.dev("--dev-cleanup", e2.mid)
    e2.dev("--dev-delete-this-device")
    check("10.74b the stuck device (never reported) is a GC blocker until it is removed",
          blk == 1 and cloud(a).get("gc_blockers") == 0, json.dumps({"blockers_before_removal": blk}))

    # ---------------------------------------------------------------- CASE 13 dormant/retired device returns
    xe.start(r, fixtures=FIX)
    trr = wait_digest(r, a)
    r.dev("--dev-report-cursors", r.mid)
    ri = local(r)
    rr = rows(a, pid)
    check("13.87 the retired device returns behind compacted + collected history: re-bootstraps (G2) and converges; it STAYS retired (a report never clears retired_at)",
          trr is not None and ri.get("snapshot_generation") == 2 and digest(r) == digest(a) and rr.get(r.device_id, {}).get("retired") is True
          and cloud(a).get("gc_blockers") == 0,
          json.dumps({"snapshot_generation": ri.get("snapshot_generation"), "retired": rr.get(r.device_id, {}).get("retired")}))
    a.dev("--dev-reactivate-device", pid, r.device_id)
    rr = rows(a, pid)
    check("14 explicit reactivation clears retired_at (user action), and only that",
          rr.get(r.device_id, {}).get("retired") is False and rr.get(r.device_id, {}).get("retired_at") is None, "")
    check("13.86 a device that simply stopped reporting (D) is never retired automatically", rr.get(d.device_id, {}).get("retired") is False, "")
    xe.stop(r)

    # ---------------------------------------------------------------- CASE 15 sync_cursors
    names = {a.device_id: "A", b.device_id: "B", c.device_id: "C", d.device_id: "D", e.device_id: "E", r.device_id: "R"}
    check("15.92 A/B/C report applied, published_through, snapshot_generation, snapshot_protocol = 1",
          all(rr.get(x, {}).get("snapshot_protocol") == 1 and rr.get(x, {}).get("applied") for x in (a.device_id, b.device_id, c.device_id))
          and rr.get(c.device_id, {}).get("snapshot_generation") == 2,
          json.dumps({names[x]: {k: rr.get(x, {}).get(k) for k in ("snapshot_protocol", "snapshot_generation", "published_through", "retired")} for x in names}))
    a.dev("--dev-report-cursors", a.mid)
    u0 = rows(a, pid).get(a.device_id, {}).get("updated_at")
    s1_ = a.dev("--dev-report-cursors", a.mid)
    s2_ = a.dev("--dev-report-cursors", a.mid)
    u1 = rows(a, pid).get(a.device_id, {}).get("updated_at")
    check("15.93-95 reports only when changed (nothing sent, server updated_at unchanged); retired_at untouched without an explicit action",
          s1_.get("sent") is False and s2_.get("sent") is False and u0 == u1 and u0
          and all(rr.get(x, {}).get("retired") is False for x in (a.device_id, b.device_id, c.device_id)), "")

    # ---------------------------------------------------------------- CASE 18 restart A and B apps; CASE 22 UI
    ua, ub = AppUI(a), AppUI(b)
    ok = ra.wait(lambda: rt(a).get("connected") and rt(b).get("connected") and rt(a).get("pulls", 0) >= 1 and rt(b).get("pulls", 0) >= 1
                 and rt(a).get("inflight") == 0 and rt(b).get("inflight") == 0, 60, 0.5)
    time.sleep(3)
    ev0 = usage(a, pid).get("store_events")
    cur0 = rows(a, pid)
    ua.stop()
    ub.stop()
    ua, ub = AppUI(a), AppUI(b)
    ok2 = ra.wait(lambda: rt(a).get("connected") and rt(b).get("connected") and rt(a).get("pulls", 0) >= 1 and rt(b).get("pulls", 0) >= 1
                  and rt(a).get("inflight") == 0 and rt(b).get("inflight") == 0, 60, 0.5)
    time.sleep(3)
    cur1 = rows(a, pid)
    ev1 = usage(a, pid).get("store_events")
    check("18.101-107 restart of A's and B's apps: Realtime reconnects (1 connection each) + catch-up; cursors persist; no duplicates",
          bool(ok) and bool(ok2) and rt(a).get("connects") == 1 and rt(b).get("connects") == 1 and ev1 == ev0
          and all(cur1.get(x, {}).get("applied") == cur0.get(x, {}).get("applied") for x in (a.device_id, b.device_id)) and digest(a) == digest(b),
          json.dumps({"store_events": [ev0, ev1]}))
    ha = ua.health()
    check("22 Sync health (A, after GC): Realtime connected, generation 2, 2 kept, journals cleaned, not blocked, no ids",
          len(ha) == 1 and "Realtime connected" in ha[0] and "generation 2" in ha[0] and "2 kept" in ha[0] and "journals cleaned" in ha[0]
          and "paused" not in ha[0] and not UUID.search(ha[0]), json.dumps(ha))
    xe.restart(c, fixtures=FIX)
    uc = AppUI(c)
    hc = uc.health()
    uc.stop()
    ue = AppUI(e)
    he = ue.health()
    ue.stop()
    check("22 Sync health shows 'started from snapshot generation 2' (C) and 'previous generation' (E)",
          len(hc) == 1 and "started from snapshot generation 2." in hc[0] and len(he) == 1 and "generation 1 (previous generation" in he[0]
          and not UUID.search(hc[0] + he[0]), json.dumps({"C": hc, "E": he}))
    c.dev("--dev-sync", c.mid)
    ci2 = local(c)
    check("18 restart of C: snapshot generation and bootstrap persisted (no second import), still converged",
          ci2.get("bootstrapped_at") == ci.get("bootstrapped_at") and ci2.get("snapshot_generation") == 2 and digest(c) == digest(a), "")

    # ---------------------------------------------------------------- CASE 19 logout / login in B's app
    ua.stop()
    so = ui_sign_out(ub)
    off = ra.wait(lambda: rt(b).get("connected") is False and rt(b).get("account_set") is False, 20, 0.5)
    sx = rt(b)
    check("19.108-110 logout in the app: Realtime connection closed, nothing subscribed, Keychain empty",
          so is True and bool(off) and not h.keychain_has_session(), "")
    si = ui_sign_in(ub, creds, "A")
    on = ra.wait(lambda: rt(b).get("connected") is True and rt(b).get("connects", 0) == sx.get("connects", 0) + 1, 60, 0.5)
    ra.wait(lambda: rt(b).get("inflight") == 0, 30, 0.5)
    time.sleep(2)
    s0 = rt(b)
    before = cursor(b, pid, a.device_id)
    publish_change(a, 7)
    wait_cursor(b, pid, a.device_id, before, 25)
    time.sleep(3)
    s1 = rt(b)
    acc = a.dev("--dev-account")
    check("19.111-112 login again in the app (exact id): exactly one new connection, one event per publication",
          si and bool(on) and acc.get("user_id") == h.EXPECTED["A"] and s1.get("events", 0) - s0.get("events", 0) == 1,
          json.dumps({"connects": s1.get("connects"), "events_per_publication": s1.get("events", 0) - s0.get("events", 0)}))
    ub.stop()

    # ---------------------------------------------------------------- CASE 16 refusal + CASE 20 aux profile delete
    p = device("P")
    p.dev("--dev-sign-in", creds, "A")
    p.device_id = p.dev("--dev-register-device").get("device_id")
    mp = p.dev("--dev-create-mapping", f"E2E AUX {os.urandom(3).hex()}", "helium", "Default")
    p.mid, ppid = mp.get("mapping_id"), mp.get("profile_id")
    p.dev("--dev-write-token", p.mid, p.udd)
    p.dev("--dev-install-host", p.udd)
    xe.start(p, fixtures=FIX)
    bm_round(p, 1, 4)
    p.dev("--dev-report-cursors", p.mid)
    pg1 = p.dev("--dev-compact", p.mid, "force").get("outcome") or {}
    bm_round(p, 2, 4)
    p.dev("--dev-sync", p.mid)
    p.dev("--dev-report-cursors", p.mid)
    pu0, pr0 = usage(p, ppid), ranges(p, ppid)
    p.env["SYNCHROME_DEV_TAMPER_WATERMARK"] = "1"
    rej = p.dev("--dev-compact", p.mid, "force").get("outcome") or {}
    p.env.pop("SYNCHROME_DEV_TAMPER_WATERMARK")
    pu1, pr1 = usage(p, ppid), ranges(p, ppid)
    again = p.dev("--dev-compact", p.mid, "force").get("outcome") or {}
    prow = {g["generation"]: g["state"] for g in (p.dev("--dev-snapshot-rows", ppid).get("rows") or [])}
    check("16.96-97 a watermark the database refuses: compaction aborts (rejected), journals intact, no automatic correction (claim held → busy)",
          pg1.get("outcome") == "ready" and rej.get("outcome") == "rejected" and pr1 == pr0 and pu1.get("journal_objects") == pu0.get("journal_objects")
          and again.get("outcome") == "busy" and prow == {1: "ready", 2: "building"},
          json.dumps({"rejected": rej.get("outcome"), "reason": (rej.get("reason") or "")[:60], "next": again.get("outcome"), "rows": prow}))
    xe.stop(p)
    pu = usage(p, ppid)
    um = p.dev("--dev-unmap", p.mid)
    pdel = p.dev("--dev-delete-profile", ppid)
    after = (usage(a, ppid), a.dev("--dev-cursor-rows", ppid).get("rows"), a.dev("--dev-snapshot-rows", ppid).get("rows"))
    check("20.113-117 aux profile (journals + ready G1 + live claim) deleted: objects allowed → row (cascades) → remaining objects; nothing left",
          pdel.get("deleted_profile") is True and pdel.get("leftovers") == 0 and pdel.get("objects_after_row", 0) > 0
          and pdel.get("objects_before_row", 0) + pdel.get("objects_after_row", 0) == pu.get("objects")
          and after[0].get("objects") == 0 and (after[0].get("store_events") or 0) == 0 and not after[1] and not after[2],
          json.dumps({"before": {k: pu.get(k) for k in ("journal_objects", "snapshot_objects", "store_events")},
                      "phase1": pdel.get("objects_before_row"), "phase3": pdel.get("objects_after_row"), "leftovers": pdel.get("leftovers"),
                      "unmapped": "error" not in um}))
    p.dev("--dev-delete-this-device")
    check("20.117 the main E2E profile is untouched", usage(a, pid).get("snapshot_objects", 0) > 0 and digest(a) == digest(b), "")

    # ---------------------------------------------------------------- security
    for dv in (a, b, c, d, e, e2, r, p):
        lg_ = dv.log()
        check(f"S.127 device {dv.name} host log: no tokens/JWTs, no titles/URLs, no setting/preference values",
              h.secret_free(lg_) and not any(x in lg_ for x in SENSITIVE), "")
    uil = "".join(open(os.path.join(ra.WORK, f)).read() for f in os.listdir(ra.WORK) if f.startswith("ui-app-"))
    check("S.127 app process logs: no tokens/JWTs, no titles/URLs, no values", h.secret_free(uil) and "eyJ" not in uil and not any(x in uil for x in SENSITIVE), "")
    rtxt = "".join(open(os.path.join(dv.data, "realtime-status.json")).read() for dv in (a, b, c, e) if os.path.exists(os.path.join(dv.data, "realtime-status.json")))
    check("S.125 realtime status files: counters only (no token, no ids)", bool(rtxt) and "eyJ" not in rtxt and not UUID.search(rtxt), "")
    ext_src = "".join(open(os.path.join(h.EXT, f)).read() for f in os.listdir(h.EXT) if f.endswith((".js", ".json", ".html")))
    ext_code = re.sub(r"(?m)^\s*//.*$", "", ext_src)  # comments are not code
    check("S.126 the Synchrome extension: no Supabase, no Realtime/WebSocket, no service role, no JWT",
          not re.search(r"supabase|websocket|realtime|service_role|eyJ", ext_code, re.I), "")

    # ---------------------------------------------------------------- metrics
    for dv in (a, b, c, d, e, e2, r):
        try:
            xe.stop(dv)
        except Exception:  # noqa: BLE001
            pass
    ub_, ug, ua_ = M["usage_before_compaction"], M["usage_before_gc"], M["usage_after_gc"]
    print("METRICS " + json.dumps({
        "objects_before_compaction": ub_.get("objects"), "store_events_before_compaction": ub_.get("store_events"),
        "journal_bytes_before_compaction": ub_.get("journal_bytes"),
        "objects_before_gc": ug.get("objects"), "objects_after_gc": ua_.get("objects"),
        "store_events_before_gc": ug.get("store_events"), "store_events_after_gc": ua_.get("store_events"),
        "journal_bytes_before_gc": ug.get("journal_bytes"), "journal_bytes_after_gc": ua_.get("journal_bytes"),
        "snapshot_bytes_stored_after_gc": ua_.get("snapshot_bytes"),
        "snapshot_bytes": M["snapshot_bytes"], "compaction_ms": M["compaction_ms"],
        "bootstrap_full_replay_ms": M["bootstrap_full_ms"], "bootstrap_snapshot_ms": M["bootstrap_snapshot_ms"],
        "bootstrap_snapshot_import_ms": M["bootstrap_import_ms"], "bootstrap_previous_fallback_ms": M["bootstrap_previous_ms"],
        "realtime_publish_to_pull_start_ms": M["realtime_start_ms"], "realtime_publish_to_applied_ms": M["realtime_apply_ms"],
        "polling_fallback_ms": M["polling_ms"], "burst": M["rt"].get("burst"), "duplicate": M["rt"].get("duplicate"),
        "last_app_process_counters": M["rt"].get("last_process"), "gc": M["gc"],
        "blockers_observed": sorted(x for x in M["blockers_seen"] if x is not None),
    }))

    # ---------------------------------------------------------------- CASE 21 USER_B / RLS
    snap_path = f"profiles/{pid}/snapshots/0000000002/manifest.json"
    first_chunk = (a.dev("--dev-cloud-tree", pid).get("paths") or [""])[0]
    u_pre = usage(a, pid)
    x = device("X")
    u = x.dev("--dev-sign-in", creds, "B")
    check("21.118 login USER_B (exact id)", u.get("user_id") == h.EXPECTED["B"], f"user_id={u.get('user_id')}")
    pr = x.dev("--dev-compaction-probe-foreign", h.EXPECTED["A"], pid, a.device_id, snap_path) if u.get("signed_in") else {}
    pf = x.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, first_chunk) if u.get("signed_in") else {}
    # that (slice 7) probe prints Rust Results: {"Ok": v} / {"Err": why}
    pf = {k: (v["Ok"] if isinstance(v, dict) and "Ok" in v else ("refused" if isinstance(v, dict) and "Err" in v else v)) for k, v in pf.items()}
    check("21.119-120 USER_B: no snapshots/cursors/status/watermarks of A; cannot claim, mark ready/superseded, retire, GC, write cursors, delete rows",
          pr.get("snapshot_rows_visible") == 0 and pr.get("status_sees_generation") is False and pr.get("status_blockers") in (0, None)
          and pr.get("cursor_rows_visible") == 0 and pr.get("gc_refused") and pr.get("claim_refused") and pr.get("retire_refused")
          and pr.get("mark_ready_refused") and pr.get("supersede_refused_or_noop") and pr.get("cursor_write_refused")
          and pr.get("snapshot_row_delete") in (False, None), json.dumps({k: v for k, v in pr.items() if k != "as_user"}))
    check("21.119 USER_B: cannot read A's snapshot objects / store_events / chunks, write under A's prefix, delete A's journals, act on A's profile",
          pr.get("snapshot_object_readable") is False and pr.get("snapshot_write_refused") and pr.get("store_events_visible") in (0, None)
          and pr.get("journal_delete_count") in (0, None) and pr.get("profile_rename_refused") and pr.get("profile_delete_refused")
          and pf.get("store_events_sees") == 0 and pf.get("foreign_profile_visible") is False and pf.get("get_first_chunk") in (False, "refused")
          and pf.get("list_sees") in (0, "refused") and pf.get("list_foreign_prefix_sees") == 0 and pf.get("put_into_foreign_profile") != "accepted"
          and pf.get("store_event_insert") != "accepted" and pf.get("put_under_foreign_prefix") != "accepted" and pf.get("overwrite_foreign_chunk") != "accepted",
          json.dumps({k: v for k, v in pf.items() if k != "as_user"}))
    x.dev("--dev-sign-out")
    a.dev("--dev-sign-in", creds, "A")
    u_post = usage(a, pid)
    check("21 A's cloud data intact after the probes", u_pre == u_post and len(gens(a)) == 2, "")

    # ---------------------------------------------------------------- cleanup
    for dv in (b, c, d, e, r):
        dv.dev("--dev-cleanup", dv.mid)
        dv.dev("--dev-delete-this-device")
    for fx in (f1, f2):
        a.dev("--dev-delete-device-id", fx.get("device_id"))
    ra_ = a.dev("--dev-cleanup", a.mid)
    gone = usage(a, pid)
    inv = a.dev("--dev-inventory")
    check("C.1 cloud: profile deleted (objects allowed → row/cascades → remaining objects); 0 objects, 0 store_events, 0 cursors/snapshots; no devices/mappings",
          ra_.get("deleted_profile") and ra_.get("leftover_objects") == 0 and gone.get("objects") == 0 and (gone.get("store_events") or 0) == 0
          and not a.dev("--dev-cursor-rows", pid).get("rows") and not a.dev("--dev-snapshot-rows", pid).get("rows")
          and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [], json.dumps({"A": ra_}))
    so = a.dev("--dev-sign-out")
    check("C.2 Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in DEVS.values():
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    for f in os.listdir(ra.WORK):
        if f.startswith(("ui-driver", "ui-app")):
            pth = os.path.join(ra.WORK, f)
            shutil.rmtree(pth, ignore_errors=True) if os.path.isdir(pth) else os.remove(pth)
    check("C.3 local: data dirs (state.sqlite, compaction_state, logs, backups), Helium profiles, manifests, UI files removed",
          not any(os.path.exists(p_) for dv in DEVS.values() for p_ in (dv.udd, dv.data)), json.dumps(os.listdir(ra.WORK)))


def emergency_cleanup():
    print("!! aborted: emergency cleanup")
    for p_ in cf.UI_PROCS:
        if p_.poll() is None:
            p_.kill()
    for dv in DEVS.values():
        try:
            if dv.hl and dv.hl.p.poll() is None:
                dv.hl.p.kill()
        except Exception:  # noqa: BLE001
            pass
    creds = sys.argv[1]
    a = DEVS.get("A")
    if not a:
        return
    a.dev("--dev-sign-in", creds, "A")
    for n, dv in DEVS.items():
        if n in ("A", "L", "X") or not dv.mid:
            continue
        print(f"   {n}:", json.dumps(dv.dev("--dev-cleanup", dv.mid)), json.dumps(dv.dev("--dev-delete-this-device")))
    for fx in FXS:
        a.dev("--dev-delete-device-id", fx)
    if a.mid:
        print("   A:", json.dumps(a.dev("--dev-cleanup", a.mid)))
    inv = a.dev("--dev-inventory")
    print("   A inventory:", json.dumps({k: (len(v) if isinstance(v, list) else v) for k, v in inv.items() if k in ("profiles", "mappings", "devices")}))
    a.dev("--dev-sign-out")


if __name__ == "__main__":
    try:
        main(sys.argv[1])
    except Exception as ex:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        check("harness completed without exception", False, type(ex).__name__ + ": " + str(ex)[:200])
        emergency_cleanup()
    for p_ in cf.UI_PROCS:
        if p_.poll() is None:
            p_.kill()
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== compaction: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
