"""Two-device E2E of browser preferences (allowlisted `Preferences` paths) — M1 slice 12. 27 cases.

Devices A and B: two simulated installations (own SYNCHROME_DATA_DIR each), both signed in as
USER_A, each with its own DISPOSABLE Helium profile; USER_B only for isolation. No real profile
(Helium or Chrome) is read or written. The user's changes are made the way the browser's own
settings page does (chrome.settingsPrivate on chrome://settings inside the disposable Helium);
Synchrome reads and writes `Preferences` only with Helium closed (--dev-prefs-cycle: the same
Rust path as the background loop and the UI's Retry). Allowlisted values are non-sensitive by
construction (booleans, language tags); everything else of the file is compared by digest.

  EXPECTED_A_ID=… EXPECTED_B_ID=… python3 e2e_prefs.py <creds-env-file>
"""
import fcntl, json, os, re, shutil, stat, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import e2e_handshake as h  # noqa: E402
import e2e_bookmarks as eb  # noqa: E402
import e2e_remote_apply as ra  # noqa: E402
import e2e_conflicts as cf  # noqa: E402
import e2e_extensions as xe  # noqa: E402  (Helium rig)

check = h.check
ALLOW = ["bookmark_bar.show_on_all_tabs", "browser.enable_spellchecking", "download.prompt_for_download", "intl.selected_languages"]
EXCLUDED = ["homepage", "homepage_is_newtabpage", "browser.show_home_button", "intl.accept_languages", "spellcheck.dictionaries", "translate.enabled", "spellcheck.use_spelling_service"]
A_VALUES = {"download.prompt_for_download": True, "browser.enable_spellchecking": True, "intl.selected_languages": "es,en", "bookmark_bar.show_on_all_tabs": True}
B_VALUES = {"download.prompt_for_download": False, "browser.enable_spellchecking": False, "intl.selected_languages": "fr", "bookmark_bar.show_on_all_tabs": False}
UNKNOWN = {"x": "<keep-me>", "n": 1.5, "nested": {"list": [1, "two", None]}}
M = {"cycles": 0, "applies": 0, "paths_written": 0, "keep_local": 0, "keep_remote": 0, "retries_browser_open": 0, "retries_lock": 0,
     "version_mismatch": 0, "unsupported": 0, "idempotent": 0, "capture_to_cloud_ms": [], "pull_to_apply_ms": [], "notes": []}


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


def eff(d, keys=None):
    """Effective values in the running browser (what the user sees)."""
    keys = keys or ALLOW + ["intl.accept_languages"]
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


def file_edit(d, fn):
    """Edit Preferences with the browser closed (a harness fixture / a reset to default)."""
    doc = json.load(open(prefs_path(d)))
    fn(doc)
    json.dump(doc, open(prefs_path(d), "w"), separators=(",", ":"))


def drop(path):
    def fn(doc):
        segs = path.split(".")
        cur = doc
        for s in segs[:-1]:
            cur = cur.get(s, {})
        cur.pop(segs[-1], None)
        if path == "bookmark_bar.show_on_all_tabs":
            doc.get("bookmark_bar", {}).pop("visibility_state", None)
    return fn


def cycle(d, offline=False, kind=None):
    t = time.time()
    if kind:
        d.env["SYNCHROME_DEV_PREFS_BROWSER_KIND"] = kind
    try:
        r = d.dev("--dev-prefs-cycle", d.mid, offline=offline).get("report") or {}
    finally:
        d.env.pop("SYNCHROME_DEV_PREFS_BROWSER_KIND", None)
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


def conflict_pair(a, b, path, b_change, a_change, pid):
    """B makes an unpublished change (b_change: values dict or a file-edit fn); A publishes a_change."""
    if callable(b_change):
        xe.stop(b)
        file_edit(b, b_change)
        cycle(b, offline=True)
        xe.start(b, offline=True)
    else:
        user_sets(b, b_change)
        closed_cycle(b, offline=True)
    if callable(a_change):
        xe.stop(a)
        file_edit(a, a_change)
        cycle(a)
        xe.start(a)
    else:
        user_sets(a, a_change)
        closed_cycle(a)
    closed_cycle(b)
    cv = [c for c in pconflicts(b) if c.get("name") == path]
    return cv[0] if cv else {}


