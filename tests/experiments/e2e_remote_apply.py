"""Two-device E2E of cloud -> browser apply (M1 slice 8). Disposable profiles only.

Device A and device B are two simulated installations on this machine: each has its own
SYNCHROME_DATA_DIR (device.json => its own device id, state.sqlite, token registry, logs)
and its own disposable Helium --user-data-dir. Both sign in to the SAME test account (the
Keychain item is shared, which is fine for one account). Edits are made through CDP inside
each extension's own worker. Prints ids, counts, titles of the test fixture only.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_remote_apply.py <creds-env-file>
"""
import json, os, re, shutil, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402

check = h.check
WORK = "/tmp/synchrome-e2e"
STATE = {"A": None, "B": None, "C": None}


class Device:
    def __init__(self, name):
        self.name = name
        self.data = os.path.join(WORK, f"dev{name}-data")
        self.udd = os.path.join(WORK, f"dev{name}-helium")
        shutil.rmtree(self.data, ignore_errors=True)
        shutil.rmtree(self.udd, ignore_errors=True)
        os.makedirs(self.udd)
        self.env = {"SYNCHROME_DATA_DIR": self.data}
        self.hl = None
        self.mid = None
        self.device_id = None
        STATE[name] = self

    def dev(self, *args):
        e = dict(os.environ)
        e.update(self.env)
        p = subprocess.run([h.APP, *args], capture_output=True, text=True, timeout=120, env=e)
        line = (p.stdout.strip().splitlines() or ["{}"])[-1]
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            return {"error": f"non-json output (rc={p.returncode})"}

    def start(self, offline=False):
        env = dict(self.env)
        if offline:
            env["SUPABASE_URL"] = eb.OFFLINE_URL
        self.hl = eb.Helium(self.udd, env=env)
        return self.hl

    def stop(self):
        if self.hl:
            self.hl.stop()
            self.hl = None

    def log(self):
        p = os.path.join(self.data, "logs", "native-host.log")
        return open(p, "rb").read().decode("utf-8", "replace") if os.path.exists(p) else ""

    def tree(self):
        return self.hl.js("""(async () => {
          const [root] = await chrome.bookmarks.getTree();
          const bar = root.children.find(c => c.folderType === "bookmarks-bar" || c.id === "1");
          const f = (n) => ({t: n.title, u: n.url ?? null, c: (n.children || []).map(f)});
          return (bar.children || []).map(f);
        })()""")

    def status(self):
        return self.dev("--dev-bookmarks-status", self.mid).get("status") or {}

    def idle(self):
        return bool(self.hl.js("queue.length === 0 && running === null && flushTimer === null"))


def wait(pred, timeout=90, step=0.5):
    end = time.time() + timeout
    while time.time() < end:
        r = pred()
        if r:
            return r
        time.sleep(step)
    return None


def edit_and_publish(a, js):
    t = eb.now_ms()
    a.hl.js(f"(async () => {{ {js}; return true; }})()")
    st = a.hl.wait(lambda s: eb.up_to_date(s) and s.get("bookmarks_at", 0) >= t, 60, after=t)
    wait(a.idle, 30)
    return st


def b_pull(b):
    """Trigger B's extension run (pull + apply + reconcile), wait for it to finish."""
    t = time.time()
    b.hl.js("run(true)", timeout=120)
    wait(b.idle, 60)
    s = b.status()
    return s, int((time.time() - t) * 1000)


def find(title):
    return eb.find(title)


def flat(tree, d=0):
    out = []
    for n in tree:
        out.append((d, n["t"], n["u"]))
        out += flat(n["c"], d + 1)
    return out


