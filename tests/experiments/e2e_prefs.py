"""Two-device E2E of browser preferences (allowlisted `Preferences` paths) — M1 slice 12.

Devices A and B: two simulated installations (own SYNCHROME_DATA_DIR each), both signed in as
USER_A, each with its own DISPOSABLE Helium profile; USER_B only for isolation. No real profile
is read or written. The user's changes are made the way the browser's own settings page does
(chrome.settingsPrivate on chrome://settings inside the disposable Helium); Synchrome reads and
writes `Preferences` only with Helium closed (--dev-prefs-cycle: the same Rust path as the
background loop and the UI's Retry). Allowlisted values are non-sensitive by construction
(booleans, language tags); everything else of the file is compared by digest only.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_prefs.py <creds-env-file>
"""
import fcntl, json, os, re, shutil, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402
import e2e_conflicts as cf  # noqa: E402
import e2e_extensions as xe  # noqa: E402  (Helium rig)

check = h.check
ALLOW = ["bookmark_bar.show_on_all_tabs", "browser.enable_spellchecking", "download.prompt_for_download", "intl.selected_languages"]
A_VALUES = {"bookmark_bar.show_on_all_tabs": True, "download.prompt_for_download": True, "intl.selected_languages": "es,en", "browser.enable_spellchecking": False}
B_VALUES = {"bookmark_bar.show_on_all_tabs": False, "download.prompt_for_download": False, "intl.selected_languages": "fr", "browser.enable_spellchecking": True}
M = {"cycles": 0, "applies": 0, "paths_written": 0, "keep_local": 0, "keep_remote": 0, "retries_browser_open": 0, "retries_lock": 0,
     "version_mismatch": 0, "unsupported": 0, "idempotent": 0, "capture_to_cloud_ms": [], "pull_to_apply_ms": []}


# ------------------------------------------------------------------ helpers
def settings(d, expr):
    """Evaluate on the browser's own settings page (chrome.settingsPrivate)."""
    c = d.hl.c
    t = c.call("Target.createTarget", {"url": "chrome://settings/"})
    s = c.call("Target.attachToTarget", {"targetId": t["targetId"], "flatten": True})["sessionId"]
    v = None
    for _ in range(40):
        r = c.call("Runtime.evaluate", {"expression": "!!(window.chrome && chrome.settingsPrivate)", "returnByValue": True}, session=s)
        if r.get("result", {}).get("value"):
            r = c.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True}, session=s)
            v = r.get("result", {}).get("value")
            break
        time.sleep(0.25)
    c.call("Target.closeTarget", {"targetId": t["targetId"]})
    return v


def eff(d):
    """Effective values in the running browser (what the user sees)."""
    keys = ALLOW + ["intl.accept_languages"]
    v = settings(d, "Promise.all(%s.map(k => new Promise(r => chrome.settingsPrivate.getPref(k, p => r(p ? p.value : null)))))" % json.dumps(keys)) or []
    return dict(zip(keys, v))


def user_sets(d, values):
    """The user changes settings in the browser's settings page."""
    for k, v in values.items():
        settings(d, "new Promise(r => chrome.settingsPrivate.setPref(%s, %s, '', ok => r(ok)))" % (json.dumps(k), json.dumps(v)))


def prefs_path(d):
    return os.path.join(d.udd, "Default", "Preferences")


def read(d):
    """Closed profile: allowlisted values + digests of the rest (product reader)."""
    return d.dev("--dev-read-prefs", d.udd, "Default", "helium")


def reset_to_default(d, path):
    """A 'reset' of one preference (Chromium drops the key), made with the browser closed."""
    doc = json.load(open(prefs_path(d)))
    segs = path.split(".")
    cur = doc
    for s in segs[:-1]:
        cur = cur.get(s, {})
    cur.pop(segs[-1], None)
    if path == "bookmark_bar.show_on_all_tabs":
        doc.get("bookmark_bar", {}).pop("visibility_state", None)
    json.dump(doc, open(prefs_path(d), "w"), separators=(",", ":"))


def cycle(d, offline=False):
    t = time.time()
    r = d.dev("--dev-prefs-cycle", d.mid, offline=offline).get("report") or {}
    ms = int((time.time() - t) * 1000)
    M["cycles"] += 1
    if r.get("captured_ops") and not offline:
        M["capture_to_cloud_ms"].append(ms)
    if r.get("paths_written"):
        M["applies"] += 1
        M["paths_written"] += r["paths_written"]
        M["pull_to_apply_ms"].append(ms)
    return r


