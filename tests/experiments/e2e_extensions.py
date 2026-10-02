"""Two-device E2E of the extensions inventory + enabled state (M1 slice 10).

Devices A and B: two simulated installations on this machine (own SYNCHROME_DATA_DIR each),
both signed in as test user A, each with its own disposable Helium --user-data-dir. Test
user B is used only for the isolation probe. Extensions involved: Synchrome itself (never
synchronised) and the inert fixture tests/fixtures/dummy-extension (fixed key => same id on
both devices); Helium's bundled uBlock Origin is present on both fresh profiles and is used
for the offline step. No extension of the user is involved.

Test mechanisms (documented, Helium only):
- "install" / "uninstall" of the dummy = relaunching Helium with / without
  --load-extension (Chromium requires a user gesture and shows its own confirmation dialog to
  uninstall another extension; a headless run cannot accept it);
- enable/disable = chrome.management.setEnabled from Synchrome's worker (E7: Helium allows it
  without a gesture; NOT validated on Google Chrome stable / Web Store items).

Prints ids, counts, field names and op codes only — no tokens, no passwords.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_extensions.py <creds-env-file>
"""
import json, os, re, shutil, signal, subprocess, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402
import e2e_conflicts as cf  # noqa: E402  (UI driver, robust start)
from cdp import CDP  # noqa: E402

check = h.check
DUMMY_DIR = os.path.join(h.ROOT, "tests", "fixtures", "dummy-extension")
DUMMY = "dlijhhlcpppmppbekhaonjhlahfidkgf"
UBO = "blockjmkbacgjkknlgpkjjiijinjdanf"  # bundled with Helium
NAMES = ("Synchrome Test Dummy", "uBlock")
M = {"latency_ms": {}, "suppressed": {"A": 0, "B": 0}}


class XHelium(eb.Helium):
    """eb.Helium with extra unpacked extensions loaded next to Synchrome."""

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


def start(d, offline=False, dummy=False):
    env = dict(d.env)
    if offline:
        env["SUPABASE_URL"] = eb.OFFLINE_URL
    d.hl = XHelium(d.udd, env=env, extra=[DUMMY_DIR] if dummy else [])
    for i in range(20):
        try:
            d.hl.js("chrome.alarms.clearAll().then(() => true)")
            break
        except RuntimeError:
            if i == 19:
                raise
            time.sleep(0.5)
    t = eb.now_ms()
    want = "connected_offline" if offline else "connected"
    d.hl.wait(lambda s: s.get("status") == want, 60, after=t - 60000)
    ra.wait(d.idle, 60)
    return d.hl


def stop(d):
    if d.hl:
        try:
            M["suppressed"][d.name] += int(d.hl.js("suppressedEvents") or 0)
        except Exception:  # noqa: BLE001
            pass
        d.hl.stop()
        d.hl = None


def restart(d, offline=False, dummy=False):
    stop(d)
    return start(d, offline=offline, dummy=dummy)


def mgmt(d):
    """{id: enabled} of the browser's extensions (Synchrome excluded)."""
    return {e["id"]: e["en"] for e in d.hl.js("chrome.management.getAll().then(l => l.map(e => ({id: e.id, en: e.enabled})))")
            if e["id"] != h.EXT_ID}


def set_enabled(d, ext, on):
    """A change in the browser (as the user would make it on chrome://extensions)."""
    d.hl.js(f"chrome.management.setEnabled('{ext}', {str(on).lower()}).then(() => true)")
    time.sleep(0.5)  # the onEnabled/onDisabled event queues the change
    ra.wait(lambda: bool(d.hl.js("extQueue.length === 0 && running === null && flushTimer === null")), 60, 0.5)


def views(d):
    return {x["ext_id"]: x for x in (d.dev("--dev-ext-views", d.mid).get("extensions") or [])}


def ext_ops(a, pid, dev_id=None):
    ops = a.dev("--dev-cloud-tree", pid).get("ext_ops") or []
    return [o for o in ops if dev_id is None or o["by"] == dev_id]


def pull(d):
    sl, ms = ra.pull_b(d)
    log = open(os.path.join(d.data, "logs", "native-host.log"), errors="replace").read()
    return sl, ms, log