def main(creds):
    h.prepare_binary()
    os.makedirs(WORK, exist_ok=True)
    a, b = Device("A"), Device("B")
    print("=== two-device remote apply E2E ===")
    check("precondition: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())

    # 1. device A
    s = a.dev("--dev-sign-in", creds, "A")
    check("1a login (device A) with the exact user id", s.get("signed_in") and s.get("user_id") == h.EXPECTED["A"], f"user_id={s.get('user_id')}")
    if not s.get("signed_in"):
        return
    a.device_id = a.dev("--dev-register-device").get("device_id")
    m = a.dev("--dev-create-mapping", f"E2E RA {os.urandom(3).hex()}", "helium", "Default")
    a.mid, pid = m.get("mapping_id"), m.get("profile_id")
    a.dev("--dev-write-token", a.mid, a.udd)
    a.dev("--dev-install-host", a.udd)
    t = eb.now_ms()
    a.start()
    st = a.hl.wait(lambda s: s.get("status") == "connected" and s.get("token_state") == "confirmed", 60, after=t)
    check("1b device A mapping confirmed end-to-end", st is not None, f"device={a.device_id} mapping={a.mid}")

    # 2. device B (same account, its own device id, same cloud profile)
    s = b.dev("--dev-sign-in", creds, "A")
    b.device_id = b.dev("--dev-register-device").get("device_id")
    m = b.dev("--dev-map-existing", pid, "helium", "Default")
    b.mid = m.get("mapping_id")
    b.dev("--dev-write-token", b.mid, b.udd)
    b.dev("--dev-install-host", b.udd)
    t = eb.now_ms()
    b.start()
    st = b.hl.wait(lambda s: s.get("status") == "connected" and s.get("token_state") == "confirmed", 60, after=t)
    wait(b.idle, 30)
    check("2 device B (own device id) mapped to the same cloud profile and confirmed",
          st is not None and b.device_id and b.device_id != a.device_id and m.get("profile_id") == pid,
          f"device={b.device_id} mapping={b.mid}")

    # 3–4. A creates and publishes a nested structure
    st = edit_and_publish(a, """
      const t = await chrome.bookmarks.create({parentId: "1", title: "Synchrome Test"});
      const fa = await chrome.bookmarks.create({parentId: t.id, title: "Folder A"});
      await chrome.bookmarks.create({parentId: fa.id, title: "Bookmark 1", url: "https://example.com/1"});
      await chrome.bookmarks.create({parentId: fa.id, title: "Bookmark 2", url: "https://example.com/2"});
      const fb = await chrome.bookmarks.create({parentId: t.id, title: "Folder B"});
      const n = await chrome.bookmarks.create({parentId: fb.id, title: "Nested"});
      await chrome.bookmarks.create({parentId: n.id, title: "Bookmark 3", url: "https://example.com/3"})""")
    c = a.dev("--dev-cloud-tree", pid)
    check("3-4 A created and published the structure (7 ops)", eb.up_to_date(st) and c.get("ops") == 7, f"ops={c.get('ops')}")

    # 5–8. B downloads, applies, matches, and echoes nothing
    mark = len(b.log())
    s, ms = b_pull(b)
    log_b = b.log()[mark:]
    d = s.get("download") or {}
    check("5-6 B downloaded the chunk and applied it (explicit ops)",
          "bookmarks apply plan ops=" in log_b and "create_bookmark" in log_b and d.get("state") == "up_to_date" and d.get("remote_pending") == 0,
          f"{ms} ms; " + "; ".join(re.findall(r"apply plan ops=\d+ \{[^}]*\}", log_b)))
    check("7 tree B == tree A", b.tree() == a.tree(), json.dumps(flat(b.tree())))
    cur = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    events_from_b = lambda: sum(1 for e in a.dev("--dev-cloud-tree", pid).get("store_event_ranges", []) if f"/journal/{b.device_id}/" in e[2])
    check("8 B uploads no echoes (post-apply reconcile = 0 ops, nothing pending, no journal from B)",
          cur == 7 and s.get("pending") == 0 and events_from_b() == 0 and "Applied { ops: 0" in log_b,
          f"cursor={cur} pending={s.get('pending')}")

    def step(label, js, expect_kinds):
        st = edit_and_publish(a, js)
        mark = len(b.log())
        s, ms = b_pull(b)
        lg = b.log()[mark:]
        plans = re.findall(r"apply plan ops=\d+ \{([^}]*)\}", lg)
        kinds_ok = any(all(k in p for k in expect_kinds) for p in plans)
        check(label, eb.up_to_date(st) and kinds_ok and b.tree() == a.tree() and s.get("pending") == 0
              and (s.get("download") or {}).get("state") == "up_to_date",
              f"{ms} ms; plans={plans}")
        return ms

    lat = {}
    lat["rename"] = step("9-10 rename on A -> B receives a rename op, trees equal",
                         f"await chrome.bookmarks.update(({find('Bookmark 1')}).id, {{title: 'Bookmark 1 renamed'}})", ['"rename"'])
    lat["move"] = step("11-12 move on A -> B receives move + reorder, trees equal",
                       f"await chrome.bookmarks.move(({find('Bookmark 2')}).id, {{parentId: ({find('Folder B')}).id, index: 0}})", ['"move"'])
    lat["reorder"] = step("13-14 reorder on A -> B receives reorder, trees equal",
                          f"await chrome.bookmarks.move(({find('Nested')}).id, {{parentId: ({find('Folder B')}).id, index: 0}})", ['"reorder"'])
    lat["delete"] = step("15-16 delete folder with children on A -> B deletes child-first, trees equal",
                         f"await chrome.bookmarks.removeTree(({find('Nested')}).id)", ['"delete": 2'])
    check("8b still no journal from B after all applies", events_from_b() == 0, "")

    # 17–20. B offline, A makes 2 changes, B comes back and applies both in order
    b.stop()
    edit_and_publish(a, f"const fa = {find('Folder A')}; await chrome.bookmarks.create({{parentId: fa.id, title: 'While B offline', url: 'https://example.org/off'}})")
    edit_and_publish(a, f"await chrome.bookmarks.update(({find('While B offline')}).id, {{title: 'While B offline v2'}})")
    a_max = a.dev("--dev-cloud-tree", pid).get("max_seq")
    t = eb.now_ms()
    b.start()  # startup run: snapshot, pull from checkpoint, apply
    wait(lambda: b.status().get("download", {}).get("state") == "up_to_date" and b.idle(), 90)
    cur = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    flat_b = flat(b.tree())
    check("17-20 B was offline; on return it downloaded from its checkpoint and applied both changes in order",
          b.tree() == a.tree() and cur == a_max and any(r[1] == "While B offline v2" for r in flat_b) and not any(r[1] == "While B offline" for r in flat_b),
          f"cursor={cur} a_max={a_max}")

    # 21–24. conflict: B changes X while offline (pending), A changes X, B must not overwrite
    b.stop()
    t = eb.now_ms()
    b.start(offline=True)
    b.hl.wait(lambda s: s.get("status") == "connected_offline", 60, after=t)
    t = eb.now_ms()
    b.hl.js(f"(async () => {{ await chrome.bookmarks.update(({find('Bookmark 2')}).id, {{title: 'B local edit'}}); return true; }})()")
    b.hl.wait(lambda s: eb.bm(s).get("pending", 0) >= 1, 60, after=t)
    b.stop()
    check("21 B (offline) changed entity X locally; change pending in its outbox", b.status().get("pending") == 1, "")
    edit_and_publish(a, f"await chrome.bookmarks.update(({find('Bookmark 2')}).id, {{title: 'A remote edit'}})")
    b.start()
    wait(lambda: b.idle() and (b.status().get("download") or {}).get("state") in ("conflict", "up_to_date"), 90)
    s = b.status()
    conf = b.dev("--dev-conflicts", pid).get("conflicts", [])
    titles_b = [r[1] for r in flat(b.tree())]
    check("22-23 A changed X too: B did NOT overwrite its local value",
          "B local edit" in titles_b and "A remote edit" not in titles_b, json.dumps(titles_b))
    check("24 B state = Conflict (local_pending conflict on X), its own change still published",
          (s.get("download") or {}).get("state") == "conflict" and (s.get("download") or {}).get("conflicts") == 1
          and any(c["kind"] == "local_pending" for c in conf) and s.get("pending") == 0, json.dumps({"download": s.get("download"), "conflicts": conf}))

    # account isolation (user B of the test pair)
    cdev = Device("C")
    u = cdev.dev("--dev-sign-in", creds, "B")
    probe = cdev.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, (a.dev("--dev-cloud-tree", pid).get("paths") or ["x"])[0]) if u.get("signed_in") else {}
    check("RLS: another account sees no objects / events of the profile and cannot write",
          u.get("user_id") == h.EXPECTED["B"] and probe.get("list_sees") == {"Ok": 0} and probe.get("store_events_sees") == {"Ok": 0}
          and probe.get("get_first_chunk") == {"Ok": False} and "Err" in json.dumps(probe.get("put_under_foreign_prefix")), "")
    a.dev("--dev-sign-in", creds, "A")

    # security: logs of both devices
    for dv in (a, b):
        lg = dv.log()
        leaks = [w for w in ("example.com", "example.org", "Bookmark", "Synchrome Test", "Folder", "While B", "local edit", "remote edit") if w in lg]
        check(f"S logs of device {dv.name}: no URLs, titles or tokens", not leaks and h.secret_free(lg), f"leaks={leaks}")
    print("LATENCY_MS " + json.dumps(lat))

    # 25. cleanup
    a.stop()
    b.stop()
    rb = b.dev("--dev-cleanup", b.mid)
    rbd = b.dev("--dev-delete-this-device")
    ra = a.dev("--dev-cleanup", a.mid)
    gone = a.dev("--dev-cloud-tree", pid)
    inv = a.dev("--dev-inventory")
    check("25a cleanup: objects, store_events, profile, both mappings, both devices removed",
          rb.get("unmapped") and rbd.get("deleted") and ra.get("unmapped") and ra.get("deleted_profile") and ra.get("device_deleted")
          and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [],
          json.dumps({"B": rb, "B_device": rbd, "A": ra}))
    so = a.dev("--dev-sign-out")
    check("25b Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, cdev):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    check("25c disposable profiles, host manifests, per-device data dirs (state.sqlite, registry, logs) removed",
          not any(os.path.exists(p) for dv in (a, b, cdev) for p in (dv.udd, dv.data)), "")


def emergency():
    print("!! aborted: emergency cleanup")
    for dv in (STATE["A"], STATE["B"]):
        if dv and dv.hl and dv.hl.p.poll() is None:
            dv.hl.p.kill()
    if STATE["B"] and STATE["B"].mid:
        STATE["B"].dev("--dev-sign-in", sys.argv[1], "A")
        print("   B:", STATE["B"].dev("--dev-cleanup", STATE["B"].mid), STATE["B"].dev("--dev-delete-this-device"))
    if STATE["A"] and STATE["A"].mid:
        print("   A:", STATE["A"].dev("--dev-cleanup", STATE["A"].mid))
    if STATE["A"]:
        STATE["A"].dev("--dev-sign-out")


if __name__ == "__main__":
    try:
        main(sys.argv[1])
    except Exception as e:  # noqa: BLE001
        check("harness completed without exception", False, type(e).__name__ + ": " + str(e)[:200])
        emergency()
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== remote apply: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
