"""E13 — browser preferences allowlist (slice 12). DISPOSABLE profiles only.

For each candidate preference, on Helium and Google Chrome:
  1. does the settings UI know it (chrome.settingsPrivate.getPref from chrome://settings),
     with which type and default;
  2. set it to a non-default value through settingsPrivate, quit cleanly, and look where
     Chromium persisted it (Preferences vs Secure Preferences, MAC under protection.macs);
  3. edit it externally in Preferences (browser closed), relaunch, and check whether the
     browser kept it (effective value via settingsPrivate) or reset it;
  4. record `Last Version`, and whether Chromium rewrites unrelated bytes of Preferences
     (key order, escaping, formatting) after a relaunch without changes.

  python3 e13_preferences.py helium|chrome
"""
import json, os, shutil, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cdp import CDP  # noqa: E402

BINS = {
    "helium": "/Applications/Helium.app/Contents/MacOS/Helium",
    "chrome": "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
}
# candidate -> (value set via settingsPrivate, value written externally afterwards)
CANDIDATES = {
    "browser.show_home_button": (True, False),
    "homepage": ("https://example.com/synchrome-a", "https://example.com/synchrome-b"),
    "homepage_is_newtabpage": (False, True),
    "bookmark_bar.show_on_all_tabs": (True, False),
    "download.prompt_for_download": (True, False),
    "translate.enabled": (False, True),
    "spellcheck.dictionaries": (["es"], ["fr", "en-US"]),
    "spellcheck.use_spelling_service": (True, False),
    "intl.accept_languages": ("es,en", "fr,de"),
    "browser.enable_spellchecking": (False, True),
    "intl.selected_languages": (None, None),  # read-only probe
    "download.default_directory": (None, None),  # read-only probe (expected: local path)
}


PROCS = []


def launch(bin_, udd):
    try:
        os.remove(os.path.join(udd, "DevToolsActivePort"))
    except FileNotFoundError:
        pass
    PROCS.append(None)
    PROCS[-1] = subprocess.Popen([bin_, f"--user-data-dir={udd}", "--headless=new", "--no-first-run",
                             "--no-default-browser-check", "--disable-gpu", "--remote-debugging-port=0",
                             "chrome://settings/"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return PROCS[-1]


def settings_session(c):
    c.call("Target.createTarget", {"url": "chrome://settings/"})
    for _ in range(60):
        t = [x for x in c.call("Target.getTargets")["targetInfos"] if x["type"] == "page" and x["url"].startswith("chrome://settings")]
        if t:
            s = c.call("Target.attachToTarget", {"targetId": t[0]["targetId"], "flatten": True})["sessionId"]
            for _ in range(40):
                r = c.call("Runtime.evaluate", {"expression": "!!(window.chrome && chrome.settingsPrivate)", "returnByValue": True}, session=s)
                if r.get("result", {}).get("value"):
                    return s
                time.sleep(0.25)
        time.sleep(0.25)
    raise TimeoutError("chrome://settings not reachable: " + json.dumps([(x["type"], x["url"][:60]) for x in c.call("Target.getTargets")["targetInfos"]]))


def ev(c, s, expr):
    r = c.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True}, session=s)
    if "exceptionDetails" in r:
        return {"__error": r["exceptionDetails"].get("exception", {}).get("description", r["exceptionDetails"].get("text"))[:160]}
    return r.get("result", {}).get("value")


def quit_(p, c):
    try:
        c.call("Browser.close", timeout=10)
    except Exception:  # noqa: BLE001
        pass
    try:
        p.wait(30)
    except subprocess.TimeoutExpired:
        p.kill()
        print("  (killed)")


def get(d, path):
    for k in path.split("."):
        if not isinstance(d, dict) or k not in d:
            return "<absent>"
        d = d[k]
    return d


def put(d, path, v):
    ks = path.split(".")
    for k in ks[:-1]:
        d = d.setdefault(k, {})
    d[ks[-1]] = v


def prefs_of(c, s):
    return ev(c, s, "Promise.all(%s.map(k => new Promise(r => chrome.settingsPrivate.getPref(k, p => r(p ? {type: p.type, value: p.value, enforcement: p.enforcement || null, controlledBy: p.controlledBy || null} : null)))))" % json.dumps(list(CANDIDATES)))


