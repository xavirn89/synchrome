"""Two-device E2E of the extensions inventory + enabled state (M1 slice 10). 24 cases.

Devices A and B: two simulated installations on this machine (own SYNCHROME_DATA_DIR each),
both signed in as test user A, each with its own disposable Helium --user-data-dir. USER_B
is used only for isolation. Extensions: Synchrome (never synchronised), the inert fixtures
tests/fixtures/dummy-extension (fixed key => same id everywhere) and dummy-theme (themes are
excluded), dummy-extension-2 (on B, for local edits), and uBlock Origin, which Helium
bundles in every fresh profile (it reports mayDisable=true but Helium refuses setEnabled on
it: "cannot be modified by user" — the product classifies that as Protected).

TEST MECHANISMS (never production behaviour):
- install / uninstall of the dummy = relaunching Helium with / without --load-extension
  (Chromium needs a user gesture to uninstall another extension and then shows a native
  dialog that a headless browser cannot accept);
- chrome.management is wrapped inside Synchrome's worker (via CDP) where Helium offers no
  safe real case: a policy item (installType admin), mayDisable=false, a refused
  setEnabled, a store-sourced item (for the Install action). The wrapper records every
  setEnabled/uninstall call so the harness can prove which were (not) made;
- the real app window (embedded frontend) is driven with the debug-only UI driver.

Prints ids, counts, field names and op codes only — no tokens, no passwords.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_extensions.py <creds-env-file>
"""
import json, os, re, shutil, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402
import e2e_conflicts as cf  # noqa: E402  (UI driver)
from cdp import CDP  # noqa: E402

check = h.check
FIX = os.path.join(h.ROOT, "tests", "fixtures")
DUMMY_DIR, THEME_DIR = os.path.join(FIX, "dummy-extension"), os.path.join(FIX, "dummy-theme")
DUMMY2_DIR = os.path.join(FIX, "dummy-extension-2")  # on B only: for local edits that must succeed
DUMMY = "dlijhhlcpppmppbekhaonjhlahfidkgf"
DUMMY2 = "emklmcialpkedlojojnnkdeanalhiifk"
UBO = "blockjmkbacgjkknlgpkjjiijinjdanf"  # bundled with Helium
FAKE_ADMIN = "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk"  # mocked policy item
FAKE_STORE = "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm"  # mocked Web Store item
NAMES = ("Synchrome Test Dummy", "Synchrome Second Dummy", "Synchrome Test Theme", "uBlock", "Policy Item", "Store Item")
ALLOWED_FIELDS = {"enabled", "name", "source", "type", "version"}
M = {"latency_ms": [], "suppressed": {"A": 0, "B": 0}, "ui_sessions": 0, "user_action": 0}
UI_LOG = []


# ------------------------------------------------------------------ rig
class XHelium(eb.Helium):
    """eb.Helium with extra unpacked extensions next to Synchrome."""

    def __init__(self, udd, env=None, extra=()):
        self.udd = udd
        try:
            os.remove(os.path.join(udd, "DevToolsActivePort"))
        except FileNotFoundError:
            pass
        e = dict(os.environ)
        e.update(env or {})
        exts = ",".join([h.EXT, *extra])
        self.p = subprocess.Popen(
            [h.HELIUM, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
             "--no-default-browser-check", "--disable-gpu", "--remote-debugging-port=0",
             f"--load-extension={exts}", f"--disable-extensions-except={exts}", "about:blank"],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=e)
        eb.RUN["helium"] = self
        self.c = CDP(udd)
        self.c.attach_extension_worker(h.EXT_ID)


def idle(d):
    return bool(d.hl.js("queue.length === 0 && extQueue.length === 0 && running === null && flushTimer === null"))


def start(d, offline=False, fixtures=()):
    env = dict(d.env)
    if offline:
        env["SUPABASE_URL"] = eb.OFFLINE_URL
    d.hl = XHelium(d.udd, env=env, extra=list(fixtures))
    for i in range(20):
        try:
            d.hl.js("chrome.alarms.clearAll().then(() => true)")
            break
        except RuntimeError:
            if i == 19:
                raise
            time.sleep(0.5)
    want = "connected_offline" if offline else "connected"
    d.hl.wait(lambda s: s.get("status") == want, 60)
    ra.wait(lambda: idle(d), 60)


def stop(d):
    if d.hl:
        try:
            M["suppressed"][d.name] += int(d.hl.js("suppressedEvents") or 0)
        except Exception:  # noqa: BLE001
            pass
        d.hl.stop()
        d.hl = None


def restart(d, offline=False, fixtures=()):
    stop(d)
    start(d, offline=offline, fixtures=fixtures)


MOCK_JS = r"""(() => {
  const m = chrome.management;
  if (!globalThis.__real) globalThis.__real = { getAll: m.getAll.bind(m), get: m.get.bind(m), setEnabled: m.setEnabled.bind(m), uninstall: m.uninstall.bind(m) };
  globalThis.__calls = globalThis.__calls || [];
  globalThis.__mock = Object.assign({ extra: [], protect: [], reject: false }, %s);
  const fix = (e) => (__mock.protect.includes(e.id) ? { ...e, mayDisable: false } : e);
  m.getAll = async () => (await __real.getAll()).map(fix).concat(__mock.extra);
  m.get = async (id) => { const x = __mock.extra.find((e) => e.id === id); return x || fix(await __real.get(id)); };
  m.setEnabled = async (id, on) => {
    __calls.push(["setEnabled", id, on]);
    if (__mock.reject) throw new Error("chrome.management.setEnabled requires a user gesture.");
    const x = __mock.extra.find((e) => e.id === id);
    if (x) { x.enabled = on; return; }
    return __real.setEnabled(id, on);
  };
  m.uninstall = async (id, o) => { __calls.push(["uninstall", id]); return __real.uninstall(id, o); };
  extNeedSnapshot = true;
  return true;
})()"""