def closed_cycle(d, offline=False):
    xe.stop(d)
    r = cycle(d, offline=offline)
    xe.start(d, offline=offline)
    return r


def pview(d):
    return d.dev("--dev-prefs-view", d.mid).get("prefs") or {}


def pops(a, pid, dev_id=None):
    ops = a.dev("--dev-cloud-tree", pid).get("prefs_ops") or []
    return [o for o in ops if dev_id is None or o["by"] == dev_id]


def pconflicts(d, closed=False):
    return [c for c in (d.dev("--dev-conflict-views", d.mid, *(["all"] if closed else [])).get("conflicts") or []) if c["item_kind"] == "preference"]


def backups(d):
    p = os.path.join(d.data, "backups", "browser-preferences")
    return sorted(os.listdir(p)) if os.path.isdir(p) else []


def leftovers(d):
    return [n for n in os.listdir(os.path.join(d.udd, "Default")) if "synchrome-staging" in n or "synchrome-undo" in n]


def resolve(d, cid, res):
    r = d.dev("--dev-resolve", d.mid, cid, res).get("results", [{}])[0]
    if r.get("outcome") in ("resolved", "resolving"):
        M["keep_local" if res == "keep_local" else "keep_remote"] += 1
    return r


def set_major(d, version):
    """Browser closed: the major the next cycle sees (restored before Helium starts again)."""
    p = os.path.join(d.udd, "Last Version")
    old = open(p).read()
    open(p, "w").write(version)
    return old


def ui_state(d):
    ui = cf.UI(d)
    J = cf.UI_JS + "const prow = () => all('div.prefs-row')[0];"
    ok = bool(ui.ready) and bool(ui.until(J + "return !!prow();", 60))
    row = ui.until(J + """const r = prow(); if (!r) return null;
        return {text: r.innerText, badge: (r.querySelector('.badge')?.textContent || '').trim(), mode: r.querySelector('select').value,
                options: all('option', r).map(o => o.textContent), buttons: all('button', r).map(b => b.textContent.trim())};""", 30) or {}
    return ui, J, ok, row