# ------------------------------------------------------------------ main
def main(creds):
    h.prepare_binary()
    os.makedirs(ra.WORK, exist_ok=True)
    a, b = ra.Device("A"), ra.Device("B")
    print("=== two-device browser preferences E2E ===")
    check("pre: signed out, no Keychain session", a.dev("--dev-account").get("signed_in") is False and not h.keychain_has_session())
    check("pre: only disposable Helium profiles (under the harness work dir)", a.udd.startswith(ra.WORK) and b.udd.startswith(ra.WORK))

    # ---------------------------------------------------------------- CASE 1 setup
    sa = a.dev("--dev-sign-in", creds, "A")
    check("C1.1 login USER_A on A (exact id)", sa.get("user_id") == h.EXPECTED["A"], f"user_id={sa.get('user_id')}")
    if not sa.get("signed_in"):
        return
    sb = b.dev("--dev-sign-in", creds, "A")
    check("C1.2 login USER_A on B (exact id)", sb.get("user_id") == h.EXPECTED["A"], f"user_id={sb.get('user_id')}")
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
    files = lambda d: [os.path.join(d.data, f) for f in ("device.json", "state.sqlite", "logs/native-host.log")]
    check("C1.3-5 cloud profile; confirmed mappings A and B to it; distinct SYNCHROME_DATA_DIR, device.json, state.sqlite, logs, Helium profiles",
          mb.get("profile_id") == pid and a.device_id != b.device_id and a.mid != b.mid and a.data != b.data and a.udd != b.udd
          and all(os.path.exists(f) for f in files(a) + files(b))
          and a.dev("--dev-registry", a.mid).get("state") == b.dev("--dev-registry", b.mid).get("state") == "confirmed", f"profile={pid}")
    v0a, v0b = pview(a), pview(b)
    # each user's own (different) choices, through the settings page
    user_sets(a, A_VALUES)
    user_sets(b, B_VALUES)
    # an unknown key and known blocks on B, to prove they survive Synchrome's writes byte for byte
    xe.stop(b)
    file_edit(b, lambda doc: doc.update({"synchrome_e2e_unknown": UNKNOWN}))
    xe.start(b)
    ea0, eb0 = eff(a), eff(b)
    check("C2.8 A and B have different known values, set through chrome://settings (settingsPrivate)",
          all(ea0[k] == v for k, v in A_VALUES.items()) and all(eb0[k] == v for k, v in B_VALUES.items()), json.dumps({"A": ea0, "B": eb0}))
    ui, J, ok, row = ui_state(b)
    ui.js(J + "const s = prow().querySelector('select'); s.value = 'recommended'; s.dispatchEvent(new Event('change', {bubbles: true})); return true;")
    offered = ui.until(J + "return btn(prow(), 'Enable') && prow().innerText.includes('Browser preferences may affect how your browser behaves on this device.') ? true : null;", 10)
    ui.js(J + "btn(prow(), 'Enable').click(); return true;")
    enabled = ui.until(J + "const r = prow(); return r && r.querySelector('select').value === 'recommended' && !btn(r, 'Enable') ? (r.querySelector('.badge')?.textContent || '').trim() : null;", 90)
    ui.stop()
    check("C1.7 + C25.129-133 Off by default on A and B; UI (embedded frontend): Browser preferences, selector Off / Sync recommended preferences, Off, exact warning; B enabled with 'Enable'",
          v0a.get("mode") == v0b.get("mode") == "off" and v0a.get("state") == "off" and ok and row.get("badge") == "Off" and row.get("mode") == "off"
          and row.get("options") == ["Off", "Sync recommended preferences"] and offered is True and bool(enabled),
          json.dumps({"badge": row.get("badge"), "options": row.get("options"), "after_enable": enabled}))
    check("C1.7b while Off on A the cycle reads nothing", not closed_cycle(a).get("captured_ops") and not pops(a, pid), "")
    ea = a.dev("--dev-prefs-mode", a.mid, "recommended")
    check("C1.6 A enabled explicitly (app command); B enabled (UI)", ea.get("ok") and pview(a).get("mode") == pview(b).get("mode") == "recommended", "")

    # ---------------------------------------------------------------- CASE 2-3 snapshot A, cloud
    xe.stop(a)
    ra_ = read(a)
    r1 = cycle(a)
    xe.start(a)
    ops = pops(a, pid, a.device_id)
    types = {o["id"]: type(o["value"]).__name__ for o in ops}
    check("C2.9-11 A closed in order; capture extracted exactly the 4 allowlisted paths with A's values; 4 ops",
          ra_.get("values") == A_VALUES and r1.get("captured_ops") == 4, json.dumps({"values": ra_.get("values"), "ops": r1.get("captured_ops")}))
    check("C2.12 types: boolean, boolean, language string, boolean",
          types == {"download.prompt_for_download": "bool", "browser.enable_spellchecking": "bool", "intl.selected_languages": "str", "bookmark_bar.show_on_all_tabs": "bool"}, json.dumps(types))
    blob = json.dumps(a.dev("--dev-cloud-tree", pid).get("prefs_ops"))
    check("C3.14-17 published: col browser_preferences, id = path, each op carries value + browser kind (helium) + major (154)",
          sorted(o["id"] for o in ops) == sorted(ALLOW) and all(sorted(o["fields"]) == ["bk", "maj", "value"] and o["bk"] == "helium" and o["maj"] == 154 for o in ops),
          json.dumps(sorted(o["id"] for o in ops)))
    check("C2.13 + C3.18 cloud has nothing else: no other path, signin, sync, account_info, autofill, password_manager, extensions, content_settings, local paths",
          not any(w in blob for w in ("/Users", "/tmp", "default_directory", "homepage", "signin", "\"sync", "account_info", "autofill", "password_manager",
                                      "extensions", "content_settings", "accept_languages", "\"fr\"")), "")

    # ---------------------------------------------------------------- CASE 4-5 B pull/apply, byte preservation
    xe.stop(b)
    before_b = read(b)
    nbk = len(backups(b))
    r2 = cycle(b)
    after_b = read(b)
    xe.start(b)
    eb1 = eff(b)
    check("C4.19-21 B pulled before publishing its own snapshot (joining), Helium closed, applied the 4 paths; B's 'fr' never published",
          r2.get("paths_written") == 4 and r2.get("captured_ops") == 0 and not any(o.get("value") == "fr" for o in pops(a, pid)), json.dumps(r2))
    check("C4.22 backup outside the profile, staging removed (atomic swap), file valid JSON, verified",
          len(backups(b)) == nbk + 1 and not leftovers(b) and after_b.get("values") == A_VALUES, json.dumps({"backups": len(backups(b)) - nbk, "leftovers": leftovers(b)}))
    check("C4.23-24 B opened: the 4 effective values equal A's (settingsPrivate)", all(eb1[k] == v for k, v in A_VALUES.items()), json.dumps(eb1))
    check("C4.25 Helium adapter on B: show_on_all_tabs written, visibility_state not left overriding it",
          after_b.get("raw", {}).get("bookmark_bar.show_on_all_tabs") is True and "bookmark_bar.visibility_state" not in after_b.get("raw", {}), json.dumps(after_b.get("raw")))
    xe.restart(b)
    eb2 = eff(b)
    check("C4.26 values stay effective after another Helium relaunch", all(eb2[k] == v for k, v in A_VALUES.items()), json.dumps(eb2))
    check("C5.27-28 B's Preferences minus the allowlisted spans: byte-identical before/after the apply",
          before_b.get("rest_bytes_sha") and before_b.get("rest_bytes_sha") == after_b.get("rest_bytes_sha"), json.dumps({"before": before_b.get("rest_bytes_sha"), "after": after_b.get("rest_bytes_sha")}))
    check("C5.29 signin, extensions, content settings, account_info and an unknown key unchanged; Secure Preferences untouched",
          before_b.get("blocks") == after_b.get("blocks") and before_b["blocks"].get("synchrome_e2e_unknown") and before_b["blocks"].get("extensions")
          and before_b["blocks"].get("profile.content_settings") and before_b.get("secure_sha") == after_b.get("secure_sha"), json.dumps(before_b.get("blocks")))

    # ---------------------------------------------------------------- CASE 6 idempotency
    xe.stop(b)
    _, cb0, ev0 = ra.journal(a, pid, b.device_id)
    r6 = cycle(b)
    _, cb1, ev1 = ra.journal(a, pid, b.device_id)
    xe.start(b)
    ra6 = closed_cycle(a)
    M["idempotent"] += (r6.get("captured_ops") == 0) + (ra6.get("captured_ops") == 0)
    check("C6.30-32 B closed, equivalent snapshot: 0 ops, 0 chunks, 0 store_events (A too: 0 ops)",
          r6.get("captured_ops") == 0 and not r6.get("paths_written") and len(cb1) == len(cb0) and len(ev1) == len(ev0) and ra6.get("captured_ops") == 0, json.dumps([r6, ra6]))

    # ---------------------------------------------------------------- CASE 7 A -> B
    na = len(pops(a, pid, a.device_id))
    user_sets(a, {"download.prompt_for_download": False})
    closed_cycle(a)
    new = pops(a, pid, a.device_id)[na:]
    closed_cycle(b)
    e7 = eff(b)
    check("C7.33-36 A changed download.prompt_for_download: exactly one delta op for that path", [o["id"] for o in new] == ["download.prompt_for_download"] and new[0]["value"] is False,
          json.dumps([o["id"] for o in new]))
    check("C7.37-40 B applied closed: false; the rest intact", e7["download.prompt_for_download"] is False and all(e7[k] == A_VALUES[k] for k in ALLOW if k != "download.prompt_for_download"), json.dumps(e7))

    # ---------------------------------------------------------------- CASE 8 B -> A
    user_sets(b, {"intl.selected_languages": "ca,es,en"})
    closed_cycle(b)
    closed_cycle(a)
    e8 = eff(a)
    check("C8.41-46 B changed intl.selected_languages; A applied: equal", e8["intl.selected_languages"] == "ca,es,en", json.dumps(e8))
    check("C8.47 accept_languages recomputed by Helium from selected_languages, never synchronised by Synchrome",
          e8["intl.accept_languages"] == "ca,es,en" and not any(o["id"] == "intl.accept_languages" for o in pops(a, pid)), json.dumps(e8["intl.accept_languages"]))

    # ---------------------------------------------------------------- CASE 9 different paths concurrently
    user_sets(a, {"browser.enable_spellchecking": False})
    closed_cycle(a, offline=True)
    user_sets(b, {"download.prompt_for_download": True})
    closed_cycle(b, offline=True)
    closed_cycle(a)
    closed_cycle(b)
    closed_cycle(a)
    va, vb = eff(a), eff(b)
    check("C9.48-52 offline changes of different paths (A spellcheck, B download prompt), published later: both survive on both, no conflict",
          va["browser.enable_spellchecking"] is False and vb["browser.enable_spellchecking"] is False and va["download.prompt_for_download"] is True
          and vb["download.prompt_for_download"] is True and not pconflicts(a) and not pconflicts(b), json.dumps({"A": va, "B": vb}))

    # ---------------------------------------------------------------- CASE 10 same path, local pending (both choices)
    for choice, (bv, av) in (("keep_local", ("eu", "gl")), ("keep_remote", ("it", "nl"))):
        c = conflict_pair(a, b, "intl.selected_languages", {"intl.selected_languages": bv}, {"intl.selected_languages": av}, pid)
        check(f"C10.53-56 [{choice}] same path with B pending: browser_preferences/local_pending/edit_edit; B's value not overwritten",
              c.get("kind") == "local_pending" and c.get("subtype") == "edit_edit" and eff(b)["intl.selected_languages"] == bv
              and pview(b).get("state") == "conflict", json.dumps({k: c.get(k) for k in ("kind", "subtype", "name")}))
        if choice == "keep_local":
            ui, J, ok, row = ui_state(b)
            ui.js(J + "btn(prow(), 'Review conflicts').click(); return true;")
            card = ui.until(J + "const c = card('intl.selected_languages'); return c ? all('tr', c).map(r => r.innerText.replace(/\\s+/g, ' ').trim()) : null;", 20) or []
            page = ui.js(J + "return [prow().innerText, ...all('.conflicts').map(x => x.innerText)].join('\\n');") or ""
            ui.stop()
            check("C25.134-138 UI: Conflict badge + counts + 'Review conflicts'; the card compares Exists / Value of the path; no internal UUIDs shown",
                  row.get("badge") == "Conflict" and "Review conflicts" in row.get("buttons", []) and "1 conflicts" in row.get("text", "")
                  and any(r_.startswith("Exists") for r_ in card) and any(r_.startswith("Value") for r_ in card) and not cf.UUID.search(page), json.dumps(card))
        rr = resolve(b, c.get("conflict_id", ""), choice)
        closed_cycle(b)
        closed_cycle(a)
        r = closed_cycle(b)
        want = bv if choice == "keep_local" else av
        check(f"C10.57-59 {choice} through the common resolver: both converge on {want!r}; no echo afterwards",
              rr.get("outcome") in ("resolved", "resolving") and eff(a)["intl.selected_languages"] == eff(b)["intl.selected_languages"] == want
              and not pconflicts(b) and r.get("captured_ops") == 0, json.dumps(rr))

    # ---------------------------------------------------------------- CASE 11 LWW published
    user_sets(a, {"intl.selected_languages": "pt-BR,pt"})
    closed_cycle(a)
    closed_cycle(b)
    user_sets(b, {"intl.selected_languages": "sv"})
    closed_cycle(b)
    closed_cycle(a)
    lt = [o for o in pops(a, pid) if o["id"] == "intl.selected_languages"]
    newest = max(lt, key=lambda o: o["hlc"])
    check("C11.60-62 both published one after the other (no pending): the later HLC wins on both, no spurious conflict",
          eff(a)["intl.selected_languages"] == eff(b)["intl.selected_languages"] == "sv" and newest["by"] == b.device_id and newest["value"] == "sv"
          and not pconflicts(a) and not pconflicts(b), "")

    # ---------------------------------------------------------------- CASE 12 tombstones
    xe.stop(a)
    file_edit(a, drop("download.prompt_for_download"))
    rt = cycle(a)
    xe.start(a)
    closed_cycle(b)
    xe.stop(b)
    raw_b = read(b).get("raw", {})
    xe.start(b)
    regen_b = eff(b)["download.prompt_for_download"]
    xe.stop(b)
    regen_file = "download.prompt_for_download" in read(b).get("raw", {})
    rb12 = cycle(b)
    xe.start(b)
    check("C12.63-65 download.prompt_for_download removed from A's Preferences → tombstone; B deletes the key (B falls back to its default)",
          rt.get("captured_ops") == 1 and any(o["op"] == "delete" and o["id"] == "download.prompt_for_download" for o in pops(a, pid)) and "download.prompt_for_download" not in raw_b,
          json.dumps(rt))
    M["notes"].append(f"prompt_for_download after delete + relaunch: effective {regen_b}, regenerated in file: {regen_file}")
    check("C12.66a after a Helium relaunch the default is effective; if Helium regenerated the key it is captured as a value, not a conflict",
          regen_b is False and not pconflicts(b) and (rb12.get("captured_ops") == (1 if regen_file else 0)), json.dumps({"regenerated": regen_file, "ops": rb12.get("captured_ops")}))
    # a path Helium regenerates at startup: intl.selected_languages
    xe.stop(a)
    file_edit(a, drop("intl.selected_languages"))
    rt2 = cycle(a)
    xe.start(a)  # Helium writes its default languages again
    ra12 = closed_cycle(a)
    closed_cycle(b)
    closed_cycle(a)
    rq_a, rq_b = closed_cycle(a), closed_cycle(b)
    la, lb = eff(a)["intl.selected_languages"], eff(b)["intl.selected_languages"]
    M["notes"].append(f"selected_languages removed: A tombstone ops={rt2.get('captured_ops')}, A regenerated value captured ops={ra12.get('captured_ops')}")
    check("C12.66b intl.selected_languages removed → tombstone; Helium regenerates its default at startup, which is a new value (spec: absent = default; a regenerated default is an explicit value) → both converge, then quiet",
          rt2.get("captured_ops") == 1 and la == lb and la and rq_a.get("captured_ops") == 0 and rq_b.get("captured_ops") == 0,
          json.dumps({"tombstone_ops": rt2.get("captured_ops"), "regenerated_ops": ra12.get("captured_ops"), "A": la, "B": lb}))
    # an invalid value never becomes a tombstone
    xe.stop(a)
    keep = read(a).get("values", {})
    file_edit(a, lambda doc: doc.setdefault("download", {}).update({"prompt_for_download": "yes"}))
    r67 = cycle(a)
    sk = read(a).get("skipped", {})
    file_edit(a, lambda doc: doc.setdefault("download", {}).update({"prompt_for_download": keep.get("download.prompt_for_download", False)}) if "download.prompt_for_download" in keep else doc.get("download", {}).pop("prompt_for_download", None))
    xe.start(a)
    check("C12.67 a value outside the schema in A's file: skipped (invalid), no op, no tombstone",
          r67.get("captured_ops") == 0 and sk.get("download.prompt_for_download") == "invalid"
          and sum(o["op"] == "delete" and o["id"] == "download.prompt_for_download" for o in pops(a, pid)) == 1, json.dumps({"ops": r67.get("captured_ops"), "skipped": sk}))
    closed_cycle(a)

    # ---------------------------------------------------------------- CASE 13 edit vs delete (both choices)
    for choice in ("keep_local", "keep_remote"):
        user_sets(a, {"download.prompt_for_download": True})
        closed_cycle(a)
        closed_cycle(b)
        c = conflict_pair(a, b, "download.prompt_for_download", {"download.prompt_for_download": False}, drop("download.prompt_for_download"), pid)
        check(f"C13.68-70 [{choice}] B pending edit vs A's delete: edit_delete", c.get("subtype") == "edit_delete", json.dumps(c.get("subtype")))
        resolve(b, c.get("conflict_id", ""), choice)
        closed_cycle(b)
        closed_cycle(a)
        xe.stop(a)
        ra_raw = read(a).get("raw", {})
        xe.start(a)
        xe.stop(b)
        rb_raw = read(b).get("raw", {})
        xe.start(b)
        if choice == "keep_local":
            check("C13.71 Keep local: B's edit (false) comes back on A", ra_raw.get("download.prompt_for_download") is False and rb_raw.get("download.prompt_for_download") is False, json.dumps(ra_raw))
        else:
            check("C13.72 Keep remote: the key is deleted on B too", "download.prompt_for_download" not in rb_raw and "download.prompt_for_download" not in ra_raw, json.dumps(rb_raw))

    # ---------------------------------------------------------------- CASE 14 delete vs edit (both choices)
    for choice in ("keep_local", "keep_remote"):
        user_sets(a, {"download.prompt_for_download": False})
        closed_cycle(a)
        closed_cycle(b)
        c = conflict_pair(a, b, "download.prompt_for_download", drop("download.prompt_for_download"), {"download.prompt_for_download": True}, pid)
        check(f"C14.73-75 [{choice}] B pending delete vs A's edit: delete_edit", c.get("subtype") == "delete_edit", json.dumps(c.get("subtype")))
        resolve(b, c.get("conflict_id", ""), choice)
        closed_cycle(b)
        closed_cycle(a)
        xe.stop(a)
        ra_raw = read(a).get("raw", {})
        xe.start(a)
        xe.stop(b)
        rb_raw = read(b).get("raw", {})
        xe.start(b)
        if choice == "keep_local":
            check("C14.76 Keep local: the delete wins on both", "download.prompt_for_download" not in ra_raw and "download.prompt_for_download" not in rb_raw, json.dumps(ra_raw))
        else:
            check("C14.77 Keep remote: A's value restored on B", rb_raw.get("download.prompt_for_download") is True and ra_raw.get("download.prompt_for_download") is True, json.dumps(rb_raw))

    # ---------------------------------------------------------------- CASE 15 browser open
    user_sets(a, {"browser.enable_spellchecking": True})
    closed_cycle(a)
    nbk = len(backups(b))
    cur_b = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    rw = cycle(b)  # Helium B is OPEN
    M["retries_browser_open"] += 1
    sv = pview(b)
    ui, J, ok, row = ui_state(b)
    ui.stop()
    check("C15.78-84 B open: remote pending, Retry → Waiting for browser to close; Preferences not read/written; no backup; pending kept (downloaded, not applied)",
          rw.get("browser_running") is True and not rw.get("paths_written") and sv.get("state") == "waiting_for_browser_close" and sv.get("pending", 0) >= 1
          and len(backups(b)) == nbk and eff(b)["browser.enable_spellchecking"] is False, json.dumps({"report": rw, "state": sv.get("state"), "cursor_before": cur_b}))
    check("C25 UI: 'Waiting for browser to close' with Retry", row.get("badge") == "Waiting for browser to close" and "Retry" in row.get("buttons", []), json.dumps(row.get("badge")))
    closed_cycle(b)
    check("C15.85-87 closed + Retry: applied", eff(b)["browser.enable_spellchecking"] is True and pview(b).get("state") == "synced", "")

    # ---------------------------------------------------------------- CASE 16 LOCK
    user_sets(a, {"intl.selected_languages": "pl,en"})
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
    check("C16.88-90 Preferences flock held (Helium closed): refused, file byte-identical, no backup, Waiting",
          not rl.get("paths_written") and open(prefs_path(b), "rb").read() == raw0 and len(backups(b)) == nbk and sv.get("state") == "waiting_for_browser_close",
          json.dumps({"report": rl, "state": sv.get("state")}))
    r = cycle(b)
    xe.start(b)
    check("C16.91 lock released: Retry applies", r.get("paths_written", 0) >= 1 and eff(b)["intl.selected_languages"] == "pl,en", json.dumps(r))

    # ---------------------------------------------------------------- CASE 17 invalid JSON (harness temp file only)
    tmp = tempfile.mkdtemp(prefix="synchrome-e2e-badjson-", dir=ra.WORK)
    os.makedirs(os.path.join(tmp, "Default"))
    bad = b'{"intl":{"selected_languages":"es"},'
    open(os.path.join(tmp, "Default", "Preferences"), "wb").write(bad)
    w17 = b.dev("--dev-write-prefs", tmp, "Default", "helium", json.dumps({"intl.selected_languages": "de"}))
    r17 = b.dev("--dev-read-prefs", tmp, "Default", "helium")
    left17 = sorted(os.listdir(os.path.join(tmp, "Default")))
    check("C17.92-96 invalid JSON in a harness temp Preferences: writer and reader refuse, file not overwritten, no corrupt/staging file, B's test profile unaffected",
          "error" in w17 and "error" in r17 and open(os.path.join(tmp, "Default", "Preferences"), "rb").read() == bad and left17 == ["Preferences"]
          and eff(b)["intl.selected_languages"] == "pl,en", json.dumps({"write": w17.get("error"), "read": r17.get("error"), "files": left17}))
    shutil.rmtree(tmp, ignore_errors=True)

    # ---------------------------------------------------------------- CASE 18 schema-invalid remote ops
    for path, bad_v in (("download.prompt_for_download", "yes"), ("intl.selected_languages", ["es", "en"])):
        xe.stop(a)
        pr = a.dev("--dev-prefs-publish-raw", a.mid, path, json.dumps(bad_v))
        xe.start(a)
        xe.stop(b)
        before18 = open(prefs_path(b), "rb").read()
        r18 = cycle(b)
        same = open(prefs_path(b), "rb").read() == before18
        v18 = pview(b)
        xe.start(b)
        check(f"C18.97-100 remote op {path} = {json.dumps(bad_v)} (defective writer, schema bypassed): not applied, file byte-identical, counted invalid (state stays non-destructive), not a tombstone",
              pr.get("ops") == 1 and not r18.get("paths_written") and r18.get("invalid") == 1 and same and v18.get("invalid", 0) >= 1
              and v18.get("state") in ("synced", "error", "unsupported") and not any(o["op"] == "delete" and o["id"] == path and o["by"] == b.device_id for o in pops(a, pid)),
              json.dumps({"report": r18, "state": v18.get("state"), "invalid": v18.get("invalid")}))
        r18b = closed_cycle(a)  # A's real value is re-published by its next snapshot
        closed_cycle(b)
        check(f"C18.100b A's next snapshot re-publishes its real {path}; B converges", r18b.get("captured_ops") == 1 and eff(a)[path] == eff(b)[path], json.dumps(r18b))

    # ---------------------------------------------------------------- CASE 19 version mismatch
    user_sets(a, {"intl.selected_languages": "fi"})
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
    check("C19.101-105 op written by major 155 (unvalidated) on B (154): Version mismatch, NOT applied, Preferences byte-identical",
          not rv.get("paths_written") and rv.get("version_mismatch") == 1 and sv.get("state") == "version_mismatch" and open(prefs_path(b), "rb").read() == raw1,
          json.dumps({"report": rv, "state": sv.get("state")}))
    check("C25 UI: 'Version mismatch' with Retry", row.get("badge") == "Version mismatch" and "Retry" in row.get("buttons", []), json.dumps(row.get("badge")))
    old_b = set_major(b, "155.0.9999.1")
    rv2 = cycle(b)
    set_major(b, old_b)
    xe.start(b)
    check("C19.106-107 compatible (same) major on B + Retry: applied", rv2.get("paths_written") == 1 and eff(b)["intl.selected_languages"] == "fi", json.dumps(rv2))
    closed_cycle(a)
    closed_cycle(b)
    # an older browser than validated, also in the UI
    xe.stop(b)
    old_b = set_major(b, "150.0.1.1")
    cycle(b)
    su = pview(b)
    ui, J, ok, row = ui_state(b)
    ui.stop()
    set_major(b, old_b)
    cycle(b)
    xe.start(b)
    M["unsupported"] += su.get("state") == "unsupported"
    check("C19b + C25 browser major older than validated (150): Unsupported, 4 unsupported, UI 'Unsupported'; back on 154: synced",
          su.get("state") == "unsupported" and su.get("unsupported") == 4 and row.get("badge") == "Unsupported" and pview(b).get("state") == "synced",
          json.dumps({"state": su.get("state"), "badge": row.get("badge")}))

    # ---------------------------------------------------------------- CASE 20 browser kind not validated
    user_sets(a, {"download.prompt_for_download": False})
    closed_cycle(a)
    xe.stop(b)
    raw2 = open(prefs_path(b), "rb").read()
    rk = cycle(b, kind="brave")
    sk_ = pview(b)
    same_k = open(prefs_path(b), "rb").read() == raw2
    rk2 = cycle(b)
    xe.start(b)
    M["unsupported"] += sk_.get("state") == "unsupported"
    check("C20.108-111 B seen as Brave (not validated by E13): Unsupported, nothing captured or written; as Helium again: applied",
          sk_.get("state") == "unsupported" and not rk.get("paths_written") and not rk.get("captured_ops") and same_k and rk2.get("paths_written") == 1
          and not any(o.get("bk") == "brave" for o in pops(a, pid)), json.dumps({"report": rk, "state": sk_.get("state")}))

    # ---------------------------------------------------------------- CASE 21 Helium adapter
    for val in (False, True):
        user_sets(a, {"bookmark_bar.show_on_all_tabs": val})
        closed_cycle(a)
        xe.stop(b)
        before21 = read(b).get("raw", {})
        cycle(b)
        after21 = read(b).get("raw", {})
        xe.start(b)
        e21 = eff(b)["bookmark_bar.show_on_all_tabs"]
        r21 = closed_cycle(b)
        xe.stop(b)
        relaunched = read(b).get("raw", {})
        xe.start(b)
        check(f"C21.112-115 bookmark bar {val}: Synchrome writes show_on_all_tabs={val} and removes visibility_state; effective {val} after relaunch; next snapshot 0 ops",
              after21.get("bookmark_bar.show_on_all_tabs") is val and "bookmark_bar.visibility_state" not in after21 and e21 is val and r21.get("captured_ops") == 0,
              json.dumps({"before": {k: before21.get(k) for k in ("bookmark_bar.show_on_all_tabs", "bookmark_bar.visibility_state")},
                          "written": {k: after21.get(k) for k in ("bookmark_bar.show_on_all_tabs", "bookmark_bar.visibility_state")},
                          "after_helium": {k: relaunched.get(k) for k in ("bookmark_bar.show_on_all_tabs", "bookmark_bar.visibility_state")}}))

    # ---------------------------------------------------------------- CASE 22 excluded prefs
    na = len(pops(a, pid))
    user_sets(a, {"homepage": "https://example.com/synchrome-e2e", "homepage_is_newtabpage": False, "browser.show_home_button": True,
                  "spellcheck.dictionaries": ["es"], "translate.enabled": True, "spellcheck.use_spelling_service": True})
    xe.stop(a)
    exa = read(a).get("excluded", {})
    r22 = cycle(a)
    xe.start(a)
    xe.stop(b)
    exb0 = read(b).get("excluded", {})
    cycle(b)
    exb1 = read(b).get("excluded", {})
    xe.start(b)
    present = [k for k, v in exa.items() if v.get("preferences") or v.get("secure")]
    check("C22.116-118 homepage, homepage_is_newtabpage, show_home_button, accept_languages, dictionaries, translate, spelling service exist on A (Preferences or Secure Preferences) but produce 0 ops, never reach the cloud, B untouched",
          r22.get("captured_ops") == 0 and len(pops(a, pid)) == na and len(present) >= 5 and exb0 == exb1
          and not any(o["id"] in EXCLUDED for o in pops(a, pid)), json.dumps({"present_on_A": present, "ops": r22.get("captured_ops")}))

    # ---------------------------------------------------------------- CASE 23 restart (with a pending and a conflict alive)
    c = conflict_pair(a, b, "browser.enable_spellchecking", {"browser.enable_spellchecking": False}, {"browser.enable_spellchecking": True}, pid)
    user_sets(a, {"download.prompt_for_download": True})
    closed_cycle(a)
    b.dev("--dev-sync", b.mid)  # downloaded while open: pending
    cfg0, cur0 = pview(b), b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    xe.restart(b)
    restarted = cf.restart_app(b)
    cfg1 = pview(b)
    dup = b.dev("--dev-pull-duplicates", b.mid)
    check("C23.119-121 app + Helium restart: mode, checkpoint, pending, open conflict and last capture persisted",
          restarted and cfg0.get("mode") == cfg1.get("mode") == "recommended" and cfg1.get("conflicts") == 1 and cfg1.get("pending", 0) >= 1
          and cfg0.get("last_capture_at") == cfg1.get("last_capture_at") and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == cur0
          and [x["conflict_id"] for x in pconflicts(b)] == [c.get("conflict_id")], json.dumps({"before": cfg0, "after": cfg1}))
    r23 = closed_cycle(b)
    check("C23.122 duplicated events + repeated pull/capture: 0 duplicates, 0 ops", "chunks: 0, ops: 0" in dup.get("pull", "") and r23.get("captured_ops") == 0, json.dumps({"dup": dup.get("pull"), "r": r23}))
    resolve(b, c.get("conflict_id", ""), "keep_remote")
    closed_cycle(b)
    closed_cycle(a)

    # ---------------------------------------------------------------- CASE 24 offline
    before = eff(b)["intl.selected_languages"]
    user_sets(a, {"intl.selected_languages": "da,en"})
    closed_cycle(a)
    cur1 = b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through")
    xe.stop(b)
    raw24 = open(prefs_path(b), "rb").read()
    ro = cycle(b, offline=True)
    same24 = open(prefs_path(b), "rb").read() == raw24
    off = b.dev("--dev-sync", b.mid, offline=True)
    xe.start(b, offline=True)
    check("C24.123-125 B without Cloud: cursor intact, no partial apply (file byte-identical), Offline",
          not ro.get("paths_written") and same24 and eff(b)["intl.selected_languages"] == before and b.dev("--dev-remote-cursor", pid, a.device_id).get("applied_through") == cur1
          and "offline" in off.get("pull", ""), json.dumps({"pull": off.get("pull")}))
    closed_cycle(b)
    check("C24.126-128 Cloud back: pull/apply, converged", eff(b)["intl.selected_languages"] == "da,en", "")

    # ---------------------------------------------------------------- CASE 25 remaining UI states: Syncing, Error, Synced, Disable
    user_sets(a, {"browser.enable_spellchecking": False})
    closed_cycle(a)
    xe.stop(b)
    b.dev("--dev-sync", b.mid)
    sy = pview(b)
    labels = "".join(open(os.path.join(r_, f), errors="replace").read() for r_, _, fs in os.walk(os.path.join(h.ROOT, "apps", "desktop", "dist")) for f in fs if f.endswith(".js"))
    check("C25 Syncing (remote value downloaded, not yet written): state of list_browser_preferences; the UI maps it to 'Syncing'",
          sy.get("state") == "syncing" and sy.get("pending", 0) >= 1 and "Syncing" in labels, json.dumps({"state": sy.get("state")}))
    os.chmod(prefs_path(b), 0)
    re_ = cycle(b)
    ui, J, ok, row_e = ui_state(b)
    ui.stop()
    os.chmod(prefs_path(b), stat.S_IRUSR | stat.S_IWUSR)
    check("C25 Error (Preferences unreadable): badge Error + Retry; nothing written", re_.get("errors", 0) >= 1 and row_e.get("badge") == "Error" and "Retry" in row_e.get("buttons", []),
          json.dumps({"badge": row_e.get("badge"), "errors": re_.get("errors")}))
    ui, J, ok, row_s = ui_state(b)  # browser closed
    ui.js(J + "const r = prow(); const b = btn(r, 'Retry'); if (b) b.click(); return true;")
    retried = ui.until(J + "const t = (prow().querySelector('.badge')?.textContent || '').trim(); return t === 'Synced' ? t : null;", 90)
    row_s = ui.until(J + "const r = prow(); return r ? {text: r.innerText} : null;", 10) or {}
    ui.js(J + "const s = prow().querySelector('select'); s.value = 'off'; s.dispatchEvent(new Event('change', {bubbles: true})); return true;")
    disable = ui.until(J + "return btn(prow(), 'Disable') ? true : null;", 10)
    page = ui.js(J + "return [prow().innerText, ...all('.conflicts').map(x => x.innerText)].join('\\n');") or ""
    ui.stop()
    xe.start(b)
    check("C25 UI: Retry (clicked) after the error → Synced; counts 'preferences synced · unsupported · pending · conflicts'; Off offers 'Disable' (not clicked); no UUIDs or local paths shown",
          retried == "Synced" and all(w in row_s.get("text", "") for w in ("preferences synced", "unsupported", "pending", "conflicts")) and disable is True
          and pview(b).get("mode") == "recommended" and not cf.UUID.search(page) and "/tmp/" not in page and "/Users/" not in page, row_s.get("text", "")[:160])
    check("C25 applied after Retry", eff(b)["browser.enable_spellchecking"] is False, "")

    # ---------------------------------------------------------------- CASE 27 security (before USER_B takes the shared Keychain)
    for dv in (a, b):
        lg = dv.log()
        leaks = [w for w in ("selected_languages", "prompt_for_download", "show_on_all_tabs", "/Users/", "Application Support", "Preferences") if w in lg]
        check(f"C27.143 device {dv.name} host log: no preference paths/values, JWT, refresh/mapping tokens or local paths", not leaks and h.secret_free(lg), f"leaks={leaks}")
    check("C27.144 cloud holds only the four allowlisted paths", {o["id"] for o in pops(a, pid)} <= set(ALLOW), json.dumps(sorted({o["id"] for o in pops(a, pid)})))
    check("C27.145-147 Secure Preferences never written by Synchrome (digest equal across every B apply checked above); only disposable profiles used",
          before_b.get("secure_sha") == after_b.get("secure_sha") and a.udd.startswith(ra.WORK) and b.udd.startswith(ra.WORK), "")

    # ---------------------------------------------------------------- metrics, CASE 26 isolation
    xe.stop(a)
    xe.stop(b)
    allops = pops(a, pid)
    _, ca_, ea_ = ra.journal(a, pid, a.device_id)
    _, cb_, eb2_ = ra.journal(a, pid, b.device_id)
    avg = lambda l: int(sum(l) / len(l)) if l else None
    conf = pconflicts(b, closed=True)
    print("METRICS " + json.dumps({
        "preferences_synchronised": sorted({o["id"] for o in allops}),
        "ops_per_path": {p: sum(o["id"] == p for o in allops) for p in ALLOW},
        "sets": sum(o["op"] == "set" for o in allops), "tombstones": sum(o["op"] == "delete" for o in allops),
        "conflicts": len(conf), "conflict_kinds": sorted({c_["subtype"] for c_ in conf}), "keep_local": M["keep_local"], "keep_remote": M["keep_remote"],
        "applies": M["applies"], "paths_written": M["paths_written"], "backups": {"A": len(backups(a)), "B": len(backups(b))},
        "retries_browser_open": M["retries_browser_open"], "retries_lock": M["retries_lock"], "version_mismatch": M["version_mismatch"], "unsupported": M["unsupported"],
        "idempotent_snapshots": M["idempotent"], "chunks": {"A": len(ca_), "B": len(cb_)}, "store_events": {"A": len(ea_), "B": len(eb2_)},
        "capture_to_cloud_ms_mean": avg(M["capture_to_cloud_ms"]), "pull_to_apply_ms_mean": avg(M["pull_to_apply_ms"]), "cycles": M["cycles"], "notes": M["notes"],
    }))
    check("metrics collected; chunks == store_events per device", len(ca_) == len(ea_) and len(cb_) == len(eb2_), f"A {len(ca_)}/{len(ea_)} B {len(cb_)}/{len(eb2_)}")
    first = ca_[0]["path"] if ca_ else ""
    cdev = ra.Device("C")
    u = cdev.dev("--dev-sign-in", creds, "B")
    probe = cdev.dev("--dev-cloud-probe-foreign", h.EXPECTED["A"], pid, a.device_id, first) if u.get("signed_in") else {}
    refd = lambda v_: isinstance(v_, dict) and "Err" in v_
    check("C26.139 login USER_B (exact id)", u.get("user_id") == h.EXPECTED["B"], f"user_id={u.get('user_id')}")
    check("C26.140 USER_B cannot see A's profile/mappings, list/download A's journals, read A's store_events, write under A's prefix",
          probe.get("foreign_profile_visible") == {"Ok": False} and probe.get("foreign_mappings_visible") == {"Ok": 0} and probe.get("list_sees") == {"Ok": 0}
          and probe.get("get_first_chunk") == {"Ok": False} and probe.get("store_events_sees") == {"Ok": 0} and refd(probe.get("put_under_foreign_prefix")),
          json.dumps({k: v_ for k, v_ in probe.items() if k != "as_user"}))
    lv = b.dev("--dev-prefs-view", b.mid)
    lm = b.dev("--dev-prefs-mode", b.mid, "off")
    lc = b.dev("--dev-conflict-views", b.mid, "all")
    lr = b.dev("--dev-resolve", b.mid, (conf[0]["conflict_id"] if conf else "x"), "keep_local")
    leak = json.dumps([lv, lm, lc, lr])
    check("C26.141-142 USER_B cannot list or change A's installation's preferences, nor list/resolve its conflicts; no values leak",
          "error" in lv and "error" in lm and "error" in lc and ("error" in lr or (lr.get("results") or [{}])[0].get("outcome") == "error")
          and not any(w in leak for w in ("selected_languages", "\"da,en\"")), json.dumps({"view": lv.get("error"), "mode": lm.get("error"), "conflicts": lc.get("error")}))
    a.dev("--dev-sign-in", creds, "A")

    # ---------------------------------------------------------------- cleanup
    rb_ = b.dev("--dev-cleanup", b.mid)
    rbd = b.dev("--dev-delete-this-device")
    rA = a.dev("--dev-cleanup", a.mid)
    gone = a.dev("--dev-cloud-tree", pid)
    inv = a.dev("--dev-inventory")
    check("Z1 cloud: Storage objects, store_events, profile, mappings, devices removed", rb_.get("unmapped") and rbd.get("deleted") and rA.get("deleted_profile")
          and rA.get("device_deleted") and gone.get("objects") == 0 and gone.get("store_events") == 0 and inv.get("profiles") == 0 and inv.get("mappings") == 0
          and inv.get("devices") == [], json.dumps({"B": rb_, "A": rA}))
    so = a.dev("--dev-sign-out")
    check("Z2 Keychain session removed", so.get("signed_out") and not h.keychain_has_session(), "")
    for dv in (a, b, cdev):
        shutil.rmtree(dv.udd, ignore_errors=True)
        shutil.rmtree(dv.data, ignore_errors=True)
    shutil.rmtree(os.path.join(ra.WORK, "ui-driver"), ignore_errors=True)
    for f in os.listdir(ra.WORK):
        if f.startswith("synchrome-e2e-badjson-"):
            shutil.rmtree(os.path.join(ra.WORK, f), ignore_errors=True)
    if os.path.exists(os.path.join(ra.WORK, "ui-app.log")):
        os.remove(os.path.join(ra.WORK, "ui-app.log"))
    check("Z3 local: data dirs (state.sqlite v6 with browser_preferences_entities/_config + conflicts, registry, logs, Preferences backups), Helium profiles + manifests, temp profiles, UI files",
          not any(os.path.exists(p_) for dv in (a, b, cdev) for p_ in (dv.udd, dv.data)) and not os.listdir(ra.WORK), json.dumps(os.listdir(ra.WORK)))


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