def main(kind):
    bin_ = BINS[kind]
    udd = tempfile.mkdtemp(prefix=f"synchrome-e13-{kind}-")
    prof = os.path.join(udd, "Default")
    load = lambda f: json.load(open(os.path.join(prof, f))) if os.path.exists(os.path.join(prof, f)) else {}
    out = {"browser": kind, "version": subprocess.run([bin_, "--version"], capture_output=True, text=True).stdout.strip()}
    try:
        # run 1: defaults, then set through the settings API
        p = launch(bin_, udd); c = CDP(udd); s = settings_session(c)
        defaults = dict(zip(CANDIDATES, prefs_of(c, s) or []))
        set_res = {}
        for k, (v1, _) in CANDIDATES.items():
            if v1 is None or not defaults.get(k):
                continue
            set_res[k] = ev(c, s, "new Promise(r => chrome.settingsPrivate.setPref(%s, %s, '', ok => r({ok, err: chrome.runtime.lastError ? chrome.runtime.lastError.message : null})))" % (json.dumps(k), json.dumps(v1)))
        time.sleep(1)
        quit_(p, c)
        pr, sp = load("Preferences"), load("Secure Preferences")
        macs = get(sp, "protection.macs")
        def mac_of(k):
            return get(macs, k) != "<absent>" if isinstance(macs, dict) else False
        stored = {k: {"in_Preferences": get(pr, k) != "<absent>", "in_Secure": get(sp, k) != "<absent>", "mac": mac_of(k)} for k in CANDIDATES}
        last_version = open(os.path.join(udd, "Last Version")).read().strip() if os.path.exists(os.path.join(udd, "Last Version")) else None
        raw1 = open(os.path.join(prof, "Preferences"), "rb").read()
        # run 2: relaunch without changes -> does Chromium rewrite unrelated bytes?
        p = launch(bin_, udd); c = CDP(udd); s = settings_session(c); time.sleep(2); quit_(p, c)
        raw2 = open(os.path.join(prof, "Preferences"), "rb").read()
        # external edit (browser closed) of every candidate, in Preferences
        pr = load("Preferences")
        for k, (_, v2) in CANDIDATES.items():
            if v2 is not None and defaults.get(k):
                put(pr, k, v2)
        json.dump(pr, open(os.path.join(prof, "Preferences"), "w"), separators=(",", ":"))
        p = launch(bin_, udd); c = CDP(udd); s = settings_session(c)
        after = dict(zip(CANDIDATES, prefs_of(c, s) or []))
        time.sleep(1)
        quit_(p, c)
        pr3, sp3 = load("Preferences"), load("Secure Preferences")
        out["candidates"] = {}
        for k, (v1, v2) in CANDIDATES.items():
            d = defaults.get(k)
            out["candidates"][k] = {
                "known": d is not None,
                "type": d and d.get("type"),
                "default": (d or {}).get("value") if k != "download.default_directory" else ("<local path>" if d and d.get("value") else None),
                "enforcement": d and d.get("enforcement"),
                "set_via_api": set_res.get(k),
                "stored": stored[k],
                "external_edit_kept": (after.get(k) or {}).get("value") == v2 if v2 is not None and d else None,
                "file_after": "<redacted>" if k == "download.default_directory" else get(pr3, k),
            }
        out["last_version_file"] = last_version
        out["reset_marker"] = get(sp3, "prefs.preference_reset_time") != "<absent>" or get(pr3, "prefs.preference_reset_time") != "<absent>"
        out["preferences_format"] = {
            "single_line": b"\n" not in raw1.strip(),
            "keys_sorted_top": list(json.loads(raw1)) == sorted(json.loads(raw1)),
            "escapes_lt": b"\\u003C" in raw1,
            "relaunch_without_change_identical_bytes": raw1 == raw2,
            "relaunch_same_json": json.loads(raw1) == json.loads(raw2),
            "size": len(raw1),
        }
        if raw1 != raw2:
            a, b = json.loads(raw1), json.loads(raw2)
            out["preferences_format"]["changed_top_keys_on_relaunch"] = sorted(k for k in set(a) | set(b) if a.get(k) != b.get(k))
    finally:
        for p_ in PROCS:  # only the browsers this probe started
            if p_ and p_.poll() is None:
                p_.kill()
                p_.wait(10)
        shutil.rmtree(udd, ignore_errors=True)
    print(json.dumps(out, indent=1))


if __name__ == "__main__":
    main(sys.argv[1])