def mock(d, **cfg):
    d.hl.js(MOCK_JS % json.dumps(cfg))


def calls(d):
    return d.hl.js("globalThis.__calls || []") or []


def fake(id_, name, install_type, may_disable, update_url=None):
    return {"id": id_, "name": name, "version": "1.0.0", "enabled": True, "type": "extension", "installType": install_type,
            "mayDisable": may_disable, "updateUrl": update_url, "disabledReason": None}


def mgmt(d, real=False):
    fn = "(globalThis.__real ? __real.getAll() : chrome.management.getAll())" if real else "chrome.management.getAll()"
    return {e["id"]: e["en"] for e in d.hl.js(f"{fn}.then(l => l.map(e => ({{id: e.id, en: e.enabled}})))") if e["id"] != h.EXT_ID}


def local_change(d, ext, on):
    """A change in the browser, as the user would make it (real API, not the mock)."""
    fn = "(globalThis.__real ? __real.setEnabled : chrome.management.setEnabled.bind(chrome.management))"
    d.hl.js(f"{fn}('{ext}', {str(on).lower()}).then(() => true)")
    time.sleep(0.5)
    ra.wait(lambda: idle(d), 60, 0.5)


def snapshot_now(d):
    d.hl.js("(async () => { extNeedSnapshot = true; await run(true); return true; })()", timeout=120)
    ra.wait(lambda: idle(d), 60)


def views(d):
    return {x["ext_id"]: x for x in (d.dev("--dev-ext-views", d.mid).get("extensions") or [])}


def ext_ops(a, pid, dev_id=None):
    ops = a.dev("--dev-cloud-tree", pid).get("ext_ops") or []
    return [o for o in ops if dev_id is None or o["by"] == dev_id]


def log_mark(d):
    return len(open(os.path.join(d.data, "logs", "native-host.log"), "rb").read())


def log_since(d, mark):
    return open(os.path.join(d.data, "logs", "native-host.log"), "rb").read()[mark:].decode("utf-8", "replace")


def ext_plans(sl):
    return [(int(n), o, int(u)) for n, o, u in re.findall(r"extensions apply plan ops=(\d+) order=(\S*) user_actions=(\d+)", sl)]


def ext_snaps(sl):
    return re.findall(r"extensions snapshot parts=\d+ items=\d+ -> (.*)", sl)


def pull(d, js_after=None):
    mark = log_mark(d)
    t = time.time()
    ra.pull_b(d, js_after)
    sl = log_since(d, mark)
    if any(n for n, _, _ in ext_plans(sl)):
        M["latency_ms"].append(int((time.time() - t) * 1000))
    return sl


def popup_text(d):
    c = d.hl.c
    t = c.call("Target.createTarget", {"url": f"chrome-extension://{h.EXT_ID}/popup.html"})
    s = c.call("Target.attachToTarget", {"targetId": t["targetId"], "flatten": True})["sessionId"]
    txt = ""
    for _ in range(20):
        time.sleep(0.5)
        r = c.call("Runtime.evaluate", {"expression": "document.body.innerText", "returnByValue": True}, session=s)
        txt = r.get("result", {}).get("value") or ""
        if "Extensions:" in txt and ("“" in txt or "need you" not in txt):
            break
    c.call("Target.closeTarget", {"targetId": t["targetId"]})
    return txt


def tabs(d):
    return d.hl.js("chrome.tabs.query({}).then(t => t.length)")


class AppUI:
    """The real Synchrome window (embedded frontend) on B's installation."""

    J = cf.UI_JS + r"""
      const xrow = (name) => all('tr').find(r => r.cells.length === 4 && r.cells[0].innerText.includes(name));
      const xstate = (name) => { const r = xrow(name); return r ? (r.cells[2].querySelector('.badge')?.textContent || '').trim() : null; };
      const xacts = (name) => { const r = xrow(name); return r ? all('button', r.cells[3]).map(b => b.textContent.trim()) : null; };
      const xcounts = () => (all('.bookmarks-row').find(r => r.innerText.startsWith('Extensions'))?.innerText || '');
    """

    def __init__(self, dev):
        self.ui = cf.UI(dev)
        M["ui_sessions"] += 1
        self.ok = bool(self.ui.ready) and self.ui.open_profiles() and bool(self.ui.until(self.J + "return !!btn(document, 'Extensions');", 40))
        self.ui.js(self.J + "btn(document, 'Extensions').click(); return true;")
        self.ui.until(self.J + "return !!all('tr').find(r => r.cells.length === 4 && r.cells[0].tagName === 'TD');", 20)

    def js(self, body, timeout=20):
        return self.ui.js(self.J + body, timeout)

    def until(self, body, timeout=30):
        return self.ui.until(self.J + body, timeout)

    def state(self, name):
        return self.js(f"return xstate({json.dumps(name)});")

    def actions(self, name):
        return self.js(f"return xacts({json.dumps(name)});")

    def counts(self):
        return self.js("return xcounts();") or ""

    def panel(self):
        return self.js("return all('.conflicts').map(x => x.innerText).join('\\n');") or ""

    def close(self):
        self.ui.stop()
        p = os.path.join(ra.WORK, "ui-app.log")
        if os.path.exists(p):
            UI_LOG.append(open(p, errors="replace").read())


def open_conflicts(d):
    return [c_ for c_ in (d.dev("--dev-conflict-views", d.mid).get("conflicts") or []) if c_["status"] in ("unresolved", "resolving")]


