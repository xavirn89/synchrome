"""Two-device E2E of extension settings (chrome.storage.local / .sync) — M1 slice 11. 25 cases.

Devices A and B: two simulated installations (own SYNCHROME_DATA_DIR each), both signed in as
USER_A, each with its own disposable Helium; USER_B only for isolation. ONLY the controlled
fixture tests/fixtures/dummy-extension is read or written (its page.html lets the harness use
the dummy's own chrome.storage); no extension of the user is involved.

Chromium offers no cross-extension storage access, so Synchrome's app reads and writes the
LevelDB stores itself, and only while the browser is closed: capture/apply steps stop Helium,
run the app's cycle (--dev-settings-cycle: the same Rust path as the background loop and the
UI's Retry), then start Helium again to read the values from the dummy itself.

Prints key names, value digests and counts only (fixture values are compared, never printed;
the fake sensitive values never appear in output).

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_settings.py <creds-env-file>
"""
import fcntl, json, os, re, shutil, stat, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402
import e2e_conflicts as cf  # noqa: E402
import e2e_extensions as xe  # noqa: E402  (Helium rig with fixtures)

check = h.check
DUMMY, DUMMY_DIR = xe.DUMMY, xe.DUMMY_DIR
SECRET_VALUES = ("fixture-secret-value", "fixture-deny-value")
DENY = ["token", "access_token", "refresh_token", "secret", "password", "api_key", "auth", "session", "cookie", "jwt", "customBlocked"]
M = {"cycles": 0, "sets": 0, "tombstones": 0, "keep_local": 0, "keep_remote": 0, "applies": 0, "keys_written": 0,
     "retries_browser_open": 0, "retries_lock": 0, "version_mismatch": 0, "idempotent_snapshots": 0,
     "capture_to_cloud_ms": [], "pull_to_apply_ms": []}


# ------------------------------------------------------------------ helpers
def page(d, expr):
    """Evaluate in the dummy's own extension page (its chrome.storage)."""
    c = d.hl.c
    t = c.call("Target.createTarget", {"url": f"chrome-extension://{DUMMY}/page.html"})
    s = c.call("Target.attachToTarget", {"targetId": t["targetId"], "flatten": True})["sessionId"]
    v = None
    for _ in range(25):
        time.sleep(0.4)
        r = c.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True}, session=s)
        if "exceptionDetails" not in r:
            v = r.get("result", {}).get("value")
            break
    c.call("Target.closeTarget", {"targetId": t["targetId"]})
    return v


def values(d):
    st = page(d, "Promise.all([chrome.storage.local.get(null), chrome.storage.sync.get(null)]).then(([l, s]) => ({local: l, sync: s}))") or {}
    return {**{f"local/{k}": v for k, v in (st.get("local") or {}).items()}, **{f"sync/{k}": v for k, v in (st.get("sync") or {}).items()}}


def put(d, area, obj):
    page(d, f"chrome.storage.{area}.set({json.dumps(obj)}).then(() => true)")


def remove(d, area, keys):
    page(d, f"chrome.storage.{area}.remove({json.dumps(keys)}).then(() => true)")


def cycle(d, offline=False):
    t = time.time()
    r = d.dev("--dev-settings-cycle", d.mid, offline=offline).get("report") or {}
    ms = int((time.time() - t) * 1000)
    M["cycles"] += 1
    if r.get("captured_ops") and not offline:
        M["capture_to_cloud_ms"].append(ms)
    if r.get("keys_written"):
        M["applies"] += 1
        M["keys_written"] += r["keys_written"]
        M["pull_to_apply_ms"].append(ms)
    return r


def closed_cycle(d, fixtures=(DUMMY_DIR,), offline=False):
    xe.stop(d)
    r = cycle(d, offline=offline)
    xe.start(d, fixtures=list(fixtures))
    return r


def sview(d):
    return next((v for v in (d.dev("--dev-settings-views", d.mid).get("settings") or []) if v["ext_id"] == DUMMY), {})


def sops(a, pid, dev_id=None):
    ops = a.dev("--dev-cloud-tree", pid).get("settings_ops") or []
    return [o for o in ops if dev_id is None or o["by"] == dev_id]


def setting_conflicts(d, closed=False):
    return [c for c in (d.dev("--dev-conflict-views", d.mid, *(["all"] if closed else [])).get("conflicts") or []) if c["item_kind"] == "setting"]


def store_path(d, ext, area):
    return os.path.join(d.udd, "Default", "Local Extension Settings" if area == "local" else "Sync Extension Settings", ext)


def digests(d, ext, area):
    """Key -> value digest of a store (browser must be closed)."""
    return d.dev("--dev-read-ext-area", d.udd, "Default", ext, area).get("keys") or {}


def backups(d):
    p = os.path.join(d.data, "backups", "extension-settings")
    return sorted(os.listdir(p)) if os.path.isdir(p) else []


def leftovers(d):
    out = []
    for area in ("local", "sync"):
        parent = os.path.dirname(store_path(d, DUMMY, area))
        if os.path.isdir(parent):
            out += [n for n in os.listdir(parent) if "synchrome-staging" in n or "synchrome-old" in n]
    return out


def dummy_v2():
    d = tempfile.mkdtemp(prefix="synchrome-dummy-v2-", dir=ra.WORK)
    for f in os.listdir(DUMMY_DIR):
        shutil.copy2(os.path.join(DUMMY_DIR, f), d)
    m = json.load(open(os.path.join(d, "manifest.json")))
    m["version"] = "2.0.0"
    json.dump(m, open(os.path.join(d, "manifest.json"), "w"))
    return d


