"""Two-device E2E of extension settings (chrome.storage.local / .sync) — M1 slice 11.

Devices A and B: two simulated installations (own SYNCHROME_DATA_DIR each), both signed in as
USER_A, each with its own disposable Helium; USER_B only for isolation. ONLY the controlled
fixture tests/fixtures/dummy-extension is read or written (its page.html lets the harness use
the dummy's own chrome.storage); no extension of the user is involved.

Chromium offers no cross-extension storage access, so Synchrome's app reads and writes the
LevelDB stores itself, and only while the browser is closed: every capture/apply step below
stops Helium first, runs the app's cycle (--dev-settings-cycle, the same Rust path as the
background loop and the "Retry" button), then starts Helium again to read the values.

Prints key names, value digests and counts — the fixture values are known test strings; no
token, password or real setting is printed.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_settings.py <creds-env-file>
"""
import json, os, re, shutil, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402
import e2e_conflicts as cf  # noqa: E402
import e2e_extensions as xe  # noqa: E402  (Helium rig with fixtures)

check = h.check
DUMMY, DUMMY_DIR = xe.DUMMY, xe.DUMMY_DIR
KEYS_LOCAL = {"localTheme", "localCount"}
KEYS_SYNC = {"syncMode", "syncEnabled"}
M = {"cycles": 0, "captured_ops": 0, "keys_written": 0, "echo_cycles_zero": 0}


def page(d, expr):
    """Evaluate in the dummy's own extension page (its chrome.storage)."""
    c = d.hl.c
    t = c.call("Target.createTarget", {"url": f"chrome-extension://{DUMMY}/page.html"})
    s = c.call("Target.attachToTarget", {"targetId": t["targetId"], "flatten": True})["sessionId"]
    v = None
    for _ in range(20):
        time.sleep(0.4)
        r = c.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True}, session=s)
        if "exceptionDetails" not in r:
            v = r.get("result", {}).get("value")
            break
    c.call("Target.closeTarget", {"targetId": t["targetId"]})
    return v


def storage(d):
    return page(d, "Promise.all([chrome.storage.local.get(null), chrome.storage.sync.get(null)]).then(([l, s]) => ({local: l, sync: s}))") or {}


def put(d, area, obj):
    page(d, f"chrome.storage.{area}.set({json.dumps(obj)}).then(() => true)")


def remove(d, area, key):
    page(d, f"chrome.storage.{area}.remove({json.dumps(key)}).then(() => true)")


def cycle(d, offline=False):
    """The app's settings cycle (browser must be closed for it to read/write)."""
    r = d.dev("--dev-settings-cycle", d.mid, offline=offline).get("report") or {}
    M["cycles"] += 1
    M["captured_ops"] += r.get("captured_ops", 0)
    M["keys_written"] += r.get("keys_written", 0)
    return r


def closed_cycle(d, fixtures=(DUMMY_DIR,), offline=False):
    """Close the browser, run a cycle, open it again."""
    xe.stop(d)
    r = cycle(d, offline=offline)
    xe.start(d, fixtures=list(fixtures))
    return r


def sview(d):
    return next((v for v in (d.dev("--dev-settings-views", d.mid).get("settings") or []) if v["ext_id"] == DUMMY), {})


def sops(a, pid, dev_id=None):
    ops = a.dev("--dev-cloud-tree", pid).get("settings_ops") or []
    return [o for o in ops if dev_id is None or o["by"] == dev_id]


def values(d):
    st = storage(d)
    return {**{f"local/{k}": v for k, v in (st.get("local") or {}).items()}, **{f"sync/{k}": v for k, v in (st.get("sync") or {}).items()}}


def dummy_v2():
    """A copy of the dummy with version 2.0.0 (same key => same id): the 'other version' case."""
    d = tempfile.mkdtemp(prefix="synchrome-dummy-v2-", dir=ra.WORK)
    for f in os.listdir(DUMMY_DIR):
        shutil.copy2(os.path.join(DUMMY_DIR, f), d)
    m = json.load(open(os.path.join(d, "manifest.json")))
    m["version"] = "2.0.0"
    json.dump(m, open(os.path.join(d, "manifest.json"), "w"))
    return d