# ------------------------------------------------------------------ main
def main(creds):
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a, b = ra.Device("A"), ra.Device("B")
    print("=== two-device browser preferences E2E ===")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())
    check("pre: only disposable profiles (under the harness work dir)", a.udd.startswith(ra.WORK) and b.udd.startswith(ra.WORK))

    # ---------------------------------------------------------------- 1 setup
    sa = a.dev("--dev-sign-in", creds, "A")
    check("1.1 login USER_A on A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if not sa.get("signed_in"):
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("1.2 login USER_A on B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
    a.device_id = a.dev("--dev-register-device").get("device_id")
    b.device_id = b.dev("--dev-register-device").get("device_id")
    m = a.dev("--dev-create-mapping", f"E2E PREF {os.urandom(3).hex()}", "helium", "Default")
    a.mid, pid = m.get("mapping_id"), m.get("profile_id")
    a.dev("--dev-write-token", a.mid, a.udd)
    a.dev("--dev-install-host", a.udd)
    xe.start(a)
    mb = b.dev("--dev-map-existing", pid, "helium", "Default")
    b.mid = mb.get("mapping_id")
    b.dev("--dev-write-token", b.mid, b.udd)
    b.dev("--dev-install-host", b.udd)
    xe.start(b)
    check("1.3 same account + profile; distinct devices, mappings, data dirs, Helium profiles",
          mb.get("profile_id") == pid and a.device_id != b.device_id and a.data != b.data and a.udd != b.udd, f"profile={pid}")
    # each user's own (different) choices, through the settings page
    user_sets(a, A_VALUES)
    user_sets(b, B_VALUES)
    ea0, eb0 = eff(a), eff(b)
    check("1.4 A and B start with different values (set through chrome://settings)",
          all(ea0[k] == v for k, v in A_VALUES.items()) and all(eb0[k] == v for k, v in B_VALUES.items()), json.dumps({"A": ea0, "B": eb0}))
    v0 = pview(a)
    ui, J, ok, row = ui_state(b)
    ui.js(J + "const s = prow().querySelector('select'); s.value = 'recommended'; s.dispatchEvent(new Event('change', {bubbles: true})); return true;")
    offered = ui.until(J + "return btn(prow(), 'Enable') && prow().innerText.includes('Browser preferences may affect how your browser behaves on this device.') ? true : null;", 10)
    ui.js(J + "btn(prow(), 'Enable').click(); return true;")
    enabled = ui.until(J + "const r = prow(); return r && r.querySelector('select').value === 'recommended' && !btn(r, 'Enable') ? (r.querySelector('.badge')?.textContent || '').trim() : null;", 90)
    ui.stop()
    check("1.5 + 19 UI: Browser preferences Off by default; selector Off / Sync recommended preferences; Enable + the warning; B enabled from the UI",
          v0.get("mode") == "off" and ok and row.get("badge") == "Off" and row.get("options") == ["Off", "Sync recommended preferences"]
          and offered is True and bool(enabled), json.dumps({"badge": row.get("badge"), "options": row.get("options"), "after_enable": enabled}))
    check("1.6 while Off on A the cycle reads nothing", not closed_cycle(a).get("captured_ops") and not pops(a, pid), "")
    ea = a.dev("--dev-prefs-mode", a.mid, "recommended")
    check("1.7 A enabled (app command)", ea.get("ok") and pview(a).get("mode") == "recommended", "")

    # ---------------------------------------------------------------- 2 A snapshot -> cloud (allowlist only)
    xe.stop(a)
    ra_ = read(a)
    t0 = time.time()
    r1 = cycle(a)
    xe.start(a)
    ops = pops(a, pid, a.device_id)
    check("2.1 A closed: snapshot extracted the 4 allowlisted values; 4 ops", ra_.get("values") == A_VALUES and r1.get("captured_ops") == 4,
          json.dumps({"values": ra_.get("values"), "ops": r1.get("captured_ops")}))
    blob = json.dumps(a.dev("--dev-cloud-tree", pid).get("prefs_ops"))
    check("2.2 cloud received only the allowlist: id = path, fields {value, bk, maj}, writer helium/154",
          sorted(o["id"] for o in ops) == sorted(ALLOW) and all(sorted(o["fields"]) == ["bk", "maj", "value"] and o["bk"] == "helium" and o["maj"] == 154 for o in ops),
          json.dumps(sorted(o["id"] for o in ops)))
    check("2.3 nothing else of Preferences in the cloud (no paths, homepage, accounts, content settings, download dir, accept_languages)",
          not any(w in blob for w in ("/Users", "/tmp", "default_directory", "homepage", "account_info", "content_settings", "accept_languages", "\"fr\"")), "")

    # ---------------------------------------------------------------- 3-9 B pull, close, apply, open
    xe.stop(b)
    before_b = read(b)
    nbk = len(backups(b))
    r2 = cycle(b)
    after_b = read(b)
    xe.start(b)
    eb1 = eff(b)
    check("3-5 B joined (pull before its first capture), Helium closed, applied 4 paths", r2.get("paths_written") == 4 and r2.get("captured_ops") == 0, json.dumps(r2))
    check("6-7 B opened: the browser reflects A's values (accept_languages recomputed by Helium)",
          all(eb1[k] == v for k, v in A_VALUES.items()) and eb1["intl.accept_languages"] == "es,en", json.dumps(eb1))
    check("8 the rest of B's Preferences unchanged by the write (digest of everything but the allowlist), Secure Preferences untouched; backup outside the profile; no staging leftovers",
          before_b.get("rest_sha") == after_b.get("rest_sha") and before_b.get("secure_sha") == after_b.get("secure_sha")
          and len(backups(b)) == nbk + 1 and not leftovers(b), json.dumps({"rest_equal": before_b.get("rest_sha") == after_b.get("rest_sha"), "backups": len(backups(b)) - nbk}))
    check("8b B's own previous value ('fr') never left B", not any(o.get("value") == "fr" for o in pops(a, pid)), "")
    _, cb0, eb_ev0 = ra.journal(a, pid, b.device_id)
    ra0, rb0 = closed_cycle(a), closed_cycle(b)
    _, cb1, eb_ev1 = ra.journal(a, pid, b.device_id)
    M["idempotent"] += (ra0.get("captured_ops") == 0) + (rb0.get("captured_ops") == 0)
    check("9 next snapshots: 0 ops on A and B (echo 0), no new chunks / store_events from B",
          ra0.get("captured_ops") == 0 and rb0.get("captured_ops") == 0 and len(cb1) == len(cb0) and len(eb_ev1) == len(eb_ev0), json.dumps([ra0, rb0]))

    # ---------------------------------------------------------------- 10 B -> A
    user_sets(b, {"download.prompt_for_download": False})
    rb = closed_cycle(b)
    closed_cycle(a)
    check("10 B changed download.prompt_for_download: exactly 1 op; A shows it", rb.get("captured_ops") == 1 and eff(a)["download.prompt_for_download"] is False, json.dumps(rb))

    # ---------------------------------------------------------------- 11 different paths concurrently -> merge
    user_sets(a, {"browser.enable_spellchecking": True})
    closed_cycle(a, offline=True)
    user_sets(b, {"intl.selected_languages": "de,en"})
    closed_cycle(b, offline=True)
    closed_cycle(a)
    closed_cycle(b)
    closed_cycle(a)
    va, vb = eff(a), eff(b)
    check("11 offline edits of different paths on A and B: both survive on both, no conflict",
          va["browser.enable_spellchecking"] is True and vb["browser.enable_spellchecking"] is True
          and va["intl.selected_languages"] == vb["intl.selected_languages"] == "de,en" and not pconflicts(a) and not pconflicts(b), json.dumps({"A": va, "B": vb}))

    # ---------------------------------------------------------------- 12 same path -> conflict (both choices)
    for choice, (bv, av) in (("keep_local", ("ca", "eu")), ("keep_remote", ("it", "nl"))):
        user_sets(b, {"intl.selected_languages": bv})
        closed_cycle(b, offline=True)  # captured, unpublished
        user_sets(a, {"intl.selected_languages": av})
        closed_cycle(a)
        closed_cycle(b)
        cv = pconflicts(b)
        c = cv[0] if cv else {}
        check(f"12.1 [{choice}] same path with B pending: browser_preferences/local_pending/edit_edit; B's value not overwritten",
              c.get("kind") == "local_pending" and c.get("subtype") == "edit_edit" and c.get("name") == "intl.selected_languages"
              and eff(b)["intl.selected_languages"] == bv and pview(b).get("state") == "conflict", json.dumps({k: c.get(k) for k in ("kind", "subtype", "name")}))
        if choice == "keep_local":
            ui, J, ok, row = ui_state(b)
            ui.js(J + "btn(prow(), 'Review conflicts').click(); return true;")
            card = ui.until(J + "const c = card('intl.selected_languages'); return c ? all('tr', c).map(r => r.innerText.replace(/\\s+/g, ' ').trim()) : null;", 20) or []
            ui.stop()
            check("12.2 + 19 UI: Conflict badge, counts, 'Review conflicts' → per-path card comparing Exists / Value",
                  row.get("badge") == "Conflict" and "Review conflicts" in row.get("buttons", []) and "1 conflicts" in row.get("text", "")
                  and any(r_.startswith("Exists") for r_ in card) and any(r_.startswith("Value") for r_ in card), json.dumps(card))
        rr = resolve(b, c.get("conflict_id", ""), choice)
        closed_cycle(b)
        closed_cycle(a)
        want = bv if choice == "keep_local" else av
        r = closed_cycle(b)
        check(f"12.3 {choice}: both converge on {want!r} through the common resolver; no echo afterwards",
              rr.get("outcome") in ("resolved", "resolving") and eff(a)["intl.selected_languages"] == eff(b)["intl.selected_languages"] == want
              and not pconflicts(b) and r.get("captured_ops") == 0, json.dumps(rr))

    # ---------------------------------------------------------------- 13 reset to default -> tombstone; edit vs delete
    xe.stop(a)
    reset_to_default(a, "download.prompt_for_download")
    rt = cycle(a)
    xe.start(a)
    closed_cycle(b)
    xe.stop(b)
    gone = read(b).get("raw", {})
    xe.start(b)
    check("13.1 a reset to default on A (key gone) is a tombstone; B drops the key (B uses its default)",
          rt.get("captured_ops") == 1 and any(o["op"] == "delete" and o["id"] == "download.prompt_for_download" for o in pops(a, pid))
          and "download.prompt_for_download" not in gone and eff(b)["download.prompt_for_download"] is False, json.dumps(rt))
    user_sets(b, {"bookmark_bar.show_on_all_tabs": False})
    closed_cycle(b, offline=True)
    xe.stop(a)
    reset_to_default(a, "bookmark_bar.show_on_all_tabs")
    cycle(a)
    xe.start(a)
    closed_cycle(b)
    cv = pconflicts(b)
    c = cv[0] if cv else {}
    check("13.2 B pending edit vs A's reset: edit_delete", c.get("subtype") == "edit_delete", json.dumps(c.get("subtype")))
    resolve(b, c.get("conflict_id", ""), "keep_local")
    closed_cycle(b)
    closed_cycle(a)
    bb = [o for o in pops(a, pid) if o["id"] == "bookmark_bar.show_on_all_tabs"]
    a_del = max((o["hlc"] for o in bb if o["op"] == "delete" and o["by"] == a.device_id), default="")
    back = [o for o in bb if o["op"] == "set" and o["by"] == b.device_id and o["hlc"] > a_del]
    xe.stop(a)
    a_raw = read(a).get("raw", {})
    xe.start(a)
    check("13.3 Keep local: B's value comes back on A (new op after A's reset; the key is in A's file again)",
          back and back[-1]["value"] is False and a_raw.get("bookmark_bar.show_on_all_tabs") is False and not pconflicts(b), json.dumps(a_raw))

    # ---------------------------------------------------------------- 14 browser open -> Waiting
    user_sets(a, {"browser.enable_spellchecking": False})
    closed_cycle(a)
    nbk = len(backups(b))
    rw = cycle(b)  # Helium B is OPEN
    M["retries_browser_open"] += 1
    sv = pview(b)
    ui, J, ok, row = ui_state(b)
    ui.stop()
    check("14.1 B open: Waiting for browser to close; nothing read or written; no backup; pending kept",
          rw.get("browser_running") is True and not rw.get("paths_written") and sv.get("state") == "waiting_for_browser_close" and sv.get("pending", 0) >= 1
          and len(backups(b)) == nbk and eff(b)["browser.enable_spellchecking"] is True, json.dumps({"report": rw, "state": sv.get("state")}))
    check("14.2 + 19 UI: 'Waiting for browser to close' with Retry", row.get("badge") == "Waiting for browser to close" and "Retry" in row.get("buttons", []), json.dumps(row.get("badge")))
    closed_cycle(b)
    check("14.3 closed + Retry: applied", eff(b)["browser.enable_spellchecking"] is False and pview(b).get("state") == "synced", "")

    # ---------------------------------------------------------------- 15 LOCK -> Retry
    user_sets(a, {"intl.selected_languages": "pl"})
    closed_cycle(a)
    xe.stop(b)
    raw0 = open(prefs_path(b), "rb").read()
    lockf = open(prefs_path(b), "rb")
    fcntl.flock(lockf.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    nbk = len(backups(b))
    rl = cycle(b)
    M["retries_lock"] += 1
    sv = pview(b)
    fcntl.flock(lockf.fileno(), fcntl.LOCK_UN)
    lockf.close()
    check("15.1 Preferences locked (browser closed): refused, file byte-identical, no backup, Waiting",
          not rl.get("paths_written") and open(prefs_path(b), "rb").read() == raw0 and len(backups(b)) == nbk and sv.get("state") == "waiting_for_browser_close",
          json.dumps({"report": rl, "state": sv.get("state")}))
    r = cycle(b)
    xe.start(b)
    check("15.2 lock released: Retry applies", r.get("paths_written", 0) >= 1 and eff(b)["intl.selected_languages"] == "pl", json.dumps(r))

    # ---------------------------------------------------------------- version mismatch / unsupported (Last Version)
    user_sets(a, {"intl.selected_languages": "sv"})
    xe.stop(a)
    old_a = set_major(a, "155.0.9999.1")
    cycle(a)
    set_major(a, old_a)
    xe.start(a)
    xe.stop(b)
    raw1 = open(prefs_path(b), "rb").read()
    rv = cycle(b)
    sv = pview(b)
    M["version_mismatch"] += sv.get("state") == "version_mismatch"
    ui, J, ok, row = ui_state(b)
    ui.stop()
    check("V.1 A's value written by an unvalidated major (155) is NOT applied on B (154): Version mismatch, file byte-identical",
          not rv.get("paths_written") and rv.get("version_mismatch") == 1 and sv.get("state") == "version_mismatch" and open(prefs_path(b), "rb").read() == raw1,
          json.dumps({"report": rv, "state": sv.get("state")}))
    check("V.2 + 19 UI: 'Version mismatch' with Retry", row.get("badge") == "Version mismatch" and "Retry" in row.get("buttons", []), json.dumps(row.get("badge")))
    old_b = set_major(b, "155.0.9999.1")
    rv2 = cycle(b)
    set_major(b, old_b)
    check("V.3 same major on B: applied", rv2.get("paths_written") == 1, json.dumps(rv2))
    old_b = set_major(b, "150.0.1.1")
    cycle(b)
    su = pview(b)
    M["unsupported"] += su.get("state") == "unsupported"
    ui, J, ok, row = ui_state(b)
    ui.stop()
    set_major(b, old_b)
    check("V.4 + 19 an older browser than validated: Unsupported (4 unsupported), nothing written", su.get("state") == "unsupported" and su.get("unsupported") == 4
          and row.get("badge") == "Unsupported", json.dumps({"state": su.get("state"), "badge": row.get("badge")}))
    cycle(b)
    xe.start(b)
    check("V.5 back on 154: synced, value kept", eff(b)["intl.selected_languages"] == "sv" and pview(b).get("state") == "synced", "")

    # ---------------------------------------------------------------- 16 restart
    cfg0, cur0 = pview(b), b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    xe.restart(b)
    dup = b.dev("--dev-pull-duplicates", b.mid)
    r = closed_cycle(b)
    cfg1 = pview(b)
    M["idempotent"] += r.get("captured_ops") == 0
    check("16 app + Helium restart: mode, counts and cursor persisted; duplicated events + capture: 0 duplicates, 0 ops, 0 writes",
          cfg0.get("mode") == cfg1.get("mode") == "recommended" and cfg0.get("synced") == cfg1.get("synced") and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == cur0
          and "chunks: 0, ops: 0" in dup.get("pull", "") and r.get("captured_ops") == 0 and not r.get("paths_written"), json.dumps({"dup": dup.get("pull"), "r": r}))

    # ---------------------------------------------------------------- 17 offline
    before = eff(b)["download.prompt_for_download"]
    user_sets(a, {"download.prompt_for_download": True})
    closed_cycle(a)
    cur1 = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    ro = closed_cycle(b, offline=True)
    off = b.dev("--dev-sync", b.mid, offline=True)
    check("17.1 B without Cloud: cursor kept, nothing applied, Offline",
          not ro.get("paths_written") and eff(b)["download.prompt_for_download"] == before and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == cur1
          and "offline" in off.get("pull", ""), json.dumps({"pull": off.get("pull")}))
    closed_cycle(b)
    check("17.2 Cloud back: converged", eff(b)["download.prompt_for_download"] is True, "")

    # ---------------------------------------------------------------- 19 UI: Synced with counts, Disable offered
    xe.stop(b)
    ui, J, ok, row = ui_state(b)  # browser closed: Synced
    ui.js(J + "const s = prow().querySelector('select'); s.value = 'off'; s.dispatchEvent(new Event('change', {bubbles: true})); return true;")
    disable = ui.until(J + "return btn(prow(), 'Disable') ? true : null;", 10)
    panel = ui.js(J + "return document.body.innerText;") or ""
    ui.stop()
    xe.start(b)
    check("19 UI Synced with 'N preferences synced · N unsupported · N pending · N conflicts'; Off offers Disable (not clicked); no ids or paths of the disk shown",
          row.get("badge") == "Synced" and all(w in row.get("text", "") for w in ("preferences synced", "unsupported", "pending", "conflicts"))
          and disable is True and pview(b).get("mode") == "recommended" and "/tmp/" not in panel and "/Users/" not in panel, row.get("text", "")[:200])

    # ---------------------------------------------------------------- security
    for dv in (a, b):
        lg = dv.log()
        leaks = [w for w in ("selected_languages", "prompt_for_download", "/Users/", "Application Support") if w in lg]
        check(f"S device {dv.name} host log: no preference paths/values, tokens or local paths", not leaks and h.secret_free(lg), f"leaks={leaks}")
    check("S cloud: every op is an allowlisted path", all(o["id"] in ALLOW for o in pops(a, pid)), "")

    # ---------------------------------------------------------------- metrics, 18 isolation
    xe.stop(a)
    xe.stop(b)
    allops = pops(a, pid)
    _, ca_, ea_ = ra.journal(a, pid, a.device_id)
    _, cb_, eb2 = ra.journal(a, pid, b.device_id)
    avg = lambda l: int(sum(l) / len(l)) if l else None
    conf = pconflicts(b, closed=True)
    print("METRICS " + json.dumps({
        "paths_synchronised": len({o["id"] for o in allops}), "sets": sum(o["op"] == "set" for o in allops), "tombstones": sum(o["op"] == "delete" for o in allops),
        "conflicts": len(conf), "conflict_kinds": sorted({c["subtype"] for c in conf}), "keep_local": M["keep_local"], "keep_remote": M["keep_remote"],
        "applies": M["applies"], "paths_written": M["paths_written"], "backups": {"A": len(backups(a)), "B": len(backups(b))},
        "retries_browser_open": M["retries_browser_open"], "retries_lock": M["retries_lock"], "version_mismatch": M["version_mismatch"], "unsupported": M["unsupported"],
        "idempotent_snapshots": M["idempotent"], "chunks": {"A": len(ca_), "B": len(cb_)}, "store_events": {"A": len(ea_), "B": len(eb2)},
        "capture_to_cloud_ms_mean": avg(M["capture_to_cloud_ms"]), "pull_to_apply_ms_mean": avg(M["pull_to_apply_ms"]), "cycles": M["cycles"],
    }))
    check("metrics collected; chunks == store_events per device", len(ca_) == len(ea_) and len(cb_) == len(eb2), f"A {len(ca_)}/{len(ea_)} B {len(cb_)}/{len(eb2)}")
    first = ca_[0]["path"] if ca_ else ""
    cdev = ra.Device("C")
    u = cdev.dev("--dev-sign-in", creds, "B")
    probe = cdev.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, first) if u.get("signed_in") else {}
    refd = lambda v_: isinstance(v_, dict) and "Err" in v_
    check("18.1 login USER_B (exact id)", u.get("user_id") == h.EXPECTED["B"], f"user_id={u.get('user_id')}")
    check("18.2 USER_B cannot see A's profile/mappings, list/download A's journals, read A's store_events, write under A's prefix",
          probe.get("foreign_profile_visible") == {"Ok": False} and probe.get("foreign_mappings_visible") == {"Ok": 0} and probe.get("list_sees") == {"Ok": 0}
          and probe.get("get_first_chunk") == {"Ok": False} and probe.get("store_events_sees") == {"Ok": 0} and refd(probe.get("put_under_foreign_prefix")),
          json.dumps({k: v_ for k, v_ in probe.items() if k != "as_user"}))
    lv = b.dev("--dev-prefs-view", b.mid)
    lm = b.dev("--dev-prefs-mode", b.mid, "off")
    lc = b.dev("--dev-conflict-views", b.mid, "all")
    leak = json.dumps([lv, lm, lc])
    check("18.3 USER_B cannot read or change A's preferences state/conflicts; no values leak",
          "error" in lv and "error" in lm and "error" in lc and not any(w in leak for w in ("selected_languages", "\"sv\"")),
          json.dumps({"view": lv.get("error"), "mode": lm.get("error"), "conflicts": lc.get("error")}))
    a.dev("--dev-sign-in", creds, "A")

    # ---------------------------------------------------------------- 20 cleanup
    rb_ = b.dev("--dev-cleanup", b.mid)
    rbd = b.dev("--dev-delete-this-device")
    rA = a.dev("--dev-cleanup", a.mid)
    gone = a.dev("--dev-cloud-tree", pid)
    inv = a.dev("--dev-inventory")
    check("20.1 cloud: Storage objects, store_events, profile, mappings, devices removed", rb_.get("unmapped") and rbd.get("deleted") and rA.get("deleted_profile")
          and rA.get("device_deleted") and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0
          and inv.get("devices") == [], json.dumps({"B": rb_, "A": rA}))
    so = a.dev("--dev-sign-out")
    check("20.2 Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, cdev):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    shutil.rmtree(os.path.join(ra.WORK, "ui-driver"), ignore_errors=True)
    if os.path.exists(os.path.join(ra.WORK, "ui-app.log")):
        os.remove(os.path.join(ra.WORK, "ui-app.log"))
    check("20.3 local: data dirs (state.sqlite v6, registry, logs, Preferences backups), Helium profiles + manifests, UI files",
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
    print(f"\n=== prefs: {len(h.results) - len(failed)} pass, {len(failed)} fail ===")
    if failed:
        print("  failures: " + ", ".join(failed))
    sys.exit(1 if failed else 0)
