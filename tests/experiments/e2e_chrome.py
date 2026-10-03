"""Google Chrome (stable) matrix — v1 release closure. Disposable Chrome profiles only.

Everything validated so far on Helium is re-checked on real Chrome stable: detection and
access state, mapping, the native-messaging connector under `<user-data-dir>/NativeMessagingHosts`,
bookmarks (snapshot, local change, remote apply, echo suppression), the extension inventory
(dummy fixture; Synchrome excluded), the REAL behaviour of `chrome.management.setEnabled`
and `uninstall` from the service worker, extension settings (LevelDB, browser closed) and the
four allow-listed preferences (Chrome writes `bookmark_bar.show_on_all_tabs`, never Helium's
`visibility_state`). Two simulated devices A and B, both on Chrome, both USER_A.

Chrome 154 ignores `--load-extension`; the companion and the fixtures are side-loaded through the
DevTools pipe (`Extensions.loadUnpacked`, `--enable-unsafe-extension-debugging`) — a test
mechanism; the product ships the extension folder for "Load unpacked".

  python3 e2e_chrome.py --local                                   # no credentials: A, G(read), H(write)
  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_chrome.py <creds>    # full matrix
"""
import json, os, select, shutil, signal, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402
import e2e_extensions as xe  # noqa: E402
import e2e_settings as es  # noqa: E402
import e2e_prefs as ep  # noqa: E402
from cdp import CDP  # noqa: E402

check = h.check
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
DUMMY, DUMMY_DIR, DUMMY2, DUMMY2_DIR = xe.DUMMY, xe.DUMMY_DIR, xe.DUMMY2, xe.DUMMY2_DIR
ALLOW = ["bookmark_bar.show_on_all_tabs", "browser.enable_spellchecking", "download.prompt_for_download", "intl.selected_languages"]
DEVS = {}
M = {"notes": []}