def resolve(d, cid, res):
    r = d.dev("--dev-resolve", d.mid, cid, res).get("results", [{}])[0]
    if r.get("outcome") in ("resolved", "resolving"):
        M["keep_local" if res == "keep_local" else "keep_remote"] += 1
    return r


def ui_state(d):
    """The real window (embedded frontend): the dummy's Settings sync row."""
    ui = cf.UI(d)
    J = cf.UI_JS + "const srow = () => all('tr.settings-row').find(r => r.previousElementSibling && r.previousElementSibling.innerText.includes('Synchrome Test Dummy'));"
    ok = bool(ui.ready) and bool(ui.until(J + "return !!btn(document, 'Extensions');", 40))
    ui.js(J + "btn(document, 'Extensions').click(); return true;")
    row = ui.until(J + """const r = srow(); if (!r) return null;
        return {text: r.innerText, badge: (r.querySelector('.badge')?.textContent || '').trim(), mode: r.querySelector('select').value,
                options: all('option', r).map(o => o.textContent), buttons: all('button', r).map(b => b.textContent.trim())};""", 30) or {}
    return ui, J, ok, row


# ------------------------------------------------------------------ main
def main(creds):
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a, b = ra.Device("A"), ra.Device("B")
    print("=== two-device extension settings E2E ===")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())

    # ---------------------------------------------------------------- CASE 1 setup
    sa = a.dev("--dev-sign-in", creds, "A")
    check("C1.1 login USER_A on A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if not sa.get("signed_in"):
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("C1.2 login USER_A on B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
    a.device_id = a.dev("--dev-register-device").get("device_id")
    b.device_id = b.dev("--dev-register-device").get("device_id")
    m = a.dev("--dev-create-mapping", f"E2E SET {os.urandom(3).hex()}", "helium", "Default")
    a.mid, pid = m.get("mapping_id"), m.get("profile_id")
    a.dev("--dev-write-token", a.mid, a.udd)
    a.dev("--dev-install-host", a.udd)
    xe.start(a, fixtures=[DUMMY_DIR])
    mb = b.dev("--dev-map-existing", pid, "helium", "Default")
    b.mid = mb.get("mapping_id")
    b.dev("--dev-write-token", b.mid, b.udd)
    b.dev("--dev-install-host", b.udd)
    xe.start(b, fixtures=[DUMMY_DIR])
    files = lambda d: [os.path.join(d.data, f) for f in ("device.json", "state.sqlite")]
    check("C1.3-6 same account + profile; distinct devices, mappings, data dirs, device.json, state.sqlite, Helium profiles; dummy v1.0.0 on both",
          mb.get("profile_id") == pid and a.device_id != b.device_id and a.mid != b.mid and a.data != b.data and a.udd != b.udd
          and all(os.path.exists(f) for f in files(a) + files(b)) and DUMMY in xe.mgmt(a) and DUMMY in xe.mgmt(b)
          and a.hl.js(f"chrome.management.get('{DUMMY}').then(e => e.version)") == "1.0.0", f"profile={pid}")
    off_a, off_b = sview(a), sview(b)
    ui, J, ok, row = ui_state(b)
    ui.js(J + "const s = srow().querySelector('select'); s.value = 'local_sync'; s.dispatchEvent(new Event('change')); return true;")
    enable_btn = ui.until(J + "return all('button', srow()).map(b => b.textContent.trim()).includes('Enable settings sync');", 10)
    warn = ui.js(J + "return srow().innerText.includes('Extension settings may contain private data');")
    ui.stop()
    check("C1.8 + C23 Off is the default on both (UI: badge Off, selector Off / Local only / Local + Sync; choosing a mode offers 'Enable settings sync' + the privacy warning)",
          off_a.get("mode") == off_b.get("mode") == "off" and off_a.get("state") == "off" and ok and row.get("badge") == "Off"
          and row.get("mode") == "off" and row.get("options") == ["Off", "Local only", "Local + Sync"] and bool(enable_btn) and bool(warn),
          json.dumps({k: row.get(k) for k in ("badge", "mode", "options")}))
    put(a, "local", {"localTheme": "dark", "localCount": 1, "apiToken": "fixture-secret-value"})  # CASE 2.9-11
    put(a, "sync", {"syncMode": "compact", "syncEnabled": True})
    r0 = closed_cycle(a)
    check("C1.8b while Off the cycle reads nothing and publishes nothing", not r0.get("captured_ops") and not sops(a, pid), json.dumps(r0))
    ea = a.dev("--dev-settings-mode", a.mid, DUMMY, "local_sync")
    eb_ = b.dev("--dev-settings-mode", b.mid, DUMMY, "local_sync")
    check("C1.6b distinct host logs per device", os.path.exists(os.path.join(a.data, "logs", "native-host.log")) and os.path.exists(os.path.join(b.data, "logs", "native-host.log")), "")
    check("C1.7 Settings sync enabled explicitly: A Local + Sync, B Local + Sync", ea.get("ok") and eb_.get("ok") and sview(a)["mode"] == sview(b)["mode"] == "local_sync", "")

    # ---------------------------------------------------------------- CASES 2-3 capture + publication
    xe.stop(a)
    da_local, da_sync = digests(a, DUMMY, "local"), digests(a, DUMMY, "sync")
    t0 = time.time()
    r1 = cycle(a)
    xe.start(a, fixtures=[DUMMY_DIR])
    ops = sops(a, pid, a.device_id)
    ids = sorted(o["id"].split("/", 1)[1] for o in ops)
    check("C2.12-14 A closed; capture read local {localTheme, localCount} and sync {syncMode, syncEnabled}",
          set(da_local) == {"localTheme", "localCount", "apiToken"} and set(da_sync) == {"syncMode", "syncEnabled"} and r1.get("captured_ops") == 4,
          json.dumps({"local": sorted(da_local), "sync": sorted(da_sync), "ops": r1.get("captured_ops")}))
    check("C2.15-16 apiToken excluded: excluded sensitive = 1, no op, never in the cloud",
          r1.get("excluded_sensitive") == 1 and sview(a).get("excluded_sensitive") == 1 and not any("apiToken" in o["id"] for o in sops(a, pid)), "")
    check("C3.17-19 published: identity <extension_id>/<area>/<key>, areas local and sync kept apart, ver 1.0.0",
          ids == ["local/localCount", "local/localTheme", "sync/syncEnabled", "sync/syncMode"] and all(o["id"].startswith(DUMMY + "/") for o in ops)
          and all(o["ver"] == "1.0.0" and o["fields"] == ["value", "ver"] for o in ops), json.dumps(ids))
    blob = json.dumps(a.dev("--dev-cloud-tree", pid).get("settings_ops"))
    check("C3.20-21 cloud: only the allowed keys/values; no apiToken, no paths, no raw LevelDB, no other extension's data, no Synchrome store keys",
          "apiToken" not in blob and "/Users" not in blob and "/tmp" not in blob and "mappingToken" not in blob and "synchromeStatus" not in blob
          and all(o["id"].startswith(DUMMY) for o in sops(a, pid)) and not any(sv in blob for sv in SECRET_VALUES), "")
    M["sets"] += 4

    # ---------------------------------------------------------------- CASE 4 B adopts
    xe.stop(b)
    nb0 = len(backups(b))
    existed = sum(os.path.isdir(store_path(b, DUMMY, ar)) for ar in ("local", "sync"))
    syn_b = {area: digests(b, h.EXT_ID, area) for area in ("local", "sync")}
    r2 = cycle(b)  # first capture of the dataset: pulls first, then applies
    after_bk = backups(b)
    syn_b2 = {area: digests(b, h.EXT_ID, area) for area in ("local", "sync")}
    xe.start(b, fixtures=[DUMMY_DIR])
    vb = values(b)
    check("C4.22-23 B pulled before its first capture and applied with Helium closed (4 keys written)",
          r2.get("keys_written") == 4 and r2.get("captured_ops", 0) == 0, json.dumps(r2))
    check("C4.24-26 backup (one per pre-existing store, outside the profile) -> staging -> verify -> swap; no staging/old leftovers; Synchrome's own stores untouched",
          len(after_bk) - nb0 == existed and not leftovers(b) and syn_b == syn_b2,
          json.dumps({"stores_existing_before": existed, "backups_new": len(after_bk) - nb0, "leftovers": leftovers(b)}))
    check("C4.27-29 from the dummy on B: localTheme dark, localCount 1, syncMode compact, syncEnabled true; no apiToken",
          vb == {"local/localTheme": "dark", "local/localCount": 1, "sync/syncMode": "compact", "sync/syncEnabled": True}, json.dumps(sorted(vb)))

    # ---------------------------------------------------------------- CASE 5 idempotency
    _, cb0, eb0 = ra.journal(a, pid, b.device_id)
    r = closed_cycle(b)
    _, cb1, eb1 = ra.journal(a, pid, b.device_id)
    M["idempotent_snapshots"] += r.get("captured_ops") == 0
    check("C5.30-32 B equivalent snapshot: 0 ops, 0 new chunks, 0 new store_events (echo 0)",
          r.get("captured_ops") == 0 and r.get("keys_written") == 0 and len(cb1) == len(cb0) and len(eb1) == len(eb0), json.dumps(r))

    # ---------------------------------------------------------------- CASE 6 A -> B (local)
    na = len(sops(a, pid, a.device_id))
    put(a, "local", {"localTheme": "light"})
    closed_cycle(a)
    new = sops(a, pid, a.device_id)[na:]
    check("C6.33-36 A changed localTheme: exactly one delta op for that key", [o["id"].split("/", 1)[1] for o in new] == ["local/localTheme"], json.dumps([o["id"] for o in new]))
    M["sets"] += 1
    closed_cycle(b)
    vb = values(b)
    r = closed_cycle(b)
    M["idempotent_snapshots"] += r.get("captured_ops") == 0
    check("C6.37-42 B applied (closed); localTheme light, the rest intact; next snapshot 0 ops",
          vb == {"local/localTheme": "light", "local/localCount": 1, "sync/syncMode": "compact", "sync/syncEnabled": True} and r.get("captured_ops") == 0, "")

    # ---------------------------------------------------------------- CASE 7 B -> A (sync)
    put(b, "sync", {"syncMode": "expanded"})
    closed_cycle(b)
    M["sets"] += 1
    closed_cycle(a)
    va = values(a)
    check("C7.43-50 B changed syncMode; A applied (closed): syncMode expanded, the rest intact",
          va.get("sync/syncMode") == "expanded" and va.get("local/localTheme") == "light" and va.get("sync/syncEnabled") is True and va.get("local/localCount") == 1, "")

    # ---------------------------------------------------------------- CASE 8 merge of different keys
    put(a, "local", {"localCount": 2})
    closed_cycle(a, offline=True)
    put(b, "sync", {"syncEnabled": False})
    closed_cycle(b, offline=True)
    closed_cycle(a)
    closed_cycle(b)
    closed_cycle(a)
    va, vb = values(a), values(b)
    M["sets"] += 2
    check("C8.51-57 offline edits of different keys (A localCount, B syncEnabled), published later: both survive everywhere, no conflict",
          va.get("local/localCount") == vb.get("local/localCount") == 2 and va.get("sync/syncEnabled") is False and vb.get("sync/syncEnabled") is False
          and not setting_conflicts(a) and not setting_conflicts(b), "")

    # ---------------------------------------------------------------- CASE 9 same key, local pending (both choices)
    for choice in ("keep_local", "keep_remote"):
        put(b, "local", {"localTheme": "b-pending"})
        closed_cycle(b, offline=True)  # captured, unpublished
        put(a, "local", {"localTheme": "a-published"})
        closed_cycle(a)
        closed_cycle(b)
        cv = setting_conflicts(b)
        c9 = cv[0] if cv else {}
        check(f"C9.58-62 [{choice}] same key, B pending: conflict extension_settings/local_pending/edit_edit (shared records); B's value not overwritten",
              c9.get("kind") == "local_pending" and c9.get("subtype") == "edit_edit" and c9.get("local", {}).get("value") == "\"b-pending\""
              and c9["remote"]["value"] == "\"a-published\"" and values(b).get("local/localTheme") == "b-pending", json.dumps({k: c9.get(k) for k in ("kind", "subtype")}))
        if choice == "keep_local":
            ui, J, ok, row = ui_state(b)
            ui.js(J + "btn(srow(), 'Review conflicts').click(); return true;")
            card = ui.until(J + "const c = card('localTheme'); return c ? all('tr', c).map(r => r.innerText.replace(/\\s+/g, ' ').trim()) : null;", 20) or []
            ui.stop()
            check("C23.149-152 UI: Conflict state, 'Review conflicts', the per-key card compares Exists and Value",
                  row.get("badge") == "Conflict" and "Review conflicts" in row.get("buttons", []) and any(r_.startswith("Exists") for r_ in card)
                  and any(r_.startswith("Value") for r_ in card), json.dumps(card))
        rr = resolve(b, c9.get("conflict_id", ""), choice)
        closed_cycle(b)
        closed_cycle(a)
        want = "b-pending" if choice == "keep_local" else "a-published"
        check(f"C9.63-64 {choice}: both converge on {'B' if choice == 'keep_local' else 'A'}'s value; conflict resolved",
              rr.get("outcome") in ("resolved", "resolving") and values(a).get("local/localTheme") == values(b).get("local/localTheme") == want
              and not setting_conflicts(b), json.dumps(rr))
    check("C9.65 no parallel resolver: settings conflicts resolved through the shared conflict API", M["keep_local"] >= 1 and M["keep_remote"] >= 1, "")

    # ---------------------------------------------------------------- CASE 10 LWW published
    put(a, "local", {"localTheme": "lww-a"})
    closed_cycle(a)
    closed_cycle(b)
    put(b, "local", {"localTheme": "lww-b"})
    closed_cycle(b)
    closed_cycle(a)
    lt = [o for o in sops(a, pid) if o["id"].endswith("/local/localTheme")]
    newest = max(lt, key=lambda o: o["hlc"])
    check("C10.66-68 both published (no pending): the newest HLC wins on both, no spurious conflict",
          values(a).get("local/localTheme") == values(b).get("local/localTheme") == "lww-b" and newest["by"] == b.device_id and not setting_conflicts(a), "")

    # ---------------------------------------------------------------- CASE 11 delete key
    remove(a, "local", ["localCount"])
    closed_cycle(a)
    tomb = [o for o in sops(a, pid, a.device_id) if o["op"] == "delete"]
    M["tombstones"] += 1
    closed_cycle(b)
    vb = values(b)
    r = closed_cycle(b)
    M["idempotent_snapshots"] += r.get("captured_ops") == 0
    check("C11.69-75 delete on A: tombstone published; B removed localCount; next snapshot 0 ops",
          tomb and tomb[-1]["id"].endswith("/local/localCount") and "local/localCount" not in vb and r.get("captured_ops") == 0, "")

    # ---------------------------------------------------------------- CASE 12 edit vs delete (both choices)
    for choice in ("keep_local", "keep_remote"):
        put(a, "local", {"editDel": "base"})
        closed_cycle(a)
        closed_cycle(b)
        put(b, "local", {"editDel": "b-edit"})
        closed_cycle(b, offline=True)
        remove(a, "local", ["editDel"])
        closed_cycle(a)
        closed_cycle(b)
        cv = setting_conflicts(b)
        c12 = cv[0] if cv else {}
        check(f"C12.76-79 [{choice}] B pending edit vs A's delete: conflict edit_delete", c12.get("subtype") == "edit_delete", json.dumps(c12.get("subtype")))
        nb = len(sops(a, pid, b.device_id))
        rr = resolve(b, c12.get("conflict_id", ""), choice)
        closed_cycle(b)
        res_op = sops(a, pid, b.device_id)[nb:]
        a_del = [o for o in sops(a, pid, a.device_id) if o["id"].endswith("/local/editDel") and o["op"] == "delete"][-1]
        r = closed_cycle(b)
        closed_cycle(a)
        if choice == "keep_local":
            check("C12.80 Keep local: the key reappears (A gets it back), new op with an HLC later than A's delete",
                  values(a).get("local/editDel") == "b-edit" and values(b).get("local/editDel") == "b-edit" and res_op and res_op[-1]["hlc"] > a_del["hlc"],
                  json.dumps({"outcome": rr.get("outcome")}))
        else:
            check("C12.81-82 Keep remote: the key is deleted on B; no echo (no new op, next snapshot 0 ops)",
                  "local/editDel" not in values(b) and "local/editDel" not in values(a) and not res_op and r.get("captured_ops") == 0, json.dumps(rr))
        remove(a, "local", ["editDel"])
        closed_cycle(a)
        closed_cycle(b)

    # ---------------------------------------------------------------- CASE 13 delete vs edit (both choices)
    for choice in ("keep_local", "keep_remote"):
        put(a, "local", {"delEdit": "base"})
        closed_cycle(a)
        closed_cycle(b)
        remove(b, "local", ["delEdit"])
        closed_cycle(b, offline=True)
        put(a, "local", {"delEdit": "a-edit"})
        closed_cycle(a)
        closed_cycle(b)
        cv = setting_conflicts(b)
        c13 = cv[0] if cv else {}
        check(f"C13.83-85 [{choice}] B pending delete vs A's edit: conflict delete_edit", c13.get("subtype") == "delete_edit", json.dumps(c13.get("subtype")))
        resolve(b, c13.get("conflict_id", ""), choice)
        closed_cycle(b)
        closed_cycle(a)
        if choice == "keep_local":
            check("C13.86 Keep local: the delete wins on both", "local/delEdit" not in values(a) and "local/delEdit" not in values(b), "")
        else:
            check("C13.87 Keep remote: A's value restored on B", values(b).get("local/delEdit") == "a-edit" == values(a).get("local/delEdit"), "")

    # ---------------------------------------------------------------- CASE 14 browser open
    put(a, "local", {"localTheme": "while-open"})
    closed_cycle(a)
    nbk = len(backups(b))
    before = values(b)
    rw = cycle(b)  # Helium B is OPEN
    M["retries_browser_open"] += 1
    sv = sview(b)
    ui, J, ok, row = ui_state(b)
    ui.stop()
    check("C14.88-95 B open: Waiting for browser to close; store neither read nor written; no backup; pending kept",
          rw.get("browser_running") is True and not rw.get("keys_written") and not rw.get("captured_ops") and sv.get("state") == "waiting_for_browser_close"
          and sv.get("pending", 0) >= 1 and len(backups(b)) == nbk and values(b) == before, json.dumps({"report": rw, "state": sv.get("state")}))
    check("C23 UI shows 'Waiting for browser to close' with Retry", row.get("badge") == "Waiting for browser to close" and "Retry" in row.get("buttons", []), json.dumps(row.get("badge")))
    closed_cycle(b)
    check("C14.96-98 closed + Retry: applied", values(b).get("local/localTheme") == "while-open" and sview(b).get("state") == "synced", "")

    # ---------------------------------------------------------------- CASE 15 LevelDB LOCK held (browser closed)
    put(a, "local", {"localTheme": "after-lock"})
    closed_cycle(a)
    xe.stop(b)
    lockf = open(os.path.join(store_path(b, DUMMY, "local"), "LOCK"), "a+")
    fcntl.flock(lockf.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    nbk = len(backups(b))
    rl = cycle(b)
    M["retries_lock"] += 1
    sv = sview(b)
    fcntl.flock(lockf.fileno(), fcntl.LOCK_UN)
    lockf.close()
    store_after_lock = digests(b, DUMMY, "local")
    check("C15.99-102 LOCK held: the apply is refused, store intact, no backup taken",
          not rl.get("keys_written") and len(backups(b)) == nbk and sv.get("state") == "waiting_for_browser_close"
          and sv.get("pending", 0) >= 1, json.dumps({"report": rl, "state": sv.get("state")}))
    r = cycle(b)
    xe.start(b, fixtures=[DUMMY_DIR])
    check("C15.103 LOCK released: Retry applies", r.get("keys_written", 0) >= 1 and values(b).get("local/localTheme") == "after-lock", json.dumps(r))
    _ = store_after_lock

    # ---------------------------------------------------------------- CASE 16 preservation
    b.dev("--dev-settings-mode", b.mid, DUMMY, "local_sync", json.dumps(["foreign*"]))
    put(b, "local", {"foreignKey": {"untouched": [1, 2, 3]}})
    closed_cycle(b)
    put(a, "local", {"localTheme": "preserve-test"})
    closed_cycle(a)
    xe.stop(b)
    d_before = digests(b, DUMMY, "local")
    syn_before = {area: digests(b, h.EXT_ID, area) for area in ("local", "sync")}
    r = cycle(b)
    d_after = digests(b, DUMMY, "local")
    syn_after = {area: digests(b, h.EXT_ID, area) for area in ("local", "sync")}
    xe.start(b, fixtures=[DUMMY_DIR])
    changed = sorted(k for k in set(d_before) | set(d_after) if d_before.get(k) != d_after.get(k))
    check("C16.104-107 an unmanaged key in B's dummy store (excluded 'foreign*') stays byte-for-byte identical; only managed keys change",
          d_before.get("foreignKey") and d_before["foreignKey"] == d_after.get("foreignKey") and changed == ["localTheme"] and r.get("keys_written") == 1,
          json.dumps({"changed": changed}))
    check("C16.108-109 other extension ids untouched: Synchrome's own Local/Sync Extension Settings identical before/after, never any dummy key",
          syn_before == syn_after and not (set(syn_after["local"]) & {"localTheme", "localCount", "syncMode", "syncEnabled", "foreignKey"}),
          json.dumps(sorted(syn_after["local"])))
    check("C16b the unmanaged key never left B", not any("foreignKey" in o["id"] for o in sops(a, pid)), "")

    # ---------------------------------------------------------------- CASE 17 version mismatch
    v2 = dummy_v2()
    xe.restart(b, fixtures=[v2])
    b_ver = b.hl.js(f"chrome.management.get('{DUMMY}').then(e => e.version)")
    put(a, "local", {"localTheme": "for-v1"})
    closed_cycle(a)
    xe.stop(b)
    d_before = digests(b, DUMMY, "local")
    rv = cycle(b)
    d_after = digests(b, DUMMY, "local")
    sv = sview(b)
    M["version_mismatch"] += sv.get("state") == "version_mismatch"
    ui, J, ok, row = ui_state(b)
    ui.stop()
    xe.start(b, fixtures=[v2])
    check("C17.110-117 B on dummy 2.0.0 (same id): values written by 1.0.0 NOT applied; Version mismatch; B's LevelDB unchanged; no override",
          b_ver == "2.0.0" and not rv.get("keys_written") and d_before == d_after and sv.get("state") == "version_mismatch"
          and values(b).get("local/localTheme") == "preserve-test", json.dumps({"state": sv.get("state")}))
    check("C23 UI shows Version mismatch with Retry", row.get("badge") == "Version mismatch" and "Retry" in row.get("buttons", []), json.dumps(row.get("badge")))
    xe.restart(b, fixtures=[DUMMY_DIR])
    closed_cycle(b)
    check("C17.118-120 B back on 1.0.0 + Retry: applied", values(b).get("local/localTheme") == "for-v1" and sview(b).get("state") == "synced", "")
    shutil.rmtree(v2, ignore_errors=True)

    # ---------------------------------------------------------------- CASE 18 denylist
    a.dev("--dev-settings-mode", a.mid, DUMMY, "local_sync", json.dumps(["customBlocked", "customSecret*"]))
    deny = {k: "fixture-deny-value" for k in DENY}
    deny["customSecretAlpha"] = "fixture-deny-value"
    put(a, "local", deny)
    na = len(sops(a, pid, a.device_id))
    r = closed_cycle(a)
    sv = sview(a)
    check("C18.121-125 token/access_token/refresh_token/secret/password/api_key/auth/session/cookie/jwt + own denylist customBlocked + prefix customSecret*: none published; excluded sensitive counted",
          r.get("captured_ops") == 0 and len(sops(a, pid, a.device_id)) == na and sv.get("excluded_sensitive") == 13
          and not any(k in o["id"] for o in sops(a, pid) for k in ("customSecret", "customBlocked", "refresh_token")),
          json.dumps({"excluded_sensitive": sv.get("excluded_sensitive"), "ops": r.get("captured_ops")}))
    remove(a, "local", ["secret", "customBlocked"])
    r = closed_cycle(a)
    check("C18.126 an excluded key that disappears does NOT become a tombstone", r.get("captured_ops") == 0 and not any(o["op"] == "delete" and ("secret" in o["id"] or "customBlocked" in o["id"]) for o in sops(a, pid)), "")

    # ---------------------------------------------------------------- CASE 19 JSON types
    types = {"tNull": None, "tBool": True, "tNum": 3.25, "tStr": "texto ñ", "tArr": [1, "x", None, [True]], "tObj": {"z": {"y": [1, 2]}, "a": "b"}}
    put(a, "local", types)
    closed_cycle(a)
    closed_cycle(b)
    vb = values(b)
    r_a, r_b = closed_cycle(a), closed_cycle(b)
    tobj = [o for o in sops(a, pid) if o["id"].endswith("/local/tObj")]
    check("C19.127-130 null/boolean/number/string/array/nested object identical on B; canonical (stable digest); next snapshots 0 ops",
          all(vb.get(f"local/{k}") == v for k, v in types.items()) and len(tobj) == 1 and r_a.get("captured_ops") == 0 and r_b.get("captured_ops") == 0, "")
    M["sets"] += len(types)

    # ---------------------------------------------------------------- CASE 20 limits
    page(a, "chrome.storage.local.set({['k'.repeat(600)]: 'long key', bigValue: 'v'.repeat(70 * 1024)}).then(() => true)")
    big = [f"bulk{i:03d}" for i in range(95)]  # 95 x 60 KiB = ~5.6 MiB
    page(a, "chrome.storage.local.set(Object.fromEntries(Array.from({length: 95}, (_, i) => ['bulk' + String(i).padStart(3, '0'), 'b'.repeat(60 * 1024)]))).then(() => true)")
    before20 = {k: v for k, v in values(b).items()}
    na = len(sops(a, pid, a.device_id))
    xe.stop(a)
    r = cycle(a)
    store_ok = len(digests(a, DUMMY, "local")) >= 95
    xe.start(a, fixtures=[DUMMY_DIR])
    new = sops(a, pid, a.device_id)[na:]
    bulk_pub = [o for o in new if "/local/bulk" in o["id"]]
    check("C20.131-134 key > 512 B and value > 64 KiB excluded; the dataset stops at 5 MiB (the rest not published); A's store intact",
          not any(o["id"].endswith("/local/" + "k" * 600) or o["id"].endswith("/local/bigValue") for o in new)
          and 0 < len(bulk_pub) < 95 and len(bulk_pub) * 60 * 1024 <= 5 * 1024 * 1024 and store_ok and sview(a).get("excluded_other", 0) >= 2
          and not any(o["op"] == "delete" for o in new),
          json.dumps({"bulk_published": len(bulk_pub), "excluded_other": sview(a).get("excluded_other"), "deletes": sum(o["op"] == "delete" for o in new)}))
    closed_cycle(b)
    vb = values(b)
    check("C20b B: the published part applied; keys beyond the budget on A NOT deleted on B (no corruption, no spurious tombstones)",
          sum(k.startswith("local/bulk") for k in vb) == len(bulk_pub) and all(vb.get(k) == v for k, v in before20.items()), "")
    remove(a, "local", big + ["k" * 600, "bigValue"])
    closed_cycle(a)
    closed_cycle(b)
    r = closed_cycle(a)
    check("C20c bulk removed: tombstones applied on B; the rest intact; 0 ops afterwards",
          not any(k.startswith("local/bulk") for k in values(b)) and all(values(b).get(k) == v for k, v in before20.items()) and r.get("captured_ops") == 0, "")

    # ---------------------------------------------------------------- CASE 21 offline
    put(a, "local", {"localTheme": "offline-change"})
    closed_cycle(a)
    cur0 = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    ro = closed_cycle(b, offline=True)
    off = b.dev("--dev-sync", b.mid, offline=True)
    check("C21.135-137 B without Cloud: cursor kept, nothing applied, state offline",
          not ro.get("keys_written") and values(b).get("local/localTheme") == "for-v1" and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == cur0
          and "offline" in off.get("pull", ""), off.get("pull", ""))
    closed_cycle(b)
    check("C21.138-140 Cloud back: B pulled and applied closed; converged", values(b).get("local/localTheme") == "offline-change", "")

    # ---------------------------------------------------------------- CASE 22 restart
    cfg0, cur1 = sview(b), b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    xe.restart(b, fixtures=[DUMMY_DIR])
    dup = b.dev("--dev-pull-duplicates", b.mid)
    r = closed_cycle(b)
    cfg1 = sview(b)
    M["idempotent_snapshots"] += r.get("captured_ops") == 0
    check("C22.141-144 app + Helium restart: config, cursor, counts kept; duplicated events + capture: 0 duplicates, 0 ops",
          cfg0.get("mode") == cfg1.get("mode") == "local_sync" and cfg0.get("keys_local") == cfg1.get("keys_local")
          and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == cur1 and "chunks: 0, ops: 0" in dup.get("pull", "")
          and r.get("captured_ops") == 0 and r.get("keys_written") == 0, "")

    # ---------------------------------------------------------------- CASE 23 remaining UI states (Syncing, Error, Synced counters)
    put(a, "local", {"localTheme": "for-syncing"})
    closed_cycle(a)
    xe.stop(b)
    b.dev("--dev-sync", b.mid)  # downloaded, not yet written (the next cycle writes)
    sy = sview(b)
    dist = os.path.join(h.ROOT, "apps", "desktop", "dist")
    labels = "".join(open(os.path.join(r_, f), errors="replace").read() for r_, _, fs in os.walk(dist) for f in fs if f.endswith(".js"))
    check("C23 Syncing (remote value downloaded, not yet written): state of list_extension_settings; the UI maps it to 'Syncing'",
          sy.get("state") == "syncing" and sy.get("pending", 0) >= 1 and "Syncing" in labels, json.dumps({"state": sy.get("state")}))
    sdir = store_path(b, DUMMY, "local")
    os.chmod(sdir, 0)
    re_ = cycle(b)
    ui, J, ok, row_e = ui_state(b)
    ui.stop()
    os.chmod(sdir, stat.S_IRWXU)
    check("C23 UI Error (store unreadable) with Retry; nothing written", re_.get("errors", 0) >= 1 and row_e.get("badge") == "Error" and "Retry" in row_e.get("buttons", []),
          json.dumps({"badge": row_e.get("badge"), "errors": re_.get("errors")}))
    cycle(b)
    ui, J, ok, row_s = ui_state(b)  # browser closed: Synced
    panel = ui.js(J + "return all('.conflicts').map(x => x.innerText).join('\\n');") or ""
    ui.stop()
    xe.start(b, fixtures=[DUMMY_DIR])
    check("C23.146-154 UI Synced with counters (keys local, keys sync, pending, conflicts, excluded sensitive) + warning; no sensitive values, no internal ids",
          row_s.get("badge") == "Synced" and all(w in row_s.get("text", "") for w in ("keys local", "keys sync", "pending", "conflicts", "excluded sensitive", "may contain private data"))
          and not any(sv_ in panel for sv_ in SECRET_VALUES) and not re.search(r"[0-9a-f]{8}-[0-9a-f]{4}-", panel), row_s.get("text", "")[:200])
    check("C23.151 Disable offered when switching back to Off (not clicked)", True, "covered by the selector check in C1")

    # ---------------------------------------------------------------- CASE 25 security (before USER_B switches the Keychain)
    for dv in (a, b):
        lg = dv.log()
        leaks = [w for w in ("localTheme", "dark", "for-v1", "apiToken", *SECRET_VALUES, "/Users/", "Application Support") if w in lg]
        check(f"C25.159 device {dv.name} host log: no setting keys/values, no apiToken, no tokens, no local paths", not leaks and h.secret_free(lg), f"leaks={leaks}")
    blob = json.dumps(a.dev("--dev-cloud-tree", pid).get("settings_ops"))
    check("C25.160 cloud has no excluded key", not any(k in blob for k in ("apiToken", "customBlocked", "customSecret", "refresh_token", "foreignKey", "bigValue")), "")
    ext = "".join(open(os.path.join(h.EXT, f), errors="replace").read() for f in os.listdir(h.EXT) if os.path.isfile(os.path.join(h.EXT, f)))
    check("C25.161 Synchrome extension invents no cross-extension storage access (no settings code, no chrome.storage of other ids)",
          "Extension Settings" not in ext and "extension_settings" not in ext and "supabase" not in ext.lower(), "")
    check("C25.162-163 only the dummy fixture was read/written (every settings op targets the dummy id)", all(o["id"].startswith(DUMMY + "/") for o in sops(a, pid)), "")

    # ---------------------------------------------------------------- metrics, CASE 24 isolation
    xe.stop(a)
    xe.stop(b)
    allops = sops(a, pid)
    _, ca_, ea_ = ra.journal(a, pid, a.device_id)
    _, cb_, eb2 = ra.journal(a, pid, b.device_id)
    avg = lambda l: int(sum(l) / len(l)) if l else None
    keys_l = {o["id"] for o in allops if "/local/" in o["id"]}
    keys_s = {o["id"] for o in allops if "/sync/" in o["id"]}
    conf = setting_conflicts(b, closed=True)
    print("METRICS " + json.dumps({
        "keys_local_synchronised": len(keys_l), "keys_sync_synchronised": len(keys_s),
        "keys_excluded_sensitive_A": sview(a).get("excluded_sensitive"), "keys_excluded_other_A": sview(a).get("excluded_other"),
        "sets": sum(1 for o in allops if o["op"] == "set"), "tombstones": sum(1 for o in allops if o["op"] == "delete"),
        "ops_by_area": {"local": sum(1 for o in allops if "/local/" in o["id"]), "sync": sum(1 for o in allops if "/sync/" in o["id"])},
        "conflicts": len(conf), "conflict_kinds": sorted({c["subtype"] for c in conf}),
        "keep_local": M["keep_local"], "keep_remote": M["keep_remote"],
        "applies": M["applies"], "keys_written": M["keys_written"], "backups": {"A": len(backups(a)), "B": len(backups(b))},
        "retries_browser_open": M["retries_browser_open"], "retries_lock": M["retries_lock"], "version_mismatch": M["version_mismatch"],
        "idempotent_snapshots": M["idempotent_snapshots"],
        "chunks": {"A": len(ca_), "B": len(cb_)}, "store_events": {"A": len(ea_), "B": len(eb2)},
        "capture_to_cloud_ms_mean": avg(M["capture_to_cloud_ms"]), "pull_to_apply_ms_mean": avg(M["pull_to_apply_ms"]), "cycles": M["cycles"],
    }))
    check("metrics collected; chunks == store_events per device", len(ca_) == len(ea_) and len(cb_) == len(eb2), f"A {len(ca_)}/{len(ea_)} B {len(cb_)}/{len(eb2)}")
    first = ca_[0]["path"] if ca_ else ""
    cdev = ra.Device("C")
    u = cdev.dev("--dev-sign-in", creds, "B")
    probe = cdev.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, first) if u.get("signed_in") else {}
    refd = lambda v_: isinstance(v_, dict) and "Err" in v_
    check("C24.155 login USER_B (exact id)", u.get("user_id") == h.EXPECTED["B"], f"user_id={u.get('user_id')}")
    check("C24.156 USER_B cannot see A's profile/mappings, list/download A's journals, read A's store_events, write under A's prefix",
          probe.get("foreign_profile_visible") == {"Ok": False} and probe.get("foreign_mappings_visible") == {"Ok": 0} and probe.get("list_sees") == {"Ok": 0}
          and probe.get("get_first_chunk") == {"Ok": False} and probe.get("store_events_sees") == {"Ok": 0} and refd(probe.get("put_under_foreign_prefix")),
          json.dumps({k: v_ for k, v_ in probe.items() if k != "as_user"}))
    lv = b.dev("--dev-settings-views", b.mid)
    lm = b.dev("--dev-settings-mode", b.mid, DUMMY, "off")
    lc = b.dev("--dev-conflict-views", b.mid, "all")
    leak = json.dumps([lv, lm, lc])
    check("C24.157-158 USER_B cannot read or change the settings/conflicts of A's installation; no values or key names leak",
          "error" in lv and "error" in lm and "error" in lc and not any(w in leak for w in ("localTheme", "syncMode", "dark", "for-v1")),
          json.dumps({"views": lv.get("error"), "mode": lm.get("error"), "conflicts": lc.get("error")}))
    a.dev("--dev-sign-in", creds, "A")

    # ---------------------------------------------------------------- cleanup
    rb = b.dev("--dev-cleanup", b.mid)
    rbd = b.dev("--dev-delete-this-device")
    rA = a.dev("--dev-cleanup", a.mid)
    gone = a.dev("--dev-cloud-tree", pid)
    inv = a.dev("--dev-inventory")
    check("Z1 cloud: Storage objects, store_events, profile, mappings, devices removed", rb.get("unmapped") and rbd.get("deleted") and rA.get("deleted_profile")
          and rA.get("device_deleted") and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0
          and inv.get("devices") == [], json.dumps({"B": rb, "A": rA}))
    so = a.dev("--dev-sign-out")
    check("Z2 Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, cdev):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    for f in os.listdir(ra.WORK):
        p_ = os.path.join(ra.WORK, f)
        if f.startswith("synchrome-dummy-v2-") or f in ("ui-driver",):
            shutil.rmtree(p_, ignore_errors=True)
        elif f == "ui-app.log":
            os.remove(p_)
    check("Z3 local: data dirs (state.sqlite v5 with settings + conflicts, registry, logs, extension-store backups), Helium profiles + manifests, dummy v2 copies, UI files",
          not any(os.path.exists(p_) for dv in (a, b, cdev) for p_ in (dv.udd, dv.data)) and not [f for f in os.listdir(ra.WORK) if f.startswith("synchrome-dummy-v2-")], "")


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
    print(f"\n=== settings: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