def ext_plans(log_slice):
    return re.findall(r"extensions apply plan ops=(\d+) order=(\S*) user_actions=(\d+)", log_slice)


def popup_text(d):
    """Open Synchrome's popup page in a tab (as a click on the toolbar icon would) and read it."""
    c = d.hl.c
    t = c.call("Target.createTarget", {"url": f"chrome-extension://{h.EXT_ID}/popup.html"})
    s = c.call("Target.attachToTarget", {"targetId": t["targetId"], "flatten": True})["sessionId"]
    txt = ""
    for _ in range(20):
        time.sleep(0.5)
        r = c.call("Runtime.evaluate", {"expression": "document.body.innerText", "returnByValue": True}, session=s)
        txt = r.get("result", {}).get("value") or ""
        if "Extensions:" in txt:
            break
    c.call("Target.closeTarget", {"targetId": t["targetId"]})
    return txt


def main(creds):
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a, b = ra.Device("A"), ra.Device("B")
    print("=== two-device extensions E2E ===")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())
    sa = a.dev("--dev-sign-in", creds, "A")
    check("pre: login user A for device A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if not sa.get("signed_in"):
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("pre: login user A for device B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
    a.device_id = a.dev("--dev-register-device").get("device_id")
    b.device_id = b.dev("--dev-register-device").get("device_id")
    check("pre: distinct device ids / data dirs", a.device_id != b.device_id and a.data != b.data, f"A={a.device_id} B={b.device_id}")

    # 1 mappings
    m = a.dev("--dev-create-mapping", f"E2E EXT {os.urandom(3).hex()}", "helium", "Default")
    a.mid, pid = m.get("mapping_id"), m.get("profile_id")
    a.dev("--dev-write-token", a.mid, a.udd)
    a.dev("--dev-install-host", a.udd)
    start(a)
    mb = b.dev("--dev-map-existing", pid, "helium", "Default")
    b.mid = mb.get("mapping_id")
    b.dev("--dev-write-token", b.mid, b.udd)
    b.dev("--dev-install-host", b.udd)
    start(b)
    check("1 A and B mapped to the same profile, confirmed", (a.hl.status() or {}).get("token_state") == "confirmed"
          and (b.hl.status() or {}).get("token_state") == "confirmed" and mb.get("profile_id") == pid, f"profile={pid}")

    # 2-4 dummy on A
    restart(a, dummy=True)
    ra.wait(lambda: any(o["id"] == DUMMY for o in ext_ops(a, pid, a.device_id)), 60)
    ops = ext_ops(a, pid, a.device_id)
    d_op = next((o for o in ops if o["id"] == DUMMY), {})
    check("2-3 dummy loaded on A; A's snapshot captured it (Synchrome itself never)", DUMMY in mgmt(a) and not any(o["id"] == h.EXT_ID for o in ext_ops(a, pid)), "")
    check("4 cloud: dummy installed + enabled, fields = enabled/name/source/type/version only (no URLs, paths, permissions)",
          d_op.get("op") == "set" and d_op.get("enabled") is True and d_op.get("fields") == ["enabled", "name", "source", "type", "version"],
          json.dumps(d_op.get("fields")))

    # 5 B: missing
    pull(b)
    vb = views(b)
    check("5 B does not have it: Missing, Unavailable (unpacked: no store), no automatic install, no Install action",
          vb.get(DUMMY, {}).get("state") == "missing" and vb[DUMMY]["availability"] == "unavailable" and vb[DUMMY]["actions"] == []
          and DUMMY not in mgmt(b), json.dumps({k: vb.get(DUMMY, {}).get(k) for k in ("state", "availability", "actions")}))
    check("5b the extension both fresh Helium profiles carry (uBlock Origin, bundled) is synced on both", vb.get(UBO, {}).get("state") == "synced", "")

    # 6-7 install on B (test mechanism)
    nb = len(ext_ops(a, pid, b.device_id))
    restart(b, dummy=True)
    pull(b)
    vb = views(b)
    check("6-7 dummy installed on B (relaunch with --load-extension): B converges (Synced), publishing nothing for it",
          vb.get(DUMMY, {}).get("state") == "synced" and not any(o["id"] == DUMMY for o in ext_ops(a, pid, b.device_id)[nb:]), "")

    # 8-10 disable on A -> B applies, echo 0
    nb = len(ext_ops(a, pid, b.device_id))
    t0 = time.time()
    set_enabled(a, DUMMY, False)
    last = ext_ops(a, pid, a.device_id)[-1]
    check("8 A disabled the dummy: one op [enabled]=false published", last["id"] == DUMMY and last["fields"] == ["enabled"] and last["enabled"] is False, json.dumps(last))
    mark = len(open(os.path.join(b.data, "logs", "native-host.log"), "rb").read())
    sup0 = int(b.hl.js("suppressedEvents") or 0)
    pull(b)
    M["latency_ms"]["disable_A_to_B"] = int((time.time() - t0) * 1000)
    sl = open(os.path.join(b.data, "logs", "native-host.log"), "rb").read()[mark:].decode("utf-8", "replace")
    plans = [p_ for p_ in ext_plans(sl) if p_[0] != "0"]
    check("9 B applied it with chrome.management.setEnabled (plan 'off')", mgmt(b).get(DUMMY) is False and plans and plans[0][1] == "off", json.dumps(plans))
    check("10 echo = 0: B published nothing; the onDisabled event was suppressed",
          len(ext_ops(a, pid, b.device_id)) == nb and int(b.hl.js("suppressedEvents") or 0) > sup0 and views(b)[DUMMY]["state"] == "synced", "")

    # 11-12 enable on B -> A
    t0 = time.time()
    set_enabled(b, DUMMY, True)
    pull(a)
    M["latency_ms"]["enable_B_to_A"] = int((time.time() - t0) * 1000)
    check("11-12 B enabled it; A received and applied it", mgmt(a).get(DUMMY) is True and views(a)[DUMMY]["state"] == "synced", "")

    # 13 concurrent, already-published values: three-way/LWW, no decision needed
    stop(a)
    start(a, offline=True, dummy=True)
    a.hl.js(f"chrome.management.setEnabled('{DUMMY}', false).then(() => true)")
    ra.wait(lambda: a.status().get("pending", 0) >= 1 and a.idle(), 60)
    stop(a)
    set_enabled(b, DUMMY, False)
    set_enabled(b, DUMMY, True)  # B's newest published value = A's pre-edit state
    start(a, dummy=True)
    pull(a)
    pull(b)
    ca = a.dev("--dev-conflict-views", a.mid, "all").get("conflicts") or []
    check("13 concurrent changes (A unpublished disable vs B's published disable->enable): no decision asked, A's change wins on both",
          not [c for c in ca if c["status"] == "unresolved"] and mgmt(a).get(DUMMY) is False and mgmt(b).get(DUMMY) is False,
          json.dumps([(c["subtype"], c["status"], c.get("detail")) for c in ca]))

    # 14-15 explicit conflict: B unpublished enable vs A's uninstall
    stop(b)
    start(b, offline=True, dummy=True)
    b.hl.js(f"chrome.management.setEnabled('{DUMMY}', true).then(() => true)")
    ra.wait(lambda: b.status().get("pending", 0) >= 1 and b.idle(), 60)
    stop(b)
    restart(a, dummy=False)  # A uninstalls it
    ra.wait(lambda: any(o["id"] == DUMMY and o["op"] == "delete" for o in ext_ops(a, pid, a.device_id)), 60)
    t0 = time.time()
    start(b, dummy=True)
    ra.wait(lambda: views(b).get(DUMMY, {}).get("state") == "conflict", 60)
    cv = [c for c in (b.dev("--dev-conflict-views", b.mid).get("conflicts") or []) if c["item_kind"] == "extension"]
    c14 = cv[0] if cv else {}
    check("14 B's unpublished enable vs A's uninstall: explicit conflict (extension, local_pending/edit_delete) in the shared conflict records",
          c14.get("kind") == "local_pending" and c14.get("subtype") == "edit_delete" and c14["local"]["enabled"] is True and c14["remote"]["exists"] is False
          and DUMMY in mgmt(b), json.dumps({k: c14.get(k) for k in ("kind", "subtype", "status")}))
    r = b.dev("--dev-resolve", b.mid, c14.get("conflict_id", ""), "keep_local").get("results", [{}])[0]
    pull(b)
    pull(a)
    M["latency_ms"]["conflict_to_convergence"] = int((time.time() - t0) * 1000)
    va = views(a)
    check("15 Keep local: resolved with one op; the dummy stays installed+enabled on B and is back in the synchronised state (A: Missing)",
          r.get("outcome") == "resolved" and r.get("ops") == 1 and views(b)[DUMMY]["state"] == "synced" and mgmt(b).get(DUMMY) is True
          and va.get(DUMMY, {}).get("state") == "missing", json.dumps(r))

    # 16-20 uninstall on A, Removed on another device, explicit Remove here
    restart(a, dummy=True)  # A has it again (installed => follows the synced state)
    pull(a)
    restart(a, dummy=False)  # 16 A uninstalls it
    ra.wait(lambda: sum(1 for o in ext_ops(a, pid, a.device_id) if o["id"] == DUMMY and o["op"] == "delete") >= 2, 60)
    pull(b)
    vb = views(b)
    check("16-17 A uninstalled it: B shows 'Removed on another device' with Remove here / Keep here",
          vb.get(DUMMY, {}).get("state") == "removed_elsewhere" and vb[DUMMY]["actions"] == ["remove", "keep"], json.dumps(vb.get(DUMMY)))
    check("18 nothing removed automatically on B", DUMMY in mgmt(b), "")
    # 19 Remove here from the REAL app window (embedded frontend), with its confirmation
    ui = cf.UI(b)
    J = cf.UI_JS + "const row = () => all('tr').find(r => r.innerText.includes('Synchrome Test Dummy'));"
    ok_ui = bool(ui.ready) and bool(ui.until(J + "return !!btn(document, 'Extensions');", 40))
    ui.js(J + "btn(document, 'Extensions').click(); return true;")
    shown = ui.until(J + "return !!row() && row().innerText.includes('Removed on another device');", 30)
    ui.js(J + "btn(row(), 'Remove here').click(); return true;")
    asks = ui.until(J + "return !!btn(row(), 'Confirm: remove');", 10)
    still = DUMMY in mgmt(b)
    ui.js(J + "btn(row(), 'Confirm: remove').click(); return true;")
    saved = ui.until(J + "const s = document.querySelector('[role=status]'); return !!s && s.textContent.includes('Remove here');", 20)
    panel = ui.js(J + "return all('.conflicts').map(x => x.innerText).join('\\n');") or ""
    ui.stop()
    check("19 'Remove here' in the real app: listed as Removed on another device, explicit confirmation (first click only asks), then saved",
          ok_ui and bool(shown) and bool(asks) and still and bool(saved), "")
    sl, _, _ = pull(b)
    vb = views(b)
    ptxt = popup_text(b)
    check("19b Chromium requires a user gesture to uninstall: the attempt from the worker is refused, the popup offers the removal to the user",
          vb.get(DUMMY, {}).get("state") == "user_action_required" and DUMMY in mgmt(b) and "Remove “Synchrome Test Dummy”" in ptxt,
          json.dumps({"state": vb.get(DUMMY, {}).get("state"), "popup_has_button": "Remove “" in ptxt}))
    nb = len(ext_ops(a, pid, b.device_id))
    restart(b, dummy=False)  # the user confirmed in the popup / Chromium dialog (test mechanism)
    pull(b)
    check("20 B removed it: gone from the browser, recorded locally, nothing published (the removal was A's)",
          DUMMY not in mgmt(b) and DUMMY not in views(b) and len(ext_ops(a, pid, b.device_id)) == nb, "")

    # 21-23 offline B, A changes state, B recovers
    stop(b)
    start(b, offline=True)
    t0 = time.time()
    set_enabled(a, UBO, False)
    b.hl.js("run(true)", timeout=90)
    ra.wait(b.idle, 60)
    check("21-22 B offline while A disabled an extension: B unchanged, nothing lost", mgmt(b).get(UBO) is True, "")
    stop(b)
    start(b)
    pull(b)
    M["latency_ms"]["offline_recovery"] = int((time.time() - t0) * 1000)
    check("23 B back online: applies A's change, both converge", mgmt(b).get(UBO) is False and mgmt(a).get(UBO) is False
          and views(b)[UBO]["state"] == "synced", "")

    # isolation (USER_B) + security
    stop(a)
    stop(b)
    first = ra.journal(a, pid, a.device_id)[1]
    cdev = ra.Device("C")
    u = cdev.dev("--dev-sign-in", creds, "B")
    probe = cdev.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, first[0]["path"] if first else "") if u.get("signed_in") else {}
    refd = lambda v_: isinstance(v_, dict) and "Err" in v_
    as_b = b.dev("--dev-ext-views", b.mid)
    as_b_act = b.dev("--dev-ext-action", b.mid, UBO, "enable")
    check("S1 USER_B (exact id) cannot read A's journals (extension ops included), store_events, mappings or profile; nor list/act on A's extensions",
          u.get("user_id") == h.EXPECTED["B"] and probe.get("list_sees") == {"Ok": 0} and probe.get("get_first_chunk") == {"Ok": False}
          and probe.get("store_events_sees") == {"Ok": 0} and refd(probe.get("put_under_foreign_prefix"))
          and probe.get("foreign_profile_visible") == {"Ok": False} and "error" in as_b and "error" in as_b_act,
          json.dumps({"views": as_b.get("error"), "action": as_b_act.get("error")}))
    a.dev("--dev-sign-in", creds, "A")
    for dv in (a, b):
        lg = dv.log()
        leaks = [w_ for w_ in NAMES if w_ in lg]
        check(f"S2 device {dv.name} host log: no extension names, no tokens", not leaks and h.secret_free(lg), f"leaks={leaks}")
    ext = "".join(open(os.path.join(h.EXT, f), errors="replace").read() for f in os.listdir(h.EXT) if os.path.isfile(os.path.join(h.EXT, f)))
    check("S3 extension: no Supabase/network API; no conflict-decision logic", "supabase" not in ext.lower() and not h.JWT.search(ext)
          and not re.search(r"\bfetch\(|XMLHttpRequest|WebSocket|EventSource", ext) and not re.search(r"keep_local|keep_remote|resolve_conflict", ext), "")
    check("S4 UI panel shows no internal ids beyond extension ids", not re.search(r"[0-9a-f]{8}-[0-9a-f]{4}-", panel), "")

    # metrics
    M["ext_ops"] = {"A": len(ext_ops(a, pid, a.device_id)), "B": len(ext_ops(a, pid, b.device_id))}
    M["conflicts"] = [(c["subtype"], c["status"], c.get("resolution")) for dv in (a, b)
                      for c in (dv.dev("--dev-conflict-views", dv.mid, "all").get("conflicts") or []) if c["item_kind"] == "extension"]
    print("METRICS " + json.dumps(M))

    # cleanup
    rb = b.dev("--dev-cleanup", b.mid)
    rbd = b.dev("--dev-delete-this-device")
    rA = a.dev("--dev-cleanup", a.mid)
    gone = a.dev("--dev-cloud-tree", pid)
    inv = a.dev("--dev-inventory")
    check("Z1 cloud cleanup: Storage objects, store_events, profile, mappings, devices",
          rb.get("unmapped") and rbd.get("deleted") and rA.get("unmapped") and rA.get("deleted_profile") and rA.get("device_deleted")
          and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [],
          json.dumps({"B": rb, "A": rA}))
    so = a.dev("--dev-sign-out")
    check("Z2 Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, cdev):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    shutil.rmtree(os.path.join(ra.WORK, "ui-driver"), ignore_errors=True)
    try:
        os.remove(os.path.join(ra.WORK, "ui-app.log"))
    except FileNotFoundError:
        pass
    check("Z3 local cleanup: data dirs (state.sqlite, registry, logs, backups), Helium profiles + manifests, UI driver files",
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
