"""End-to-end validation of bookmarks capture + upload (M1 slice 7). REQUIRES migration 0004.

Real debug binary (copied out of ~/Documents, see e2e_handshake.py), real extension in a
DISPOSABLE Helium --user-data-dir, live edits through CDP inside the extension's own service
worker (chrome.bookmarks), Synchrome Cloud through A's / B's normal sessions. Prints ids,
counts, titles of the known test fixture and URL hosts only — never tokens, JWTs, passwords
or full URLs.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_bookmarks.py <creds-env-file>
"""
import json, os, re, shutil, signal, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402  (shared helpers: dev(), check(), prepare_binary(), …)
from cdp import CDP  # noqa: E402

check, dev = h.check, h.dev
EXT_ID = h.EXT_ID
OFFLINE_URL = "http://127.0.0.1:9"  # nothing listens there: connection refused at once


class Helium:
    """A running disposable Helium with the extension, attached over CDP to its worker."""

    def __init__(self, udd, env=None):
        self.udd = udd
        for f in ("DevToolsActivePort",):
            try:
                os.remove(os.path.join(udd, f))
            except FileNotFoundError:
                pass
        e = dict(os.environ)
        e.update(env or {})
        self.p = subprocess.Popen(
            [h.HELIUM, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
             "--no-default-browser-check", "--disable-gpu", "--remote-debugging-port=0",
             f"--load-extension={h.EXT}", f"--disable-extensions-except={h.EXT}", "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=e)
        self.c = CDP(udd)
        self.c.attach_extension_worker(EXT_ID)

    def js(self, expr, timeout=60):
        return self.c.eval(expr, timeout)

    def status(self):
        return self.js("chrome.storage.local.get('synchromeStatus').then(r => r.synchromeStatus ?? null)")

    def wait(self, pred, timeout=90, after=0):
        end = time.time() + timeout
        st = None
        while time.time() < end:
            st = self.status()
            if st and st.get("at", 0) >= after and pred(st):
                return st
            time.sleep(1)
        return st

    def stop(self):
        self.p.send_signal(signal.SIGTERM)
        try:
            self.p.wait(25)
        except subprocess.TimeoutExpired:
            self.p.kill()
        for _ in range(40):
            if not os.path.lexists(os.path.join(self.udd, "SingletonLock")):
                break
            time.sleep(0.25)


def bm(st):
    return (st or {}).get("bookmarks") or {}


def up_to_date(st):
    b = bm(st)
    return b.get("state") == "up_to_date" and b.get("pending") == 0


def now_ms():
    return int(time.time() * 1000)


def bar(tree):
    return [(r[1], r[2], r[3], r[4]) for r in tree.get("tree", []) if r[0] == "bar"]


KNOWN = [
    (0, "folder", "Synchrome Test", None),
    (1, "folder", "Folder A", None),
    (2, "url", "Bookmark 1", "example.com"),
    (2, "url", "Bookmark 2", "example.com"),
    (1, "folder", "Folder B", None),
    (2, "url", "Bookmark 3", "example.com"),
]

CREATE_KNOWN = """(async () => {
  const t = await chrome.bookmarks.create({parentId: "1", title: "Synchrome Test"});
  const a = await chrome.bookmarks.create({parentId: t.id, title: "Folder A"});
  await chrome.bookmarks.create({parentId: a.id, title: "Bookmark 1", url: "https://example.com/1?q=private"});
  await chrome.bookmarks.create({parentId: a.id, title: "Bookmark 2", url: "https://example.com/2"});
  const b = await chrome.bookmarks.create({parentId: t.id, title: "Folder B"});
  await chrome.bookmarks.create({parentId: b.id, title: "Bookmark 3", url: "https://example.com/3"});
  return true;
})()"""


def find(title):
    return f"(await chrome.bookmarks.search({{title: {json.dumps(title)}}}))[0]"


def main(creds):
    h.prepare_binary()
    log_start = os.path.getsize(h.LOG) if os.path.exists(h.LOG) else 0
    print("=== bookmarks e2e ===")
    a = dev("--dev-sign-in", creds, "A")
    check("login A", a.get("signed_in") and (not h.EXPECTED["A"] or a.get("user_id") == h.EXPECTED["A"]),
          f"user_id={a.get('user_id')}")
    if not a.get("signed_in"):
        return
    a_id = a["user_id"]
    me = dev("--dev-register-device").get("device_id")
    udd = h.fresh_udd("bookmarks")
    dev("--dev-install-host", udd)

    # 0. disposable profile with the known structure (extension not linked yet)
    hl = Helium(udd)
    hl.js(CREATE_KNOWN)
    hl.wait(lambda s: s.get("status") == "token_missing", 30)
    hl.stop()

    # A. mapping confirmed (token written while closed, handshake on next start)
    m = dev("--dev-create-mapping", f"E2E BM {os.urandom(3).hex()}", "helium", "Default")
    mid, pid = m.get("mapping_id"), m.get("profile_id")
    dev("--dev-write-token", mid, udd)

    # B–E. snapshot -> app -> cloud -> checkpoint
    t0 = now_ms()
    hl = Helium(udd)
    st = hl.wait(up_to_date, 120, after=t0)
    check("A mapping confirmed by the handshake", st and st.get("status") == "connected" and st.get("token_state") == "confirmed",
          f"status={(st or {}).get('status')}")
    cloud = dev("--dev-cloud-tree", pid)
    check("1 initial snapshot reached the cloud", up_to_date(st) and cloud.get("ops") == 6 and cloud.get("store_events") == 1,
          f"state={bm(st).get('state')} ops={cloud.get('ops')} objects={cloud.get('objects')} events={cloud.get('store_events')}")
    check("2 structure and order are correct in the cloud", bar(cloud) == KNOWN and cloud.get("seq_contiguous"),
          json.dumps(bar(cloud)))
    local = dev("--dev-local-tree", pid)
    check("2b local materialised tree == cloud tree", bar(local) == bar(cloud), "")
    first_path = (cloud.get("paths") or [""])[0]

    # 3. repeated snapshot (forced, same epoch) -> no new ops, no upload
    hl.js("chrome.storage.session.clear().then(() => { needSnapshot = true; return run(true); })")
    time.sleep(3)
    c2 = dev("--dev-cloud-tree", pid)
    s2 = dev("--dev-bookmarks-status", mid).get("status") or {}
    check("3 repeating the snapshot duplicates nothing", c2.get("ops") == 6 and c2.get("objects") == cloud.get("objects")
          and s2.get("pending") == 0 and s2.get("bookmarks") == 3, f"ops={c2.get('ops')} pending={s2.get('pending')}")

    # 4–7. live edits -> deltas only
    def edit(js, label, expect):
        t = now_ms()
        hl.js(f"(async () => {{ {js}; return true; }})()")
        s = hl.wait(lambda s: up_to_date(s) and s.get("bookmarks", {}).get("last_upload_at", 0) * 1000 >= t - 1000, 90, after=t)
        c = dev("--dev-cloud-tree", pid)
        ok, detail = expect(c)
        check(label, up_to_date(s) and ok and c.get("seq_contiguous"), detail)
        return c

    def has(c, row):
        return row in bar(c)

    c = edit(f"const a = {find('Folder A')}; await chrome.bookmarks.create({{parentId: a.id, title: 'Bookmark 4', url: 'https://example.org/4'}})",
             "4 created bookmark appears in the cloud (delta = 1 op)",
             lambda c: (has(c, (2, "url", "Bookmark 4", "example.org")) and c.get("ops") == 7, f"ops={c.get('ops')}"))
    c = edit(f"await chrome.bookmarks.update(({find('Bookmark 1')}).id, {{title: 'Bookmark 1 renamed'}})",
             "5 rename appears (1 op, title only)",
             lambda c: (has(c, (2, "url", "Bookmark 1 renamed", "example.com")) and c.get("ops") == 8, f"ops={c.get('ops')}"))
    c = edit(f"await chrome.bookmarks.move(({find('Bookmark 3')}).id, {{parentId: ({find('Folder A')}).id, index: 0}})",
             "6 move appears (Bookmark 3 first in Folder A)",
             lambda c: (bar(c)[2] == (2, "url", "Bookmark 3", "example.com") and c.get("ops") == 9, json.dumps(bar(c))))
    c = edit(f"await chrome.bookmarks.removeTree(({find('Folder B')}).id); await chrome.bookmarks.remove(({find('Bookmark 2')}).id)",
             "7 deletes appear (Folder B + Bookmark 2 tombstoned)",
             lambda c: (not any(r[2] in ("Folder B", "Bookmark 2") for r in bar(c)) and c.get("deletes") == 2,
                        f"deletes={c.get('deletes')} ops={c.get('ops')}"))
    after_edits = c
    hl.stop()

    # H–I / 8. restart Helium -> consistent continuation, no new ops
    t = now_ms()
    hl = Helium(udd)
    st = hl.wait(up_to_date, 90, after=t)
    c = dev("--dev-cloud-tree", pid)
    check("8 restarting Helium keeps continuity (snapshot reconcile = 0 ops)",
          up_to_date(st) and c.get("ops") == after_edits.get("ops") and bar(c) == bar(after_edits), f"ops={c.get('ops')}")
    hl.stop()

    # 9. cloud offline: host sees connection refused -> connected_offline, ops queue durably
    t = now_ms()
    hl = Helium(udd, env={"SUPABASE_URL": OFFLINE_URL})
    st = hl.wait(lambda s: s.get("status") == "connected_offline", 60, after=t)
    check("9a cloud unreachable -> extension connected_offline (capture continues)",
          st and st.get("status") == "connected_offline", f"status={(st or {}).get('status')}")
    t = now_ms()
    hl.js(f"(async () => {{ const a = {find('Folder A')}; await chrome.bookmarks.create({{parentId: a.id, title: 'Offline 1', url: 'https://offline.example/1'}}); await chrome.bookmarks.update(({find('Bookmark 4')}).id, {{title: 'Bookmark 4 offline'}}); return true; }})()")
    st = hl.wait(lambda s: bm(s).get("state") == "offline" and bm(s).get("pending", 0) >= 2, 90, after=t)
    c = dev("--dev-cloud-tree", pid)
    check("9 offline changes are kept locally (Offline, pending=2) and nothing reached the cloud",
          bm(st).get("state") == "offline" and bm(st).get("pending") == 2 and c.get("ops") == after_edits.get("ops"),
          f"state={bm(st).get('state')} pending={bm(st).get('pending')} cloud_ops={c.get('ops')}")
    hl.stop()

    # 10. network restored -> pending published on the next connection
    t = now_ms()
    hl = Helium(udd)
    st = hl.wait(up_to_date, 120, after=t)
    c = dev("--dev-cloud-tree", pid)
    check("10 network restored: pending published, order preserved",
          up_to_date(st) and c.get("ops") == after_edits.get("ops") + 2 and has(c, (2, "url", "Offline 1", "offline.example"))
          and has(c, (2, "url", "Bookmark 4 offline", "example.org")) and c.get("seq_contiguous"),
          f"ops={c.get('ops')} contiguous={c.get('seq_contiguous')}")
    final = c
    hl.stop()

    # security
    log = open(h.LOG, "rb").read()[log_start:].decode("utf-8", "replace")
    check("S1 native-host.log: no URLs, no titles, no tokens/JWTs",
          "example.com" not in log and "offline.example" not in log and "Bookmark" not in log and h.secret_free(log),
          f"{len(log)} bytes")
    st_json = json.dumps(st)
    check("S2 extension status stores no URLs/titles/tokens", "example" not in st_json and "Bookmark" not in st_json and h.secret_free(st_json), "")
    views = json.dumps(dev("--dev-list-mappings"))
    check("S3 UI mapping views: bookmarks counts only, no URLs, no token",
          "example" not in views and '"mapping_token"' not in views and h.secret_free(views) and '"bookmarks"' in views, "")

    # 11. account B cannot access A's bookmarks
    b = dev("--dev-sign-in", creds, "B")
    probe = dev("--dev-cloud-probe-foreign", a_id, pid, me, first_path)
    inv = dev("--dev-inventory")
    refused = lambda v: isinstance(v, dict) and "Err" in v
    check("11 B cannot list A's journal objects nor see A's store events",
          b.get("signed_in") and b.get("user_id") != a_id and probe.get("list_sees") == {"Ok": 0}
          and probe.get("store_events_sees") == {"Ok": 0} and inv.get("profiles") == 0,
          json.dumps({k: v for k, v in probe.items() if k != "as_user"}))
    check("11b B: GET of A's object -> not found; PUT into A's profile and event insert -> refused",
          probe.get("get_first_chunk") == {"Ok": False} and refused(probe.get("put_into_foreign_profile"))
          and refused(probe.get("store_event_insert")), "")

    # 12. cleanup
    dev("--dev-sign-in", creds, "A")
    cl = dev("--dev-cleanup", mid)
    after = dev("--dev-cloud-tree", pid)
    inv = dev("--dev-inventory")
    local = dev("--dev-bookmarks-status", mid)
    check("12a cleanup: Storage objects purged, profile/mapping/device rows gone (store_events cascade)",
          cl.get("purged_objects") == final.get("objects") and cl.get("deleted_profile") and cl.get("device_deleted")
          and after.get("objects") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [],
          json.dumps(cl))
    check("12b local bookmarks state of the profile forgotten", local.get("profile_id") is None, "")
    so = dev("--dev-sign-out")
    check("12c Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    shutil.rmtree(udd, ignore_errors=True)
    check("12d disposable profile + host manifest removed", not os.path.exists(udd), udd)


if __name__ == "__main__":
    main(sys.argv[1])
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== bookmarks: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