class Chrome:
    """A disposable Chrome stable with extensions side-loaded over the DevTools pipe."""

    def __init__(self, udd, env=None, extensions=()):
        self.udd = udd
        try:
            os.remove(os.path.join(udd, "DevToolsActivePort"))
        except FileNotFoundError:
            pass
        e = dict(os.environ)
        e.update(env or {})
        r_in, self.w_in = os.pipe()
        self.r_out, w_out = os.pipe()

        def pre():
            a, b = os.dup(r_in), os.dup(w_out)
            os.dup2(a, 3)
            os.dup2(b, 4)
        self.p = subprocess.Popen(
            [CHROME, f"--user-data-dir={udd}", "--headless=new", "--no-first-run", "--no-default-browser-check",
             "--disable-gpu", "--remote-debugging-pipe", "--remote-debugging-port=0",
             "--enable-unsafe-extension-debugging", "--disable-background-networking", "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, pass_fds=(3, 4), preexec_fn=pre, env=e)
        os.close(r_in)
        os.close(w_out)
        self.pid = 0
        self.buf = b""
        eb.RUN["helium"] = self
        self.pipe("Browser.getVersion")
        self.loaded = []
        for ext in extensions:
            r = self.pipe("Extensions.loadUnpacked", {"path": ext})
            self.loaded.append((r.get("result") or {}).get("id"))
        self.c = CDP(udd)
        self.c.attach_extension_worker(h.EXT_ID)

    def pipe(self, method, params=None, timeout=30):
        self.pid += 1
        os.write(self.w_in, (json.dumps({"id": self.pid, "method": method, "params": params or {}}) + "\0").encode())
        end = time.time() + timeout
        while time.time() < end:
            r, _, _ = select.select([self.r_out], [], [], 1)
            if not r:
                continue
            ch = os.read(self.r_out, 65536)
            if not ch:
                raise RuntimeError("devtools pipe closed")
            self.buf += ch
            while b"\0" in self.buf:
                msg, self.buf = self.buf.split(b"\0", 1)
                m = json.loads(msg)
                if m.get("id") == self.pid:
                    return m
        raise TimeoutError(method)

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
            time.sleep(0.5)
        return st

    def stop(self):
        time.sleep(4)  # Chromium commits Bookmarks ~2.5 s after the last change
        self.p.send_signal(signal.SIGTERM)
        try:
            self.p.wait(25)
        except subprocess.TimeoutExpired:
            self.p.kill()
        for fd in (self.w_in, self.r_out):
            try:
                os.close(fd)
            except OSError:
                pass
        for _ in range(40):
            if not os.path.lexists(os.path.join(self.udd, "SingletonLock")):
                break
            time.sleep(0.25)


def device(name):
    d = ra.Device(name)
    d.udd = os.path.join(ra.WORK, f"dev{name}-chrome")
    shutil.rmtree(d.udd, ignore_errors=True)
    os.makedirs(d.udd)
    DEVS[name] = d
    return d


def start(d, fixtures=(), offline=False):
    env = dict(d.env)
    if offline:
        env["SUPABASE_URL"] = eb.OFFLINE_URL
    d.hl = Chrome(d.udd, env=env, extensions=[h.EXT, *fixtures])
    for i in range(20):
        try:
            d.hl.js("chrome.alarms.clearAll().then(() => true)")
            break
        except RuntimeError:
            if i == 19:
                raise
            time.sleep(0.5)
    d.hl.wait(lambda s: s.get("status") == ("connected_offline" if offline else "connected"), 60)


def stop(d):
    if d.hl:
        d.hl.stop()
        d.hl = None


def restart(d, fixtures=()):
    stop(d)
    start(d, fixtures)


def closed(d):
    stop(d)
    s = d.dev("--dev-settings-cycle", d.mid).get("report") or {}
    p = d.dev("--dev-prefs-cycle", d.mid).get("report") or {}
    start(d, [DUMMY_DIR])
    return s, p


def mgmt(d):
    return {e["id"]: e["enabled"] for e in d.hl.js("chrome.management.getAll().then(l => l.map(e => ({id: e.id, enabled: e.enabled})))")}


def views(d):
    return {x["ext_id"]: x for x in (d.dev("--dev-ext-views", d.mid).get("extensions") or [])}


def ext_ops(a, pid, dev=None):
    return xe.ext_ops(a, pid, dev) if dev else xe.ext_ops(a, pid)


def bm_ops(a, pid):
    """Bookmark ops across every journal chunk, in publication order (id/kind/field names)."""
    out = []
    for c in a.dev("--dev-cloud-tree", pid).get("chunks") or []:
        out.extend(c.get("ops") or [])
    return out


def chrome_sets(d, values):
    """The user changes settings in Chrome's own settings page (no Helium adapter)."""
    for k, v in values.items():
        ep.settings(d, "new Promise(r => chrome.settingsPrivate.setPref(%s, %s, '', ok => r(ok)))" % (json.dumps(k), json.dumps(v)))


def eff(d):
    v = ep.settings(d, "Promise.all(%s.map(k => new Promise(r => chrome.settingsPrivate.getPref(k, p => r(p ? p.value : null)))))" % json.dumps(ALLOW)) or []
    return dict(zip(ALLOW, v))


def prefs_doc(d):
    return json.load(open(os.path.join(d.udd, "Default", "Preferences")))


def browsers(d):
    return {b["kind"]: b for b in (d.dev("--dev-browsers").get("browsers") or [])}


# ------------------------------------------------------------------ local part (no account)
def local_part():
    print("=== Chrome stable — local checks (no account) ===")
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a = device("A")
    bs = browsers(a)
    ch = bs.get("chrome") or {}
    check("A.1 detection: Google Chrome is discovered on this Mac with its standard data folder (version unreadable without FDA)",
          bool(ch) and ch.get("user_data_dir", "").endswith("Google/Chrome"), json.dumps({k: ch.get(k) for k in ("version", "access", "profiles")}))
    check("A.2 without Full Disk Access the access state is requires_full_disk_access and no profile names are read",
          ch.get("access") == "requires_full_disk_access" and ch.get("profiles") == 0, json.dumps({"access": ch.get("access"), "profiles": ch.get("profiles")}))
    check("A.3 Helium needs no permission (accessible / browser running)", (bs.get("helium") or {}).get("access") in ("accessible", "browser_running"), json.dumps((bs.get("helium") or {}).get("access")))
    # the extra data folder mechanism lists a disposable Chrome folder as its own entry
    os.makedirs(a.data, exist_ok=True)
    json.dump([{"kind": "chrome", "user_data_dir": a.udd, "label": "E2E disposable"}], open(os.path.join(a.data, "browsers.json"), "w"))
    start(a, [DUMMY_DIR])
    es.put(a, "local", {"k1": "v1", "theme": "dark"})
    chrome_sets(a, {"download.prompt_for_download": True, "intl.selected_languages": "es,en", "bookmark_bar.show_on_all_tabs": True})
    stop(a)
    bs = browsers(a)
    extra = next((b for b in (a.dev("--dev-browsers").get("browsers") or []) if b["user_data_dir"] == a.udd), {})
    check("A.4 an extra data folder (browsers.json) is listed as its own Chrome entry with its profiles, accessible",
          extra.get("kind") == "chrome" and extra.get("access") == "accessible" and extra.get("profiles") == 1 and "disposable" in extra.get("display_name", ""), json.dumps(extra))
    kv = a.dev("--dev-read-ext-area", a.udd, "Default", DUMMY, "local").get("keys") or {}
    check("G.1 extension settings of the dummy read from Chrome's LevelDB with the browser closed (keys only)", set(kv) == {"k1", "theme"}, json.dumps(sorted(kv)))
    r = a.dev("--dev-read-prefs", a.udd, "Default", "chrome")
    vals = (r.get("values") or r.get("prefs") or {})
    check("H.1 the four allow-listed preferences read from Chrome's Preferences (closed); no visibility_state adapter",
          vals.get("download.prompt_for_download") is True and vals.get("intl.selected_languages") == "es,en" and vals.get("bookmark_bar.show_on_all_tabs") is True
          and "visibility_state" not in json.dumps(r), json.dumps({k: vals.get(k) for k in ALLOW}))
    before = prefs_doc(a)
    w = a.dev("--dev-write-prefs", a.udd, "Default", "chrome", json.dumps({"download.prompt_for_download": False, "intl.selected_languages": "fr,en", "bookmark_bar.show_on_all_tabs": False}))
    after = prefs_doc(a)
    start(a, [DUMMY_DIR])
    e = eff(a)
    stop(a)
    check("H.2 the product writer changed exactly the three paths (backup made); Chrome shows the new values after relaunch; visibility_state untouched",
          w.get("changed") == 3 and w.get("backup") and e.get("download.prompt_for_download") is False and e.get("intl.selected_languages") == "fr,en"
          and e.get("bookmark_bar.show_on_all_tabs") is False and after.get("bookmark_bar", {}).get("visibility_state") == before.get("bookmark_bar", {}).get("visibility_state"),
          json.dumps({"changed": w.get("changed"), "eff": e}))
    for d in DEVS.values():
        shutil.rmtree(d.udd, ignore_errors=True)
        shutil.rmtree(d.data, ignore_errors=True)


# ------------------------------------------------------------------ full matrix
def main(creds):
    print("=== Chrome stable matrix (two disposable Chrome profiles, USER_A) ===")
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a, b = device("A"), device("B")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())
    sa = a.dev("--dev-sign-in", creds, "A")
    check("B.1 login USER_A on A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if sa.get("user_id") != h.EXPECTED["A"]:
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("B.2 login USER_A on B (exact id)", sb.get("user_id") == h.EXPECTED["A"], "")
    a.device_id = a.dev("--dev-register-device").get("device_id")
    b.device_id = b.dev("--dev-register-device").get("device_id")
    m = a.dev("--dev-create-mapping", f"E2E CHROME {os.urandom(3).hex()}", "chrome", "Default")
    a.mid, pid = m.get("mapping_id"), m.get("profile_id")
    wa = a.dev("--dev-write-token", a.mid, a.udd)
    ha = a.dev("--dev-install-host", a.udd)
    start(a, [DUMMY_DIR])
    mb = b.dev("--dev-map-existing", pid, "chrome", "Default")
    b.mid = mb.get("mapping_id")
    b.dev("--dev-write-token", b.mid, b.udd)
    b.dev("--dev-install-host", b.udd)
    start(b, [DUMMY_DIR])
    sta, stb = a.hl.status() or {}, b.hl.status() or {}
    check("B.3 mapping on Chrome: token written into the closed profile; the connector manifest under <user-data-dir>/NativeMessagingHosts is read by Chrome; both extensions confirmed",
          wa.get("display_status") in ("token_written", "extension_confirmed") and "NativeMessagingHosts" in json.dumps(ha)
          and sta.get("token_state") == "confirmed" and stb.get("token_state") == "confirmed" and mb.get("profile_id") == pid,
          json.dumps({"A": sta.get("status"), "B": stb.get("status")}))
    for d in (a, b):
        d.dev("--dev-settings-mode", d.mid, DUMMY, "local")
        d.dev("--dev-prefs-mode", d.mid, "recommended")

    # ---- C bookmarks
    ra.edit_and_publish(a, """const bar = (await chrome.bookmarks.getTree())[0].children.find(c => c.folderType === 'bookmarks-bar' || c.id === '1');
      const f = await chrome.bookmarks.create({parentId: bar.id, title: 'Chrome Folder'});
      for (let i = 0; i < 3; i++) await chrome.bookmarks.create({parentId: f.id, title: 'Chrome item ' + i, url: 'https://example.com/chrome/' + i});""")
    sta = a.status()
    tree_a = a.dev("--dev-local-tree", pid)
    check("C.1 snapshot on Chrome: the extension captured the tree and A published it (state up to date, 4 entities)",
          sta.get("state") == "up_to_date" and tree_a.get("bookmarks") == 3 and tree_a.get("folders") >= 1, json.dumps({"state": sta.get("state"), "bookmarks": tree_a.get("bookmarks")}))
    n0 = len(bm_ops(a, pid))
    ra.edit_and_publish(a, """const bar = (await chrome.bookmarks.getTree())[0].children.find(c => c.folderType === 'bookmarks-bar' || c.id === '1');
      const f = (await chrome.bookmarks.getChildren(bar.id)).find(x => x.title === 'Chrome Folder');
      const kids = await chrome.bookmarks.getChildren(f.id); await chrome.bookmarks.update(kids[0].id, {title: 'Chrome item 0 renamed'});""")
    ops_a = bm_ops(a, pid)
    check("C.2 local change on Chrome: exactly one [title] op published", bool(ops_a) and ops_a[-1].get("fields") == ["title"] and len(ops_a) == n0 + 1,
          json.dumps({"ops": len(ops_a), "last": {k: ops_a[-1].get(k) for k in ("op", "fields")} if ops_a else None}))
    nb0 = len([x for x in (a.dev("--dev-cloud-tree", pid).get("store_event_ranges") or []) if b.device_id in x[2]])
    sl, _ = ra.pull_b(b)
    tb, ta = b.tree(), a.tree()
    check("C.3 remote apply on Chrome B: the extension applied A's tree exactly (hierarchy, titles, URLs, order)", ra.show(tb) == ra.show(ta) and len(ra.flat(tb)) == 4, json.dumps(ra.show(tb)))
    time.sleep(3)
    nb1 = len([x for x in (a.dev("--dev-cloud-tree", pid).get("store_event_ranges") or []) if b.device_id in x[2]])
    check("C.4 echo suppression on Chrome: the apply produced no ops, no B journal, no B store_events", nb1 == nb0 == 0 and (b.status() or {}).get("pending", 0) == 0, json.dumps({"B_events": nb1}))

    # ---- D inventory
    xe.snapshot_now(a)
    ids = {o["id"] for o in ext_ops(a, pid)}
    check("D.1 inventory on Chrome: the dummy fixture (development) is published; Synchrome itself is never listed", DUMMY in ids and h.EXT_ID not in ids and h.EXT_ID not in views(a), json.dumps(sorted(ids)))
    blob = json.dumps(a.dev("--dev-cloud-tree", pid).get("ext_ops"))
    check("D.2 cloud carries only allowed fields (no paths, URLs, permissions)", "/Users" not in blob and "http" not in blob and "/tmp" not in blob, "")

    # ---- E setEnabled (REAL Chrome behaviour)
    xe.pull(b)
    xe.local_change(a, DUMMY, False)
    t0 = time.time()
    sl = xe.pull(b)
    vb = views(b)
    real = mgmt(b).get(DUMMY)
    st = vb.get(DUMMY, {}).get("state")
    M["setEnabled_disable"] = {"state": st, "real_enabled": real}
    check("E.1 setEnabled(false) from the service worker on Chrome stable: either applied (Synced, disabled) or reported honestly as User action required — never a silent wrong state",
          (st == "synced" and real is False) or (st in ("user_action_required", "apply_failed") and real is True), json.dumps(M["setEnabled_disable"]))
    xe.local_change(a, DUMMY, True)
    xe.pull(b)
    vb = views(b)
    real = mgmt(b).get(DUMMY)
    st = vb.get(DUMMY, {}).get("state")
    M["setEnabled_enable"] = {"state": st, "real_enabled": real}
    check("E.2 setEnabled(true) on Chrome: applied or User action required (consistent with the real state)",
          (st == "synced" and real is True) or (st in ("user_action_required", "apply_failed") and real is False), json.dumps(M["setEnabled_enable"]))

    # ---- F uninstall (real Chrome behaviour)
    direct = b.hl.js(f"chrome.management.uninstall('{DUMMY}', {{showConfirmDialog: false}}).then(() => 'ok', e => e.message)")
    check("F.1 chrome.management.uninstall from the worker without a user gesture is refused by Chrome stable", "gesture" in str(direct).lower(), str(direct)[:80])
    restart(b, fixtures=())  # the user uninstalled the dummy on B (test mechanism: relaunch without it)
    xe.snapshot_now(b)
    xe.pull(a)
    va = views(a)
    check("F.2 A sees 'Removed on another device', keeps the dummy installed (no automatic uninstall) and offers Remove here / Keep here",
          va.get(DUMMY, {}).get("state") == "removed_elsewhere" and DUMMY in mgmt(a) and va[DUMMY].get("actions") == ["remove", "keep"], json.dumps({"state": va.get(DUMMY, {}).get("state")}))
    a.dev("--dev-ext-action", a.mid, DUMMY, "remove")
    xe.pull(a)
    va = views(a)
    still = DUMMY in mgmt(a)
    M["uninstall"] = {"state": va.get(DUMMY, {}).get("state"), "still_installed": still}
    check("F.3 'Remove here' on Chrome: the worker's uninstall is refused (gesture) → User action required (popup gesture); nothing removed silently",
          va.get(DUMMY, {}).get("state") in ("user_action_required", "apply_failed") and still, json.dumps(M["uninstall"]))
    a.dev("--dev-ext-action", a.mid, DUMMY, "keep")
    xe.pull(a)
    restart(b, fixtures=[DUMMY_DIR])  # the dummy is back on B for the settings tests
    xe.snapshot_now(b)
    xe.pull(a)

    # ---- G extension settings (LevelDB, closed)
    es.put(a, "local", {"k1": "chrome-v1", "theme": "dark"})
    sa_, _ = closed(a)
    sb_, _ = closed(b)
    va_, vb_ = es.values(a), es.values(b)
    check("G.2 extension settings synced Chrome → Chrome with both browsers closed (LevelDB read on A, written on B); values equal",
          sa_.get("captured_ops", 0) >= 1 and vb_.get("local/k1") == "chrome-v1" and vb_.get("local/theme") == "dark" and va_ == vb_,
          json.dumps({"captured": sa_.get("captured_ops"), "applied": sb_.get("keys_written") or sb_.get("applied")}))
    rep = a.dev("--dev-settings-cycle", a.mid).get("report") or {}
    check("G.3 with Chrome open the cycle refuses to touch the LevelDB (browser_running)", rep.get("browser_running") is True, json.dumps({k: rep.get(k) for k in ("browser_running", "status")}))

    # ---- H preferences
    chrome_sets(a, {"download.prompt_for_download": True, "browser.enable_spellchecking": False, "intl.selected_languages": "es,en", "bookmark_bar.show_on_all_tabs": True})
    _, pa = closed(a)
    _, pb = closed(b)
    ea, eb_ = eff(a), eff(b)
    doc_b = prefs_doc(b)
    check("H.3 the four allow-listed preferences synced Chrome → Chrome (closed cycles); effective values equal; Chrome-native show_on_all_tabs, no visibility_state written",
          pa.get("captured_ops", 0) >= 1 and pb.get("paths_written", 0) >= 1 and ea == eb_ and eb_.get("intl.selected_languages") == "es,en"
          and "visibility_state" not in doc_b.get("bookmark_bar", {}), json.dumps({"A": ea, "B": eb_, "written": pb.get("paths_written")}))

    # ---- security
    for d in (a, b):
        lg = d.log()
        check(f"S device {d.name} host log: no tokens/JWTs, no titles/URLs", h.secret_free(lg) and "example.com" not in lg and "Chrome item" not in lg, "")
    print("MATRIX " + json.dumps({k: v for k, v in M.items() if k != "notes"}))

    # ---- cleanup
    for d in (a, b):
        stop(d)
    b.dev("--dev-cleanup", b.mid)
    b.dev("--dev-delete-this-device")
    ra_ = a.dev("--dev-cleanup", a.mid)
    inv = a.dev("--dev-inventory")
    check("C.cleanup cloud: profile, mappings, devices, objects, events gone",
          ra_.get("deleted_profile") and ra_.get("leftover_objects") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [], json.dumps(ra_))
    so = a.dev("--dev-sign-out")
    check("C.cleanup Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for d in DEVS.values():
        shutil.rmtree(d.udd, ignore_errors=True)
        shutil.rmtree(d.data, ignore_errors=True)
    check("C.cleanup local: Chrome profiles and data dirs removed", not any(os.path.exists(p) for d in DEVS.values() for p in (d.udd, d.data)), "")


def emergency():
    print("!! aborted: emergency cleanup")
    for d in DEVS.values():
        try:
            if d.hl and d.hl.p.poll() is None:
                d.hl.p.kill()
        except Exception:  # noqa: BLE001
            pass
    if len(sys.argv) > 1 and sys.argv[1] != "--local":
        a = DEVS.get("A")
        if a:
            a.dev("--dev-sign-in", sys.argv[1], "A")
            for n, d in DEVS.items():
                if n != "A" and d.mid:
                    print(f"   {n}:", json.dumps(d.dev("--dev-cleanup", d.mid)), json.dumps(d.dev("--dev-delete-this-device")))
            if a.mid:
                print("   A:", json.dumps(a.dev("--dev-cleanup", a.mid)))
            a.dev("--dev-sign-out")


if __name__ == "__main__":
    try:
        if sys.argv[1] == "--local":
            local_part()
        else:
            main(sys.argv[1])
    except Exception as ex:  # noqa: BLE001
        import traceback
        traceback.print_exc()
        check("harness completed without exception", False, type(ex).__name__ + ": " + str(ex)[:200])
        emergency()
    failed = [n for n, ok in h.results if not ok]
    print(f"\n=== chrome: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