# ------------------------------------------------------------------ main
def main(creds):
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a, b = ra.Device("A"), ra.Device("B")
    print("=== two-device extensions E2E ===")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())

    # ---------------------------------------------------------------- CASE 1 setup
    sa = a.dev("--dev-sign-in", creds, "A")
    check("C1.1 login USER_A on device A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if not sa.get("signed_in"):
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("C1.2 login USER_A on device B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
    a.device_id = a.dev("--dev-register-device").get("device_id")
    b.device_id = b.dev("--dev-register-device").get("device_id")
    m = a.dev("--dev-create-mapping", f"E2E EXT {os.urandom(3).hex()}", "helium", "Default")
    a.mid, pid = m.get("mapping_id"), m.get("profile_id")
    a.dev("--dev-write-token", a.mid, a.udd)
    a.dev("--dev-install-host", a.udd)
    start(a, fixtures=[DUMMY_DIR, THEME_DIR])  # CASE 2.7
    mb = b.dev("--dev-map-existing", pid, "helium", "Default")
    b.mid = mb.get("mapping_id")
    b.dev("--dev-write-token", b.mid, b.udd)
    b.dev("--dev-install-host", b.udd)
    start(b)  # CASE 2.8: Synchrome only
    files = lambda d: [os.path.join(d.data, f) for f in ("device.json", "state.sqlite", "logs/native-host.log")]
    check("C1.3-6 same account + same cloud profile; distinct devices, mappings, data dirs, device.json, state.sqlite, logs, Helium profiles",
          a.dev("--dev-account").get("user_id") == b.dev("--dev-account").get("user_id") == h.EXPECTED["A"] and mb.get("profile_id") == pid
          and a.device_id != b.device_id and a.mid != b.mid and a.data != b.data and a.udd != b.udd
          and all(os.path.exists(f) for f in files(a) + files(b)) and json.load(open(files(a)[0]))["device_id"] != json.load(open(files(b)[0]))["device_id"]
          and (a.hl.status() or {}).get("token_state") == (b.hl.status() or {}).get("token_state") == "confirmed",
          f"profile={pid} A={a.device_id} B={b.device_id}")

    # ---------------------------------------------------------------- CASE 2 inventory
    ma = mgmt(a)
    theme_id = next((e["id"] for e in a.hl.js("chrome.management.getAll().then(l => l.map(e => ({id: e.id, t: e.type})))") if e["t"] == "theme"), None)
    check("C2.9 the dummy carries its fixed id", DUMMY in ma, DUMMY)
    # a policy-installed item (mocked: Helium has no safe real one) + a store item for the UI's Install
    mock(a, extra=[fake(FAKE_ADMIN, "Policy Item", "admin", False),
                   fake(FAKE_STORE, "Store Item", "normal", True, "https://clients2.google.com/service/update2/crx")])
    snapshot_now(a)
    ops = ext_ops(a, pid)
    ids = {o["id"] for o in ops}
    d_op = next((o for o in ops if o["id"] == DUMMY), {})
    check("C2.10 A sent its extensions snapshot (dummy + bundled uBlock + store item published)", {DUMMY, UBO, FAKE_STORE} <= ids, json.dumps(sorted(ids)))
    check("C2.11 Synchrome itself is NOT in the synchronised inventory", h.EXT_ID not in ids and h.EXT_ID not in views(a), "")
    check("C2.12 themes (and apps) are excluded", bool(theme_id) and theme_id not in ids and theme_id not in views(a), f"theme={theme_id}")
    va = views(a)
    check("C2.13 a policy item is not published; listed locally as Protected without actions",
          FAKE_ADMIN not in ids and va.get(FAKE_ADMIN, {}).get("state") == "protected" and va[FAKE_ADMIN]["actions"] == [], "")
    check("C2.14 cloud for the dummy: installed (set), enabled, id, name, version, source, type",
          d_op.get("op") == "set" and d_op.get("enabled") is True and d_op.get("fields") == sorted(ALLOWED_FIELDS), json.dumps(d_op.get("fields")))
    blob = json.dumps(a.dev("--dev-cloud-tree", pid).get("ext_ops"))
    check("C2.15 cloud has no paths, permissions, updateUrl, internal data or storage: only allowed fields, no URL/path values",
          all(set(o["fields"] or []) <= ALLOWED_FIELDS for o in ops) and "http" not in blob and "/Users" not in blob and "/tmp" not in blob, "")

    # ---------------------------------------------------------------- CASE 3 missing on B
    t_before = tabs(b)
    sl = pull(b)
    vb = views(b)
    check("C3.16-18 B pulled: the dummy is not installed there -> Missing", vb.get(DUMMY, {}).get("state") == "missing" and DUMMY not in mgmt(b), "")
    check("C3.19 availability for an unpacked/development item: Unavailable (no Install action)",
          vb[DUMMY]["availability"] == "unavailable" and vb[DUMMY]["actions"] == [], json.dumps({k: vb[DUMMY][k] for k in ("availability", "actions")}))
    check("C3.20-21 nothing installed automatically: no apply op, no tab opened, no CRX, no external process",
          not any(n for n, _, _ in ext_plans(sl)) and tabs(b) == t_before and DUMMY not in mgmt(b), json.dumps(ext_plans(sl)))
    ui = AppUI(b)
    st1 = {n: ui.state(n) for n in ("Synchrome Test Dummy", "uBlock Origin", "Store Item")}
    ac1 = {n: ui.actions(n) for n in ("Synchrome Test Dummy", "uBlock Origin", "Store Item")}
    cnt1 = ui.counts()
    ui.close()
    check("C20.107-111 UI#1 Extensions block: Synced (uBlock: Disable), Missing (dummy: no Install), Missing store item: Install; counters",
          ui.ok and st1["uBlock Origin"] == "Synced" and st1["Synchrome Test Dummy"] == "Missing" and st1["Store Item"] == "Missing"
          and ac1["uBlock Origin"] == ["Disable"] and ac1["Synchrome Test Dummy"] == [] and ac1["Store Item"] == ["Install"]
          and "1 installed" in cnt1 and "2 missing" in cnt1 and "differences" in cnt1 and "conflicts" in cnt1,
          json.dumps({"states": st1, "actions": ac1, "counts": cnt1}))

    # ---------------------------------------------------------------- CASE 4 controlled test install on B
    nb = len(ext_ops(a, pid, b.device_id))
    restart(b, fixtures=[DUMMY_DIR, DUMMY2_DIR])  # TEST MECHANISM: --load-extension
    pull(b)
    check("C4.22-24 dummy loaded on B (test mechanism): B converges; installed+enabled on A and B; B published nothing for it",
          views(b).get(DUMMY, {}).get("state") == "synced" and mgmt(a).get(DUMMY) is True and mgmt(b).get(DUMMY) is True
          and not any(o["id"] == DUMMY for o in ext_ops(a, pid, b.device_id)[nb:]), "")
    check("C4.25 (record) --load-extension is an E2E-only install; production opens the official store page and the user installs", True, "")

    # ---------------------------------------------------------------- CASE 5 disable A -> B (+ CASE 17)
    na = len(ext_ops(a, pid, a.device_id))
    local_change(a, DUMMY, False)
    new_a = ext_ops(a, pid, a.device_id)[na:]
    check("C5.26-27 A disabled the dummy: exactly one delta op [enabled]=false", len(new_a) == 1 and new_a[0]["fields"] == ["enabled"]
          and new_a[0]["enabled"] is False and new_a[0]["id"] == DUMMY, json.dumps(new_a))
    b.dev("--dev-sync", b.mid)  # the app downloads it; the browser has not applied it yet
    ui = AppUI(b)
    st2 = ui.state("Synchrome Test Dummy")
    ui.close()
    check("C20.110 UI#2 state Applying between download and browser apply", st2 == "Applying", str(st2))
    nb = len(ext_ops(a, pid, b.device_id))
    sup0 = int(b.hl.js("suppressedEvents") or 0)
    sl = pull(b, js_after=f"await chrome.management.setEnabled('{DUMMY2}', false);")  # CASE 17: legit edit right after
    ra.wait(lambda: idle(b), 60)
    plans = [p_ for p_ in ext_plans(sl) if p_[0]]
    new_b = ext_ops(a, pid, b.device_id)[nb:]
    check("C5.28-30 B pulled and applied it with chrome.management.setEnabled (plan 'off'); real state disabled",
          bool(plans) and plans[0][1] == "off" and mgmt(b).get(DUMMY) is False, json.dumps(plans))
    check("C5.31 echo suppression: B published nothing for the dummy; its onDisabled was suppressed",
          not any(o["id"] == DUMMY for o in new_b) and int(b.hl.js("suppressedEvents") or 0) > sup0, json.dumps(new_b))
    check("C5.32 reconcile snapshot after the apply = 0 ops", any("ops: 0" in x for x in ext_snaps(sl)), json.dumps(ext_snaps(sl)))
    check("C17.95-98 apply events suppressed; a legit local edit right after the reconcile (same task, no delay) IS published: no blind time window",
          [(o["id"], o["enabled"]) for o in new_b] == [(DUMMY2, False)], json.dumps(new_b))
    pull(a)
    check("C17b A received B's legit edit (synchronised state disabled; A does not have that extension: Missing)",
          views(a).get(DUMMY2, {}).get("enabled_synced") is False and views(a)[DUMMY2]["state"] == "missing", "")

    # ---------------------------------------------------------------- CASE 6 enable B -> A
    nb, na = len(ext_ops(a, pid, b.device_id)), len(ext_ops(a, pid, a.device_id))
    local_change(b, DUMMY, True)
    new_b = ext_ops(a, pid, b.device_id)[nb:]
    pull(a)
    check("C6.33-34 B enabled the dummy locally and published [enabled]=true", [(o["id"], o["fields"], o["enabled"]) for o in new_b] == [(DUMMY, ["enabled"], True)], json.dumps(new_b))
    check("C6.35-37 A pulled and applied enabled=true; A echoed nothing", mgmt(a).get(DUMMY) is True and len(ext_ops(a, pid, a.device_id)) == na, "")
    check("C6.38 A and B converge", mgmt(a).get(DUMMY) is True and mgmt(b).get(DUMMY) is True and views(a)[DUMMY]["state"] == views(b)[DUMMY]["state"] == "synced", "")

    # ---------------------------------------------------------------- CASE 7 restart
    _, cb0, eb0 = ra.journal(a, pid, b.device_id)
    cur0 = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    mark = log_mark(b)
    restart(b, fixtures=[DUMMY_DIR, DUMMY2_DIR])
    app = b.dev("--dev-sync", b.mid)  # host/app restart: a fresh process
    pull(b)
    sl = log_since(b, mark)
    _, cb1, eb1 = ra.journal(a, pid, b.device_id)
    check("C7.39-44 restart Helium B + app/host B: snapshot 0 ops (or hash-skipped), no duplicated entity/chunks/store_events, cursor unchanged",
          all("ops: 0" in x for x in ext_snaps(sl)) and len(cb1) == len(cb0) and len(eb1) == len(eb0) and "chunks: 0" in app.get("pull", "")
          and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == cur0 and DUMMY in views(b),
          f"snaps={ext_snaps(sl)} chunks {len(cb0)}->{len(cb1)} cursor={cur0}")

    # ---------------------------------------------------------------- CASE 8 offline
    restart(b, offline=True, fixtures=[DUMMY_DIR, DUMMY2_DIR])
    cur_off0 = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    local_change(a, DUMMY, False)
    b.hl.js("run(true)", timeout=90)
    ra.wait(lambda: idle(b), 60)
    off = b.dev("--dev-sync", b.mid, offline=True)
    check("C8.45-48 B without Cloud while A changed it and published: B Offline, unchanged, checkpoint kept",
          mgmt(b).get(DUMMY) is True and "offline" in off.get("pull", "") and (b.status().get("download") or {}).get("state") == "offline"
          and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == cur_off0, off.get("pull", ""))
    t0 = time.time()
    restart(b, fixtures=[DUMMY_DIR, DUMMY2_DIR])
    pull(b)
    M["latency_ms"].append(int((time.time() - t0) * 1000))
    a_seqs = sorted(o["seq"] for x in ra.journal(a, pid, a.device_id)[1] for o in x["ops"])
    check("C8.49-51 Cloud restored: B downloaded and converged; A's journal seq continuous and fully applied (nothing lost)",
          mgmt(b).get(DUMMY) is False and a_seqs == list(range(1, len(a_seqs) + 1))
          and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == a_seqs[-1], "")

    # ---------------------------------------------------------------- CASE 9 protected (mayDisable=false, mocked)
    mock(b, protect=[DUMMY])
    snapshot_now(b)
    nb = len(ext_ops(a, pid, b.device_id))
    local_change(a, DUMMY, True)
    sl = pull(b)
    vb = views(b)
    ui = AppUI(b)
    st3, ac3 = ui.state("Synchrome Test Dummy"), ui.actions("Synchrome Test Dummy")
    ui.close()
    check("C9.52-53 mayDisable=false (mocked): Protected; setEnabled and uninstall NEVER called; nothing published; no plan op",
          vb.get(DUMMY, {}).get("state") == "protected" and calls(b) == [] and len(ext_ops(a, pid, b.device_id)) == nb
          and not any(n for n, _, _ in ext_plans(sl)) and mgmt(b, real=True).get(DUMMY) is False, json.dumps({"calls": calls(b), "plans": ext_plans(sl)}))
    check("C20.110 UI#3 Protected shown without actions", st3 == "Protected" and ac3 == [], json.dumps([st3, ac3]))

    # ---------------------------------------------------------------- CASE 10 setEnabled refused (mocked)
    restart(b, fixtures=[DUMMY_DIR, DUMMY2_DIR])  # mock gone: protection lifted -> the synced state is applied
    pull(b)
    check("C9b protection lifted: B applies the synchronised state (enabled) without publishing",
          mgmt(b).get(DUMMY) is True and views(b)[DUMMY]["state"] == "synced" and len(ext_ops(a, pid, b.device_id)) == nb, "")
    mock(b, reject=True)
    local_change(a, DUMMY, False)
    pull(b)
    sl2 = pull(b)
    sl3 = pull(b)
    vb = views(b)
    ptxt = popup_text(b)
    M["user_action"] += 1
    check("C10.54-55 setEnabled refused (mocked 'requires a user gesture'): User action required",
          vb.get(DUMMY, {}).get("state") == "user_action_required" and [c_[0] for c_ in calls(b)] == ["setEnabled"], json.dumps(calls(b)))
    check("C10.56 the popup offers the manual action", "Disable “Synchrome Test Dummy”" in ptxt, "button present" if "Disable “" in ptxt else ptxt[:160])
    check("C10.57 no infinite retry: the next pulls plan nothing for it", not any(n for n, _, _ in ext_plans(sl2) + ext_plans(sl3)) and len(calls(b)) == 1, "")
    check("C10.58 no false echo: B published nothing", len(ext_ops(a, pid, b.device_id)) == nb, "")
    ui = AppUI(b)
    st4, ac4, acu = ui.state("Synchrome Test Dummy"), ui.actions("Synchrome Test Dummy"), ui.actions("Synchrome Second Dummy")
    ui.close()
    check("C20.110-111 UI#4 User action required with Retry; a disabled synced item offers Enable",
          st4 == "User action required" and ac4 == ["Retry"] and acu == ["Enable"], json.dumps([st4, ac4, acu]))
    local_change(b, DUMMY, False)  # the user's manual click (real API)
    pull(b)
    check("C10b after the user's manual action: Synced, nothing published", views(b)[DUMMY]["state"] == "synced" and len(ext_ops(a, pid, b.device_id)) == nb, "")
    check("C22.121 when setEnabled fails, the product falls back to User action required", vb.get(DUMMY, {}).get("state") == "user_action_required", "")

    # ---------------------------------------------------------------- CASE 11 uninstall on A
    restart(b, fixtures=[DUMMY_DIR, DUMMY2_DIR])  # plain B (no mock)
    restart(a)  # TEST MECHANISM: relaunch without the dummy (and without the theme / mocks)
    del_a = [o for o in ext_ops(a, pid, a.device_id) if o["id"] == DUMMY and o["op"] == "delete"]
    check("C11.59-61 A detected the dummy's absence (snapshot) and published it as uninstalled", len(del_a) == 1, json.dumps(del_a))
    pull(b)
    vb = views(b)
    check("C11.62-64 B: Removed on another device, still installed (no automatic uninstall), actions Remove here / Keep here",
          vb.get(DUMMY, {}).get("state") == "removed_elsewhere" and DUMMY in mgmt(b) and vb[DUMMY]["actions"] == ["remove", "keep"], json.dumps(vb.get(DUMMY)))

    # ---------------------------------------------------------------- CASE 12 Remove here
    direct = b.hl.js(f"chrome.management.uninstall('{DUMMY}', {{showConfirmDialog: false}}).then(() => 'ok', e => e.message)")
    check("C12.65 chrome.management.uninstall without a gesture is refused by Chromium", "gesture" in str(direct), str(direct))
    ui = AppUI(b)
    st5 = ui.state("Synchrome Test Dummy")
    ui.js("btn(xrow('Synchrome Test Dummy'), 'Remove here').click(); return true;")
    asks = ui.until("return !!btn(xrow('Synchrome Test Dummy'), 'Confirm: remove');", 10)
    present_before = DUMMY in mgmt(b)
    ui.js("btn(xrow('Synchrome Test Dummy'), 'Confirm: remove').click(); return true;")
    saved = ui.until("const s = document.querySelector('[role=status]'); return !!s && s.textContent.includes('Remove here');", 20)
    ui.close()
    check("C20.112 UI#5 Removed on another device; Remove here needs an explicit confirmation (first click only asks)",
          st5 == "Removed on another device" and bool(asks) and present_before and bool(saved), str(st5))
    sl = pull(b)
    vb = views(b)
    ptxt = popup_text(b)
    M["user_action"] += 1
    check("C12.66-67 the uninstall attempted from the worker is refused (gesture): User action required; the popup offers the removal",
          vb.get(DUMMY, {}).get("state") == "user_action_required" and "Remove “Synchrome Test Dummy”" in ptxt and DUMMY in mgmt(b)
          and any(o == "rm" for _, o, _ in ext_plans(sl)), json.dumps(ext_plans(sl)))
    nb = len(ext_ops(a, pid, b.device_id))
    restart(b, fixtures=[DUMMY2_DIR])  # SIMULATED: the user confirmed in the popup / Chromium's native dialog
    pull(b)
    check("C12.68-70 (simulated final confirmation) dummy absent on B, converged, no spurious ops, nothing removed before the user's confirmation",
          DUMMY not in mgmt(b) and DUMMY not in views(b) and len(ext_ops(a, pid, b.device_id)) == nb, "")

    # ---------------------------------------------------------------- CASE 13 Keep here
    restart(a, fixtures=[DUMMY_DIR])  # installed again on A (published as installed)
    restart(b, fixtures=[DUMMY_DIR, DUMMY2_DIR])
    pull(b)
    pull(a)
    restart(a)  # uninstall on A again
    del_hlc = [o for o in ext_ops(a, pid, a.device_id) if o["id"] == DUMMY and o["op"] == "delete"][-1]["hlc"]
    pull(b)
    check("C13.71 remote uninstall again: B shows Removed on another device", views(b).get(DUMMY, {}).get("state") == "removed_elsewhere", "")
    nb = len(ext_ops(a, pid, b.device_id))
    r = b.dev("--dev-ext-action", b.mid, DUMMY, "keep")
    keep = ext_ops(a, pid, b.device_id)[nb:]
    check("C13.72-74 Keep here: one normal op (installed: all fields) with an HLC later than the remote delete",
          r.get("ok") and len(keep) == 1 and keep[0]["op"] == "set" and set(keep[0]["fields"]) == ALLOWED_FIELDS and keep[0]["hlc"] > del_hlc,
          json.dumps({"ok": r.get("ok"), "after_delete": bool(keep) and keep[0]["hlc"] > del_hlc}))
    pull(a)
    va = views(a)
    check("C13.75-77 A received it: not installed automatically; Missing (unpacked: Unavailable — the equivalent of Install available)",
          DUMMY not in mgmt(a) and va.get(DUMMY, {}).get("state") == "missing", json.dumps(va.get(DUMMY)))

    # ---------------------------------------------------------------- CASE 14 conflict local_pending (both choices)
    created = 0
    for choice in ("keep_local", "keep_remote"):
        restart(a, fixtures=[DUMMY_DIR])
        pull(a)
        local_change(a, DUMMY, True)
        pull(b)
        base_ok = mgmt(a).get(DUMMY) is True and mgmt(b).get(DUMMY) is True
        restart(b, offline=True, fixtures=[DUMMY_DIR, DUMMY2_DIR])
        local_change(b, DUMMY, False)  # B: pending local change
        stop(b)
        restart(a)  # A: remote uninstall
        start(b, fixtures=[DUMMY_DIR, DUMMY2_DIR])
        ra.wait(lambda: views(b).get(DUMMY, {}).get("state") == "conflict", 60)
        cv = [c_ for c_ in open_conflicts(b) if c_["item_kind"] == "extension"]
        c14 = cv[0] if cv else {}
        created += len(cv)
        check(f"C14.78-83 [{choice}] base installed+enabled; B pending disable vs A's uninstall -> extension conflict local_pending/edit_delete (shared records)",
              base_ok and c14.get("kind") == "local_pending" and c14.get("subtype") == "edit_delete" and c14.get("local", {}).get("exists")
              and c14["local"]["enabled"] is False and c14["remote"]["exists"] is False
              and [o["resolution"] for o in c14.get("options", [])] == ["keep_local", "keep_remote"], json.dumps({k: c14.get(k) for k in ("kind", "subtype")}))
        if choice == "keep_local":
            ui = AppUI(b)
            st6 = ui.state("Synchrome Test Dummy")
            ac6 = ui.actions("Synchrome Test Dummy")
            ui.js("btn(xrow('Synchrome Test Dummy'), 'Resolve conflict').click(); return true;")
            card = ui.until("const c = card('Synchrome Test Dummy'); return c ? all('tr', c).map(r => r.innerText.replace(/\\s+/g, ' ').trim()) : null;", 20) or []
            ui.js("btn(card('Synchrome Test Dummy'), \"Keep this device's version\").click(); return true;")
            done = ui.until("const s = document.querySelector('.conflicts [role=status]'); return !!s && s.textContent.includes('Resolved');", 20)
            panel = ui.panel()
            ui.close()
            rows = " | ".join(card)
            check("C20.110-113 UI#6 Conflict + 'Resolve conflict'; the card shows Installed and Enabled of both sides; Keep local from the UI",
                  st6 == "Conflict" and "Resolve conflict" in (ac6 or []) and "Installed yes removed" in rows and "Enabled Disabled" in rows and bool(done), rows)
            check("C20.114 the UI shows no internal UUIDs or secrets", not re.search(r"[0-9a-f]{8}-[0-9a-f]{4}-", panel) and not h.JWT.search(panel), "")
        else:
            rr = b.dev("--dev-resolve", b.mid, c14.get("conflict_id", ""), "keep_remote").get("results", [{}])[0]
            check("C14.85 Keep remote: resolved; the removal is accepted (nothing uninstalled automatically)", rr.get("outcome") == "resolved", json.dumps(rr))
        pull(b)
        pull(a)
        hist = [c_ for c_ in (b.dev("--dev-conflict-views", b.mid, "all").get("conflicts") or []) if c_.get("conflict_id") == c14.get("conflict_id")]
        if choice == "keep_local":
            check("C14.84/87 Keep local: B keeps it (disabled), it is in the synchronised state again (A: Missing), conflict resolved",
                  mgmt(b).get(DUMMY) is False and views(b)[DUMMY]["state"] == "synced" and views(a).get(DUMMY, {}).get("state") == "missing"
                  and bool(hist) and hist[0]["status"] == "resolved", "")
        else:
            check("C14.85b/87 after Keep remote: B shows Removed on another device, still installed; conflict resolved",
                  views(b).get(DUMMY, {}).get("state") == "removed_elsewhere" and DUMMY in mgmt(b) and bool(hist) and hist[0]["status"] == "resolved", "")
    check("C14.86 no parallel resolver: both resolved through the shared conflict records/API", created == 2, f"created={created}")

    # ---------------------------------------------------------------- CASES 15 + 16 LWW / three-way
    b.dev("--dev-ext-action", b.mid, DUMMY, "keep")
    pull(a)  # A receives B's decision first (otherwise A's reinstall and B's Keep are concurrent: a real conflict, case 14)
    restart(a, fixtures=[DUMMY_DIR])
    pull(b)
    pull(a)
    local_change(a, DUMMY, True)
    pull(b)
    local_change(b, DUMMY, False)
    pull(a)
    newest = max((o for o in ext_ops(a, pid) if o["id"] == DUMMY and o["op"] == "set"), key=lambda o: o["hlc"])
    check("C15.88-90 published changes from A and B merge by HLC: both end with the newest op's value, no conflict",
          mgmt(a).get(DUMMY) is False and mgmt(b).get(DUMMY) is False and newest["enabled"] is False and not open_conflicts(a) and not open_conflicts(b),
          json.dumps({"newest_enabled": newest["enabled"], "a": mgmt(a).get(DUMMY), "b": mgmt(b).get(DUMMY),
                      "va": {k: views(a).get(DUMMY, {}).get(k) for k in ("state", "enabled_here", "enabled_synced", "installed_here")},
                      "vb": {k: views(b).get(DUMMY, {}).get(k) for k in ("state", "enabled_here", "enabled_synced", "installed_here")},
                      "open": [(c_["item_kind"], c_["subtype"], c_["status"]) for d_ in (a, b) for c_ in open_conflicts(d_)],
                      "ops": [(o["by"][:4], o["op"], o["fields"] and len(o["fields"]), o["enabled"]) for o in ext_ops(a, pid) if o["id"] == DUMMY][-6:]}))
    local_change(b, DUMMY, True)
    pull(a)  # base: enabled on both
    restart(a, offline=True, fixtures=[DUMMY_DIR])
    local_change(a, DUMMY, False)  # A: unpublished disable (BASE: enabled)
    stop(a)
    local_change(b, DUMMY, False)
    local_change(b, DUMMY, True)  # B's newest published value = A's BASE
    na = len(ext_ops(a, pid, a.device_id))
    start(a, fixtures=[DUMMY_DIR])
    pull(a)
    pull(b)
    reassert = [o for o in ext_ops(a, pid, a.device_id)[na:] if o["id"] == DUMMY]
    ca = [c_ for c_ in (a.dev("--dev-conflict-views", a.mid, "all").get("conflicts") or []) if c_["item_kind"] == "extension"]
    check("C16.91-94 remote = BASE: the local edit wins on both, re-emitted with a fresh HLC (LWW alone picks the remote), no open conflict",
          mgmt(a).get(DUMMY) is False and mgmt(b).get(DUMMY) is False and not [c_ for c_ in ca if c_["status"] == "unresolved"]
          and any(c_["status"] == "superseded" for c_ in ca) and len(reassert) >= 2 and reassert[-1]["enabled"] is False,
          json.dumps({"superseded": [c_.get("detail") for c_ in ca if c_["status"] == "superseded"], "a_ops": [o["enabled"] for o in reassert]}))

    # ---------------------------------------------------------------- CASE 18 duplicates / checkpoint
    cur = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    nb = len(ext_ops(a, pid, b.device_id))
    dup = b.dev("--dev-pull-duplicates", b.mid)
    again = b.dev("--dev-sync", b.mid)  # fresh process (state/app restart)
    check("C18.99-102 events presented twice, then app restart and pull again: 0 duplicates, 0 new ops, cursor unchanged",
          "chunks: 0, ops: 0" in dup.get("pull", "") and "chunks: 0, ops: 0" in again.get("pull", "")
          and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == cur and len(ext_ops(a, pid, b.device_id)) == nb,
          json.dumps({"dup": dup.get("pull"), "again": again.get("pull")}))

    # ---------------------------------------------------------------- CASE 21 security (before USER_B switches the shared Keychain)
    ext = "".join(open(os.path.join(h.EXT, f), errors="replace").read() for f in os.listdir(h.EXT) if os.path.isfile(os.path.join(h.EXT, f)))
    env = open(os.path.join(h.ROOT, ".env")).read()
    anon = next((l.split("=", 1)[1].strip().strip('"') for l in env.splitlines() if l.startswith("SUPABASE_ANON_KEY=")), "")
    check("C21.115 Synchrome extension: no SUPABASE_URL, no anon key, no JWT, no direct fetch/XHR/WebSocket",
          bool(anon) and anon not in ext and "supabase" not in ext.lower() and not h.JWT.search(ext)
          and not re.search(r"\bfetch\(|XMLHttpRequest|WebSocket|EventSource", ext), "")
    for dv in (a, b):
        lg = dv.log()
        leaks = [w_ for w_ in NAMES if w_ in lg] + [p_ for p_ in ("/Users/", "/tmp/", "Application Support") if p_ in lg]
        check(f"C21.116 device {dv.name} host log: no extension names, no local paths, no JWT/refresh/mapping tokens", not leaks and h.secret_free(lg), f"leaks={leaks}")
    ul = "\n".join(UI_LOG)
    check("C21.116b app (UI) output: no extension names, no tokens", not any(w_ in ul for w_ in NAMES) and h.secret_free(ul), "")
    allops = ext_ops(a, pid)
    blob = json.dumps(allops)
    check("C21.117 cloud: no permissions, settings, paths or updateUrl in any extensions op",
          all(set(o["fields"] or []) <= ALLOWED_FIELDS for o in allops) and "http" not in blob and "/Users" not in blob, "")
    check("C21.118 Synchrome never manages itself: no op targets its id", not any(o["id"] == h.EXT_ID for o in allops), "")
    check("C21.119 protected items never received an apply (no call recorded on the protected dummy, nothing published for the policy item)",
          FAKE_ADMIN not in {o["id"] for o in allops}, "")
    check("C22.120 (record) Helium 154 dev: setEnabled without gesture OK, uninstall needs a gesture; Chrome stable / Web Store: automatic setEnabled NOT validated", True, "")

    # ---------------------------------------------------------------- metrics, then CASE 19 isolation
    stop(a)
    stop(b)
    _, ca_, ea_ = ra.journal(a, pid, a.device_id)
    _, cb_, eb_ = ra.journal(a, pid, b.device_id)
    logs = a.log() + b.log()
    codes = [c_ for o in re.findall(r"extensions apply plan ops=\d+ order=(\S+)", logs) for c_ in o.split(",") if c_]
    full = lambda o: o["op"] == "set" and len(o["fields"] or []) > 1
    lat = M["latency_ms"]
    print("METRICS " + json.dumps({
        "extensions_synchronised": sorted({o["id"][:6] for o in allops}),
        "snapshots_received": len(re.findall(r"extensions snapshot parts=", logs)),
        "ops_installed": sum(1 for o in allops if full(o)),
        "ops_uninstall": sum(1 for o in allops if o["op"] == "delete"),
        "ops_enabled": sum(1 for o in allops if o["op"] == "set" and o["fields"] == ["enabled"] and o["enabled"] is True),
        "ops_disabled": sum(1 for o in allops if o["op"] == "set" and o["fields"] == ["enabled"] and o["enabled"] is False),
        "applies_setEnabled": sum(1 for c_ in codes if c_ in ("on", "off")),
        "uninstall_attempts": codes.count("rm"),
        "suppressed_echo_events": M["suppressed"],
        "user_action_required": M["user_action"],
        "conflicts_created": created, "conflicts_resolved": created,
        "chunks": {"A": len(ca_), "B": len(cb_)}, "store_events": {"A": len(ea_), "B": len(eb_)},
        "latency_pull_to_apply_ms": {"mean": int(sum(lat) / len(lat)) if lat else None, "max": max(lat) if lat else None, "n": len(lat)},
        "ui_sessions": M["ui_sessions"],
    }))
    check("C23 metrics collected; chunks == store_events per device", len(ca_) == len(ea_) and len(cb_) == len(eb_), f"A {len(ca_)}/{len(ea_)} B {len(cb_)}/{len(eb_)}")

    first = ca_[0]["path"] if ca_ else ""
    cdev = ra.Device("C")
    u = cdev.dev("--dev-sign-in", creds, "B")
    probe = cdev.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, first) if u.get("signed_in") else {}
    refd = lambda v_: isinstance(v_, dict) and "Err" in v_
    check("C19.103 login USER_B (exact id)", u.get("user_id") == h.EXPECTED["B"], f"user_id={u.get('user_id')}")
    check("C19.104 USER_B cannot see A's profile/mappings, list or download A's journals, read A's store_events, or write under A's prefix",
          probe.get("foreign_profile_visible") == {"Ok": False} and probe.get("foreign_mappings_visible") == {"Ok": 0}
          and probe.get("list_sees") == {"Ok": 0} and probe.get("get_first_chunk") == {"Ok": False} and probe.get("store_events_sees") == {"Ok": 0}
          and refd(probe.get("put_under_foreign_prefix")) and refd(probe.get("store_event_insert")), json.dumps({k: v_ for k, v_ in probe.items() if k != "as_user"}))
    lv = b.dev("--dev-ext-views", b.mid)
    la = b.dev("--dev-ext-action", b.mid, DUMMY, "enable")
    lc = b.dev("--dev-conflict-views", b.mid, "all")
    leak = json.dumps([lv, la, lc])
    check("C19.105-106 USER_B cannot list or act on the extensions/conflicts of A's installation; no names or states leak",
          "error" in lv and "error" in la and "error" in lc and not any(w_ in leak for w_ in NAMES) and "enabled" not in leak,
          json.dumps({"views": lv.get("error"), "action": la.get("error"), "conflicts": lc.get("error")}))
    a.dev("--dev-sign-in", creds, "A")

    # ---------------------------------------------------------------- CASE 24 cleanup
    rb = b.dev("--dev-cleanup", b.mid)
    rbd = b.dev("--dev-delete-this-device")
    rA = a.dev("--dev-cleanup", a.mid)
    gone = a.dev("--dev-cloud-tree", pid)
    inv = a.dev("--dev-inventory")
    check("C24 cloud: Storage objects, store_events, profile, both mappings, both devices removed",
          rb.get("unmapped") and rbd.get("deleted") and rA.get("unmapped") and rA.get("deleted_profile") and rA.get("device_deleted")
          and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0
          and inv.get("devices") == [], json.dumps({"B": rb, "A": rA}))
    so = a.dev("--dev-sign-out")
    check("C24 Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, cdev):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    shutil.rmtree(os.path.join(ra.WORK, "ui-driver"), ignore_errors=True)
    try:
        os.remove(os.path.join(ra.WORK, "ui-app.log"))
    except FileNotFoundError:
        pass
    check("C24 local: data dirs (state.sqlite incl. ext_entities + conflicts, registry, logs, backups), Helium profiles + host manifests, UI driver files",
          not any(os.path.exists(p_) for dv in (a, b, cdev) for p_ in (dv.udd, dv.data)), "")


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
    print(f"\n=== extensions: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