def main(creds):
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a, b = ra.Device("A"), ra.Device("B")
    print("=== two-device extension settings E2E ===")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())
    sa = a.dev("--dev-sign-in", creds, "A")
    check("pre: login USER_A on A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if not sa.get("signed_in"):
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("pre: login USER_A on B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
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
    check("setup: same account/profile, distinct devices + data dirs + Helium profiles; dummy v1 on both",
          mb.get("profile_id") == pid and a.device_id != b.device_id and a.data != b.data
          and DUMMY in xe.mgmt(a) and DUMMY in xe.mgmt(b), f"profile={pid}")

    # settings Off by default: nothing read
    st0 = sview(a)
    check("default: settings sync Off for the dummy (never enabled automatically)", st0.get("mode") == "off" and st0.get("state") == "off", json.dumps(st0.get("state")))
    put(a, "local", {"localTheme": "dark", "localCount": 1, "apiToken": "fixture-not-a-secret"})
    put(a, "sync", {"syncMode": "compact", "syncEnabled": True})
    r0 = closed_cycle(a)
    check("Off: the cycle reads nothing and publishes nothing", r0.get("captured_ops", 0) == 0 and not sops(a, pid), "")
    # enable on both devices (local + sync)
    ea = a.dev("--dev-settings-mode", a.mid, DUMMY, "local_sync")
    eb_ = b.dev("--dev-settings-mode", b.mid, DUMMY, "local_sync")
    check("settings sync enabled explicitly on A and B (Local + Sync)", ea.get("ok") and eb_.get("ok"), json.dumps([ea, eb_]))

    # 1-2 A snapshot -> cloud
    r1 = closed_cycle(a)
    ops = sops(a, pid, a.device_id)
    ids = {o["id"].split("/", 1)[1] for o in ops}
    check("1 A captured local + sync (browser closed)", r1.get("captured_ops") == 4 and r1.get("excluded_sensitive") == 1, json.dumps(r1))
    check("2 cloud has the 4 keys (areas kept apart), ver=1.0.0 on each, the sensitive key absent",
          ids == {"local/localTheme", "local/localCount", "sync/syncMode", "sync/syncEnabled"}
          and all(o["ver"] == "1.0.0" for o in ops) and not any("apiToken" in o["id"] for o in sops(a, pid)), json.dumps(sorted(ids)))

    # 3-4 B applies
    before_b = values(b)
    r2 = closed_cycle(b)
    vb, va = values(b), values(a)
    check("3 B applied (browser closed): 4 keys written", r2.get("keys_written") == 4 and before_b == {}, json.dumps(r2))
    check("4 values identical on A and B (except the sensitive key, which stays on A only)",
          {k: v for k, v in va.items() if "apiToken" not in k} == vb and "local/apiToken" in va and "local/apiToken" not in vb,
          json.dumps(sorted(vb)))
    r = closed_cycle(b)
    M["echo_cycles_zero"] += r.get("captured_ops") == 0
    check("18 echo = 0: B's next capture of what it wrote publishes nothing", r.get("captured_ops") == 0, json.dumps(r))

    # 5-6 change on A -> B
    put(a, "local", {"localTheme": "light"})
    closed_cycle(a)
    closed_cycle(b)
    check("5-6 A changed localTheme; B converged", values(b).get("local/localTheme") == "light", "")
    # 7-8 change on B -> A
    put(b, "sync", {"syncMode": "wide"})
    closed_cycle(b)
    closed_cycle(a)
    check("7-8 B changed syncMode; A converged", values(a).get("sync/syncMode") == "wide", "")

    # 9 concurrent edits of different keys -> merge
    put(a, "local", {"localCount": 2})
    put(b, "local", {"localTheme": "blue"})
    closed_cycle(a)
    closed_cycle(b)
    closed_cycle(a)
    va, vb = values(a), values(b)
    check("9 concurrent edits of different keys: both survive on both devices, no conflict",
          va.get("local/localCount") == vb.get("local/localCount") == 2 and va.get("local/localTheme") == vb.get("local/localTheme") == "blue"
          and not [c for c in (b.dev("--dev-conflict-views", b.mid).get("conflicts") or []) if c["item_kind"] == "setting"], "")

    # 10 same key: pending (explicit conflict) and published (LWW)
    put(b, "local", {"localTheme": "b-pending"})
    closed_cycle(b, offline=True)  # captured on B, not published (cloud unreachable)
    put(a, "local", {"localTheme": "a-published"})
    closed_cycle(a)
    t0 = time.time()
    closed_cycle(b)
    cv = [c for c in (b.dev("--dev-conflict-views", b.mid).get("conflicts") or []) if c["item_kind"] == "setting"]
    c10 = cv[0] if cv else {}
    check("10a same key with B's change unpublished: explicit setting conflict (local_pending/edit_edit) in the shared records; nothing overwritten",
          c10.get("kind") == "local_pending" and c10.get("subtype") == "edit_edit" and c10["local"]["value"] == "\"b-pending\""
          and c10["remote"]["value"] == "\"a-published\"" and values(b).get("local/localTheme") == "b-pending"
          and sview(b).get("state") == "conflict", json.dumps({k: c10.get(k) for k in ("kind", "subtype")}))
    rr = b.dev("--dev-resolve", b.mid, c10.get("conflict_id", ""), "keep_remote").get("results", [{}])[0]
    closed_cycle(b)
    closed_cycle(a)
    M["conflict_ms"] = int((time.time() - t0) * 1000)
    check("10b Keep remote (shared resolver): B writes A's value; both converge", rr.get("outcome") in ("resolving", "resolved")
          and values(b).get("local/localTheme") == values(a).get("local/localTheme") == "a-published", json.dumps(rr))
    put(a, "sync", {"syncEnabled": False})
    closed_cycle(a)
    closed_cycle(b)
    put(b, "sync", {"syncEnabled": True})
    closed_cycle(b)
    closed_cycle(a)
    check("10c both published one after the other: LWW (newest wins) on both, no conflict",
          values(a).get("sync/syncEnabled") is True and values(b).get("sync/syncEnabled") is True
          and not [c for c in (a.dev("--dev-conflict-views", a.mid).get("conflicts") or []) if c["item_kind"] == "setting"], "")

    # 11 delete
    remove(a, "local", "localCount")
    closed_cycle(a)
    closed_cycle(b)
    dl = [o for o in sops(a, pid, a.device_id) if o["op"] == "delete"]
    check("11 delete on A: tombstone published, the key removed on B, other keys kept",
          dl and dl[-1]["id"].endswith("/local/localCount") and "local/localCount" not in values(b) and "local/localTheme" in values(b), "")

    # 12 offline + recovery
    put(a, "local", {"localTheme": "offline-change"})
    closed_cycle(a)
    ro = closed_cycle(b, offline=True)
    check("12a B offline: nothing written, nothing lost", values(b).get("local/localTheme") == "a-published" and ro.get("keys_written") == 0, json.dumps(ro))
    closed_cycle(b)
    check("12b B online again: converged", values(b).get("local/localTheme") == "offline-change", "")

    # 13 restart (fresh app process each cycle + Helium restart)
    xe.restart(b, fixtures=[DUMMY_DIR])
    r = closed_cycle(b)
    check("13 restart of Helium and app: equivalent snapshot = 0 ops, nothing rewritten", r.get("captured_ops") == 0 and r.get("keys_written") == 0, json.dumps(r))

    # 14-15 browser open: wait; closed: apply
    put(a, "local", {"localTheme": "while-open"})
    closed_cycle(a)
    rw = cycle(b)  # B's browser is OPEN
    sv = sview(b)
    check("14 B's browser open: the cycle neither reads nor writes; state Waiting for browser to close",
          rw.get("browser_running") is True and rw.get("keys_written", 0) == 0 and sv.get("state") == "waiting_for_browser_close"
          and values(b).get("local/localTheme") == "offline-change", json.dumps({"report": rw, "state": sv.get("state")}))
    closed_cycle(b)
    check("15 browser closed: applied", values(b).get("local/localTheme") == "while-open", "")

    # 16 version mismatch on B
    v2 = dummy_v2()
    xe.restart(b, fixtures=[v2])
    put(a, "local", {"localTheme": "for-v1"})
    closed_cycle(a)
    rv = closed_cycle(b, fixtures=[v2])
    sv = sview(b)
    check("16a B runs dummy 2.0.0: values written by 1.0.0 are NOT applied; state Version mismatch",
          rv.get("keys_written") == 0 and values(b).get("local/localTheme") == "while-open" and sv.get("state") == "version_mismatch", json.dumps({"state": sv.get("state"), "report": rv}))
    xe.restart(b, fixtures=[DUMMY_DIR])
    closed_cycle(b)
    check("16b B back on 1.0.0: applied", values(b).get("local/localTheme") == "for-v1" and sview(b).get("state") == "synced", "")
    shutil.rmtree(v2, ignore_errors=True)

    # 17 sensitive key
    allops = sops(a, pid)
    check("17 the sensitive fixture key (apiToken) never left A: not in the cloud, not on B; counted as excluded on A",
          not any("apiToken" in o["id"] for o in allops) and "local/apiToken" not in values(b) and "local/apiToken" in values(a)
          and sview(a).get("excluded_sensitive", 0) >= 1, f"excluded_sensitive={sview(a).get('excluded_sensitive')}")

    # unrelated keys / extensions preserved; UI
    xe.stop(b)
    synchrome_keys = b.dev("--dev-read-ext-area", b.udd, "Default", h.EXT_ID, "local").get("keys") or {}
    xe.start(b, fixtures=[DUMMY_DIR])
    check("other extension ids untouched: Synchrome's own store holds only its own keys (no dummy key ever written there)",
          "mappingToken" in synchrome_keys and not (set(synchrome_keys) & (KEYS_LOCAL | KEYS_SYNC | {"apiToken"})), json.dumps(sorted(synchrome_keys)))
    ui = cf.UI(b)
    J = cf.UI_JS + "const srow = () => all('tr.settings-row').find(r => r.previousElementSibling && r.previousElementSibling.innerText.includes('Synchrome Test Dummy'));"
    ok = bool(ui.ready) and bool(ui.until(J + "return !!btn(document, 'Extensions');", 40))
    ui.js(J + "btn(document, 'Extensions').click(); return true;")
    txt = ui.until(J + "const r = srow(); return r ? r.innerText : null;", 30) or ""
    sel = ui.js(J + "const r = srow(); return r ? r.querySelector('select').value : null;")
    panel = ui.js(J + "return all('.conflicts').map(x => x.innerText).join('\\n');") or ""
    ui.stop()
    check("UI: Settings sync row with state, mode Local + Sync, counters, privacy warning; no values or ids shown",
          ok and "Settings sync" in txt and "keys local" in txt and "excluded sensitive" in txt and "may contain private data" in txt
          and sel == "local_sync" and "for-v1" not in panel and not re.search(r"[0-9a-f]{8}-[0-9a-f]{4}-", panel), txt[:200])

    # security / isolation
    for dv in (a, b):
        lg = dv.log()
        leaks = [w for w in ("localTheme", "for-v1", "fixture-not-a-secret", "apiToken", "syncMode") if w in lg]
        check(f"S1 device {dv.name} host log: no keys, values or tokens", not leaks and h.secret_free(lg), f"leaks={leaks}")
    xe.stop(a)
    xe.stop(b)
    first = ra.journal(a, pid, a.device_id)[1]
    cdev = ra.Device("C")
    u = cdev.dev("--dev-sign-in", creds, "B")
    probe = cdev.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, first[0]["path"] if first else "") if u.get("signed_in") else {}
    lv = b.dev("--dev-settings-views", b.mid)
    lm = b.dev("--dev-settings-mode", b.mid, DUMMY, "off")
    check("S2 USER_B (exact id) cannot read A's journals (settings included) or act on A's settings",
          u.get("user_id") == h.EXPECTED["B"] and probe.get("list_sees") == {"Ok": 0} and probe.get("get_first_chunk") == {"Ok": False}
          and probe.get("store_events_sees") == {"Ok": 0} and "error" in lv and "error" in lm, json.dumps({"views": lv.get("error"), "mode": lm.get("error")}))
    a.dev("--dev-sign-in", creds, "A")

    _, ca_, ea_ = ra.journal(a, pid, a.device_id)
    _, cb_, eb2 = ra.journal(a, pid, b.device_id)
    allops = sops(a, pid)
    M.update({
        "settings_ops": {"A": len(sops(a, pid, a.device_id)), "B": len(sops(a, pid, b.device_id))},
        "deletes": sum(1 for o in allops if o["op"] == "delete"),
        "chunks": {"A": len(ca_), "B": len(cb_)}, "store_events": {"A": len(ea_), "B": len(eb2)},
        "conflicts": [(c["subtype"], c["status"], c.get("resolution")) for c in (b.dev("--dev-conflict-views", b.mid, "all").get("conflicts") or []) if c["item_kind"] == "setting"],
    })
    print("METRICS " + json.dumps(M))
    check("metrics collected; chunks == store_events", len(ca_) == len(ea_) and len(cb_) == len(eb2), "")

    # cleanup
    rb = b.dev("--dev-cleanup", b.mid)
    rbd = b.dev("--dev-delete-this-device")
    rA = a.dev("--dev-cleanup", a.mid)
    gone = a.dev("--dev-cloud-tree", pid)
    inv = a.dev("--dev-inventory")
    check("Z1 cloud cleanup", rb.get("unmapped") and rbd.get("deleted") and rA.get("deleted_profile") and rA.get("device_deleted")
          and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0 and inv.get("devices") == [],
          json.dumps({"B": rb, "A": rA}))
    so = a.dev("--dev-sign-out")
    check("Z2 Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, cdev):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    shutil.rmtree(os.path.join(ra.WORK, "ui-driver"), ignore_errors=True)
    for f in os.listdir(ra.WORK):
        if f.startswith("synchrome-dummy-v2-") or f == "ui-app.log":
            shutil.rmtree(os.path.join(ra.WORK, f), ignore_errors=True) if os.path.isdir(os.path.join(ra.WORK, f)) else os.remove(os.path.join(ra.WORK, f))
    check("Z3 local cleanup: data dirs (state.sqlite incl. settings, backups of extension stores), Helium profiles, v2 copy, UI files",
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
    print(f"\n=== settings: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
